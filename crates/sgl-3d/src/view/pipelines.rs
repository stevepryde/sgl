//! The geometry pipelines: every pipeline that draws scene geometry from a
//! draw list, in one cache keyed by pass, by what the material and instance
//! require (face culling, alpha mode and deformed vertices: `variant`), by
//! the diagnostics layer constants and by whether the scene holds rectangle
//! lights and decals (`LitConstants`). The lighting pass while ray-traced
//! shadows run composes the shadow mask's provider (`shading::SHADOW_MASK`)
//! and binds the mask at its group 3; every other pass composes the
//! provider that holds no slot (`shading::SHADOW_MASK_NONE`). Every program
//! composes the lit and material-map providers of the device's binding tier
//! (`shading::lit_provider`, `shading::material_provider`), and every
//! pipeline layout takes group 0's and group 2's layouts of that tier, fixed
//! for the device, so no key holds it.
use crate::Scene;
use crate::shading::bind::{BindingTier, LitLayout};
use crate::shading::programs::*;
use crate::shading::{self, gbuffer};
use crate::view::targets::{composition_targets, mask_targets};
use std::collections::HashMap;

mod key;
mod pass;
mod variant;
use key::PipelineKey;
pub(crate) use key::{LayerConstants, LitConstants};
pub(crate) use pass::{GeometryPass, depth};
use pass::{attachments_fit, targets};
pub(crate) use variant::{Alpha, Cull, Variant};

/// Which alpha modes the scene's materials use beyond opaque, whether one
/// is a receiver of screen-space reflections, whether an opaque or masked
/// one's surface moves, whether it holds a deforming model, and whether a
/// material is transmissive: the masked, blended, receiver, FSR2
/// composition and deformed pipelines are prepared once it holds such
/// content, and the blended ones compile transmission in while it holds a
/// transmissive material.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Content {
    mask: bool,
    blend: bool,
    receivers: bool,
    moving: bool,
    deformed: bool,
    transmissive: bool,
}

impl Content {
    /// What `scene` holds.
    fn of(scene: &Scene) -> Self {
        Self {
            mask: scene.materials.holds_masked(),
            blend: scene.materials.holds_blended(),
            receivers: scene.materials.holds_receivers(),
            moving: scene.materials.holds_moving_surfaces(),
            deformed: scene.models.holds_deforming(),
            transmissive: scene.materials.holds_transmissive(),
        }
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
    /// `shadow`'s, then the caster positions group 3, for the GPU-built
    /// cascades' casters.
    pulled_shadow: wgpu::PipelineLayout,
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
    /// The device's binding tier, which every program's material maps and
    /// group 2's layout follow.
    pub tier: BindingTier,
}

impl GeometryPipelines {
    /// Creates every pipeline the frame and probe captures draw with for
    /// `layers`, over group 0's `lit` layout, on the device's binding tier,
    /// whose providers every program composes, and `shadow` layout, the
    /// scene's, a material's of that tier (`shading::bind::material`), and
    /// the blended, shadow-mask and caster positions group 3 layouts.
    pub fn new(
        device: &wgpu::Device,
        lit: &LitLayout,
        [shadow, scene, blended, shadow_mask, positions]: [&wgpu::BindGroupLayout; 5],
        layers: LayerConstants,
    ) -> Self {
        let tier = lit.tier;
        let lit = &lit.layout;
        let layout = |label, groups: &[&wgpu::BindGroupLayout]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &groups.iter().map(|&group| Some(group)).collect::<Vec<_>>(),
                immediate_size: 0,
            })
        };
        let limits = device.limits();
        let material = &shading::bind::material(device, tier);
        let mut pipelines = Self {
            lit: layout("lit scene geometry", &[lit, scene, material]),
            blended: layout("blended scene geometry", &[lit, scene, material, blended]),
            shadow_masked: layout(
                "shadow-masked scene lighting",
                &[lit, scene, material, shadow_mask],
            ),
            shadow: layout("shadow casters", &[shadow, scene, material]),
            pulled_shadow: layout(
                "GPU-built cascade casters",
                &[shadow, scene, material, positions],
            ),
            geometry: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("SGL material"),
                source: wgpu::ShaderSource::Wgsl(geometry_program(false, tier).into()),
            }),
            geometry_shadow_masked: None,
            caster: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("shadow casters"),
                source: wgpu::ShaderSource::Wgsl(caster_program().into()),
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
            tier,
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
                        source: wgpu::ShaderSource::Wgsl(geometry_program(true, self.tier).into()),
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
            GeometryPass::PairedShadow,
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
        if self.content.moving {
            passes.push(GeometryPass::Fsr2Composition);
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
                        let key = PipelineKey::new(
                            pass,
                            variant,
                            layers,
                            lit_constants,
                            self.content.transmissive,
                        );
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
        let key = PipelineKey::new(
            pass,
            variant,
            self.layers,
            self.lit_constants,
            self.content.transmissive,
        );
        self.cache
            .get(&key)
            .unwrap_or_else(|| panic!("geometry pipeline {key:?} was not prepared"))
    }

    fn create(&self, device: &wgpu::Device, key: PipelineKey) -> wgpu::RenderPipeline {
        use GeometryPass::*;
        let masked = key.variant.alpha == Alpha::Mask;
        let vertex_buffers = &shading::vertex::GEOMETRY_BUFFERS;
        let (module, layout, label) = match key.pass {
            DirectionalShadow | PairedShadow => {
                (&self.caster, &self.pulled_shadow, "shadow caster")
            }
            CaptureShadow | LocalShadow => (&self.caster, &self.shadow, "shadow caster"),
            Blended { .. } => (&self.geometry, &self.blended, "blended scene geometry"),
            Receivers => (&self.geometry, &self.lit, "blended receivers"),
            Fsr2Composition => (
                &self.geometry,
                &self.lit,
                "FSR2 composition of moving surfaces",
            ),
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
                SHADOW_PULLED_MASKED_UNCLIPPED_VS_ENTRY,
                Some(SHADOW_MASKED_UNCLIPPED_FS_ENTRY),
            ),
            DirectionalShadow if masked => {
                (SHADOW_PULLED_MASKED_VS_ENTRY, Some(SHADOW_MASKED_FS_ENTRY))
            }
            DirectionalShadow if !self.unclipped_depth => (
                SHADOW_PULLED_UNCLIPPED_VS_ENTRY,
                Some(SHADOW_UNCLIPPED_FS_ENTRY),
            ),
            DirectionalShadow => (SHADOW_PULLED_VS_ENTRY, None),
            PairedShadow if !self.unclipped_depth => (
                SHADOW_PAIRED_UNCLIPPED_VS_ENTRY,
                Some(SHADOW_UNCLIPPED_FS_ENTRY),
            ),
            PairedShadow => (SHADOW_PAIRED_VS_ENTRY, None),
            CaptureShadow if masked && !self.unclipped_depth => (
                SHADOW_MASKED_UNCLIPPED_VS_ENTRY,
                Some(SHADOW_MASKED_UNCLIPPED_FS_ENTRY),
            ),
            CaptureShadow | LocalShadow if masked => {
                (SHADOW_MASKED_VS_ENTRY, Some(SHADOW_MASKED_FS_ENTRY))
            }
            Forward => (SOURCE_VS_ENTRY, Some(FS_ENTRY)),
            GBuffer if self.anisotropy_inline => (SOURCE_VS_ENTRY, Some(STABLE_FS_ENTRY)),
            GBuffer => (SOURCE_VS_ENTRY, Some(STABLE_LEGACY_FS_ENTRY)),
            GBufferAnisotropy => (SOURCE_VS_ENTRY, Some(ANISOTROPY_FS_ENTRY)),
            Lighting { .. } => (SOURCE_VS_ENTRY, Some(SOURCE_FS_ENTRY)),
            Fused => (SOURCE_VS_ENTRY, Some(FUSED_OPAQUE_FS_ENTRY)),
            CaptureShadow if !self.unclipped_depth => {
                (SHADOW_UNCLIPPED_VS_ENTRY, Some(SHADOW_UNCLIPPED_FS_ENTRY))
            }
            CaptureShadow | LocalShadow => (SHADOW_VS_ENTRY, None),
            Blended { fsr2_masks: false } => (SOURCE_VS_ENTRY, Some(BLENDED_FS_ENTRY)),
            Blended { fsr2_masks: true } => (SOURCE_VS_ENTRY, Some(BLENDED_FSR2_MASKED_FS_ENTRY)),
            Receivers => (SOURCE_VS_ENTRY, Some(RECEIVER_FS_ENTRY)),
            Fsr2Composition => (SOURCE_VS_ENTRY, Some(FSR2_COMPOSITION_FS_ENTRY)),
        };
        let unclipped_depth = matches!(key.pass, DirectionalShadow | PairedShadow | CaptureShadow)
            && self.unclipped_depth;
        // Blended surfaces blend over the beauty premultiplied, their colour
        // carrying its alpha (blended_color in geometry.wgsl), as Filament
        // ef1a133 blends both its transparent and fade modes (One,
        // OneMinusSrcAlpha; docs_src/src_markdeep/Filament.md.html,
        // Transparency) and Bevy 9d12036 its `Premultiplied` materials
        // (crates/bevy_pbr/src/render/mesh.rs 3534–3541).
        let blend = matches!(key.pass, Blended { .. })
            .then_some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
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
        if key.pass == Fsr2Composition {
            targets.extend(composition_targets());
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
