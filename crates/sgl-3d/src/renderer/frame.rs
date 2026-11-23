//! One frame: the frame's scene work once (`shared::Shared::run_scene`),
//! then each of its views in two parts (`view_stages`): its head (prepare,
//! the sky's tables and, in the primary view's order, the scene's GPU work)
//! and its remaining stages. The primary view's head runs first; the views
//! then render in the frame's order (`order::of`), a view that draws a
//! render texture before the views that show it, and each view's reflected
//! views, of the planar reflectors it shows (`reflected`), between its head
//! and its remaining stages.
use super::reflected::{self, Shown};
use super::shared::Shared;
use super::view::{ViewFrame, ViewOutput};
use super::view_stages::{Drawn, Role, begin_view, finish_view};
use crate::Scene;
use crate::settings::Settings;
use crate::stages::prepare::Prepare;
use crate::view::effective::Effective;
use crate::view::history::HistoryFrame;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// A secondary view's `settings` with what a scene has one of taken from
/// the frame's `primary` view's: hardware ray tracing, dynamic GI and
/// anisotropic filtering. The diagnostics layers follow in `secondary`.
fn scene_wide(settings: &Settings, primary: &Settings) -> Settings {
    Settings {
        hardware_ray_tracing: primary.hardware_ray_tracing,
        dynamic_gi: primary.dynamic_gi,
        anisotropic_filtering: primary.anisotropic_filtering,
        ..*settings
    }
}

/// A secondary view's `effective` configuration with the primary view's
/// pipeline layers and material filtering, which the scene and the shared
/// pipelines have one of.
fn secondary(effective: Effective, primary: &Effective) -> Effective {
    Effective {
        layers: primary.layers,
        anisotropy: primary.anisotropy,
        ..effective
    }
}

/// Encodes one frame of `scene` seen by `views`, the first the primary:
/// the scene's frame begins, the frame's order of views is settled, each
/// view it renders begins its frame (settling its last render and beginning
/// its history), the frame's scene work runs once, steered by the primary
/// view, whose head then runs, and each view renders in the frame's order,
/// holding `finished`, the flag `Renderer::finish_frame` sets for this
/// attempt. Returns the scene's frame attempt.
pub(super) fn render(
    shared: &mut Shared,
    (device, queue, encoder): (&wgpu::Device, &wgpu::Queue, &mut wgpu::CommandEncoder),
    scene: &mut Scene,
    views: &mut [ViewFrame<'_>],
    finished: &Arc<AtomicBool>,
) -> u64 {
    let planning = shared.compiler.planning();
    let attempt = scene.begin_frame(device, queue);
    let primary_settings = *views[0].settings;
    // Each view's settings, with what a scene has one of the primary's.
    let settings: Vec<Settings> = views
        .iter()
        .enumerate()
        .map(|(index, frame)| match index {
            0 => primary_settings,
            _ => scene_wide(frame.settings, &primary_settings),
        })
        .collect();
    // The scene's surfaces that name a planar reflector: none, and nothing
    // more of this, where no material names one or no view's level asks
    // for a picture. Then, per view, the reflectors it shows.
    let pictured = settings
        .iter()
        .any(|settings| settings.planar_reflections != crate::settings::PlanarReflections::Off);
    let mirrors = if pictured {
        scene.mirrors()
    } else {
        Vec::new()
    };
    let mut shows: Vec<Vec<Shown>> = views
        .iter()
        .zip(&settings)
        .map(|(frame, settings)| {
            if mirrors.is_empty() {
                return Vec::new();
            }
            let view = &*frame.view;
            reflected::shown(
                scene,
                &mirrors,
                (frame.input, settings, view.sizes.render),
                |reflector| {
                    view.reflected
                        .iter()
                        .find(|held| held.reflector == reflector)
                        .map(|held| held.size)
                },
            )
        })
        .collect();
    // The views the frame renders, in render order; none where no view
    // draws a render texture: all of them, as listed.
    let order = super::order::of(scene, views, &shows);
    let renders = |index: usize| order.as_ref().is_none_or(|order| order.contains(&index));
    let mut begun: Vec<Option<(HistoryFrame, Effective)>> = Vec::with_capacity(views.len());
    // Per view, the planar reflectors it shows, each with its reflected
    // view's begun frame.
    let mut reflections: Vec<Vec<(Shown, (HistoryFrame, Effective))>> =
        Vec::with_capacity(views.len());
    let mut primary_effective = None;
    for (index, frame) in views.iter_mut().enumerate() {
        let mut shows = std::mem::take(&mut shows[index]);
        let settings = settings[index];
        // The view's reflected views: those of the reflectors it shows,
        // and those it showed within the idle period. A view the frame
        // leaves out shows nothing, which counts as a frame of it.
        let reflected_settings = reflected::settings(&settings);
        if !renders(index) {
            shows.clear();
        }
        if !shows.is_empty() || !frame.view.reflected.is_empty() {
            let full = reflected::full_size(frame.view.sizes.render, settings.planar_reflections);
            reflected::keep(
                (device, queue),
                shared,
                frame.view,
                &shows,
                (scene, &reflected_settings, full),
            );
            let held = &frame.view.reflected;
            shows.retain(|shown| held.iter().any(|held| held.reflector == shown.reflector));
        }
        if !renders(index) {
            begun.push(None);
            reflections.push(Vec::new());
            continue;
        }
        // What the view shows, for the stages that mark its surfaces.
        let mirrors = &mut frame.view.views.mirrors;
        mirrors.clear();
        mirrors.extend(shows.iter().map(|shown| shown.reflector));
        let (history, effective) = frame.view.begin(
            device,
            shared,
            scene,
            frame.input,
            &settings,
            !shows.is_empty(),
        );
        let effective = match &primary_effective {
            Some(primary) => secondary(effective, primary),
            None => effective,
        };
        let primary = *primary_effective.get_or_insert(effective);
        begun.push(Some((history, effective)));
        let held = &mut frame.view.reflected;
        reflections.push(
            shows
                .into_iter()
                .filter_map(|shown| {
                    let reflected = held
                        .iter_mut()
                        .find(|held| held.reflector == shown.reflector)?;
                    let (history, effective) = reflected.view.begin(
                        device,
                        shared,
                        scene,
                        &shown.input,
                        &reflected_settings,
                        false,
                    );
                    #[allow(unused_mut)]
                    let mut effective = secondary(effective, &primary);
                    // The frame probe observes the views a game lists.
                    #[cfg(feature = "diagnostics")]
                    {
                        effective.frame_probe = false;
                    }
                    Some((shown, (history, effective)))
                })
                .collect(),
        );
    }
    let primary_effective = primary_effective.expect("the primary view renders");
    // Update ray instances when any view of the frame traces.
    let traced = begun
        .iter()
        .flatten()
        .chain(reflections.iter().flatten().map(|(_, begun)| begun))
        .any(|(_, effective)| Prepare::traces(scene, effective));
    shared.run_scene(
        (device, queue),
        scene,
        &primary_effective,
        views[0].input,
        traced,
    );
    // The render textures the frame's views draw into, whose pictures
    // change with it.
    let mut pictures: Vec<Option<wgpu::TextureView>> = Vec::new();
    if order.is_some() {
        for (index, frame) in views.iter().enumerate() {
            pictures.push(match frame.output {
                ViewOutput::RenderTexture(id) if renders(index) => {
                    scene.render_textures.drawn(id);
                    scene
                        .render_textures
                        .get(id)
                        .ok()
                        .map(|held| held.view.clone())
                }
                _ => None,
            });
        }
    }
    let scene = &*scene;
    let head = |shared: &mut Shared,
                encoder: &mut wgpu::CommandEncoder,
                frame: &mut ViewFrame<'_>,
                begun: (HistoryFrame, Effective),
                role: Role| {
        begin_view(
            shared,
            frame.view,
            (device, queue, encoder),
            scene,
            // A planning pass's commands are never submitted.
            (frame.input, frame.timing.filter(|_| !planning)),
            begun,
            (role, attempt, finished),
        )
    };
    // The primary view's head first: the scene's GPU work runs in it, before
    // any view draws.
    let mut primary = Some(head(
        shared,
        encoder,
        &mut views[0],
        begun[0].take().expect("the primary view began"),
        Role::Primary,
    ));
    let mut listed = 0..views.len();
    let mut ordered = order.iter().flatten().copied();
    while let Some(index) = match &order {
        Some(_) => ordered.next(),
        None => listed.next(),
    } {
        let frame = &mut views[index];
        let begun = if index == 0 {
            primary.take().expect("the primary view renders once")
        } else {
            let began = begun[index].take().expect("a rendered view began");
            head(shared, encoder, frame, began, Role::Secondary)
        };
        // Its reflected views, whole, before the stages that sample their
        // pictures: each an ordinary view at the mirrored eye that stops
        // after its transparent stage.
        // A planning pass's commands are never submitted.
        let timing = frame.timing.filter(|_| !planning);
        let mut held = std::mem::take(&mut frame.view.reflected);
        let mut shown_pictures = Vec::new();
        for (shown, began) in std::mem::take(&mut reflections[index]) {
            let Some(reflected) = held
                .iter_mut()
                .find(|held| held.reflector == shown.reflector)
            else {
                continue;
            };
            let view = &mut *reflected.view;
            view.views.reflected = Some(crate::view::Reflected {
                reflector: shown.reflector,
                plane: shown.plane,
            });
            let began = begin_view(
                shared,
                view,
                (device, queue, encoder),
                scene,
                (&shown.input, timing),
                began,
                (Role::Secondary, attempt, finished),
            );
            finish_view(
                shared,
                view,
                (device, queue, encoder),
                scene,
                Drawn {
                    input: &shown.input,
                    output: None,
                    pictures: &[],
                    keeps_pictures: false,
                    timing,
                },
                began,
            );
            shown_pictures.push(shown.picture(view));
        }
        frame.view.reflected = held;
        let keeps_pictures = !frame.view.reflected.is_empty();
        let output = match frame.output {
            ViewOutput::Texture(output) => output,
            ViewOutput::RenderTexture(_) => pictures[index]
                .as_ref()
                .expect("a rendered view's render texture lives"),
        };
        finish_view(
            shared,
            frame.view,
            (device, queue, encoder),
            scene,
            Drawn {
                input: frame.input,
                output: Some(output),
                pictures: &shown_pictures,
                // The planar method's targets and pipeline live as long as
                // the reflected views it composites.
                keeps_pictures,
                // A planning pass's commands are never submitted.
                timing: frame.timing.filter(|_| !planning),
            },
            begun,
        );
    }
    attempt
}
