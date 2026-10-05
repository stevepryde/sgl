//! Reflections: ambient occlusion of opaque surfaces' ambient diffuse,
//! environment and probe specular, the screen-space method, world-space
//! rays, and their one composition.
pub(crate) mod source;
pub(crate) mod velvet;
pub(crate) mod world;

use crate::scene::probes::UploadedProbes;
use crate::settings::{ReflectionMethod, WorldSpaceReflections};
use crate::view::bindings::FogVolume;
use crate::view::effective::{Effective, ScreenSpace};
use crate::view::frame::FrameContext;
use crate::view::pipelines::LitConstants;
use crate::view::post_fx::PostFx;
use crate::view::targets::SharedTargets;

/// Wicked Engine's SSR temporal reprojection, which Velvet and world-space
/// rays accumulate through. Reads `temporal_current`, `temporal_history`,
/// `temporal_depth_history` and `linear_sampler`.
static TEMPORAL_REPROJECTION: crate::shading::Module = crate::shading::Module {
    name: "temporal_reprojection",
    source: include_str!("temporal_reprojection.wgsl"),
    deps: &[&crate::shading::LUMINANCE, &crate::shading::DEPTH],
};

/// Reflections, in two operations with the transparent stage drawn between
/// them. `complete` culls the probe collection per tile, takes the share of
/// each receiver's ambient diffuse its ambient visibility hides out of the
/// opaque beauty, and adds its environment and probe specular, writing the
/// composite and, while a screen-space method runs, the incident
/// radiance. `resolve` runs the screen-space method over the surface and
/// the incident radiance, which returns premultiplied radiance and
/// confidence (Crystal through the lent post-effect context, or Velvet);
/// world-space rays fill its misses with what they reach (moving objects,
/// or everything), from opaque surfaces not under a receiver; one
/// composition adds both to the composite's opaque
/// lobes, giving a lobe under a receiver its fallback alone, since the
/// result there is the receiver's, which `resolve` returns for the blended
/// draw. Another method returns the same and plugs in beside these.
///
/// Reads: the G-buffer, colour, ambient diffuse, depth, motion and source
/// identity, the surface (the surface depth and receiver layer), the
/// reflection camera, the ambient visibility opaque passes on
/// when ambient occlusion ran, the scene's
/// environment, probes and DFG table, the lent post-effect context
/// (Crystal), the ray-hit lit group 0 and the scene's group 1 (world rays).
/// Writes: the composite; its own incident radiance and probe tiles, and
/// each method's own targets.
/// Honours: ambient occlusion (whether it ran), screen-space reflections
/// (method and half resolution), world-space reflections, the fog
/// (completion fogs from its volume), the source-environment diagnostics
/// layer.
/// Timing groups: `probe culling`, `reflection source completion`, `SSR *`
/// (Crystal, through the context), `Godot SSR *` (Velvet), `world reflection
/// rays`, `world reflection denoise`, `reflection composition`.
/// History: Velvet's and world rays' accumulation, each continuing across
/// consecutive valid frames and dropped while it does not run; Crystal's is
/// in the context.
pub(crate) struct Reflections {
    /// Probe culling, source completion and composition.
    source: source::ReflectionSource,
    /// Completion's and composition's `source::ReflectionEnvironment`.
    environment_parameters: wgpu::Buffer,
    /// While Velvet is the screen-space method.
    velvet: Option<velvet::Velvet>,
    /// While world-space rays run.
    world: Option<world::WorldReflections>,
    /// Full ambient visibility, which completion and composition read in
    /// frames without ambient occlusion.
    full_visibility: wgpu::TextureView,
}

impl Reflections {
    /// Reflections at `size`, with source completion built for the
    /// `first_frame` the renderer expects.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: [u32; 2],
        first_frame: &Effective,
    ) -> Self {
        let variant = source_variant(first_frame, first_frame.ambient_occlusion.is_some());
        Self {
            source: source::ReflectionSource::new(device, size, variant),
            environment_parameters: source::environment_uniform(device, 0., 1.),
            velvet: None,
            world: None,
            full_visibility: crate::counters::texture_init(
                device,
                queue,
                &wgpu::TextureDescriptor {
                    label: Some("full ambient visibility"),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R32Uint,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                bytemuck::bytes_of(&255u32),
            )
            .create_view(&Default::default()),
        }
    }

    pub fn resize(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        self.source.resize(device, size);
    }

    /// Incident radiance for the screen-space method, including environment
    /// and probe specular; `None` while no method runs. The transparent
    /// stage draws into it between `complete` and `resolve`.
    pub fn incident(&self) -> Option<&wgpu::TextureView> {
        self.source.incident()
    }

    /// Probe culling and source completion: the opaque beauty with its
    /// ambient diffuse occluded by `ambient_occlusion` (the opaque stage's
    /// visibility, when it ran), and environment and probe specular occluded
    /// by it, into the composite and, while a screen-space method runs, the
    /// incident radiance.
    pub fn complete(
        &mut self,
        ctx: &mut FrameContext<'_>,
        ambient_occlusion: Option<&wgpu::TextureView>,
    ) {
        // `traced`: alpha roughness below which SSR traces a receiver's
        // reflected lobe (the coat of a coated receiver, else the base); 0
        // keeps every lobe's environment specular when SSR is off. `fade`:
        // the width of the fade below it.
        let (traced, fade) = traced(ctx);
        let frame = &ctx.values.frame;
        crate::counters::write_buffer(
            ctx.queue,
            &self.environment_parameters,
            0,
            bytemuck::bytes_of(&source::ReflectionEnvironment {
                yaw: frame.reflection_yaw,
                intensity: frame.reflection_intensity,
                fade,
                traced,
            }),
        );
        let probes = ctx
            .scene
            .specular_probes()
            .unwrap_or(&ctx.bindings.empty_probes);
        self.source.use_variant(
            ctx.device,
            source_variant(ctx.effective, ambient_occlusion.is_some()),
        );
        let visibility = ambient_occlusion.unwrap_or(&self.full_visibility).clone();
        let fog = ctx.bindings.fog();
        let inputs = source_inputs(ctx, probes, fog, &self.environment_parameters, &visibility);
        self.source
            .encode(ctx.encoder, ctx.device, ctx.queue, inputs, ctx.timing);
    }

    /// After the transparent stage drew into the incident radiance: the
    /// screen-space method over the surface, world-space rays filling its
    /// misses on opaque surfaces, and their composition into the
    /// composite. Returns the method's result, which receivers that are the
    /// surface compose; `None` while no method runs.
    pub fn resolve(
        &mut self,
        ctx: &mut FrameContext<'_>,
        post_fx: Option<&mut PostFx>,
        ambient_occlusion: Option<&wgpu::TextureView>,
    ) -> Option<wgpu::TextureView> {
        let screen_space = ctx.effective.screen_space;
        // A method that does not run keeps no history.
        if !screen_space.is_some_and(|ssr| ssr.method == ReflectionMethod::Velvet) {
            self.velvet = None;
        }
        let reach = ctx.effective.world_space;
        if reach == WorldSpaceReflections::Off {
            self.world = None;
        }
        let ScreenSpace {
            method,
            half_resolution,
            ..
        } = screen_space?;
        let (traced, _) = traced(ctx);
        let t = ctx.targets;
        let surface = ctx.surface;
        let camera = ctx.views.reflection_camera;
        let reflected = match method {
            ReflectionMethod::Crystal => post_fx
                .expect("Crystal SSR runs in the post-effect context")
                .screen_space_reflections(
                    ctx.device,
                    ctx.queue,
                    ctx.encoder,
                    ctx.input.frame_time_ms / 1000.,
                    half_resolution,
                    &self.source.incident,
                    ctx.timing,
                ),
            ReflectionMethod::Velvet => self
                .velvet
                .get_or_insert_with(|| velvet::Velvet::new(ctx.device))
                .encode(
                    ctx.device,
                    ctx.queue,
                    ctx.encoder,
                    ctx.sizes.render,
                    half_resolution,
                    velvet::Inputs {
                        depth: surface.depth,
                        opaque_depth: &t.depth,
                        receivers: surface.receivers,
                        normal: &t.normal,
                        material: &t.material,
                        f0: &t.f0,
                        radiance: &self.source.incident,
                        motion: &t.motion,
                        camera,
                        frame: ctx.history,
                    },
                    ctx.timing,
                ),
        };
        let world_space = if reach != WorldSpaceReflections::Off {
            let world = self.world.get_or_insert_with(|| {
                world::WorldReflections::new(
                    ctx.device,
                    &ctx.bindings.lit,
                    &ctx.bindings.scene,
                    ctx.sizes.render,
                )
            });
            world.encode(
                ctx.encoder,
                ctx.device,
                ctx.queue,
                [ctx.bindings.ray_hit_lit(), &ctx.scene.scene_group],
                LitConstants::of(ctx.scene),
                ctx.hardware_rays,
                reach,
                ctx.history,
                ctx.sizes.render,
                world::Inputs {
                    depth: &t.depth,
                    surface_depth: surface.depth,
                    normal: &t.normal,
                    material: &t.material,
                    f0: &t.f0,
                    motion: &t.motion,
                    source_id: &t.source_id,
                    screen_space: reflected,
                    camera,
                    traced,
                },
                ctx.timing,
            );
            Some(world.output())
        } else {
            None
        };
        // Composition completes the opaque surface, so it precedes the
        // alpha-blended mist as source completion's specular does.
        let probes = ctx
            .scene
            .specular_probes()
            .unwrap_or(&ctx.bindings.empty_probes);
        let visibility = ambient_occlusion.unwrap_or(&self.full_visibility).clone();
        let fog = ctx.bindings.fog();
        let inputs = source_inputs(ctx, probes, fog, &self.environment_parameters, &visibility);
        self.source.compose(
            ctx.encoder,
            ctx.device,
            inputs,
            surface.depth,
            reflected,
            world_space,
            ctx.timing,
        );
        Some(reflected.clone())
    }
}

/// What source completion is built for under `effective`, occluding ambient
/// diffuse while `ambient_occlusion`'s visibility is bound.
fn source_variant(effective: &Effective, ambient_occlusion: bool) -> source::Variant {
    source::Variant {
        environment: effective.source_environment,
        // Only the method reads it; world-space rays run only with one.
        incident: effective.screen_space.is_some(),
        diffuse_occlusion: ambient_occlusion,
    }
}

/// The alpha roughness below which the screen-space method traces a lobe,
/// and the width of its fade below that (0 and 0 with no method). The
/// methods compare perceptual roughness with their own cutoff.
fn traced(ctx: &FrameContext<'_>) -> (f32, f32) {
    ctx.effective
        .screen_space
        .map_or((0., 0.), |ssr| (ssr.cutoff * ssr.cutoff, ssr.fade))
}

/// Source completion's and composition's inputs: `probes`, the frame's
/// `fog` volume and `parameters` (`source::ReflectionEnvironment`) with
/// the frame's targets, scene and camera, occluded by `visibility`.
fn source_inputs<'a: 'p, 'p>(
    ctx: &FrameContext<'a>,
    probes: &'p UploadedProbes,
    fog: FogVolume<'p>,
    parameters: &'p wgpu::Buffer,
    visibility: &'p wgpu::TextureView,
) -> source::Inputs<'p> {
    let t: &'a SharedTargets = ctx.targets;
    let scene: &'a crate::Scene = ctx.scene;
    let frame = &ctx.values.frame;
    let environments = &scene.environments;
    source::Inputs {
        fog,
        frame_fog: ctx.effective.fog.map(|_| source::SourceFog {
            inverse_length: frame.fog_inverse_length,
            inverse_detail_spread: frame.fog_inverse_detail_spread,
            sky_affect: ctx.input.fog.sky_affect.clamp(0., 1.),
        }),
        camera: ctx.views.reflection_camera,
        scene: &t.color,
        ambient: &t.ambient,
        output: &t.composite,
        normal: &t.normal,
        anisotropy: &t.anisotropy,
        f0: &t.f0,
        depth: &t.depth,
        lookup_tables: &scene.lookup_tables,
        ambient_occlusion: visibility,
        environment: source::Environment {
            sky: &environments.frame(ctx.input.environment).pmrem,
            sampler: &environments.sampler,
            parameters,
            material: &t.material,
            baked: &probes.view,
            collection: &probes.metadata,
        },
    }
}
