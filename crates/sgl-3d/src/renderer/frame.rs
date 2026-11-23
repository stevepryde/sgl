//! One frame: the frame's scene work once (`shared::Shared::run_scene`),
//! then each of its views in two parts (`view_stages`): its head (prepare,
//! the sky's tables and, in the primary view's order, the scene's GPU work)
//! and its remaining stages. The primary view's head runs first; the views
//! then render in the frame's order (`order::of`), a view that draws a
//! render texture before the views that show it.
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
    let attempt = scene.begin_frame(device, queue);
    // The views the frame renders, in render order; none where no view
    // draws a render texture: all of them, as listed.
    let order = super::order::of(scene, views);
    let renders = |index: usize| order.as_ref().is_none_or(|order| order.contains(&index));
    let primary_settings = *views[0].settings;
    let mut begun: Vec<Option<(HistoryFrame, Effective)>> = Vec::with_capacity(views.len());
    let mut primary_effective = None;
    for (index, frame) in views.iter_mut().enumerate() {
        if !renders(index) {
            begun.push(None);
            continue;
        }
        let settings = match index {
            0 => primary_settings,
            _ => scene_wide(frame.settings, &primary_settings),
        };
        let (history, effective) = frame
            .view
            .begin(device, shared, scene, frame.input, &settings);
        let effective = match &primary_effective {
            Some(primary) => secondary(effective, primary),
            None => effective,
        };
        primary_effective.get_or_insert(effective);
        begun.push(Some((history, effective)));
    }
    let primary_effective = primary_effective.expect("the primary view renders");
    // Update ray instances when any view of the frame traces.
    let traced = begun
        .iter()
        .flatten()
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
            (frame.input, frame.timing),
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
                output,
                timing: frame.timing,
            },
            begun,
        );
    }
    attempt
}
