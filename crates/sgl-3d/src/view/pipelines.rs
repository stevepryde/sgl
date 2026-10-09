//! The geometry pipelines: every pipeline that draws scene geometry from a
//! draw list, in one cache keyed by pass, by what the material and instance
//! require (face culling, alpha mode and deformed vertices: `variant`), by
//! the diagnostics layer constants and by whether the scene holds rectangle
//! lights and decals (`LitConstants`), and by the material's shader, whose
//! programs (`programs`) a pipeline is created from. The lighting pass while ray-traced
//! shadows run composes the shadow mask's provider (`shading::SHADOW_MASK`)
//! and binds the mask at its group 3; every other pass composes the
//! provider that holds no slot (`shading::SHADOW_MASK_NONE`). Every program
//! composes the lit and material-map providers of the device's binding tier
//! (`shading::lit_provider`, `shading::material_provider`), and every
//! pipeline layout takes group 0's and group 2's layouts of that tier, fixed
//! for the device, so no key holds it.
use crate::Scene;
use crate::content::identity::{Identity, ShaderId};
use crate::shading::bind::{BindingTier, LitLayout};
use crate::shading::programs::*;
use crate::shading::{self, gbuffer};
use crate::view::targets::{composition_targets, mask_targets};
use std::collections::HashMap;

mod key;
mod pass;
mod programs;
mod variant;
use key::PipelineKey;
pub(crate) use key::{LayerConstants, LitConstants};
pub(crate) use pass::{GeometryPass, depth};
use pass::{attachments_fit, targets};
use programs::{Program, ProgramSet};
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

/// What the materials that name one shader need of its pipelines: the
/// alpha modes they use, opaque among them, and whether one is a receiver.
/// A shader's opaque or masked material's surface always counts as moving
/// (`Material::surface_moves`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ShaderContent {
    opaque: bool,
    mask: bool,
    blend: bool,
    receivers: bool,
}

impl ShaderContent {
    /// Each shader the materials of `scene` name, with what they need, in
    /// the order of their identities' indices.
    fn of(scene: &Scene) -> Vec<(ShaderId, Self)> {
        let mut shaders: Vec<(ShaderId, Self)> = Vec::new();
        for (_, material) in scene.materials.slots.iter() {
            let Some(shader) = material.values.shader else {
                continue;
            };
            let at = match shaders.iter().position(|(id, _)| *id == shader.shader) {
                Some(at) => at,
                None => {
                    shaders.push((shader.shader, Self::default()));
                    shaders.len() - 1
                }
            };
            let content = &mut shaders[at].1;
            match Alpha::of(&material.values) {
                Alpha::Opaque => content.opaque = true,
                Alpha::Mask => content.mask = true,
                Alpha::Blend => content.blend = true,
            }
            content.receivers |= material.values.receives_screen_space_reflections();
        }
        shaders.sort_by_key(|(id, _)| id.index());
        shaders
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
    /// The programs of materials without a shader.
    programs: ProgramSet,
    /// The programs of each shader a material names, dropped with their
    /// pipelines when none does.
    shader_programs: HashMap<ShaderId, ProgramSet>,
    cache: HashMap<PipelineKey, wgpu::RenderPipeline>,
    /// The diagnostics layers `get` returns pipelines for.
    layers: LayerConstants,
    /// The lit constants `get` returns pipelines for.
    lit_constants: LitConstants,
    /// The content `get` returns pipelines for beyond opaque, rigid
    /// geometry.
    content: Content,
    /// The shaders `get` returns pipelines for, with what their materials
    /// need.
    shaders: Vec<(ShaderId, ShaderContent)>,
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
            programs: ProgramSet::default_set(),
            shader_programs: HashMap::new(),
            cache: HashMap::new(),
            layers,
            lit_constants: LitConstants::default(),
            content: Content::default(),
            shaders: Vec::new(),
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
    /// and for each shader its materials name, created here on first use,
    /// with the lighting pipelines that take the shadow mask once
    /// `shadow_mask` (ray-traced shadows run). A shader no material names
    /// any longer loses its programs and pipelines. Without diagnostics
    /// every frame uses `ALL`.
    pub fn specialise(
        &mut self,
        device: &wgpu::Device,
        layers: LayerConstants,
        scene: &Scene,
        shadow_mask: bool,
    ) {
        let lit_constants = LitConstants::of(scene);
        let content = Content::of(scene);
        let shaders = if scene.materials.holds_shaders() {
            ShaderContent::of(scene)
        } else {
            Vec::new()
        };
        let shadow_mask = self.shadow_mask || shadow_mask;
        if (
            self.layers,
            self.lit_constants,
            self.content,
            self.shadow_mask,
            &self.shaders,
        ) != (layers, lit_constants, content, shadow_mask, &shaders)
        {
            self.layers = layers;
            self.lit_constants = lit_constants;
            self.content = content;
            self.shadow_mask = shadow_mask;
            self.shader_programs
                .retain(|id, _| shaders.iter().any(|(shader, _)| shader == id));
            self.cache.retain(|key, _| {
                key.shader
                    .is_none_or(|id| shaders.iter().any(|(shader, _)| *shader == id))
            });
            for &(id, _) in &shaders {
                self.shader_programs
                    .entry(id)
                    .or_insert_with(|| ProgramSet::of(scene, id));
            }
            self.shaders = shaders;
            self.prepare_layers(device);
        }
    }

    /// Creates the current specialisation's pipelines: each geometry pass
    /// the device takes and the two casters, for each cull, each alpha mode
    /// the scene uses and, for pulled passes, deformed vertices while it
    /// holds a deforming model; and the same of each shader's, for the alpha
    /// modes its materials use.
    fn prepare_layers(&mut self, device: &wgpu::Device) {
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
        if self.shadow_mask {
            passes.push(GeometryPass::Lighting { shadow_mask: true });
        }
        let content = self.content;
        let mut alphas = vec![Alpha::Opaque];
        if content.mask {
            alphas.push(Alpha::Mask);
        }
        if content.blend {
            alphas.push(Alpha::Blend);
        }
        let mut sets = vec![(None, alphas, content.receivers, content.moving)];
        for &(id, shader) in &self.shaders {
            let alphas: Vec<_> = [
                (shader.opaque, Alpha::Opaque),
                (shader.mask, Alpha::Mask),
                (shader.blend, Alpha::Blend),
            ]
            .into_iter()
            .filter_map(|(used, alpha)| used.then_some(alpha))
            .collect();
            let moving = shader.opaque || shader.mask;
            sets.push((Some(id), alphas, shader.receivers, moving));
        }
        let deformed: &[bool] = if content.deformed {
            &[false, true]
        } else {
            &[false]
        };
        for (shader, alphas, receivers, moving) in sets {
            let mut passes = passes.clone();
            if receivers {
                passes.push(GeometryPass::Receivers);
            }
            if moving {
                passes.push(GeometryPass::Fsr2Composition);
            }
            for pass in passes {
                for &alpha in alphas.iter().filter(|&&alpha| pass.draws(alpha)) {
                    for cull in [Cull::None, Cull::Back, Cull::Front] {
                        for &deformed in deformed {
                            let variant = Variant {
                                cull,
                                alpha,
                                deformed,
                            };
                            let key = self.key(pass, variant, shader);
                            self.prepare(device, key);
                        }
                    }
                }
            }
        }
    }

    /// The key of `pass`'s pipeline for `variant` and `shader` in the
    /// current specialisation.
    fn key(&self, pass: GeometryPass, variant: Variant, shader: Option<ShaderId>) -> PipelineKey {
        PipelineKey::new(
            pass,
            variant,
            shader,
            (self.layers, self.lit_constants),
            self.content.transmissive,
        )
    }

    /// Creates the pipeline for `key` unless it exists.
    fn prepare(&mut self, device: &wgpu::Device, key: PipelineKey) {
        if !self.cache.contains_key(&key) {
            let pipeline = self.create(device, key);
            self.cache.insert(key, pipeline);
        }
    }

    /// `pass`'s pipeline for `variant`, of the material's `shader`.
    pub fn get(
        &self,
        pass: GeometryPass,
        variant: Variant,
        shader: Option<ShaderId>,
    ) -> &wgpu::RenderPipeline {
        let key = self.key(pass, variant, shader);
        self.cache
            .get(&key)
            .unwrap_or_else(|| panic!("geometry pipeline {key:?} was not prepared"))
    }

    /// The shader modules and pipelines it holds for game shaders.
    #[cfg(test)]
    pub fn shader_programs(&self) -> (usize, usize) {
        (
            self.shader_programs.values().map(ProgramSet::modules).sum(),
            self.cache.keys().filter(|key| key.shader.is_some()).count(),
        )
    }

    fn create(&mut self, device: &wgpu::Device, key: PipelineKey) -> wgpu::RenderPipeline {
        use GeometryPass::*;
        let masked = key.variant.alpha == Alpha::Mask;
        let shaded = key.shader.is_some();
        let vertex_buffers = &shading::vertex::GEOMETRY_BUFFERS;
        let (program, layout, label) = match key.pass {
            DirectionalShadow | PairedShadow => {
                (Program::Caster, &self.pulled_shadow, "shadow caster")
            }
            CaptureShadow | LocalShadow => (Program::Caster, &self.shadow, "shadow caster"),
            Blended { .. } => (
                Program::Geometry(GeometryForm::Blended),
                &self.blended,
                "blended scene geometry",
            ),
            Receivers => (
                Program::Geometry(GeometryForm::Plain),
                &self.lit,
                "blended receivers",
            ),
            Fsr2Composition => (
                Program::Geometry(GeometryForm::Plain),
                &self.lit,
                "FSR2 composition of moving surfaces",
            ),
            Lighting { shadow_mask: true } => (
                Program::Geometry(GeometryForm::ShadowMask),
                &self.shadow_masked,
                "shadow-masked scene lighting",
            ),
            _ => (
                Program::Geometry(GeometryForm::Plain),
                &self.lit,
                "lit scene geometry",
            ),
        };
        let layout = layout.clone();
        let tier = self.tier;
        let programs = match key.shader {
            Some(id) => self
                .shader_programs
                .get_mut(&id)
                .expect("a specialised shader's programs are held"),
            None => &mut self.programs,
        };
        let module = programs.module(device, tier, program).clone();
        let module = &module;
        let layout = &layout;
        // Casters are depth-only: their cull selects the side that casts. A
        // directional cascade's depth is unclipped, so casters between the
        // light and the cascade are drawn at its near plane, as Bevy's
        // `UNCLIPPED_DEPTH_ORTHO` shadow views and Wicked's directional
        // shadow cameras draw them; a device without `DEPTH_CLIP_CONTROL`
        // emulates it in the shader, as Bevy does. A masked material's
        // casters discard what it cuts out, as Bevy's do (MAY_DISCARD).
        let (vertex, fragment) = match key.pass {
            // A shader's masked casters take their coverage from its surface
            // function, whose context their vertices carry.
            DirectionalShadow if masked && shaded && !self.unclipped_depth => (
                SHADOW_PULLED_SHADED_MASKED_UNCLIPPED_VS_ENTRY,
                Some(SHADOW_SHADED_MASKED_UNCLIPPED_FS_ENTRY),
            ),
            DirectionalShadow if masked && shaded => (
                SHADOW_PULLED_SHADED_MASKED_VS_ENTRY,
                Some(SHADOW_SHADED_MASKED_FS_ENTRY),
            ),
            CaptureShadow if masked && shaded && !self.unclipped_depth => (
                SHADOW_SHADED_MASKED_UNCLIPPED_VS_ENTRY,
                Some(SHADOW_SHADED_MASKED_UNCLIPPED_FS_ENTRY),
            ),
            CaptureShadow | LocalShadow if masked && shaded => (
                SHADOW_SHADED_MASKED_VS_ENTRY,
                Some(SHADOW_SHADED_MASKED_FS_ENTRY),
            ),
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
