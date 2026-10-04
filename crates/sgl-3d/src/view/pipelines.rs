//! The geometry pipelines: every pipeline that draws scene geometry from a
//! draw list, in one cache keyed by pass, by what the material and instance
//! require (face culling, alpha mode and deformed vertices: `variant`), by
//! the diagnostics layer constants and by whether the scene holds rectangle
//! lights and decals (`LitConstants`).
use crate::Scene;
use crate::settings::DisabledLayers;
use crate::shading::{self, gbuffer};
use crate::view::targets::mask_targets;
use std::collections::HashMap;

mod variant;
pub(crate) use variant::{Alpha, Cull, Variant};

/// Scene geometry's camera and probe-capture passes.
pub(crate) static GEOMETRY: shading::Module = shading::Module {
    name: "geometry",
    source: include_str!("geometry.wgsl"),
    deps: &[
        &shading::BIND_LIT,
        &shading::BIND_SCENE,
        &shading::BIND_MATERIAL,
        &shading::GBUFFER,
        &shading::VERTEX,
        &shading::VERTEX_PULL,
        &shading::SURFACE,
        &shading::SURFACE_RASTER,
        &shading::FRAME_FOG,
    ],
};
/// The directional and local-light shadow casters.
pub(crate) static CASTER: shading::Module = shading::Module {
    name: "caster",
    source: include_str!("caster.wgsl"),
    deps: &[
        &shading::BIND_SHADOW,
        &shading::BIND_SCENE,
        &shading::SCENE_RAYS,
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
    /// G-buffer's depth.
    Lighting,
    /// `GBuffer` and `Lighting` in one pass.
    Fused,
    DirectionalShadow,
    LocalShadow,
    /// Blended surfaces' lit colour over the beauty, tested against the
    /// opaque depth without writing it; with `fsr2_masks`, also FSR2's
    /// reactive and transparency-and-composition masks (`mask_targets`).
    Blended {
        fsr2_masks: bool,
    },
}

impl GeometryPass {
    fn caster(self) -> bool {
        matches!(self, Self::DirectionalShadow | Self::LocalShadow)
    }

    /// Whether this pass draws materials whose alpha mode requires `alpha`.
    fn draws(self, alpha: Alpha) -> bool {
        matches!(self, Self::Blended { .. }) == (alpha == Alpha::Blend)
    }

    /// Whether the pass draws nonindexed pulled vertices instead of indexed
    /// vertex buffers: every camera and probe pass, so that the split and
    /// fused forms rasterize one primitive stream (`source_vs`); shadow
    /// casters, which take no derivatives, draw indexed positions
    /// (`CasterVertex`).
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
        !self.caster()
    }
}

/// The geometry shader's diagnostics layers, compiled as pipeline constants.
/// Every layer is on outside diagnostics builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LayerConstants {
    pub normal_maps: bool,
    pub bump_maps: bool,
    pub baked_lighting: bool,
    pub instance_emission: bool,
}

impl LayerConstants {
    pub const ALL: Self = Self {
        normal_maps: true,
        bump_maps: true,
        baked_lighting: true,
        instance_emission: true,
    };

    /// Every layer `disable` keeps on.
    pub fn new(disable: &DisabledLayers) -> Self {
        Self {
            normal_maps: !disable.normal_maps,
            bump_maps: !disable.bump_maps,
            baked_lighting: !disable.baked_lighting,
            instance_emission: !disable.instance_emission,
        }
    }

    fn constants(self) -> [(&'static str, f64); 4] {
        [
            ("normal_maps_enabled", f64::from(u8::from(self.normal_maps))),
            ("bump_maps_enabled", f64::from(u8::from(self.bump_maps))),
            (
                "baked_lighting_enabled",
                f64::from(u8::from(self.baked_lighting)),
            ),
            (
                "instance_emission_enabled",
                f64::from(u8::from(self.instance_emission)),
            ),
        ]
    }
}

/// What lit shading compiles in only while the scene holds it, so a scene
/// without it pays nothing for it: the lit passes' and the world-space
/// reflection trace's constants that follow the scene's content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct LitConstants {
    /// Rectangle lights' shading (`rect_lights_enabled` in lights.wgsl), as
    /// Godot specialises its clustered pass on `cluster_has_area_light`.
    pub rect_lights: bool,
    /// Decals (`decals_enabled` in decals.wgsl). Neither Godot, whose
    /// clustered pass walks each cluster's decals whatever the scene holds,
    /// nor Bevy, whose `CLUSTERED_DECALS_ARE_USABLE` follows the device,
    /// specialises on them; the trade is a compile when the first decal is
    /// added or the last removed.
    pub decals: bool,
}

impl LitConstants {
    /// What `scene` holds.
    pub fn of(scene: &Scene) -> Self {
        Self {
            rect_lights: scene.lights.holds_rect(),
            decals: !scene.decals.is_empty(),
        }
    }

    pub fn constants(self) -> [(&'static str, f64); 2] {
        [
            ("rect_lights_enabled", f64::from(u8::from(self.rect_lights))),
            ("decals_enabled", f64::from(u8::from(self.decals))),
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PipelineKey {
    pass: GeometryPass,
    variant: Variant,
    layers: LayerConstants,
    lit: LitConstants,
}

impl PipelineKey {
    /// Casters take no layer or lit constants and read the positions they
    /// are given, deformed or not.
    pub fn new(
        pass: GeometryPass,
        variant: Variant,
        layers: LayerConstants,
        lit: LitConstants,
    ) -> Self {
        let caster = pass.caster();
        Self {
            pass,
            variant: Variant {
                deformed: variant.deformed && !caster,
                ..variant
            },
            layers: if caster { LayerConstants::ALL } else { layers },
            lit: if caster { LitConstants::default() } else { lit },
        }
    }

    /// The pipeline constants: a masked material's discard (`alpha_mask`,
    /// material_raster.wgsl) and, for the pulled passes, the layers, the lit
    /// constants and deformed vertices.
    fn constants(self) -> Vec<(&'static str, f64)> {
        let masked = (
            "alpha_mask",
            f64::from(u8::from(self.variant.alpha == Alpha::Mask)),
        );
        if self.pass.caster() {
            return if self.variant.alpha == Alpha::Mask {
                vec![masked]
            } else {
                Vec::new()
            };
        }
        let mut constants = self.layers.constants().to_vec();
        constants.extend(self.lit.constants());
        constants.push(masked);
        constants.push((
            "deformed_vertices",
            f64::from(u8::from(self.variant.deformed)),
        ));
        constants
    }
}

/// Which alpha modes the scene's materials use beyond opaque, and whether
/// it holds a deforming model: the masked, blended and deformed pipelines
/// are prepared once it holds such content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Content {
    mask: bool,
    blend: bool,
    deformed: bool,
}

impl Content {
    /// What `scene` holds.
    fn of(scene: &Scene) -> Self {
        Self {
            mask: scene.materials.holds_masked(),
            blend: scene.materials.holds_blended(),
            deformed: scene.models.holds_deforming(),
        }
    }
}

/// The depth write and test of each pass. A material/depth prepass and its
/// Equal passes must select the same last draw when distinct materials have
/// indistinguishable device depth.
pub(crate) fn depth(pass: GeometryPass) -> (bool, wgpu::CompareFunction) {
    use wgpu::CompareFunction::*;
    match pass {
        GeometryPass::Forward | GeometryPass::DirectionalShadow | GeometryPass::LocalShadow => {
            (true, Greater)
        }
        GeometryPass::GBuffer | GeometryPass::Fused => (true, GreaterEqual),
        GeometryPass::GBufferAnisotropy | GeometryPass::Lighting => (false, Equal),
        GeometryPass::Blended { .. } => (false, GreaterEqual),
    }
}

pub(crate) struct GeometryPipelines {
    /// Group 0 lit, then scene and material.
    lit: wgpu::PipelineLayout,
    /// Group 0 shadow, then scene and material.
    shadow: wgpu::PipelineLayout,
    geometry: wgpu::ShaderModule,
    caster: wgpu::ShaderModule,
    cache: HashMap<PipelineKey, wgpu::RenderPipeline>,
    /// The diagnostics layers `get` returns pipelines for.
    layers: LayerConstants,
    /// The lit constants `get` returns pipelines for.
    lit_constants: LitConstants,
    /// The content `get` returns pipelines for beyond opaque, rigid
    /// geometry.
    content: Content,
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
        Lighting => vec![
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
        DirectionalShadow | LocalShadow => Vec::new(),
        Blended { .. } => vec![gbuffer::COLOR],
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
    /// `layers`.
    pub fn new(
        device: &wgpu::Device,
        [lit, shadow, scene, material]: [&wgpu::BindGroupLayout; 4],
        layers: LayerConstants,
    ) -> Self {
        let layout = |label, frame| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(frame), Some(scene), Some(material)],
                immediate_size: 0,
            })
        };
        let limits = device.limits();
        let mut pipelines = Self {
            lit: layout("lit scene geometry", lit),
            shadow: layout("shadow casters", shadow),
            geometry: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("SGL material"),
                source: wgpu::ShaderSource::Wgsl(shading::compose(&[&GEOMETRY]).into()),
            }),
            caster: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("shadow casters"),
                source: wgpu::ShaderSource::Wgsl(shading::compose(&[&CASTER]).into()),
            }),
            cache: HashMap::new(),
            layers,
            lit_constants: LitConstants::default(),
            content: Content::default(),
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
    /// created here on first use. Without diagnostics every frame uses
    /// `ALL`.
    pub fn specialise(&mut self, device: &wgpu::Device, layers: LayerConstants, scene: &Scene) {
        let lit_constants = LitConstants::of(scene);
        let content = Content::of(scene);
        if (self.layers, self.lit_constants, self.content) != (layers, lit_constants, content) {
            self.layers = layers;
            self.lit_constants = lit_constants;
            self.content = content;
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
            GeometryPass::Lighting,
            GeometryPass::DirectionalShadow,
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
        let (module, layout, label) = if key.pass.caster() {
            (&self.caster, &self.shadow, "shadow caster")
        } else {
            (&self.geometry, &self.lit, "lit scene geometry")
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
                "shadow_masked_unclipped_vs",
                Some("shadow_masked_unclipped_fs"),
            ),
            DirectionalShadow | LocalShadow if masked => {
                ("shadow_masked_vs", Some("shadow_masked_fs"))
            }
            Forward => ("source_vs", Some("fs")),
            GBuffer if self.anisotropy_inline => ("source_vs", Some("stable_fs")),
            GBuffer => ("source_vs", Some("stable_legacy_fs")),
            GBufferAnisotropy => ("source_vs", Some("anisotropy_fs")),
            Lighting => ("source_vs", Some("source_fs")),
            Fused => ("source_vs", Some("fused_opaque_fs")),
            DirectionalShadow if !self.unclipped_depth => {
                ("shadow_unclipped_vs", Some("shadow_unclipped_fs"))
            }
            DirectionalShadow | LocalShadow => ("shadow_vs", None),
            Blended { fsr2_masks: false } => ("source_vs", Some("blended_fs")),
            Blended { fsr2_masks: true } => ("source_vs", Some("blended_fsr2_masked_fs")),
        };
        let unclipped_depth = key.pass == DirectionalShadow && self.unclipped_depth;
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
