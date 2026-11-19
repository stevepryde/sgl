//! Contact shadows (the architecture's Contact shadows): while
//! `Settings::contact_shadows` is not Off, a directional light holds the
//! frame's cascades, the camera's projection is a perspective one and the
//! ray-traced shadow stage does not run (`Effective::contact_shadows`), one
//! compute pass over the opaque depth finds the small casters the cascades
//! miss and writes the directional light's visibility into the contact
//! mask, which the opaque stage's lighting pass reads at its group 3
//! (`shading::bind::contact_shadow`). It ports Godot b130438's screen-space
//! contact shadows (contact/contact.wgsl), with the dispatch of
//! `SSEffects::screen_space_contact_shadows` (`ss_effects.cpp` 1794–1876)
//! and its mask's allocation (`sscs_allocate_buffers`, 1766–1792), MIT
//! (src/LICENSE-godot.txt). Both derive from Bend Studio's screen-space
//! shadows (bend_sss_gpu.h, and the dispatch of bend_sss_cpu.h's
//! `BuildDispatchList`), Copyright 2023 Sony Interactive Entertainment,
//! licensed under the Apache License, Version 2.0
//! (src/LICENSE-bend-screen-space-shadows.txt); this file is a changed
//! version of both: Rust; one dispatch over the light's bounds, as Godot's,
//! where Bend splits the screen into quadrants; SGL3D's clip space (y up,
//! reversed-Z), whose y turns over to pixels here as Bend's does; the
//! light's coordinate in f64; and the mask cleared by a render pass.
//!
//! Reads: the opaque depth (the G-buffer's, complete) and the frame's
//! jittered view-projection and shadowed directional light.
//! Writes: its own contact mask, held only while the stage runs and
//! reallocated when the render size changes, which it lends to the opaque
//! stage's lighting pass.
//! Honours: the effective contact shadows' reach, and the antialiasing,
//! whose temporal resolve turns on the dither.
//! Timing group: `contact shadows`.
use crate::settings::ContactShadows;
use crate::shading;
use crate::view::cached_group::CachedGroup;
use crate::view::frame::FrameContext;

/// Godot's wave: the pixels a workgroup walks toward the light together.
const WAVE_SIZE: i32 = 64;
/// The contact mask's format: the visibility in red. `R8Unorm`, Godot's,
/// is no core storage format.
pub(crate) const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// `ContactParams` in contact.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    screen_size: [i32; 2],
    light_offset: [i32; 2],
    light_coordinate: [f32; 4],
    blur: f32,
    frame: u32,
    /// WGSL rounds the struct up to its 16-byte alignment.
    padding: [u32; 2],
}

static READS_SHORT: shading::Module = shading::Module {
    name: "contact_reads_short",
    source: include_str!("contact/contact_reads_short.wgsl"),
    deps: &[],
};
static READS_MEDIUM: shading::Module = shading::Module {
    name: "contact_reads_medium",
    source: include_str!("contact/contact_reads_medium.wgsl"),
    deps: &[],
};
static READS_LONG: shading::Module = shading::Module {
    name: "contact_reads_long",
    source: include_str!("contact/contact_reads_long.wgsl"),
    deps: &[],
};
/// The pass at each reach: one source, its read steps its program's.
pub(crate) static SHORT: shading::Module = shading::Module {
    name: "contact_shadows_short",
    source: include_str!("contact/contact.wgsl"),
    deps: &[&shading::NOISE, &READS_SHORT],
};
pub(crate) static MEDIUM: shading::Module = shading::Module {
    name: "contact_shadows_medium",
    source: include_str!("contact/contact.wgsl"),
    deps: &[&shading::NOISE, &READS_MEDIUM],
};
pub(crate) static LONG: shading::Module = shading::Module {
    name: "contact_shadows_long",
    source: include_str!("contact/contact.wgsl"),
    deps: &[&shading::NOISE, &READS_LONG],
};
/// The entry point the pass's pipelines are created with.
pub(crate) const ENTRY: &str = "contact_shadows";

/// The program of `level`'s reach, and its index among the three; none for
/// Off.
fn program(level: ContactShadows) -> Option<(usize, &'static shading::Module)> {
    match level {
        ContactShadows::Off => None,
        ContactShadows::Short => Some((0, &SHORT)),
        ContactShadows::Medium => Some((1, &MEDIUM)),
        ContactShadows::Long => Some((2, &LONG)),
    }
}

/// Where the light falls on a screen of `size` pixels whose clip space
/// takes the direction toward it to `projected` (Godot's
/// `screen_space_contact_shadows`), as the shader's `light_coordinate`,
/// and the first wave's offset and the waves to dispatch, in its two
/// screen axes. Where `|w|` exceeds 1e-6 the light has a screen position:
/// its pixel, its depth and w's sign, which says whether rays converge on
/// it or diverge from it. Otherwise its rays are parallel on screen: their
/// direction, its larger axis 1, and w 0. SGL3D's clip y is up and its
/// pixels' y down, Godot's corrected clip y down, so y turns over here;
/// both depths are reversed-Z.
fn light_coordinate(
    projected: glam::DVec4,
    [width, height]: [f64; 2],
) -> ([f32; 4], [i32; 2], [u32; 2]) {
    let wave = f64::from(WAVE_SIZE);
    let (light, bound_start, bound_end) = if projected.w.abs() > 1e-6 {
        let light = glam::DVec4::new(
            (projected.x / projected.w * 0.5 + 0.5) * width,
            (0.5 - projected.y / projected.w * 0.5) * height,
            projected.z / projected.w,
            if projected.w > 0. { 1. } else { -1. },
        );
        let bound_start = [
            (-light.x / wave).floor() - 1.,
            (-light.y / wave).floor() - 1.,
        ];
        let bound_end = [
            ((width - light.x) / wave).ceil() + 2.,
            ((height - light.y) / wave).ceil() + 2.,
        ];
        (light, bound_start, bound_end)
    } else {
        let light = glam::DVec4::new(
            projected.x * width * 0.5,
            -projected.y * height * 0.5,
            projected.z,
            0.,
        );
        let light = light / light.x.abs().max(light.y.abs());
        let bound_start = [
            if light.x > 0. && light.x < light.y.abs() {
                -1.
            } else {
                0.
            },
            if light.y > 0. && light.y < light.x.abs() {
                -1.
            } else {
                0.
            },
        ];
        let bound_end = [
            (width / wave).ceil() + if light.x < 0. { 1. } else { 0. },
            (height / wave).ceil() + if light.y < 0. { 1. } else { 0. },
        ];
        (light, bound_start, bound_end)
    };
    let offset = bound_start.map(|start| start as i32 * WAVE_SIZE);
    let waves = [0, 1].map(|axis| (bound_end[axis] - bound_start[axis]).max(0.) as u32);
    (light.as_vec4().to_array(), offset, waves)
}

/// The contact shadow stage. Owns its pipelines, one a reach, made when
/// first used, its parameters and the contact mask while it runs.
pub(crate) struct ContactShadowStage {
    pipeline_layout: wgpu::PipelineLayout,
    pipelines: [Option<wgpu::ComputePipeline>; 3],
    params: wgpu::Buffer,
    group: CachedGroup,
    /// The contact mask and its size, while the stage runs.
    mask: Option<([u32; 2], wgpu::TextureView)>,
}

impl ContactShadowStage {
    pub fn new(device: &wgpu::Device) -> Self {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("contact shadows"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
                entry(
                    2,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
            ],
        });
        Self {
            pipeline_layout: device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("contact shadows"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            }),
            pipelines: [None, None, None],
            params: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("contact shadow parameters"),
                    size: std::mem::size_of::<Params>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            group: CachedGroup::new(layout),
            mask: None,
        }
    }

    /// Writes the contact mask of the frame's directional light with
    /// cascades, `directional` by its index in `FrameInput::directional_lights`,
    /// from the opaque depth, and returns it for the lighting pass; none,
    /// letting the mask go, where the stage does not run
    /// (`Effective::contact_shadows`).
    pub fn encode<'s>(
        &'s mut self,
        ctx: &mut FrameContext<'_>,
        directional: Option<usize>,
    ) -> Option<&'s wgpu::TextureView> {
        let Some(((index, module), directional)) = ctx
            .effective
            .contact_shadows
            .and_then(program)
            .zip(directional)
        else {
            self.mask = None;
            self.group.forget();
            return None;
        };
        let size = ctx.sizes.render;
        if self.mask.as_ref().is_none_or(|(held, _)| *held != size) {
            self.mask = Some((size, Self::mask(ctx, size)));
        }
        let pipeline = self.pipelines[index].get_or_insert_with(|| {
            let shader = ctx
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(module.name),
                    source: wgpu::ShaderSource::Wgsl(shading::compose(&[module]).into()),
                });
            ctx.device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(module.name),
                    layout: Some(&self.pipeline_layout),
                    module: &shader,
                    entry_point: Some(ENTRY),
                    compilation_options: Default::default(),
                    cache: None,
                })
        });
        // The direction toward the light, projected as the depth was
        // drawn: by the frame's jittered view-projection.
        let toward =
            glam::Vec3::from(ctx.values.frame.directional_lights[directional].direction_to_light)
                .normalize_or_zero()
                .as_dvec3();
        let projected = glam::Mat4::from_cols_array_2d(&ctx.values.view.view_projection).as_dmat4()
            * toward.extend(0.);
        let (light_coordinate, light_offset, waves) =
            light_coordinate(projected, size.map(f64::from));
        crate::counters::write_buffer(
            ctx.queue,
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                screen_size: size.map(|side| side as i32),
                light_offset,
                light_coordinate,
                blur: if ctx.effective.taa || ctx.effective.fsr {
                    1.
                } else {
                    0.
                },
                frame: ctx.values.frame.frame_count,
                padding: [0; 2],
            }),
        );
        let (_, mask) = self.mask.as_ref().unwrap();
        let group = self.group.get(
            ctx.device,
            "contact shadows",
            &[
                (0, wgpu::BindingResource::TextureView(&ctx.targets.depth)),
                (1, wgpu::BindingResource::TextureView(mask)),
                (2, self.params.as_entire_binding()),
            ],
        );
        {
            let mut pass = ctx
                .encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("contact shadows"),
                    timestamp_writes: ctx.timing.and_then(|t| t.compute_pass("contact shadows")),
                });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            if waves.iter().all(|&count| count > 0) {
                pass.dispatch_workgroups(WAVE_SIZE as u32, waves[0], waves[1]);
            }
        }
        Some(mask)
    }

    /// A contact mask of `size`, cleared to 1, as Godot clears its own when
    /// it allocates it: a pixel no wave writes is lit.
    fn mask(ctx: &mut FrameContext<'_>, [width, height]: [u32; 2]) -> wgpu::TextureView {
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("contact shadow mask"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("contact shadow mask clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        view
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
impl ContactShadowStage {
    /// The last frame's contact mask, where the stage ran.
    pub fn last(&self) -> Option<&wgpu::TextureView> {
        self.mask.as_ref().map(|(_, mask)| mask)
    }
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    [crate::shading::layout_tests::mirror!(
        "contact_shadows_medium",
        "ContactParams",
        Params,
        [screen_size, light_offset, light_coordinate, blur, frame]
    )]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
