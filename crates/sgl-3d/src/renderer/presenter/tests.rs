//! Actual GPU and presentation-boundary integration; no mock SDK algorithms.
use super::*;
use crate::{
    Camera,
    settings::{
        AmbientOcclusionQuality, Antialiasing, Bloom, FrameGeneration, RenderPreset,
        SceneResolution, ScreenSpaceReflections,
    },
    test_support,
};
use sp_fidelity_wgpu::presentation::{PresentFrame, PresentInfo};
use std::sync::Mutex;

type Presented = Arc<Mutex<Vec<(PresentInfo, Vec<u8>)>>>;
struct Target {
    device: wgpu::Device,
    config: wgpu::SurfaceConfiguration,
    images: Presented,
    failure: Arc<Mutex<Option<&'static str>>>,
}
impl PresentationTarget for Target {
    fn configure(
        &mut self,
        _: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
    ) -> Result<(), PresentationError> {
        self.config = config.clone();
        Ok(())
    }
    fn acquire(&mut self) -> Result<PresentFrame, PresentationError> {
        if let Some(error) = *self.failure.lock().unwrap() {
            return Err(PresentationError::Surface(error));
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("headless surface"),
            size: wgpu::Extent3d {
                width: self.config.width,
                height: self.config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[self.config.format.remove_srgb_suffix()],
        });
        let image = texture.clone();
        let device = self.device.clone();
        let images = self.images.clone();
        Ok(PresentFrame::new(texture, move |queue, info| {
            images.lock().unwrap().push((
                info,
                test_support::read(
                    &device,
                    queue,
                    &image,
                    image.format().block_copy_size(None).unwrap(),
                ),
            ));
            Ok(())
        }))
    }
}
fn config() -> wgpu::SurfaceConfiguration {
    wgpu::SurfaceConfiguration {
        width: 128,
        height: 72,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: wgpu::TextureFormat::Rgba8Unorm,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: wgpu::CompositeAlphaMode::Opaque,
        view_formats: vec![],
        color_space: wgpu::SurfaceColorSpace::Srgb,
    }
}
fn settings() -> Settings {
    Settings {
        preset: RenderPreset::Low,
        scene_resolution: SceneResolution::Full,
        antialiasing: Antialiasing::Off,
        frame_generation: FrameGeneration::Fsr3,
        bloom: Bloom::Off,
        screen_space_reflections: ScreenSpaceReflections::Off,
        ambient_occlusion: AmbientOcclusionQuality::Off,
        atmosphere: false,
        ..Default::default()
    }
}
fn input() -> FrameInput {
    let eye = glam::Vec3::new(2.4, 2., 3.3);
    FrameInput::new(Camera {
        eye,
        view: glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
        projection: crate::perspective(55f32.to_radians(), 128. / 72., 0.1),
    })
}
fn setup(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    settings: &Settings,
) -> (
    Presenter,
    Scene,
    Presented,
    Arc<Mutex<Option<&'static str>>>,
) {
    setup_with_config(device, queue, settings, config())
}
fn setup_with_config(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    settings: &Settings,
    config: wgpu::SurfaceConfiguration,
) -> (
    Presenter,
    Scene,
    Presented,
    Arc<Mutex<Option<&'static str>>>,
) {
    let images = Arc::new(Mutex::new(Vec::new()));
    let failure = Arc::new(Mutex::new(None));
    let target = Target {
        device: device.clone(),
        config: config.clone(),
        images: images.clone(),
        failure: failure.clone(),
    };
    let presenter = Presenter::with_target(
        device,
        queue,
        target,
        &config,
        &wgpu::SurfaceCapabilities::default(),
        1.,
        settings,
    )
    .unwrap();
    let mut scene = Scene::new(device, queue);
    let mut cube = test_support::cube();
    cube.materials[0].emissive = [1., 0.25, 0.1];
    test_support::add_static(device, queue, &mut scene, cube);
    (presenter, scene, images, failure)
}

// A miswired context, shared resource, output format or AA-dependent motion path
// fails real shader creation/dispatch, or emits fewer than a generated/real pair.
// On a reset AMD copies the real image, so that pair must match exactly.
#[test]
fn frame_generation_renders_with_every_antialiasing_mode() {
    let Some((device, queue)) = test_support::fsr_device() else {
        return;
    };
    if !device
        .features()
        .contains(sp_fidelity_wgpu::required_features())
    {
        return;
    }
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut settings = settings();
    let (mut presenter, mut scene, images, _) = setup(&device, &queue, &settings);
    for method in [
        Antialiasing::Off,
        Antialiasing::Smaa,
        Antialiasing::Taa,
        Antialiasing::Fsr2,
        Antialiasing::Fsr3,
    ] {
        settings.antialiasing = method;
        presenter
            .render(&mut scene, &input(), &settings, None)
            .unwrap()
            .present()
            .unwrap();
        presenter.wait_for_presents().unwrap();
        assert_eq!(
            presenter.frame_generation_status(),
            FrameGenerationStatus::Active
        );
        let mut observed = images.lock().unwrap();
        assert_eq!(observed.len(), 2, "{method:?}");
        assert!(observed[0].0.interpolated);
        assert!(!observed[1].0.interpolated);
        assert_eq!(observed[0].0.frame_id, observed[1].0.frame_id);
        assert_eq!(
            observed[0].1, observed[1].1,
            "reset output must be the real image: {method:?}"
        );
        observed.clear();
        drop(observed);
        // Exercise history-bearing dispatch after the reset, with camera motion.
        // Each real frame must still retire a generated/real pair; repeated
        // unsubmitted histories, stale handles and resource aliases fail here.
        for index in 1..=3 {
            let mut input = input();
            input.elapsed_seconds = f64::from(index) / 60.;
            input.camera.eye.x += index as f32 * 0.05;
            input.camera.view = glam::camera::rh::view::look_at_mat4(
                input.camera.eye,
                glam::Vec3::ZERO,
                glam::Vec3::Y,
            );
            presenter
                .render(&mut scene, &input, &settings, None)
                .unwrap()
                .present()
                .unwrap();
            presenter.wait_for_presents().unwrap();
        }
        let mut observed = images.lock().unwrap();
        assert_eq!(observed.len(), 6, "consecutive frames: {method:?}");
        for pair in observed.chunks_exact(2) {
            assert!(pair[0].0.interpolated);
            assert!(!pair[1].0.interpolated);
        }
        assert!(
            observed[1].1 != observed[5].1,
            "real camera motion must reach presentation"
        );
        observed.clear();
    }
    presenter.shutdown().unwrap();
    assert!(pollster::block_on(scope.pop()).is_none());
}

// An abandoned renderer encoder must not be presented. UI is composed after
// interpolation, with identical known premultiplied values on both outputs.
#[test]
fn dropped_frame_is_not_presented_and_ui_composes_after_generation() {
    let Some((device, queue)) = test_support::fsr_device() else {
        return;
    };
    if !device
        .features()
        .contains(sp_fidelity_wgpu::required_features())
    {
        return;
    }
    let settings = settings();
    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Rgba16Float,
    ] {
        let mut config = config();
        config.format = format;
        config.color_space = if format == wgpu::TextureFormat::Rgba16Float {
            wgpu::SurfaceColorSpace::ExtendedSrgbLinear
        } else {
            wgpu::SurfaceColorSpace::Srgb
        };
        let (mut presenter, mut scene, images, _) =
            setup_with_config(&device, &queue, &settings, config);
        drop(
            presenter
                .render(&mut scene, &input(), &settings, None)
                .unwrap(),
        );
        presenter.wait_for_presents().unwrap();
        assert!(images.lock().unwrap().is_empty());
        let mut frame = presenter
            .render(&mut scene, &input(), &settings, None)
            .unwrap();
        let ui = frame.ui_target().clone();
        {
            let _pass = frame
                .encoder()
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &ui,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.25,
                                g: 0.5,
                                b: 0.75,
                                a: 1.,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
        }
        frame.present().unwrap();
        presenter.shutdown().unwrap();
        let images = images.lock().unwrap();
        assert_eq!(images.len(), 2);
        for (_, bytes) in images.iter() {
            if format == wgpu::TextureFormat::Rgba16Float {
                // Exact IEEE binary16 values for the opaque UI clear. A second
                // transfer/tonemap or sRGB decode changes these observed values.
                for pixel in bytes.chunks_exact(8) {
                    assert_eq!(pixel, [0, 0x34, 0, 0x38, 0, 0x3a, 0, 0x3c]);
                }
            } else {
                for pixel in bytes.chunks_exact(4) {
                    assert_eq!(pixel, [64, 128, 191, 255]);
                }
            }
        }
    }
}

// Surface errors arrive after render submission; recovery must retire slots and
// accept another frame. Disabling generation must still present the real frame.
#[test]
fn presentation_recovers_resizes_and_disables_generation() {
    let Some((device, queue)) = test_support::fsr_device() else {
        return;
    };
    if !device
        .features()
        .contains(sp_fidelity_wgpu::required_features())
    {
        return;
    }
    let mut settings = settings();
    let (mut presenter, mut scene, images, failure) = setup(&device, &queue, &settings);
    *failure.lock().unwrap() = Some("occluded");
    presenter
        .render(&mut scene, &input(), &settings, None)
        .unwrap()
        .present()
        .unwrap();
    assert!(matches!(
        presenter.wait_for_presents(),
        Err(PresenterError::Presentation(PresentationError::Surface(
            "occluded"
        )))
    ));
    *failure.lock().unwrap() = None;
    presenter
        .render(&mut scene, &input(), &settings, None)
        .unwrap()
        .present()
        .unwrap();
    presenter.wait_for_presents().unwrap();
    assert_eq!(images.lock().unwrap().len(), 2);
    let mut config = config();
    config.width = 144;
    config.height = 80;
    presenter.resize(&config, 1., &settings).unwrap();
    settings.frame_generation = FrameGeneration::Off;
    presenter
        .render(&mut scene, &input(), &settings, None)
        .unwrap()
        .present()
        .unwrap();
    presenter.shutdown().unwrap();
    let images = images.lock().unwrap();
    assert_eq!(images.len(), 3);
    assert!(!images[2].0.interpolated);
    assert_eq!(images[2].1.len(), 144 * 80 * 4);
}

// Missing native features must not create FI shaders or shrink/change the
// scene output; the actual presentation boundary receives one real image.
#[test]
fn unsupported_device_falls_back_to_real_frames() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = settings();
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut config = config();
    config.format = wgpu::TextureFormat::Rgb10a2Unorm;
    let (mut presenter, mut scene, images, _) =
        setup_with_config(&device, &queue, &settings, config);
    presenter
        .render(&mut scene, &input(), &settings, None)
        .unwrap()
        .present()
        .unwrap();
    assert!(matches!(
        presenter.frame_generation_status(),
        FrameGenerationStatus::Unavailable(_)
    ));
    presenter.shutdown().unwrap();
    let images = images.lock().unwrap();
    assert_eq!(images.len(), 1);
    assert!(!images[0].0.interpolated);
    assert!(pollster::block_on(scope.pop()).is_none());
}

// A loading frame can disable generation before the initially configured SDK
// ever dispatches. Re-enabling must not execute uploads to destroyed resources.
#[test]
fn generation_survives_reconfiguration_before_first_render() {
    let Some((device, queue)) = test_support::fsr_device() else {
        return;
    };
    if !device
        .features()
        .contains(sp_fidelity_wgpu::required_features())
    {
        return;
    }
    let mut settings = settings();
    let (mut presenter, mut scene, images, _) = setup(&device, &queue, &settings);
    settings.frame_generation = FrameGeneration::Off;
    presenter.resize(&config(), 1., &settings).unwrap();
    settings.frame_generation = FrameGeneration::Fsr3;
    presenter
        .render(&mut scene, &input(), &settings, None)
        .unwrap()
        .present()
        .unwrap();
    presenter.wait_for_presents().unwrap();
    assert_eq!(
        presenter.frame_generation_status(),
        FrameGenerationStatus::Active
    );
    assert_eq!(
        images.lock().unwrap().len(),
        2,
        "generated and real surface presents"
    );
}
