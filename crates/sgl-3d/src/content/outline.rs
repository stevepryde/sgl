//! What a view's frame outlines (`FrameInput::outline`): one of the scene's
//! selections (`Scene::add_selection`) and how its outline looks.
use super::identity::SelectionId;

/// An outline of a selection's instances in one view's frame: a line
/// `width` output pixels wide around their combined silhouette, drawn over
/// the finished picture before the game's HUD, in `visible_color` beside
/// the parts of the selection in sight and `hidden_color` beside the parts
/// opaque or alpha-cut surfaces hide. `Copy`, so a `FrameInput` carries it
/// by value; the scene holds the instances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outline {
    /// The instances to outline.
    pub selection: SelectionId,
    /// The line's width in physical pixels of the view's output, whatever
    /// the render or scene scale, clamped to `0..=Outline::MAX_WIDTH` (NaN
    /// to 0); 0 draws nothing.
    pub width: f32,
    /// sRGB-encoded colour and straight alpha beside the parts in sight, as
    /// a HUD's colour: displayed as given, not exposed or tone mapped.
    pub visible_color: [f32; 4],
    /// sRGB-encoded colour and straight alpha beside the hidden parts; an
    /// alpha of 0 outlines only what is in sight.
    pub hidden_color: [f32; 4],
}

impl Outline {
    /// The widest outline, in output pixels.
    pub const MAX_WIDTH: f32 = 32.;

    /// `selection` outlined 2 pixels wide, white where it is in sight and
    /// white at 40% where it is hidden.
    pub fn new(selection: SelectionId) -> Self {
        Self {
            selection,
            width: 2.,
            visible_color: [1.; 4],
            hidden_color: [1., 1., 1., 0.4],
        }
    }

    /// `width` as drawn: clamped to `0..=MAX_WIDTH`, NaN as 0.
    pub(crate) fn drawn_width(&self) -> f32 {
        if self.width.is_nan() {
            0.
        } else {
            self.width.clamp(0., Self::MAX_WIDTH)
        }
    }
}
