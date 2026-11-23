//! Planar reflections' views (the architecture's Frame, "Planar
//! reflections"): which reflectors a view of a frame shows, and the
//! reflected views it owns for them, each an ordinary `View` at the
//! mirrored eye whose composite is the reflector's picture for that viewer.
use super::shared::Shared;
use super::view::View;
use crate::content::identity::PlanarReflectorId;
use crate::scene::planar_reflectors::Mirror;
use crate::settings::{
    AmbientOcclusionQuality, Antialiasing, Bloom, ContactShadows, DepthOfField, MotionBlur,
    PlanarReflections, SceneResolution, ScreenSpaceIndirect, ScreenSpaceReflections, Settings,
    ShadowQuality, Sharpening, Upscaling, WorldSpaceReflections,
};
use crate::shading::gbuffer;
use crate::stages::reflections::planar::Picture;
use crate::view::reflected_camera::{self, Crop, Rect};
use crate::{FrameInput, Scene};
use glam::{Mat4, Vec2, Vec3, Vec4};

/// A planar reflector a view showed in its last rendered frame
/// (`View::planar_reflections`).
#[derive(Clone, Copy, Debug)]
pub struct PlanarReflectionStats {
    pub reflector: crate::PlanarReflectorId,
    /// Its picture's size in texels: the reflector's bounds on screen at
    /// `Settings::planar_reflections`' level.
    pub size: [u32; 2],
    /// What the reflected view drew in its most recent completed frame;
    /// None until a readback has arrived, a few frames after it first
    /// renders.
    pub geometry: Option<crate::GeometryStats>,
}

impl View {
    /// The planar reflectors the last rendered frame showed in this view
    /// (the architecture's Frame, "Planar reflections"): each one's
    /// picture size in texels and what its reflected view submitted in its
    /// most recent completed frame, as `geometry_stats` reports the
    /// camera's. Empty for a view that showed none, which then rendered no
    /// reflected view; one it showed within the last
    /// `REFLECTED_VIEW_IDLE_FRAMES` of its frames it still holds, unrendered
    /// (the architecture's Frame, "Planar reflections").
    pub fn planar_reflections(&mut self, device: &wgpu::Device) -> Vec<PlanarReflectionStats> {
        self.reflected
            .iter_mut()
            .filter(|reflected| reflected.idle == 0)
            .map(|reflected| PlanarReflectionStats {
                reflector: reflected.reflector,
                size: reflected.size,
                geometry: reflected.view.geometry_stats(device),
            })
            .collect()
    }
}

/// A reflected view a view owns for a planar reflector it shows, or showed
/// within the last `REFLECTED_VIEW_IDLE_FRAMES` of its frames.
pub(crate) struct ReflectedView {
    pub reflector: PlanarReflectorId,
    pub view: Box<View>,
    /// Its picture's size in texels.
    pub size: [u32; 2],
    /// The view's frames since the last that showed the reflector: 0 in a
    /// frame that shows it.
    pub idle: u32,
    /// The whole mirrored image's size it was last shown at
    /// (`full_size`).
    pub full: [u32; 2],
}

/// A reflector a view shows in a frame, and how its reflected view renders
/// it.
pub(super) struct Shown {
    pub reflector: PlanarReflectorId,
    /// The reflector's plane (unit normal, d).
    pub plane: Vec4,
    /// The bounds of the reflector's surfaces in the viewer's image.
    pub bounds: Rect,
    /// The picture's size and the rectangle of the mirrored image it holds.
    pub crop: Crop,
    /// The viewer's frame input with the reflected camera.
    pub input: FrameInput,
}

impl Shown {
    /// The reflected camera's projection as raster takes it: clipped at
    /// the reflector's plane.
    pub fn raster_projection(&self) -> Mat4 {
        let camera = self.input.camera;
        reflected_camera::oblique(camera.projection, camera.view, self.plane)
            .unwrap_or(camera.projection)
    }

    /// The picture `view`, this reflector's reflected view, rendered in
    /// this frame, for its viewer's reflections.
    pub fn picture(&self, view: &View) -> Picture {
        let camera = self.input.camera;
        let raster = self.raster_projection();
        Picture {
            reflector: self.reflector,
            color: view.targets.composite.clone(),
            depth: view.targets.depth.clone(),
            picture_from_world: camera.projection * camera.view,
            world_from_picture: (raster * camera.view).inverse(),
            plane: self.plane,
            bounds: self.bounds,
        }
    }
}

/// The reflected view's settings for a viewer's `settings`: what it has no
/// need of off (its picture is sampled before the viewer's own exposure,
/// antialiasing and post), and one level deep. Everything that lights the
/// scene stays the viewer's, but for the shadow maps' sizes below `Full`:
/// a picture of at most half the viewer's texels a side takes `Low`'s
/// maps, a quarter of the memory (a view holds its cascades from when it
/// is made: 16 MiB at `Low`, 64 MiB at `High`).
pub(super) fn settings(settings: &Settings) -> Settings {
    let shadow_quality = match settings.planar_reflections {
        PlanarReflections::Full => settings.shadow_quality,
        _ => ShadowQuality::Low,
    };
    Settings {
        shadow_quality,
        scene_resolution: SceneResolution::Full,
        upscaling: Upscaling::Off,
        antialiasing: Antialiasing::Off,
        sharpening: Sharpening::Off,
        // Stages it stops before: none holds a target for it.
        bloom: Bloom::Off,
        depth_of_field: DepthOfField::Off,
        motion_blur: MotionBlur::Off,
        ambient_occlusion: AmbientOcclusionQuality::Off,
        screen_space_indirect: ScreenSpaceIndirect::Off,
        screen_space_reflections: ScreenSpaceReflections::Off,
        world_space_reflections: WorldSpaceReflections::Off,
        planar_reflections: PlanarReflections::Off,
        ray_traced_shadows: false,
        contact_shadows: ContactShadows::Off,
        occlusion_culling: false,
        heat_distortion: false,
        ..*settings
    }
}

/// How far in front of a reflector's plane an eye at `eye` must be for its
/// reflection to be defined: beyond what `f32` resolves there.
fn in_front(plane: Vec4, eye: Vec3) -> bool {
    let tolerance = 64. * f32::EPSILON * eye.abs().max_element().max(plane.w.abs()).max(1.);
    plane.dot(eye.extend(1.)) > tolerance
}

/// The reflectors a view of `render` pixels seeing `input` with `settings`
/// shows among `mirrors`, the scene's surfaces that name one: in the order
/// the scene lists them, each whose plane the eye is in front of and one of
/// whose surfaces lies in the view's visibility mask and in its frustum,
/// widened by a render pixel as a screen's is (`view::culling::widening`).
/// Each reflector is tested once, against all its surfaces. A view whose
/// projection is not a perspective one shows none: the picture's lookup
/// and its clip take a view ray from the eye. `held` gives the size of the
/// picture the view holds for a reflector.
pub(super) fn shown(
    scene: &Scene,
    mirrors: &[Mirror],
    (input, settings, render): (&FrameInput, &Settings, [u32; 2]),
    held: impl Fn(PlanarReflectorId) -> Option<[u32; 2]>,
) -> Vec<Shown> {
    let mut shown: Vec<Shown> = Vec::new();
    let Some(scale) = settings.planar_reflections.scale() else {
        return shown;
    };
    let camera = &input.camera;
    if mirrors.is_empty()
        || render.contains(&0)
        || !super::effective::perspective(camera.projection)
    {
        return shown;
    }
    let widen = crate::view::culling::widening(render);
    let widened = Mat4::from_scale(widen.extend(1.)) * camera.projection * camera.view;
    let mut tested: Vec<PlanarReflectorId> = Vec::new();
    for mirror in mirrors {
        if tested.contains(&mirror.name) {
            continue;
        }
        tested.push(mirror.name);
        let Ok(reflector) = scene.planar_reflectors.get(mirror.name) else {
            continue;
        };
        if !in_front(reflector.plane, camera.eye) {
            continue;
        }
        let surfaces = mirrors
            .iter()
            .filter(|other| other.name == mirror.name && other.enabled(input.visibility_mask))
            .map(|surface| (surface.pose, surface.bounds));
        let Some(bounds) = reflected_camera::image_bounds(widened, surfaces) else {
            continue;
        };
        // In the camera's own image, which the widened one holds a pixel
        // inside each edge.
        let bounds = Rect {
            min: (bounds.min / widen).max(Vec2::splat(-1.)),
            max: (bounds.max / widen).min(Vec2::splat(1.)),
        };
        let full = render.map(|side| ((side as f32 * scale).ceil() as u32).max(1));
        let needed = Rect {
            min: reflected_camera::mirrored(Vec2::new(bounds.max.x, bounds.min.y)),
            max: reflected_camera::mirrored(Vec2::new(bounds.min.x, bounds.max.y)),
        };
        let crop = reflected_camera::crop(needed, full, held(mirror.name));
        let reflected = reflected_camera::camera(camera, reflector.plane, crop.rect);
        if reflected_camera::oblique(reflected.projection, reflected.view, reflector.plane)
            .is_none()
        {
            continue;
        }
        shown.push(Shown {
            reflector: mirror.name,
            plane: reflector.plane,
            bounds,
            crop,
            // The viewer's frame input with the reflected camera, and no
            // HDR effect: the reflected view stops before it.
            input: FrameInput {
                camera: reflected,
                hdr_effect: None,
                ..*input
            },
        });
    }
    shown
}

/// How many frames a view keeps a reflector's reflected view after the
/// last frame it showed the reflector, before it releases it: two seconds
/// at 60 Hz, one at 120 Hz. A view made again costs its frame 8 to 15 ms of
/// CPU (its stages' programs and targets, #888), so a camera sweeping off
/// a mirror and back within that keeps it and makes nothing; a level of
/// mirrors left behind is released within that many frames of play.
pub(crate) const REFLECTED_VIEW_IDLE_FRAMES: u32 = 120;

/// The size of the whole mirrored image a view of `render` pixels at
/// `level` makes its pictures from, none at `Off`.
pub(super) fn full_size(render: [u32; 2], level: PlanarReflections) -> Option<[u32; 2]> {
    let scale = level.scale()?;
    Some(render.map(|side| ((side as f32 * scale).ceil() as u32).max(1)))
}

/// Makes `view`'s reflected views those of the reflectors it `shows`, in
/// that order, followed by those it keeps: a reflector it shows for the
/// first time gets a view made with `settings` (`settings` below), and
/// each is sized to its picture, whose history restarts when the size
/// changes. A reflector it no longer shows keeps its view, not rendered,
/// for `REFLECTED_VIEW_IDLE_FRAMES` of the view's frames, so the frame it
/// returns makes nothing; it is released at once where the view's level
/// is `Off` (`full` none), the scene no longer holds the reflector, or the
/// whole mirrored image's size (`full`: the view's render size at its
/// level) changed since it was last shown, which would resize it.
pub(super) fn keep(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    shared: &Shared,
    view: &mut View,
    shows: &[Shown],
    (scene, settings, full): (&Scene, &Settings, Option<[u32; 2]>),
) {
    let mut held = std::mem::take(&mut view.reflected);
    for shown in shows {
        let reflected = match held
            .iter()
            .position(|reflected| reflected.reflector == shown.reflector)
        {
            Some(index) => {
                let mut reflected = held.swap_remove(index);
                reflected.view.resize(device, shown.crop.size, 1., settings);
                reflected.size = shown.crop.size;
                reflected
            }
            None => ReflectedView {
                reflector: shown.reflector,
                view: Box::new(View::create(
                    (shared, view.renderer),
                    (device, queue),
                    crate::stages::post::Output::texture(gbuffer::COLOR),
                    shown.crop.size,
                    1.,
                    settings,
                )),
                size: shown.crop.size,
                idle: 0,
                full: [0; 2],
            },
        };
        view.reflected.push(ReflectedView {
            idle: 0,
            full: full.unwrap_or_default(),
            ..reflected
        });
    }
    // The views of the reflectors it no longer shows: kept with their
    // programs, their picture's targets and their shadow maps released the
    // first frame they are not shown, since those take about a millisecond
    // to make again where the view takes 8 to 15 (D-77).
    for mut reflected in held {
        reflected.idle += 1;
        let kept = full == Some(reflected.full)
            && reflected.idle <= REFLECTED_VIEW_IDLE_FRAMES
            && scene.planar_reflectors.get(reflected.reflector).is_ok();
        if !kept {
            continue;
        }
        if reflected.idle == 1 {
            reflected.view.resize(device, [1, 1], 1., settings);
            reflected.view.shadows.release(device);
        }
        view.reflected.push(reflected);
    }
}
