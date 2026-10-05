//! The geometry pipelines: every pipeline that draws scene geometry from a
//! draw list, in one cache keyed by pass, by what the material and instance
//! require (face culling, alpha mode and deformed vertices: `variant`), by
//! the diagnostics layer constants and by whether the scene holds rectangle
//! lights and decals (`LitConstants`). The lighting pass while ray-traced
//! shadows run composes the shadow mask's provider (`shading::SHADOW_MASK`)
//! and binds the mask at its group 3; every other pass composes the
//! provider that holds no slot (`shading::SHADOW_MASK_NONE`).
use crate::Scene;
use crate::shading::{self, gbuffer};
use crate::view::targets::mask_targets;
use std::collections::HashMap;

mod key;
mod variant;
use key::PipelineKey;
pub(crate) use key::{LayerConstants, LitConstants};
pub(crate) use variant::{Alpha, Cull, Variant};

/// Scene geometry's camera and probe-capture passes. A program composes it
/// with one shadow-mask provider (`geometry_program`).
pub(crate) static GEOMETRY: shading::Module = shading::Module {
    name: "geometry",
    source: include_str!("geometry.wgsl"),
    deps: &[
        &shading::BIND_LIT,
        &shading::BIND_SCENE,
        &shading::BIND_MATERIAL,
        &shading::BIND_BLENDED,
        &shading::GBUFFER,
        &shading::VERTEX,
        &shading::VERTEX_PULL,
        &shading::SURFACE,
        &shading::SURFACE_RASTER,
        &shading::FRAME_FOG,
    ],
};
/// The geometry program: `GEOMETRY` with the shadow mask's provider where
/// `shadow_mask`, else with the provider that holds no slot.
pub(crate) fn geometry_program(shadow_mask: bool) -> String {
    let provider = if shadow_mask {
        &shading::SHADOW_MASK
    } else {
        &shading::SHADOW_MASK_NONE
    };
    shading::compose(&[&GEOMETRY, provider])
}

/// The directional and local-light shadow casters.
pub(crate) static CASTER: shading::Module = shading::Module {
    name: "caster",
    source: include_str!("caster.wgsl"),
    deps: &[
        &shading::BIND_SHADOW,
        &shading::BIND_SCENE,
        &shading::SCENE_RAYS,
        &shading::VERTEX_PULL,
        &shading::MATERIAL_RASTER,
    ],
};

/// What a geometry pass writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum GeometryPass {
    /// Lit colour and motion with depth: probe captures and standalone
    /// camera draws.
    Forward,
    /// The G-buffer and depth.
    GBuffer,
    /// Anisotropy over the G-buffer's depth, on devices whose attachment
    /// budget leaves it out of `GBuffer`.
    GBufferAnisotropy,
    /// Lit colour, its ambient diffuse, motion and source identity over the
    /// G-buffer's depth; with `shadow_mask`, the camera's surfaces take the
    /// lights the ray-traced shadow mask holds from it, bound at group 3
    /// (`shading::bind::shadow_mask`).
    Lighting { shadow_mask: bool },
    /// `GBuffer` and `Lighting` in one pass.
    Fused,
    /// A GPU-built directional cascade's casters, pulled from the scene
    /// source as the camera's are.
    DirectionalShadow,
    /// A probe capture's directional cascades' casters, from a CPU-built
    /// list, indexed from the geometry slabs.
    CaptureShadow,
    /// A local-light shadow face's casters, indexed from the geometry slabs.
    LocalShadow,
    /// Blended surfaces' lit colour over the beauty, tested against the
    /// opaque depth without writing it, with the blended group 3
    /// (`shading::bind::blended`); with `fsr2_masks`, also FSR2's reactive
    /// and transparency-and-composition masks (`mask_targets`).
    Blended { fsr2_masks: bool },
    /// Blended receivers of screen-space reflections as the surface: their
    /// traced lobe into the receiver layer and their motion into the
    /// G-buffer's, over the surface depth, tested strictly nearer and
    /// written. Draws only the receiver batches of the blended list.
    Receivers,
}

impl GeometryPass {
    fn caster(self) -> bool {
        matches!(
            self,
            Self::DirectionalShadow | Self::CaptureShadow | Self::LocalShadow
        )
    }

    /// Whether this pass draws materials whose alpha mode requires `alpha`.
    fn draws(self, alpha: Alpha) -> bool {
        matches!(self, Self::Blended { .. } | Self::Receivers) == (alpha == Alpha::Blend)
    }

    /// Whether the pass draws nonindexed pulled vertices instead of indexed
    /// vertex buffers: every camera and probe pass, so that the split and
    /// fused forms rasterize one primitive stream (`source_vs`), and a
    /// GPU-built cascade's, whose draw instances are sections; the CPU-built
    /// lists' shadow casters, which take no derivatives, draw indexed
    /// positions (`CasterVertex`).
    ///
    /// No camera or probe pass may draw indexed vertex buffers. On Apple
    /// GPUs (M5, Metal) an indexed geometry pass is not deterministic during
    /// a process's first frames, while the driver still grows the tiler's
    /// parameter buffer and splits passes into partial renders (Rosenzweig,
    /// "The Apple GPU and the impossible bug"): two encodes of the same pass
    /// in one frame differ at primitive-edge quads in every output that
    /// uses screen-space derivatives (the mapped normal, the
    /// variance-filtered roughness), by up to a few per cent, which FSR2's
    /// history then keeps (#354). The same pass over pulled vertices is
    /// bit-exact between encodes and between runs.
    pub fn pulled(self) -> bool {
        !matches!(self, Self::CaptureShadow | Self::LocalShadow)
    }
}

/// Which alpha modes the scene's materials use beyond opaque, whether one
/// is a receiver of screen-space reflections, and whether it holds a
/// deforming model: the masked, blended, receiver and deformed pipelines are
/// prepared once it holds such content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Content {
    mask: bool,
    blend: bool,
    receivers: bool,
    deformed: bool,
}

impl Content {
    /// What `scene` holds.
    fn of(scene: &Scene) -> Self {
        Self {
            mask: scene.materials.holds_masked(),
            blend: scene.materials.holds_blended(),
            receivers: scene.materials.holds_receivers(),
            deformed: scene.models.holds_deforming(),
        }
    }
}

/// The depth write and test of each pass. A material/depth prepass and its
/// Equal passes must select the same last draw when distinct materials have
/// indistinguishable device depth. The receiver pass tests strictly nearer,
/// so a receiver coplanar with opaque geometry leaves that the surface, and
/// the blended draw nearer or equal.
pub(crate) fn depth(pass: GeometryPass) -> (bool, wgpu::CompareFunction) {
    use wgpu::CompareFunction::*;
    match pass {
        GeometryPass::Forward
        | GeometryPass::DirectionalShadow
        | GeometryPass::CaptureShadow
        | GeometryPass::LocalShadow
        | GeometryPass::Receivers => (true, Greater),
        GeometryPass::GBuffer | GeometryPass::Fused => (true, GreaterEqual),
        GeometryPass::GBufferAnisotropy | GeometryPass::Lighting { .. } => (false, Equal),
        GeometryPass::Blended { .. } => (false, GreaterEqual),
    }
}

pub(crate) struct GeometryPipelines {
    /// Group 0 lit, then scene and material.
    lit: wgpu::PipelineLayout,
    /// `lit`'s, then the blended group 3.
    blended: wgpu::PipelineLayout,
    /// `lit`'s, then the shadow mask's group 3.
    shadow_masked: wgpu::PipelineLayout,
    /// Group 0 shadow, then scene and material.
    shadow: wgpu::PipelineLayout,
    geometry: wgpu::ShaderModule,
    /// The geometry program with the shadow mask's provider, made when the
    /// ray-traced shadows first run.
    geometry_shadow_masked: Option<wgpu::ShaderModule>,
    caster: wgpu::ShaderModule,
    cache: HashMap<PipelineKey, wgpu::RenderPipeline>,
    /// The diagnostics layers `get` returns pipelines for.
    layers: LayerConstants,
    /// The lit constants `get` returns pipelines for.
    lit_constants: LitConstants,
    /// The content `get` returns pipelines for beyond opaque, rigid
    /// geometry.
    content: Content,
    /// Whether `get` returns the lighting pipelines that take the shadow
    /// mask, prepared once ray-traced shadows run.
    shadow_mask: bool,
    /// Whether `GBuffer` writes anisotropy itself; otherwise
    /// `GBufferAnisotropy` does.
    pub anisotropy_inline: bool,
    /// Whether the device has the attachments `Fused` writes.
    pub fused_supported: bool,
    /// Whether the device draws `DirectionalShadow` with unclipped depth
    /// (`DEPTH_CLIP_CONTROL`); otherwise its casters emulate it.
    unclipped_depth: bool,
}

/// The colour targets `pass` writes, `GBuffer` with anisotropy when
/// `anisotropy_inline`.
fn targets(pass: GeometryPass, anisotropy_inline: bool) -> Vec<wgpu::TextureFormat> {
    use GeometryPass::*;
    match pass {
        Forward => vec![gbuffer::COLOR, gbuffer::MOTION],
        GBuffer => {
            let mut targets = vec![
                gbuffer::NORMAL,
                gbuffer::MATERIAL,
                gbuffer::MOTION,
                gbuffer::F0,
            ];
            if anisotropy_inline {
                targets.push(gbuffer::ANISOTROPY);
            }
            targets
        }
        GBufferAnisotropy => vec![gbuffer::ANISOTROPY],
        Lighting { .. } => vec![
            gbuffer::COLOR,
            gbuffer::AMBIENT,
            gbuffer::MOTION,
            gbuffer::SOURCE_ID,
        ],
        Fused => vec![
            gbuffer::NORMAL,
            gbuffer::MATERIAL,
            gbuffer::MOTION,
            gbuffer::F0,
            gbuffer::COLOR,
            gbuffer::AMBIENT,
            gbuffer::SOURCE_ID,
            gbuffer::ANISOTROPY,
        ],
        DirectionalShadow | CaptureShadow | LocalShadow => Vec::new(),
        Blended { .. } => vec![gbuffer::COLOR],
        Receivers => vec![gbuffer::RECEIVER, gbuffer::MOTION],
    }
}

/// Whether a device with `limits` takes a pass writing `formats`: their count,
/// and their bytes per sample as WebGPU counts them, each format's byte cost
/// after aligning the running total to its component alignment
/// (https://gpuweb.github.io/gpuweb/#abstract-opdef-calculating-color-attachment-bytes-per-sample,
/// wgpu-core's `validate_color_attachment_bytes_per_sample`). That charges
/// Rgba8Unorm eight bytes although it stores four.
fn attachments_fit(limits: &wgpu::Limits, formats: &[wgpu::TextureFormat]) -> bool {
    let bytes = formats.iter().fold(0, |total: u32, format| {
        total.next_multiple_of(format.target_component_alignment().unwrap())
            + format.target_pixel_byte_cost().unwrap()
    });
    formats.len() <= limits.max_color_attachments as usize
        && bytes <= limits.max_color_attachment_bytes_per_sample
}

impl GeometryPipelines {
    /// Creates every pipeline the frame and probe captures draw with for
    /// `layers`, over group 0's `lit` and `shadow` layouts, the scene's and
    /// a material's, and the blended and shadow-mask group 3 layouts.
    pub fn new(
        device: &wgpu::Device,
        [lit, shadow, scene, material, blended, shadow_mask]: [&wgpu::BindGroupLayout; 6],
        layers: LayerConstants,
    ) -> Self {
        let layout = |label, groups: &[&wgpu::BindGroupLayout]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &groups.iter().map(|&group| Some(group)).collect::<Vec<_>>(),
                immediate_size: 0,
            })
        };
        let limits = device.limits();
        let mut pipelines = Self {
            lit: layout("lit scene geometry", &[lit, scene, material]),
            blended: layout("blended scene geometry", &[lit, scene, material, blended]),
            shadow_masked: layout(
                "shadow-masked scene lighting",
                &[lit, scene, material, shadow_mask],
            ),
            shadow: layout("shadow casters", &[shadow, scene, material]),
            geometry: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("SGL material"),
                source: wgpu::ShaderSource::Wgsl(geometry_program(false).into()),
            }),
            geometry_shadow_masked: None,
            caster: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("shadow casters"),
                source: wgpu::ShaderSource::Wgsl(shading::compose(&[&CASTER]).into()),
            }),
            cache: HashMap::new(),
            layers,
            lit_constants: LitConstants::default(),
            content: Content::default(),
            shadow_mask: false,
            anisotropy_inline: attachments_fit(&limits, &targets(GeometryPass::GBuffer, true)),
            fused_supported: attachments_fit(&limits, &targets(GeometryPass::Fused, true)),
            unclipped_depth: device
                .features()
                .contains(wgpu::Features::DEPTH_CLIP_CONTROL),
        };
        pipelines.prepare_layers(device);
        pipelines
    }

    /// `get` returns pipelines compiled with `layers` for `scene` from now
    /// on: shading rectangle lights and applying decals while it holds one,
    /// and for the alpha modes its materials use and its deforming models,
    /// created here on first use, with the lighting pipelines that take the
    /// shadow mask once `shadow_mask` (ray-traced shadows run). Without
    /// diagnostics every frame uses `ALL`.
    pub fn specialise(
        &mut self,
        device: &wgpu::Device,
        layers: LayerConstants,
        scene: &Scene,
        shadow_mask: bool,
    ) {
        let lit_constants = LitConstants::of(scene);
        let content = Content::of(scene);
        let shadow_mask = self.shadow_mask || shadow_mask;
        if (
            self.layers,
            self.lit_constants,
            self.content,
            self.shadow_mask,
        ) != (layers, lit_constants, content, shadow_mask)
        {
            self.layers = layers;
            self.lit_constants = lit_constants;
            self.content = content;
            self.shadow_mask = shadow_mask;
            if shadow_mask && self.geometry_shadow_masked.is_none() {
                self.geometry_shadow_masked =
                    Some(device.create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("SGL material with the shadow mask"),
                        source: wgpu::ShaderSource::Wgsl(geometry_program(true).into()),
                    }));
            }
            self.prepare_layers(device);
        }
    }

    /// Creates the current specialisation's pipelines: each geometry pass
    /// the device takes and the two casters, for each cull, each alpha mode
    /// the scene uses and, for pulled passes, deformed vertices while it
    /// holds a deforming model.
    fn prepare_layers(&mut self, device: &wgpu::Device) {
        let (layers, lit_constants) = (self.layers, self.lit_constants);
        let mut passes = vec![
            GeometryPass::Forward,
            GeometryPass::GBuffer,
            GeometryPass::Lighting { shadow_mask: false },
            GeometryPass::DirectionalShadow,
            GeometryPass::CaptureShadow,
            GeometryPass::LocalShadow,
            GeometryPass::Blended { fsr2_masks: false },
            GeometryPass::Blended { fsr2_masks: true },
        ];
        if !self.anisotropy_inline {
            passes.push(GeometryPass::GBufferAnisotropy);
        }
        if self.fused_supported {
            passes.push(GeometryPass::Fused);
        }
        if self.content.receivers {
            passes.push(GeometryPass::Receivers);
        }
        if self.shadow_mask {
            passes.push(GeometryPass::Lighting { shadow_mask: true });
        }
        let mut alphas = vec![Alpha::Opaque];
        if self.content.mask {
            alphas.push(Alpha::Mask);
        }
        if self.content.blend {
            alphas.push(Alpha::Blend);
        }
        let deformed: &[bool] = if self.content.deformed {
            &[false, true]
        } else {
            &[false]
        };
        for pass in passes {
            for &alpha in alphas.iter().filter(|&&alpha| pass.draws(alpha)) {
                for cull in [Cull::None, Cull::Back, Cull::Front] {
                    for &deformed in deformed {
                        let variant = Variant {
                            cull,
                            alpha,
                            deformed,
                        };
                        let key = PipelineKey::new(pass, variant, layers, lit_constants);
                        self.prepare(device, key);
                    }
                }
            }
        }
    }

    /// Creates the pipeline for `key` unless it exists.
    fn prepare(&mut self, device: &wgpu::Device, key: PipelineKey) {
        if !self.cache.contains_key(&key) {
            let pipeline = self.create(device, key);
            self.cache.insert(key, pipeline);
        }
    }

    /// `pass`'s pipeline for `variant`.
    pub fn get(&self, pass: GeometryPass, variant: Variant) -> &wgpu::RenderPipeline {
        let key = PipelineKey::new(pass, variant, self.layers, self.lit_constants);
        self.cache
            .get(&key)
            .unwrap_or_else(|| panic!("geometry pipeline {key:?} was not prepared"))
    }

    fn create(&self, device: &wgpu::Device, key: PipelineKey) -> wgpu::RenderPipeline {
        use GeometryPass::*;
        let masked = key.variant.alpha == Alpha::Mask;
        let vertex_buffers = &shading::vertex::GEOMETRY_BUFFERS;
        let (module, layout, label) = match key.pass {
            DirectionalShadow | CaptureShadow | LocalShadow => {
                (&self.caster, &self.shadow, "shadow caster")
            }
            Blended { .. } => (&self.geometry, &self.blended, "blended scene geometry"),
            Receivers => (&self.geometry, &self.lit, "blended receivers"),
            Lighting { shadow_mask: true } => (
                self.geometry_shadow_masked
                    .as_ref()
                    .expect("the shadow-masked program is made with its pipelines"),
                &self.shadow_masked,
                "shadow-masked scene lighting",
            ),
            _ => (&self.geometry, &self.lit, "lit scene geometry"),
        };
        // Casters are depth-only: their cull selects the side that casts. A
        // directional cascade's depth is unclipped, so casters between the
        // light and the cascade are drawn at its near plane, as Bevy's
        // `UNCLIPPED_DEPTH_ORTHO` shadow views and Wicked's directional
        // shadow cameras draw them; a device without `DEPTH_CLIP_CONTROL`
        // emulates it in the shader, as Bevy does. A masked material's
        // casters discard what it cuts out, as Bevy's do (MAY_DISCARD).
        let (vertex, fragment) = match key.pass {
            DirectionalShadow if masked && !self.unclipped_depth => (
                "shadow_pulled_masked_unclipped_vs",
                Some("shadow_masked_unclipped_fs"),
            ),
            DirectionalShadow if masked => ("shadow_pulled_masked_vs", Some("shadow_masked_fs")),
            DirectionalShadow if !self.unclipped_depth => {
                ("shadow_pulled_unclipped_vs", Some("shadow_unclipped_fs"))
            }
            DirectionalShadow => ("shadow_pulled_vs", None),
            CaptureShadow if masked && !self.unclipped_depth => (
                "shadow_masked_unclipped_vs",
                Some("shadow_masked_unclipped_fs"),
            ),
            CaptureShadow | LocalShadow if masked => ("shadow_masked_vs", Some("shadow_masked_fs")),
            Forward => ("source_vs", Some("fs")),
            GBuffer if self.anisotropy_inline => ("source_vs", Some("stable_fs")),
            GBuffer => ("source_vs", Some("stable_legacy_fs")),
            GBufferAnisotropy => ("source_vs", Some("anisotropy_fs")),
            Lighting { .. } => ("source_vs", Some("source_fs")),
            Fused => ("source_vs", Some("fused_opaque_fs")),
            CaptureShadow if !self.unclipped_depth => {
                ("shadow_unclipped_vs", Some("shadow_unclipped_fs"))
            }
            CaptureShadow | LocalShadow => ("shadow_vs", None),
            Blended { fsr2_masks: false } => ("source_vs", Some("blended_fs")),
            Blended { fsr2_masks: true } => ("source_vs", Some("blended_fsr2_masked_fs")),
            Receivers => ("source_vs", Some("receiver_fs")),
        };
        let unclipped_depth =
            matches!(key.pass, DirectionalShadow | CaptureShadow) && self.unclipped_depth;
        // Blended surfaces blend over the beauty with their alpha, as Bevy's
        // `BLEND_ALPHA` pipelines do (crates/bevy_pbr/src/render/mesh.rs).
        let blend = matches!(key.pass, Blended { .. }).then_some(wgpu::BlendState::ALPHA_BLENDING);
        let mut targets: Vec<_> = targets(key.pass, self.anisotropy_inline)
            .into_iter()
            .map(|format| {
                Some(wgpu::ColorTargetState {
                    format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })
            })
            .collect();
        if key.pass == (Blended { fsr2_masks: true }) {
            targets.extend(mask_targets());
        }
        let constants = key.constants();
        let (depth_write, depth_compare) = depth(key.pass);
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some(vertex),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
                buffers: if key.pass.pulled() {
                    &vertex_buffers[..1]
                } else {
                    vertex_buffers
                },
            },
            fragment: fragment.map(|fragment| wgpu::FragmentState {
                module,
                entry_point: Some(fragment),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
                targets: &targets,
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: key.variant.cull.face(),
                unclipped_depth,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: gbuffer::DEPTH,
                depth_write_enabled: Some(depth_write),
                depth_compare: Some(depth_compare),
                stencil: Default::default(),
                // Casters store their depth unbiased: Bevy offsets the
                // receiver instead, by its normal and depth bias
                // (shadow_receiver_offset).
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        })
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod primary_depth_precision_tests;
