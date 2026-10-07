//! Fixture access to the renderer's stages: a frame prepared as `render`
//! prepares it, then single stage operations, for tests that observe one
//! stage's output.
use super::Renderer;
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
            sizes: $renderer.sizes,
            targets: &$renderer.targets,
            surface: $renderer.targets.surface(false),
            scene: $scene,
            values: &$frame.values,
            input: &$frame.input,
            views: &$renderer.views,
            bindings: &mut $renderer.bindings,
            pipelines: &$renderer.pipelines,
            history: $frame.history,
            hardware_rays: crate::view::frame::HardwareRays::of(
                &$frame.effective,
                $scene,
                $renderer.prepare.hardware_rays(),
                $renderer.ray_form.as_ref(),
            ),
        }
    };
}

/// A frame prepared by `Renderer::prepare_test_frame`.
pub(crate) struct TestFrame {
    effective: Effective,
    /// The lights its ray-traced shadow slots may hold.
    slot_lights: crate::stages::shadows::traced::slots::SlotLights,
    values: FrameValues,
    input: FrameInput,
    history: HistoryFrame,
}

impl Renderer {
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
        let effective = super::effective::resolve(
            settings,
            &input,
            super::effective::SceneContent::of(scene),
            super::effective::Device {
                fsr2_running: false,
                fused_supported: self.pipelines.fused_supported,
                occlusion_supported: self.cull.occlusion_supported(),
                ray_queries: self.ray_form.as_ref().map(|form| form.form()),
                tier: self.pipelines.tier,
            },
        );
        self.pipelines.specialise(
            device,
            effective.layers,
            scene,
            effective.ray_traced_shadows,
        );
        let history = self.begin_history(scene, &input);
        let values = self.prepare.run(
            device,
            queue,
            scene,
            &input,
            history,
            &effective,
            None,
            self.sizes.render,
            &mut self.views,
            &self.bindings.frame,
        );
        if scene.materials.holds_receivers() {
            self.targets.hold_surface(device);
        }
        self.shadows.resize(device, effective.shadow_quality);
        self.shadows.local.prepare(
            device,
            queue,
            &self.bindings,
            (scene, &mut self.views.instances),
            (input.camera.view, input.camera.projection),
            values.frame.visibility_mask,
            effective.local_lights,
        );
        self.views.instances.upload(device, queue);
        self.fog.prepare(device, effective.fog, self.sizes.render);
        self.dynamic_gi.prepare(device, scene, &effective);
        self.cull_test_views(device, queue, scene);
        let slot_lights = crate::stages::shadows::traced::slots::SlotLights::of(
            &input,
            &values.frame,
            scene,
            self.shadows.local.ranking(),
        );
        let effective =
            super::effective::traced_shadows(effective, self.prepare.hardware_rays(), &slot_lights);
        self.bindings.refresh(
            device,
            scene,
            input.environment,
            &self.views,
            self.shadows.maps(),
            self.fog.volume(),
            self.dynamic_gi.probes(),
        );
        TestFrame {
            effective,
            slot_lights,
            values,
            input,
            history,
        }
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
            &mut self.views,
            frame.values.frame.visibility_mask,
            std::iter::once(clip_from_world),
        );
        self.cull_test_views(device, queue, scene);
    }

    /// The deform pass and the cull stage's early phase over the prepared
    /// views, in a submission of their own.
    pub(crate) fn cull_test_views(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
    ) {
        let mut encoder = device.create_command_encoder(&Default::default());
        self.deform
            .dispatch(device, queue, &mut encoder, scene, None);
        self.cull
            .encode(device, &mut encoder, scene, &self.views, None);
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
            std::mem::replace(&mut self.shadows.directional.layers[0], target.clone())
        });
        let mut ctx = context!(self, device, queue, encoder, scene, frame);
        self.shadows.encode_local(&mut ctx);
        self.shadows.encode_directional(&mut ctx);
        if let Some(layer) = layer {
            self.shadows.directional.layers[0] = layer;
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
            &mut self.opaque,
            &mut self.cull,
            &mut self.traced_shadows,
            &frame.slot_lights,
            &mut ctx,
        );
    }

    /// The transparent stage's glow and mist into `beauty`, over the
    /// renderer's depth target.
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
        self.transparent.encode(
            &mut ctx,
            crate::stages::transparent::Beauty::Incident(beauty),
        );
    }

    /// The receiver pass of `frame`, after its opaque stage, then the
    /// blended draw onto the composite, as `render` encodes them, composing
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
        let drew = self.transparent.encode_receivers(&mut ctx);
        ctx.surface = self.targets.surface(drew);
        self.transparent.encode(
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
            self.opaque.draw_sky(pass, self.bindings.camera_unlit());
        }
        pass.set_bind_group(0, self.bindings.camera_lit(), &[]);
        self.views
            .camera
            .list
            .draw(scene, &self.pipelines, pass, kind);
    }

    /// The camera's lit group 0, as the frame binds it.
    pub(crate) fn test_camera_lit(&self) -> &wgpu::BindGroup {
        self.bindings.camera_lit()
    }

    /// World-space ray hits' lit group 0, as the frame binds it.
    pub(crate) fn test_ray_hit_lit(&self) -> &wgpu::BindGroup {
        self.bindings.ray_hit_lit()
    }

    pub(crate) fn test_dynamic_gi(&self) -> &crate::stages::dynamic_gi::DynamicGi {
        &self.dynamic_gi
    }

    pub(crate) fn test_lit_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bindings.lit.layout
    }

    /// What each phase of the last frame appended to the camera's sets: the
    /// early phase's draw instances, then the late phase's.
    pub(crate) fn test_camera_phases(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
    ) -> [Vec<crate::shading::vertex::DrawInstance>; 2] {
        crate::view::draw_list::gpu::read_phases(&self.views.camera.list, device, queue, scene)
    }

    /// The cull stage's depth pyramid, once a frame culled occlusion.
    pub(crate) fn test_pyramid(&self) -> Option<&wgpu::Texture> {
        self.cull.pyramid()
    }

    /// Whether this device writes the G-buffer and lighting in one pass.
    pub(crate) fn test_fused_supported(&self) -> bool {
        self.pipelines.fused_supported
    }

    /// Whether the G-buffer pass writes anisotropy itself.
    pub(crate) fn test_anisotropy_inline(&self) -> bool {
        self.pipelines.anisotropy_inline
    }

    /// The reflections stage.
    pub(crate) fn test_reflections(&self) -> &crate::stages::reflections::Reflections {
        &self.reflections
    }

    /// The last frame's FSR2 context.
    pub(crate) fn test_fsr2(&self) -> Option<&crate::stages::antialiasing::fsr2::Fsr2> {
        self.antialiasing.fsr2()
    }

    /// Marks the prepared camera view as a probe capture face's, so surfaces
    /// shade as captures shade them.
    pub(crate) fn test_view_as_capture(&mut self, queue: &wgpu::Queue) {
        let mut view = self.views.camera.view;
        view.uniform.flags |= crate::shading::uniforms::VIEW_PROBE_CAPTURE;
        self.views.camera.set(queue, view);
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
            &crate::view::View::camera(bytemuck::Zeroable::zeroed()),
            None,
            Population::ProbeFace,
        );
        pass.set_bind_group(0, self.bindings.camera_lit(), &[]);
        instances.upload(device, queue);
        list.draw(
            scene,
            &self.pipelines,
            &instances,
            pass,
            GeometryPass::Forward,
        );
    }
}
