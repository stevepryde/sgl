//! Volumetric fog: Godot's froxel volume (`fog.wgsl`), after Hillaire 2015.
//! A volume of froxels fills the camera's frustum out to the fog's length,
//! its depth slices spread toward the camera. Each frame bounds the froxels
//! each of the scene's fog volumes reaches, as Godot does (`volume_froxels`),
//! sums the frame's medium and the volumes reaching each froxel, lights it
//! with the directional lights through their cascades, the camera's clustered
//! point, spot and rectangle lights through their local-light shadows, and
//! the ambient light, blends it with where the froxel was in the last frame's
//! volume, filters each slice across x and then y with Godot's Gaussian,
//! then integrates each column front to back into the light scattered
//! toward the camera and the transmittance to every slice. Every draw that
//! fogs samples that volume where its point lies (`shading/fog.wgsl`), so
//! opaque surfaces, blended surfaces, effects and the sky (by
//! `Fog::sky_affect`) take one fog.
//!
//! Placement: after shadows and before opaque, as Godot updates its
//! volumetric fog before its opaque pass (`render_forward_clustered.cpp`
//! `_pre_opaque_render`): it needs the frame's shadow maps and clusters and
//! no depth.
//!
//! Reads: the camera's lit group 0 (its clusters, the scene's lights, the
//! frame's local-light atlas and cascades, its directional lights, hemisphere
//! fill and environment), the frame's `FrameInput::fog`, the scene's fog
//! volumes, the camera history, and its own last volume.
//! Writes: the froxels each fog volume reaches, its froxel volumes and the
//! integrated volume, which group 0 lends draws and source completion
//! samples.
//! Honours: the effective fog (`Settings::atmosphere`, `Settings::fog_quality`
//! and `FrameInput::atmosphere`); without it, it does not run and nothing
//! fogs. The filter runs with `Settings::fog_filter`.
//! Timing groups: `fog injection`, `fog filter`, `fog integration`.
use crate::settings::FogQuality;
use crate::view::bindings::FogVolume;
use crate::view::frame::FrameContext;

/// The injection under the lit group 0, and the filter and integration,
/// which bind no group 0.
pub(crate) static VOLUMETRIC_FOG: crate::shading::Module = crate::shading::Module {
    name: "volumetric_fog",
    source: include_str!("fog.wgsl"),
    deps: &[
        &crate::shading::BIND_LIT,
        &crate::shading::LIGHTS,
        &crate::shading::DIRECTIONAL_SHADOW,
        &crate::shading::ENVIRONMENT,
        &crate::shading::PBR,
        &crate::shading::RECT_LIGHT,
        &crate::shading::FOG,
    ],
};

/// What a froxel and its integration store: RGBA16F, which filters and
/// stores in compute everywhere.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The injection's workgroup side, in froxels along each axis.
const INJECT_GROUP: u32 = 4;
/// The filter's workgroup side, in invocations along x and y of one slice.
const FILTER_GROUP: u32 = 8;
/// The froxels each filter invocation filters along its pass's axis.
const FILTER_RUN: u32 = 8;
/// The integration's workgroup side, in columns along x and y.
const INTEGRATE_GROUP: u32 = 8;
/// Godot's `VolumetricFog::MAX_TEMPORAL_FRAMES`: the frames its froxel jitter
/// cycles through.
const TEMPORAL_FRAMES: u32 = 16;
/// The share of the last frame's volume a froxel keeps where it reprojects:
/// Godot's default `volumetric_fog_temporal_reproject_amount` (b130438
/// `scene/resources/environment.h`).
const TEMPORAL_REPROJECT_AMOUNT: f32 = 0.9;
/// Godot's anisotropy range.
const MAX_ANISOTROPY: f32 = 0.9;

/// `FroxelVolume` in fog.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct FroxelVolumeUniform {
    world_from_view: [[f32; 4]; 4],
    previous_clip_from_world: [[f32; 4]; 4],
    projection: [f32; 4],
    size: [u32; 3],
    frame: u32,
    albedo: [f32; 3],
    density: f32,
    render_size: [f32; 2],
    length: f32,
    detail_spread: f32,
    height: f32,
    height_falloff: f32,
    anisotropy: f32,
    temporal_blend: f32,
    ambient: f32,
    volume_count: u32,
    padding: [u32; 2],
}

/// The layouts this stage mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "volumetric_fog",
        "FroxelVolume",
        FroxelVolumeUniform,
        [
            world_from_view,
            previous_clip_from_world,
            projection,
            size,
            frame,
            albedo,
            density,
            render_size,
            length,
            detail_spread,
            height,
            height_falloff,
            anisotropy,
            temporal_blend,
            ambient,
            volume_count,
        ]
    )]
}

/// The constant this stage shares with its shader.
#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 1] {
    [crate::shading::layout_tests::Constant::new(
        "volumetric_fog",
        "FOG_MOST_VOLUMES",
        naga::Literal::U32(volume_froxels::MOST_VOLUMES as u32),
    )]
}

/// The froxels of `quality` for a frame of `render` pixels: Godot's volume
/// size across the frame's mean side, so its froxels stay near square, and
/// its depth slices.
pub(crate) fn froxels(quality: FogQuality, render: [u32; 2]) -> [u32; 3] {
    let (side, depth) = quality.volume();
    let [width, height] = render.map(|v| v.max(1) as f32);
    let ratio = width / ((width + height) / 2.);
    [
        ((side as f32 * ratio) as u32).max(1),
        ((side as f32 / ratio) as u32).max(1),
        depth,
    ]
}

/// The volumes of one froxel size.
struct Volumes {
    size: [u32; 3],
    /// The froxels, alternating: a frame writes the one at its history frame
    /// count's parity and reprojects the other.
    scattering: [wgpu::TextureView; 2],
    /// The history frame count each of `scattering` holds.
    holds: [Option<u32>; 2],
    /// Which of `scattering` the last integration read: the one its frame
    /// wrote, or with the filter the other, which holds them filtered.
    integrated_from: usize,
    /// Each column's integration.
    integrated: wgpu::TextureView,
    /// Injection into `scattering[i]` from the other, with the scene's fog
    /// volume buffer and the reached froxels' buffer they bind, until either
    /// is replaced.
    inject: Option<(wgpu::Buffer, wgpu::Buffer, [wgpu::BindGroup; 2])>,
    /// Integration of `scattering[i]`.
    integrate: [wgpu::BindGroup; 2],
    /// The filter of `scattering[i]`: along x into `integrated`, then along
    /// y into the other of `scattering`, which the injection has reprojected,
    /// so the history it leaves is unfiltered.
    filter: [[wgpu::BindGroup; 2]; 2],
}

pub(crate) struct VolumetricFog {
    inject: wgpu::ComputePipeline,
    /// Along x, then y.
    filter: [wgpu::ComputePipeline; 2],
    integrate: wgpu::ComputePipeline,
    inject_layout: wgpu::BindGroupLayout,
    filter_layout: wgpu::BindGroupLayout,
    integrate_layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    reached: volume_froxels::ReachedFroxels,
    /// Linear and clamped: reprojection's and every draw's.
    sampler: wgpu::Sampler,
    volumes: Volumes,
}

impl VolumetricFog {
    /// The fog's pipelines over the lit group 0 layout `lit`, with a volume
    /// of one froxel until a frame sizes it.
    pub fn new(device: &wgpu::Device, lit: &wgpu::BindGroupLayout) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("volumetric fog"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&VOLUMETRIC_FOG]).into()),
        });
        let uniform = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let texture = |binding, filterable| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable },
                view_dimension: wgpu::TextureViewDimension::D3,
                multisampled: false,
            },
            count: None,
        };
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: FORMAT,
                view_dimension: wgpu::TextureViewDimension::D3,
            },
            count: None,
        };
        let read_only_storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let inject_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("volumetric fog injection"),
            entries: &[
                uniform,
                texture(1, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                storage(3),
                read_only_storage(6),
                read_only_storage(9),
            ],
        });
        let filter_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("volumetric fog filter"),
            entries: &[uniform, texture(7, false), storage(8)],
        });
        let integrate_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("volumetric fog integration"),
            entries: &[uniform, texture(4, false), storage(5)],
        });
        let pipeline = |label,
                        layouts: &[Option<&wgpu::BindGroupLayout>],
                        entry,
                        constants: &[(&str, f64)]| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: layouts,
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants,
                    ..Default::default()
                },
                cache: None,
            })
        };
        let inject = pipeline(
            "volumetric fog injection",
            &[Some(lit), Some(&inject_layout)],
            "inject",
            &[],
        );
        let filter = [0., 1.].map(|axis| {
            pipeline(
                "volumetric fog filter",
                &[None, Some(&filter_layout)],
                "filter_froxels",
                &[("filter_axis", axis)],
            )
        });
        let integrate = pipeline(
            "volumetric fog integration",
            &[None, Some(&integrate_layout)],
            "integrate",
            &[],
        );
        let uniform = crate::counters::buffer(
            device,
            &wgpu::BufferDescriptor {
                label: Some("volumetric fog froxels"),
                size: size_of::<FroxelVolumeUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            },
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("volumetric fog"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let volumes = Self::volumes(device, &filter_layout, &integrate_layout, &uniform, [1; 3]);
        Self {
            inject,
            filter,
            integrate,
            inject_layout,
            filter_layout,
            integrate_layout,
            uniform,
            reached: volume_froxels::ReachedFroxels::new(device),
            sampler,
            volumes,
        }
    }

    fn volumes(
        device: &wgpu::Device,
        filter_layout: &wgpu::BindGroupLayout,
        integrate_layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        size: [u32; 3],
    ) -> Volumes {
        let volume = |label| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: size[2],
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D3,
                    format: FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let scattering = [volume("fog froxels"), volume("fog froxels")];
        let integrated = volume("integrated fog");
        let view = wgpu::BindingResource::TextureView;
        let integrate = [0, 1].map(|index| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("volumetric fog integration"),
                layout: integrate_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: view(&scattering[index]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: view(&integrated),
                    },
                ],
            })
        });
        let filter_pass = |source, dest| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("volumetric fog filter"),
                layout: filter_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: view(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 8,
                        resource: view(dest),
                    },
                ],
            })
        };
        let filter = [0, 1].map(|index| {
            [
                filter_pass(&scattering[index], &integrated),
                filter_pass(&integrated, &scattering[1 - index]),
            ]
        });
        Volumes {
            size,
            scattering,
            holds: [None; 2],
            integrated_from: 0,
            integrated,
            inject: None,
            integrate,
            filter,
        }
    }

    /// Sizes the volumes for `quality`, the effective fog's, at the render
    /// size `render`; without fog they stay as they are. A new size starts
    /// the volumes' history over.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        quality: Option<FogQuality>,
        render: [u32; 2],
    ) {
        let Some(quality) = quality else {
            return;
        };
        let size = froxels(quality, render);
        if self.volumes.size != size {
            self.volumes = Self::volumes(
                device,
                &self.filter_layout,
                &self.integrate_layout,
                &self.uniform,
                size,
            );
        }
    }

    /// The integrated volume and its sampler, which group 0 binds.
    pub fn volume(&self) -> FogVolume<'_> {
        FogVolume {
            view: &self.volumes.integrated,
            sampler: &self.sampler,
        }
    }

    /// The froxels the last frame wrote, which its successor reprojects,
    /// the froxels its integration read and the integrated volume.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn test_volumes(&self) -> [&wgpu::TextureView; 3] {
        let volumes = &self.volumes;
        let holds = volumes.holds;
        let last = usize::from(holds[1] > holds[0]);
        [
            &volumes.scattering[last],
            &volumes.scattering[volumes.integrated_from],
            &volumes.integrated,
        ]
    }

    /// Lights and integrates the frame's volume while the frame has fog.
    pub fn encode(&mut self, ctx: &mut FrameContext<'_>) {
        if ctx.effective.fog.is_none() {
            return;
        }
        let history = ctx.history;
        let volumes = &mut self.volumes;
        if !history.valid {
            volumes.holds = [None; 2];
        }
        let index = (history.frames % 2) as usize;
        let reprojects = history.valid
            && history
                .frames
                .checked_sub(1)
                .is_some_and(|last| volumes.holds[1 - index] == Some(last));
        volumes.holds[index] = Some(history.frames);
        let fog = ctx.input.fog;
        let camera = ctx.input.camera;
        let projection = camera.projection;
        let mut uniform = FroxelVolumeUniform {
            world_from_view: camera.view.inverse().to_cols_array_2d(),
            previous_clip_from_world: history.previous.to_cols_array_2d(),
            projection: [
                projection.x_axis.x,
                projection.y_axis.y,
                projection.z_axis.x,
                projection.z_axis.y,
            ],
            size: volumes.size,
            frame: history.frames % TEMPORAL_FRAMES,
            albedo: fog.albedo,
            density: fog.density.max(0.),
            render_size: ctx.sizes.render.map(|v| v as f32),
            length: fog.length,
            detail_spread: crate::shading::fog::DETAIL_SPREAD,
            height: fog.height,
            height_falloff: fog.height_falloff.max(0.),
            anisotropy: fog.anisotropy.clamp(-MAX_ANISOTROPY, MAX_ANISOTROPY),
            temporal_blend: if reprojects {
                TEMPORAL_REPROJECT_AMOUNT
            } else {
                0.
            },
            ambient: fog.ambient.max(0.),
            volume_count: 0,
            padding: [0; 2],
        };
        uniform.volume_count = self.reached.write(
            ctx.device,
            ctx.queue,
            &ctx.scene.transient.fog_volume_corners,
            camera.view,
            &uniform,
        );
        let fog_volumes = &ctx.scene.transient.fog_volumes;
        let reached = &self.reached.buffer;
        if volumes
            .inject
            .as_ref()
            .is_none_or(|(scene, bound, _)| scene != fog_volumes || bound != reached)
        {
            let groups = [0, 1].map(|index| {
                let view = wgpu::BindingResource::TextureView;
                ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("volumetric fog injection"),
                    layout: &self.inject_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.uniform.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: view(&volumes.scattering[1 - index]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: view(&volumes.scattering[index]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 6,
                            resource: fog_volumes.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 9,
                            resource: reached.as_entire_binding(),
                        },
                    ],
                })
            });
            volumes.inject = Some((fog_volumes.clone(), reached.clone(), groups));
        }
        let inject = &volumes.inject.as_ref().unwrap().2[index];
        crate::counters::write_buffer(ctx.queue, &self.uniform, 0, bytemuck::bytes_of(&uniform));
        let [x, y, z] = volumes.size;
        // Two passes, so the integration binds no group 0.
        let mut pass = ctx
            .encoder
            .begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("volumetric fog injection"),
                timestamp_writes: ctx.timing.and_then(|t| t.compute_pass("fog injection")),
            });
        pass.set_pipeline(&self.inject);
        pass.set_bind_group(0, ctx.bindings.camera_lit(), &[]);
        pass.set_bind_group(1, inject, &[]);
        pass.dispatch_workgroups(
            x.div_ceil(INJECT_GROUP),
            y.div_ceil(INJECT_GROUP),
            z.div_ceil(INJECT_GROUP),
        );
        drop(pass);
        // Godot's filter, after the history is kept and before integration
        // (fog.cpp volumetric_fog_update).
        volumes.integrated_from = index;
        if ctx.effective.fog_filter {
            let mut pass = ctx
                .encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("volumetric fog filter"),
                    timestamp_writes: ctx.timing.and_then(|t| t.compute_pass("fog filter")),
                });
            // A pass's invocations each filter a run along its axis.
            let groups = |side: u32, along: bool| {
                side.div_ceil(FILTER_GROUP * if along { FILTER_RUN } else { 1 })
            };
            for (axis, (pipeline, group)) in
                self.filter.iter().zip(&volumes.filter[index]).enumerate()
            {
                pass.set_pipeline(pipeline);
                pass.set_bind_group(1, group, &[]);
                pass.dispatch_workgroups(groups(x, axis == 0), groups(y, axis == 1), z);
            }
            // The other volume now holds this frame's filtered froxels. Its
            // hold is kept: only a retry of this frame, abandoned before the
            // GPU ran it, matches it, and then it still holds the last
            // frame's unfiltered froxels.
            volumes.integrated_from = 1 - index;
        }
        let mut pass = ctx
            .encoder
            .begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("volumetric fog integration"),
                timestamp_writes: ctx.timing.and_then(|t| t.compute_pass("fog integration")),
            });
        pass.set_pipeline(&self.integrate);
        pass.set_bind_group(1, &volumes.integrate[volumes.integrated_from], &[]);
        pass.dispatch_workgroups(x.div_ceil(INTEGRATE_GROUP), y.div_ceil(INTEGRATE_GROUP), 1);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
pub(crate) mod volume_froxels;
