//! Fixture access to the renderer's stages: a renderer with its one view
//! driven one frame at a time as before views, a frame prepared as `render`
//! prepares it, then single stage operations, for tests that observe one
//! stage's output.
use super::{Renderer, View, ViewError, ViewFrame};
use crate::settings::Settings;
use crate::shading::uniforms::FrameValues;
use crate::view::draw_list::{DrawInstances, DrawList};
use crate::view::effective::Effective;
use crate::view::frame::FrameContext;
use crate::view::history::HistoryFrame;
use crate::view::pipelines::GeometryPass;
use crate::view::population::Population;
use crate::{FrameInput, Scene};

/// The context of `frame`, borrowing the renderer's lent resources.
macro_rules! context {
    ($renderer:expr, $device:expr, $queue:expr, $encoder:expr, $scene:expr, $frame:expr) => {
        FrameContext {
            device: $device,
            queue: $queue,
            encoder: $encoder,
            timing: None,
            effective: &$frame.effective,
            sizes: $renderer.view.sizes,
            targets: &$renderer.view.targets,
            surface: $renderer.view.targets.surface(false),
            scene: $scene,
            values: &$frame.values,
            input: &$frame.input,
            views: &$renderer.view.views,
            bindings: &mut $renderer.view.bindings,
            pipelines: &$renderer.renderer.shared.pipelines,
            history: $frame.history,
            hardware_rays: crate::view::frame::HardwareRays::of(
                &$frame.effective,
                $scene,
                $renderer.renderer.shared.prepare.hardware_rays(),
                $renderer.renderer.shared.ray_form.as_ref(),
            ),
        }
    };
}

/// A renderer and its one view, for tests that render one camera: the
/// renderer's methods through `Deref`, and the single-view frame.
pub(crate) struct TestRenderer {
    pub renderer: Renderer,
    pub view: View,
}

impl std::ops::Deref for TestRenderer {
    type Target = Renderer;
    fn deref(&self) -> &Renderer {
        &self.renderer
    }
}

impl std::ops::DerefMut for TestRenderer {
    fn deref_mut(&mut self) -> &mut Renderer {
        &mut self.renderer
    }
}

/// A frame prepared by `TestRenderer::prepare_test_frame`.
pub(crate) struct TestFrame {
    effective: Effective,
    /// The lights its ray-traced shadow slots may hold.
    slot_lights: crate::stages::shadows::traced::slots::SlotLights,
    values: FrameValues,
    input: FrameInput,
    history: HistoryFrame,
}

impl TestRenderer {
    /// A renderer and a view of `output_format` at `output_size` physical
    /// pixels (`View::new`).
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        output_format: wgpu::TextureFormat,
        output_size: [u32; 2],
        device_scale: f32,
        settings: &Settings,
    ) -> Result<Self, ViewError> {
        let renderer = Renderer::new(device);
        let view = View::new(
            &renderer,
            device,
            queue,
            output_format,
            output_size,
            device_scale,
            settings,
        )?;
        Ok(Self { renderer, view })
    }

    /// One frame of `scene` seen by the view alone (`Renderer::render`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene: &mut Scene,
        input: &FrameInput,
        settings: &Settings,
        output: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        self.renderer.render(
            device,
            queue,
            encoder,
            scene,
            &mut [ViewFrame {
                view: &mut self.view,
                input,
                settings,
                output,
                timing,
            }],
        );
    }

    /// `Renderer::capture_specular_probe` with the view's shadow maps.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn capture_specular_probe(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &mut Scene,
        input: &FrameInput,
        settings: &Settings,
        center: glam::Vec3,
        face_size: u32,
    ) -> Result<crate::SpecularProbeRadiance, crate::ProbeError> {
        self.renderer.capture_specular_probe(
            device,
            queue,
            scene,
            &mut self.view,
            input,
            settings,
            center,
            face_size,
        )
    }

    pub(crate) fn targets(&self) -> &crate::view::targets::SharedTargets {
        &self.view.targets
    }

    /// The fog's froxels the last frame wrote, the froxels it integrated
    /// and its integrated volume.
    pub(crate) fn fog_volumes(&self) -> [&wgpu::TextureView; 3] {
        self.view.fog.test_volumes()
    }

    /// The last frame's ray-traced shadow mask and slot table, where the
    /// stage ran.
    pub(crate) fn traced_shadows(
        &self,
    ) -> Option<(
        &wgpu::TextureView,
        crate::shading::shadow_mask::ShadowMaskSlots,
    )> {
        self.view.traced_shadows.last()
    }

    /// The ray-traced shadow denoiser's moments the last frame wrote, where
    /// the stage ran.
    pub(crate) fn traced_shadow_moments(&self) -> Option<&wgpu::TextureView> {
        self.view.traced_shadows.last_moments()
    }

    /// The world-space reflection stage, once a frame ran it.
    pub(crate) fn world_reflections(
        &self,
    ) -> Option<&crate::stages::reflections::world::WorldReflections> {
        self.view.reflections.world()
    }

    /// FSR's upscaled frame of the last frame it ran.
    pub(crate) fn fsr_output(&self) -> &wgpu::TextureView {
        self.view.antialiasing.output()
    }

    /// The motion reach's cached bind group sets, while TAA runs.
    pub(crate) fn reach_groups(&self) -> Option<usize> {
        self.view.antialiasing.reach_groups()
    }

    /// The motion-blurred frame, once motion blur has run.
    pub(crate) fn motion_blurred(&self) -> Option<&wgpu::TextureView> {
        self.view.motion_blur.output()
    }

    /// A renderer of HDR output at `size` for `settings`.
    pub(crate) fn for_test(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: [u32; 2],
        settings: &Settings,
    ) -> Self {
        Self::new(
            device,
            queue,
            crate::shading::gbuffer::COLOR,
            size,
            1.,
            settings,
        )
        .unwrap()
    }

    /// Prepares the frame of `scene` that `input` describes as `render`
    /// does, without antialiasing's jitter: group 0, the views, their draw
    /// lists and uniforms, and, in a submission of its own, the cull
    /// stage's early phase, which builds the GPU-built lists.
    pub(crate) fn prepare_test_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &mut Scene,
        input: &FrameInput,
        settings: &Settings,
    ) -> TestFrame {
        let input = *input;
        let Self { renderer, view } = self;
        let shared = &mut renderer.shared;
        let effective = super::effective::resolve(
            settings,
            &input,
            super::effective::SceneContent::of(scene),
            shared.device(None, view.cull.occlusion_supported()),
        );
        shared.pipelines.specialise(
            device,
            effective.layers,
            scene,
            effective.ray_traced_shadows,
        );
        let history = view.begin_history(scene, &input);
        let traced = crate::stages::prepare::Prepare::traces(scene, &effective);
        scene.begin_frame(device, queue);
        shared
            .prepare
            .run_scene(device, queue, scene, &effective, &input, traced);
        let values = crate::stages::prepare::Prepare::run_view(
            device,
            queue,
            scene,
            &input,
            history,
            &effective,
            None,
            view.sizes.render,
            &mut view.views,
            &view.bindings.frame,
        );
        if scene.materials.holds_receivers() {
            view.targets.hold_surface(device);
        }
        let view = &mut self.view;
        view.shadows.resize(device, effective.shadow_quality);
        view.shadows.local.prepare(
            device,
            queue,
            &view.bindings,
            (scene, &mut view.views.instances),
            (input.camera.view, input.camera.projection),
            crate::stages::shadows::local::ShadowFrame {
                mask: values.frame.visibility_mask,
                time: input.elapsed_seconds,
                layers: effective.layers,
            },
            effective.local_lights,
        );
        view.views.instances.upload(device, queue);
        view.fog.prepare(device, effective.fog, view.sizes.render);
        if let Some(dynamic_gi) = self.renderer.shared.dynamic_gi.as_mut() {
            dynamic_gi.prepare(device, scene, &effective);
        }
        let Self { renderer, view } = self;
        let shared = &renderer.shared;
        let slot_lights = crate::stages::shadows::traced::slots::SlotLights::of(
            &input,
            &values.frame,
            scene,
            view.shadows.local.ranking(),
        );
        let effective = super::effective::traced_shadows(
            effective,
            shared.prepare.hardware_rays(),
            &slot_lights,
        );
        view.bindings.refresh(
            device,
            scene,
            input.environment,
            &view.views,
            view.shadows.maps(),
            view.fog.volume(),
            shared
                .dynamic_gi
                .as_ref()
                .map(crate::stages::dynamic_gi::DynamicGi::probes),
        );
        let frame = TestFrame {
            effective,
            slot_lights,
            values,
            input,
            history,
        };
        self.cull_test_views(device, queue, scene, &frame);
        frame
    }

    /// Replaces the prepared frame's directional shadow with one cascade of
    /// view-projection `clip_from_world` and its casters, as prepare builds
    /// them and the cull stage culls them, for fixtures that need a known
    /// shadow view.
    pub(crate) fn set_test_cascade(
        &mut self,
        gpu: (&wgpu::Device, &wgpu::Queue),
        scene: &Scene,
        frame: &TestFrame,
        clip_from_world: glam::Mat4,
    ) {
        let (device, queue) = gpu;
        crate::stages::prepare::set_cascades(
            gpu,
            scene,
            &mut self.view.views,
            frame.values.frame.visibility_mask,
            std::iter::once(clip_from_world),
        );
        self.cull_test_views(device, queue, scene, frame);
    }

    /// The deform pass and the cull stage's early phase over the prepared
    /// views, in a submission of their own.
    fn cull_test_views(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        frame: &TestFrame,
    ) {
        let mut encoder = device.create_command_encoder(&Default::default());
        self.shared
            .deform
            .dispatch(device, queue, &mut encoder, scene, None);
        let mut ctx = context!(self, device, queue, &mut encoder, scene, frame);
        self.view.cull.encode_early(&mut ctx);
        queue.submit([encoder.finish()]);
    }

    /// The camera's directional shadow cascades (into the renderer's layers,
    /// or the first into `target`) and the local-light shadow faces, as
    /// `render` encodes them.
    pub(crate) fn encode_test_shadows(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene: &Scene,
        frame: &TestFrame,
        target: Option<&wgpu::TextureView>,
    ) {
        let layer = target.map(|target| {
            std::mem::replace(&mut self.view.shadows.directional.layers[0], target.clone())
        });
        let mut ctx = context!(self, device, queue, encoder, scene, frame);
        self.view.shadows.encode_local(&mut ctx);
        self.view.shadows.encode_directional(&mut ctx);
        if let Some(layer) = layer {
            self.view.shadows.directional.layers[0] = layer;
        }
    }

    /// The opaque stage of `frame`, fused or split, with the ambient
    /// occlusion its settings choose, in the parts and order `render`
    /// encodes them.
    pub(crate) fn encode_test_opaque(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene: &Scene,
        frame: &mut TestFrame,
        fused: bool,
    ) {
        frame.effective.fused = fused;
        let mut ctx = context!(self, device, queue, encoder, scene, frame);
        super::frame::encode_opaque(
            &mut self.view.opaque,
            &mut self.view.cull,
            &mut self.view.traced_shadows,
            &frame.slot_lights,
            &mut ctx,
        );
    }

    /// The transparent stage's volume layers, then its blended surfaces,
    /// glow and mist into `beauty`, over the renderer's depth target.
    pub(crate) fn encode_test_transparent(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene: &Scene,
        frame: &TestFrame,
        beauty: &wgpu::TextureView,
    ) {
        let mut ctx = context!(self, device, queue, encoder, scene, frame);
        self.view.transparent.encode_volumes(&mut ctx);
        self.view.transparent.order_mist(&ctx);
        self.view.transparent.encode(
            &mut ctx,
            crate::stages::transparent::Beauty::Incident(beauty),
        );
    }

    /// The receiver pass of `frame`, after its opaque stage, then the
    /// volume layers and the blended draw onto the composite, as `render`
    /// encodes them, composing
    /// `reflections` as the screen-space method's result.
    pub(crate) fn encode_test_receivers(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene: &Scene,
        frame: &TestFrame,
        reflections: Option<&wgpu::TextureView>,
    ) {
        let mut ctx = context!(self, device, queue, encoder, scene, frame);
        let drew = self.view.transparent.encode_receivers(&mut ctx);
        ctx.surface = self.view.targets.surface(drew);
        self.view.transparent.encode_volumes(&mut ctx);
        self.view.transparent.order_mist(&ctx);
        self.view.transparent.encode(
            &mut ctx,
            crate::stages::transparent::Beauty::Composite { reflections },
        );
    }

    /// Draws the camera's GPU-built draw list with `kind` into `pass` under
    /// the camera's lit group 0; `Forward` draws the sky first, as a probe
    /// capture face does.
    pub(crate) fn draw_test_camera(
        &self,
        scene: &Scene,
        pass: &mut wgpu::RenderPass<'_>,
        kind: GeometryPass,
    ) {
        if kind == GeometryPass::Forward {
            self.view
                .opaque
                .draw_sky(pass, self.view.bindings.camera_unlit());
        }
        pass.set_bind_group(0, self.view.bindings.camera_lit(), &[]);
        self.view
            .views
            .camera
            .list
            .draw(scene, &self.renderer.shared.pipelines, pass, kind);
    }

    /// The camera's lit group 0, as the frame binds it.
    pub(crate) fn test_camera_lit(&self) -> &wgpu::BindGroup {
        self.view.bindings.camera_lit()
    }

    /// World-space ray hits' lit group 0, as the frame binds it.
    pub(crate) fn test_ray_hit_lit(&self) -> &wgpu::BindGroup {
        self.view.bindings.ray_hit_lit()
    }

    pub(crate) fn test_dynamic_gi(&self) -> &crate::stages::dynamic_gi::DynamicGi {
        self.shared
            .dynamic_gi
            .as_ref()
            .expect("the dynamic GI stage, on the Extended binding tier")
    }

    pub(crate) fn test_lit_layout(&self) -> &wgpu::BindGroupLayout {
        &self.view.bindings.lit.layout
    }

    /// What each phase of the last frame appended to the camera's sets: the
    /// early phase's draw instances, then the late phase's.
    pub(crate) fn test_camera_phases(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
    ) -> [Vec<crate::shading::vertex::DrawInstance>; 2] {
        crate::view::draw_list::gpu::read_phases(&self.view.views.camera.list, device, queue, scene)
    }

    /// The cull stage's depth pyramid, once a frame culled occlusion.
    pub(crate) fn test_pyramid(&self) -> Option<&wgpu::Texture> {
        self.view.cull.pyramid()
    }

    /// Whether this device writes the G-buffer and lighting in one pass.
    /// The shader modules and pipelines it holds for game shaders.
    pub(crate) fn test_shader_programs(&self) -> (usize, usize) {
        self.renderer.shared.pipelines.shader_programs()
    }

    pub(crate) fn test_fused_supported(&self) -> bool {
        self.renderer.shared.pipelines.fused_supported
    }

    /// Whether the G-buffer pass writes anisotropy itself.
    pub(crate) fn test_anisotropy_inline(&self) -> bool {
        self.renderer.shared.pipelines.anisotropy_inline
    }

    /// The reflections stage.
    pub(crate) fn test_reflections(&self) -> &crate::stages::reflections::Reflections {
        &self.view.reflections
    }

    /// The last frame's FSR context.
    pub(crate) fn test_fsr(&self) -> Option<&crate::stages::antialiasing::fsr::Fsr> {
        self.view.antialiasing.fsr()
    }

    /// Marks the prepared camera view as a probe capture face's, so surfaces
    /// shade as captures shade them.
    pub(crate) fn test_view_as_capture(&mut self, queue: &wgpu::Queue) {
        let mut view = self.view.views.camera.view;
        view.uniform.flags |= crate::shading::uniforms::VIEW_PROBE_CAPTURE;
        self.view.views.camera.set(queue, view);
    }

    /// Draws the static capture-visible instances, whole and double-sided, into a
    /// colour and motion pass with depth under the camera's lit group 0 of the
    /// prepared frame: a probe face's population and pipeline from the camera.
    pub(crate) fn draw_static(
        &self,
        gpu: (&wgpu::Device, &wgpu::Queue),
        scene: &Scene,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        let (device, queue) = gpu;
        let mut instances = DrawInstances::default();
        let mut list = DrawList::default();
        list.build(
            &mut instances,
            scene,
            &crate::view::ViewData::camera(bytemuck::Zeroable::zeroed()),
            None,
            Population::ProbeFace,
        );
        pass.set_bind_group(0, self.view.bindings.camera_lit(), &[]);
        instances.upload(device, queue);
        list.draw(
            scene,
            &self.renderer.shared.pipelines,
            &instances,
            pass,
            GeometryPass::Forward,
        );
    }
}

// Defect: recording an automatic frame consumes the fixed-to-automatic
// reset even if its encoder is abandoned. Read the GPU exposure: a retry
// must snap to the bright frame's target, then resume authored adaptation
// after finish_frame rather than resetting every later frame.
#[test]
fn abandoned_automatic_exposure_still_snaps_then_adapts_after_finish() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let settings = Settings {
        antialiasing: crate::settings::Antialiasing::Off,
        bloom: crate::settings::Bloom::Off,
        ..Default::default()
    };
    let size = [16, 16];
    let mut renderer = TestRenderer::for_test(&device, &queue, size, &settings);
    let mut scene = Scene::new(&device, &queue);
    let output = crate::view::targets::target(
        &device,
        "exposure lifecycle",
        size,
        crate::shading::gbuffer::COLOR,
    );
    let mut input = FrameInput::new(crate::Camera {
        view: glam::Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: glam::Vec3::ZERO,
    });
    input.backdrop = crate::Backdrop::Color([16.; 3]);
    input.frame_time_ms = 0.;
    for frame in 0..4 {
        if frame == 1 {
            input.exposure.automatic = Some(crate::AutoExposure {
                speed_brighten: 0.,
                speed_darken: 0.,
                ..Default::default()
            });
        }
        if frame == 3 {
            input.backdrop = crate::Backdrop::Color([1.; 3]);
        }
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &output,
            None,
        );
        if frame == 1 {
            drop(encoder);
            continue;
        }
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
        if frame >= 2 {
            let multiplier: f32 = bytemuck::pod_read_unaligned(&crate::test_support::read(
                &device,
                &queue,
                renderer.view.exposure.texture(),
                4,
            ));
            assert!(
                (multiplier.log2() + 4.).abs() < 0.3,
                "frame {frame}: snap to luminance 16 then hold at zero speed: {multiplier}"
            );
        }
    }
}
