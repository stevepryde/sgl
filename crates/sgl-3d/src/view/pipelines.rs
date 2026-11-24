//! The geometry pipelines: every pipeline that draws scene geometry from a
//! draw list, in one cache keyed by pass, by what the material and instance
//! require (face culling, alpha mode and deformed vertices: `variant`), by
//! the diagnostics layer constants and by whether the scene holds rectangle
//! lights and decals (`LitConstants`), and by the material's shader, whose
//! programs (`programs`) a pipeline is created from; the volume layers'
//! passes exist for the shaders that read their volume path, on the
//! Extended binding tier alone. The lighting pass while ray-traced
//! shadows run composes the shadow mask's provider (`shading::SHADOW_MASK`)
//! and binds the mask at its group 3; every other pass composes the
//! provider that holds no slot (`shading::SHADOW_MASK_NONE`). The lighting
//! pass while contact shadows run composes the contact provider
//! (`shading::CONTACT_SHADOW`) and binds the contact mask there instead;
//! every other pass composes the one that leaves the maps alone
//! (`shading::CONTACT_SHADOW_NONE`). The fused and
//! lighting passes while screen-space indirect light has a kept result
//! compose its provider (`shading::CAMERA_SCREEN_INDIRECT`) and bind it at
//! their group 3 too; every other pass composes the one that gives none. Every program
//! composes the lit and material-map providers of the device's binding tier
//! (`shading::lit_provider`, `shading::material_provider`), and every
//! pipeline layout takes group 0's and group 2's layouts of that tier, fixed
//! for the device, so no key holds it.
use crate::Scene;
use crate::content::identity::ShaderId;
use crate::shading::bind::{BindingTier, LitLayout};
use crate::shading::programs::*;
use crate::shading::{self, gbuffer};
use crate::view::compile::{Compiled, Compiler, Creation, Creator};
use crate::view::targets::{composition_targets, mask_targets};
use std::collections::HashMap;

mod key;
mod pass;
mod programs;
mod retention;
mod variant;
pub(crate) use crate::shading::programs::CameraShadows;
pub(crate) use key::{LayerConstants, LitConstants};
use key::{PipelineKey, Specialisation};
pub(crate) use pass::{GeometryPass, depth};
use pass::{attachments_fit, targets};
use programs::{Program, ProgramSet};
pub(crate) use retention::PipelineSpecialisations;
pub(crate) use variant::{Alpha, Cull, Variant};

pub(crate) struct GeometryPipelines {
    /// Group 0 lit, then scene and material.
    lit: wgpu::PipelineLayout,
    /// `lit`'s, then the blended group 3.
    blended: wgpu::PipelineLayout,
    /// `lit`'s, then the shadow mask's group 3.
    shadow_masked: wgpu::PipelineLayout,
    /// `lit`'s, then screen-space indirect light's group 3, without and
    /// with the shadow mask's bindings.
    lit_screen_indirect: wgpu::PipelineLayout,
    shadow_masked_screen_indirect: wgpu::PipelineLayout,
    /// `lit`'s, then the contact mask's group 3, without and with
    /// screen-space indirect light's bindings.
    contact_shadowed: wgpu::PipelineLayout,
    contact_shadowed_screen_indirect: wgpu::PipelineLayout,
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
    /// Each a slot: a frame creates a missing pipeline where it binds it,
    /// and a planning pass queues it as a compile job.
    cache: HashMap<PipelineKey, Compiled<wgpu::RenderPipeline>>,
    compiler: Compiler,
    /// Whether the frame's camera lighting will take screen-space indirect
    /// light's kept result from the next frame on and does not yet: a
    /// planning pass then queues those pipelines beside this frame's.
    pub plans_screen_indirect: bool,
    current: Specialisation,
    /// A light or material toggled back can reuse its previous pipelines.
    previous: Option<Specialisation>,
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
    /// Create the shared layouts for this device and leave the cache empty
    /// until a frame or probe capture binds a geometry pipeline.
    pub fn new(
        device: &wgpu::Device,
        compiler: &Compiler,
        lit: &LitLayout,
        [
            shadow,
            scene,
            blended,
            shadow_mask,
            screen_indirect,
            shadow_mask_screen_indirect,
            contact_shadow,
            contact_shadow_screen_indirect,
            positions,
        ]: [&wgpu::BindGroupLayout; 9],
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
        Self {
            lit: layout("lit scene geometry", &[lit, scene, material]),
            blended: layout("blended scene geometry", &[lit, scene, material, blended]),
            shadow_masked: layout(
                "shadow-masked scene lighting",
                &[lit, scene, material, shadow_mask],
            ),
            lit_screen_indirect: layout(
                "scene lighting with screen-space indirect light",
                &[lit, scene, material, screen_indirect],
            ),
            shadow_masked_screen_indirect: layout(
                "shadow-masked scene lighting with screen-space indirect light",
                &[lit, scene, material, shadow_mask_screen_indirect],
            ),
            contact_shadowed: layout(
                "contact-shadowed scene lighting",
                &[lit, scene, material, contact_shadow],
            ),
            contact_shadowed_screen_indirect: layout(
                "contact-shadowed scene lighting with screen-space indirect light",
                &[lit, scene, material, contact_shadow_screen_indirect],
            ),
            shadow: layout("shadow casters", &[shadow, scene, material]),
            pulled_shadow: layout(
                "GPU-built cascade casters",
                &[shadow, scene, material, positions],
            ),
            programs: ProgramSet::default_set(),
            shader_programs: HashMap::new(),
            cache: HashMap::new(),
            compiler: compiler.clone(),
            plans_screen_indirect: false,
            current: (LayerConstants::ALL, LitConstants::default(), false),
            previous: None,
            anisotropy_inline: attachments_fit(&limits, &targets(GeometryPass::GBuffer, true)),
            fused_supported: attachments_fit(
                &limits,
                &targets(
                    GeometryPass::Fused {
                        screen_indirect: false,
                    },
                    true,
                ),
            ),
            unclipped_depth: device
                .features()
                .contains(wgpu::Features::DEPTH_CLIP_CONTROL),
            tier,
        }
    }

    /// Select the constants for this frame or capture. Keep the current and
    /// previous specialisations, and only shaders still named by a material
    /// of this scene. Shader generations are unique across scenes.
    pub fn specialise(&mut self, layers: LayerConstants, scene: &Scene) {
        // The geometry pipelines' keys leave out what only the ray traces
        // read (`LitConstants::occlusion_specular`), so it is no change of
        // their specialisation.
        let next = (
            layers,
            LitConstants {
                occlusion_specular: false,
                ..LitConstants::of(scene)
            },
            scene.materials.holds_transmissive(),
        );
        let changed = next != self.current;
        if changed {
            self.previous = Some(std::mem::replace(&mut self.current, next));
        }
        let shaders = self.shader_programs.len();
        self.shader_programs
            .retain(|&id, _| scene.shaders.get(id).is_ok_and(|shader| shader.users > 0));
        if changed || self.shader_programs.len() != shaders {
            self.cache.retain(|key, _| {
                (key.specialised(next) || self.previous.is_some_and(|s| key.specialised(s)))
                    && key
                        .shader
                        .is_none_or(|id| self.shader_programs.contains_key(&id))
            });
        }
    }

    /// Called when an executor binds a draw. GPU-built lists need the pipeline
    /// for the issued indirect command even when culling leaves zero instances.
    /// None in a planning pass (`Renderer::compile`) while the pipeline is
    /// missing: the pass queues its compile job and the caller leaves the
    /// draw out.
    pub fn with_pipeline<T>(
        &mut self,
        scene: &Scene,
        pass: GeometryPass,
        variant: Variant,
        shader: Option<ShaderId>,
        use_pipeline: impl FnOnce(&wgpu::RenderPipeline) -> T,
    ) -> Option<T> {
        let (layers, lit, transmission) = self.current;
        let key = PipelineKey::new(pass, variant, shader, (layers, lit), transmission);
        // The next frame's lighting, which a planning pass readies with
        // this frame's.
        if self.plans_screen_indirect
            && self.compiler.planning()
            && let Some(next) = pass.with_screen_indirect()
        {
            let next = PipelineKey::new(next, variant, shader, (layers, lit), transmission);
            self.pipeline(scene, next);
        }
        if let Some(pipeline) = self.cache.get(&key).and_then(Compiled::ready) {
            return Some(use_pipeline(pipeline));
        }
        self.pipeline(scene, key)
            .map(|pipeline| use_pipeline(pipeline))
    }

    /// `key`'s pipeline: created in place by a frame, queued by a planning
    /// pass.
    fn pipeline(&mut self, scene: &Scene, key: PipelineKey) -> Option<&mut wgpu::RenderPipeline> {
        if self
            .cache
            .get(&key)
            .is_some_and(|slot| slot.ready().is_some())
        {
            return self.cache.get_mut(&key).and_then(Compiled::ready_mut);
        }
        let (label, job) = self.job(scene, key);
        let compiler = self.compiler.clone();
        self.cache
            .entry(key)
            .or_default()
            .get(&compiler, label, || job)
    }

    /// The shader modules and pipelines it holds for game shaders.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn shader_programs(&self) -> (usize, usize) {
        (
            self.shader_programs.values().map(ProgramSet::modules).sum(),
            self.cache
                .iter()
                .filter(|(key, pipeline)| key.shader.is_some() && pipeline.ready().is_some())
                .count(),
        )
    }

    /// `key`'s label and what creates its pipeline, on whichever thread
    /// runs it: it owns the device, the layout, the program's module (made
    /// there if no pipeline has needed it) and the descriptor's values.
    fn job(
        &mut self,
        scene: &Scene,
        key: PipelineKey,
    ) -> (
        &'static str,
        impl Fn(&Creator<'_>) -> Creation<wgpu::RenderPipeline> + wgpu::WasmNotSend + 'static,
    ) {
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
            TemporalComposition { .. } => (
                Program::Geometry(GeometryForm::Plain),
                &self.lit,
                "temporal composition of moving surfaces",
            ),
            VolumeEntry | VolumeExit => (
                Program::Geometry(GeometryForm::Plain),
                &self.lit,
                "volume layer",
            ),
            // The second exit reads the exit layer at its blended binding,
            // which the plain program declares on the Extended tier.
            VolumeSecondExit => (
                Program::Geometry(GeometryForm::Plain),
                &self.blended,
                "second exit volume layer",
            ),
            Lighting {
                shadows,
                screen_indirect,
            } => (
                Program::Geometry(GeometryForm::Camera {
                    shadows,
                    screen_indirect,
                }),
                match (shadows, screen_indirect) {
                    (CameraShadows::Maps, false) => &self.lit,
                    (CameraShadows::ShadowMask, false) => &self.shadow_masked,
                    (CameraShadows::ContactMask, false) => &self.contact_shadowed,
                    (CameraShadows::Maps, true) => &self.lit_screen_indirect,
                    (CameraShadows::ShadowMask, true) => &self.shadow_masked_screen_indirect,
                    (CameraShadows::ContactMask, true) => &self.contact_shadowed_screen_indirect,
                },
                "scene lighting",
            ),
            Fused { screen_indirect } => (
                Program::Geometry(GeometryForm::Camera {
                    shadows: CameraShadows::Maps,
                    screen_indirect,
                }),
                if screen_indirect {
                    &self.lit_screen_indirect
                } else {
                    &self.lit
                },
                "fused scene geometry and lighting",
            ),
            _ => (
                Program::Geometry(GeometryForm::Plain),
                &self.lit,
                "lit scene geometry",
            ),
        };
        let tier = self.tier;
        let programs = match key.shader {
            Some(id) => self
                .shader_programs
                .entry(id)
                .or_insert_with(|| ProgramSet::of(scene, id)),
            None => &mut self.programs,
        };
        let module = programs.module(tier, program, key.variant.occlusion_specular);
        let layout = layout.clone();
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
            Fused { .. } => (SOURCE_VS_ENTRY, Some(FUSED_OPAQUE_FS_ENTRY)),
            CaptureShadow if !self.unclipped_depth => {
                (SHADOW_UNCLIPPED_VS_ENTRY, Some(SHADOW_UNCLIPPED_FS_ENTRY))
            }
            CaptureShadow | LocalShadow => (SHADOW_VS_ENTRY, None),
            Blended {
                temporal_masks: false,
            } => (SOURCE_VS_ENTRY, Some(BLENDED_FS_ENTRY)),
            Blended {
                temporal_masks: true,
            } => (SOURCE_VS_ENTRY, Some(BLENDED_MASKED_FS_ENTRY)),
            Receivers => (SOURCE_VS_ENTRY, Some(RECEIVER_FS_ENTRY)),
            TemporalComposition { .. } => (SOURCE_VS_ENTRY, Some(TEMPORAL_COMPOSITION_FS_ENTRY)),
            VolumeEntry => (SOURCE_VS_ENTRY, Some(VOLUME_ENTRY_FS_ENTRY)),
            VolumeExit => (SOURCE_VS_ENTRY, Some(VOLUME_EXIT_FS_ENTRY)),
            VolumeSecondExit => (SOURCE_VS_ENTRY, Some(VOLUME_SECOND_EXIT_FS_ENTRY)),
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
        if key.pass
            == (Blended {
                temporal_masks: true,
            })
        {
            targets.extend(mask_targets());
        }
        if matches!(key.pass, TemporalComposition { .. }) {
            targets.extend(composition_targets());
        }
        let constants = key.constants();
        let (depth_write, depth_compare) = depth(key.pass);
        let pulled = key.pass.pulled();
        let cull_mode = key.variant.cull.face();
        let create = move |creator: &Creator<'_>| {
            let module = module.get(creator.device());
            creator.render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some(vertex),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &constants,
                        ..Default::default()
                    },
                    buffers: if pulled {
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
                    cull_mode,
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
        };
        (label, create)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod primary_depth_precision_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod specialisation_tests;
