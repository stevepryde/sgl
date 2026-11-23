//! The order of a frame's views (the architecture's Frame, "Render
//! textures"): which of the listed views the frame renders, a view that
//! draws a render texture only where another rendered view shows it, and
//! in which order, each such producer before the views that sample its
//! picture.
use super::view::{ViewFrame, ViewOutput};
use crate::Scene;
use crate::content::identity::RenderTextureId;
use crate::scene::render_textures::Screen;
use crate::view::culling::Frustum;
use glam::{Mat4, Vec3};

/// The views of `views` the frame of `scene` renders, as indices in render
/// order; none for a frame none of whose views draws a render texture,
/// which renders them all as listed.
///
/// # Panics
///
/// For a view that draws a render texture the scene does not hold or one
/// of another size or format than its own, and for two views that draw one
/// render texture.
pub(super) fn of(scene: &Scene, views: &[ViewFrame<'_>]) -> Option<Vec<usize>> {
    if views
        .iter()
        .all(|frame| matches!(frame.output, ViewOutput::Texture(_)))
    {
        return None;
    }
    let targets: Vec<Option<RenderTextureId>> = views
        .iter()
        .map(|frame| match frame.output {
            ViewOutput::Texture(_) => None,
            ViewOutput::RenderTexture(id) => Some(id),
        })
        .collect();
    for (index, (frame, target)) in views.iter().zip(&targets).enumerate() {
        let Some(id) = *target else { continue };
        let held = scene
            .render_textures
            .get(id)
            .expect("a view draws a render texture of the frame's scene");
        assert!(
            frame.view.sizes.output == held.description.size
                && frame.view.output_format == held.description.format(),
            "a view that draws a render texture has its size and format (`RenderTexture::format`, `View::new`, `View::resize`)"
        );
        assert!(
            !targets[..index].contains(target),
            "one view draws a render texture in a frame"
        );
    }
    let screens = scene.screens();
    // For each view, the listed views whose render textures it samples.
    let samples: Vec<Vec<usize>> = views
        .iter()
        .map(|frame| {
            let sampled = sampled(frame, &screens);
            (0..views.len())
                .filter(|&producer| targets[producer].is_some_and(|id| sampled.contains(&id)))
                .collect()
        })
        .collect();
    let always: Vec<bool> = targets
        .iter()
        .enumerate()
        .map(|(index, target)| index == 0 || target.is_none())
        .collect();
    Some(order(&always, &samples))
}

/// The render textures a surface of `screens` samples within `frame`'s
/// view: in its visibility mask and its frustum, which is widened by a
/// render pixel on each side, more than antialiasing's jitter moves it.
fn sampled(frame: &ViewFrame<'_>, screens: &[Screen]) -> Vec<RenderTextureId> {
    let camera = &frame.input.camera;
    let [width, height] = frame.view.sizes.render.map(|side| side as f32);
    let widened = Mat4::from_scale(Vec3::new(width / (width + 2.), height / (height + 2.), 1.))
        * camera.projection;
    let mut sampled = Vec::new();
    for screen in screens {
        if screen.enabled(frame.input.visibility_mask)
            && !sampled.contains(&screen.texture)
            && Frustum::new(camera.view, widened, screen.pose, [0.; 2]).reaches(screen.bounds)
        {
            sampled.push(screen.texture);
        }
    }
    sampled
}

/// The views a frame renders, in render order. View `v` renders whatever
/// samples it where `always[v]`; any other is a producer, and renders where
/// a rendered view other than itself samples it (`samples[v]` lists the
/// views `v` samples). Each step takes the first listed view all of whose
/// producers still to render sample it in turn, directly or through other
/// views: one that waits on no one, or one of a loop of views that feed
/// each other, once everything the loop samples from outside has rendered.
/// So only a view inside a loop ever samples a picture of the frame before.
fn order(always: &[bool], samples: &[Vec<usize>]) -> Vec<usize> {
    let count = always.len();
    let mut rendered = always.to_vec();
    loop {
        let more = (0..count).find(|&producer| {
            !rendered[producer]
                && (0..count).any(|consumer| {
                    consumer != producer
                        && rendered[consumer]
                        && samples[consumer].contains(&producer)
                })
        });
        match more {
            Some(producer) => rendered[producer] = true,
            None => break,
        }
    }
    // Whether view `a` samples view `b`, directly or through other views.
    let mut reaches = vec![vec![false; count]; count];
    for (consumer, producers) in samples.iter().enumerate() {
        for &producer in producers {
            reaches[consumer][producer] = true;
        }
    }
    for through in 0..count {
        for from in 0..count {
            for to in 0..count {
                reaches[from][to] |= reaches[from][through] && reaches[through][to];
            }
        }
    }
    let mut remaining: Vec<usize> = (0..count).filter(|&view| rendered[view]).collect();
    let mut order = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let next = remaining.iter().position(|&view| {
            samples[view].iter().all(|&producer| {
                producer == view || !remaining.contains(&producer) || reaches[producer][view]
            })
        });
        // A view of a loop nothing still to render feeds from outside
        // always qualifies.
        order.push(remaining.remove(next.expect("a view waits on no view outside its loop")));
    }
    order
}

#[cfg(test)]
mod tests {
    use super::order;

    /// A main view (0) shows a monitor fed by view 1: the producer renders
    /// first though it is listed second.
    #[test]
    fn a_producer_renders_before_the_view_that_shows_it() {
        assert_eq!(order(&[true, false], &[vec![1], vec![]]), [1, 0]);
    }

    /// Nothing rendered samples view 1, so it is left out; view 2, a
    /// producer view 0 samples, is not.
    #[test]
    fn a_producer_nothing_rendered_samples_is_left_out() {
        assert_eq!(
            order(&[true, false, false], &[vec![2], vec![], vec![]]),
            [2, 0]
        );
    }

    /// View 1 samples only itself: no other view shows it.
    #[test]
    fn a_producer_only_it_samples_is_left_out() {
        assert_eq!(order(&[true, false], &[vec![], vec![1]]), [0]);
    }

    /// View 0 shows view 1, which shows view 2, which nothing else shows:
    /// the chain renders deepest first.
    #[test]
    fn a_chain_renders_deepest_first() {
        assert_eq!(
            order(&[true, false, false], &[vec![1], vec![2], vec![]]),
            [2, 1, 0]
        );
    }

    /// Views 1 and 2 show each other and view 0, listed first, shows
    /// both: the loop renders in listed order, and the view outside it
    /// after both, so it samples this frame's picture of each.
    #[test]
    fn a_loop_renders_in_listed_order_before_the_views_that_show_it() {
        assert_eq!(
            order(&[true, false, false], &[vec![1, 2], vec![2], vec![1]]),
            [1, 2, 0]
        );
    }

    /// As above, but view 0 shows view 1 alone: it waits for view 1 only.
    #[test]
    fn a_view_waits_only_for_the_producers_it_samples() {
        assert_eq!(
            order(&[true, false, false], &[vec![1], vec![2], vec![1]]),
            [1, 0, 2]
        );
    }

    /// The loop of views 1 and 2 samples view 3 from outside: view 3
    /// renders before the loop's view that samples it, and the loop's other
    /// view, which waits only on its loop, need not wait for it.
    #[test]
    fn a_loop_follows_what_it_samples_from_outside() {
        assert_eq!(
            order(
                &[true, false, false, false],
                &[vec![1], vec![2, 3], vec![1], vec![]]
            ),
            [2, 3, 1, 0]
        );
    }

    /// A view that samples its own picture among others waits for the
    /// others only.
    #[test]
    fn a_view_that_samples_itself_waits_for_no_one() {
        assert_eq!(order(&[true, false], &[vec![1], vec![1]]), [1, 0]);
    }

    /// Two consumers of one producer: it renders once, before both.
    #[test]
    fn two_consumers_follow_one_producer() {
        assert_eq!(
            order(&[true, true, false], &[vec![2], vec![2], vec![]]),
            [2, 0, 1]
        );
    }

    /// Without samples the views render as listed.
    #[test]
    fn views_that_sample_nothing_render_as_listed() {
        assert_eq!(
            order(&[true, true, true], &[vec![], vec![], vec![]]),
            [0, 1, 2]
        );
    }
}
