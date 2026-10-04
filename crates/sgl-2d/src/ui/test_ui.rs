//! Shared fixture, input builders and rects for the `ui` unit tests.

use super::{Ui, UiInput};
use crate::assets::{Assets, Texture};
use crate::canvas::draw::Rect;
use crate::canvas::text::TextRenderer;
use sgl_core::math::Vec2;

pub(super) fn fixture() -> (Ui, TextRenderer, Assets<Texture>) {
    let mut assets: Assets<Texture> = Assets::new();
    let ui = Ui::new(&mut assets);
    let text = TextRenderer::new(include_bytes!(
        "../../tests/fixtures/IBMPlexSans-Regular.ttf"
    ))
    .expect("font should load");
    (ui, text, assets)
}

pub(super) fn press_at(x: f32, y: f32) -> UiInput {
    UiInput {
        mouse_pos: Vec2::new(x, y),
        mouse_pressed: true,
        mouse_down: true,
        ..UiInput::default()
    }
}

pub(super) fn hover_at(x: f32, y: f32, down: bool) -> UiInput {
    UiInput {
        mouse_pos: Vec2::new(x, y),
        mouse_down: down,
        ..UiInput::default()
    }
}

pub(super) const BTN: Rect = Rect {
    min: Vec2::new(10.0, 10.0),
    max: Vec2::new(110.0, 50.0),
};

pub(super) const ROW: Rect = Rect {
    min: Vec2::new(10.0, 100.0),
    max: Vec2::new(250.0, 132.0),
};

pub(super) fn release_at(x: f32, y: f32) -> UiInput {
    UiInput {
        mouse_pos: Vec2::new(x, y),
        mouse_released: true,
        ..UiInput::default()
    }
}
