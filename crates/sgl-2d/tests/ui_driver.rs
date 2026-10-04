//! Random-input driver for the immediate-mode UI (client.md 7). A fixed
//! widget tree is rebuilt every frame from a proptest-generated `UiInput`
//! sequence with consistent press/release edges; each frame checks the
//! interaction contract against small models written from the spec, not
//! from the `ui` module. Fixed seed (testing.md 3); native only (proptest).
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::float_cmp
)]

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use sgl_2d::assets::{Assets, Texture};
use sgl_2d::canvas::text::TextRenderer;
use sgl_2d::canvas::{DrawList, Rect};
use sgl_2d::ui::{FpsCounter, NumberField, Ui, UiInput, edit_apply};
use sgl_core::math::Vec2;

const SEED: [u8; 32] = *b"sgl-client ui driver seed     01";

fn check<S: Strategy>(strategy: S, test: impl Fn(S::Value) -> Result<(), TestCaseError>) {
    let config = Config {
        cases: 128,
        failure_persistence: None,
        ..Config::default()
    };
    let mut runner =
        TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &SEED));
    if let Err(failure) = runner.run(&strategy, test) {
        panic!("{failure}");
    }
}

// ------------------------------------------------------------- the tree

const SCREEN: Rect = rect(0.0, 0.0, 960.0, 540.0);
const BUTTON: Rect = rect(10.0, 10.0, 120.0, 40.0);
const TOGGLE: Rect = rect(140.0, 10.0, 120.0, 40.0);
const RADIOS: [Rect; 3] = [
    rect(10.0, 60.0, 120.0, 32.0),
    rect(140.0, 60.0, 120.0, 32.0),
    rect(270.0, 60.0, 120.0, 32.0),
];
const EDIT: Rect = rect(10.0, 110.0, 300.0, 32.0);
const EDIT_MAX: usize = 8;
const PASSWORD: Rect = rect(10.0, 150.0, 300.0, 32.0);
const PASSWORD_MAX: usize = 6;
/// The clear button inside the password field: a square on its right.
const PASSWORD_CLEAR: Rect = rect(278.0, 150.0, 32.0, 32.0);
const DROPDOWN: Rect = rect(10.0, 200.0, 200.0, 32.0);
const OPTIONS: [&str; 3] = ["alpha", "beta", "gamma"];
/// The dropdown's popover: two pixels below the button, one row per option.
const POPOVER: Rect = rect(10.0, 234.0, 200.0, 96.0);
/// Deliberately underneath the popover, to exercise input blocking.
const OPEN_MODAL: Rect = rect(10.0, 260.0, 150.0, 40.0);
const SCROLL: Rect = rect(400.0, 10.0, 200.0, 200.0);
const CONTENT_H: f32 = 600.0;
const INNER_Y: [f32; 3] = [20.0, 300.0, 500.0];
/// The modal's CONFIRM / CANCEL buttons (a 420×160 panel centered on the
/// screen, 150×44 buttons 40 px apart, 16 px above the panel's bottom).
const CONFIRM: Rect = rect(310.0, 290.0, 150.0, 44.0);
const CANCEL: Rect = rect(500.0, 290.0, 150.0, 44.0);
/// The #264 form widgets, right of the scroll area, clear of the modal panel.
const CHECKBOX: Rect = rect(620.0, 10.0, 200.0, 32.0);
const HEADER: Rect = rect(620.0, 50.0, 300.0, 32.0);
const NUMBER: Rect = rect(620.0, 90.0, 240.0, 32.0);
/// The numeric field's `-` / `+` squares (side = the row height) and the
/// draggable middle between them.
const NUMBER_DEC: Rect = rect(620.0, 90.0, 32.0, 32.0);
const NUMBER_INC: Rect = rect(828.0, 90.0, 32.0, 32.0);
const NUMBER_MID: Rect = rect(652.0, 90.0, 176.0, 32.0);
const NUMBER_OPTS: NumberField = NumberField {
    speed: 0.5,
    step: 1.0,
    min: -50.0,
    max: 50.0,
    decimals: 2,
};
/// Pointer travel that turns a numeric-field press into a scrub (client.md 7).
const NUMBER_DRAG_THRESHOLD: f32 = 3.0;
const NUMBER_EDIT_MAX: usize = 24;
const PX: f32 = 20.0;

const fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect {
        min: Vec2::new(x, y),
        max: Vec2::new(x + w, y + h),
    }
}

fn contains(r: &Rect, p: Vec2) -> bool {
    p.x >= r.min.x && p.x < r.max.x && p.y >= r.min.y && p.y < r.max.y
}

fn inner_button(index: usize, offset: f32) -> Rect {
    rect(410.0, SCROLL.min.y - offset + INNER_Y[index], 100.0, 30.0)
}

// ----------------------------------------------------------------- input

#[derive(Debug, Clone)]
struct Step {
    target: Vec2,
    press: bool,
    release: bool,
    chars: Vec<char>,
    backspace: bool,
    scroll: f32,
    dt: f32,
}

fn point_in(r: Rect) -> impl Strategy<Value = Vec2> {
    let size = r.size();
    (0.0f32..1.0, 0.0f32..1.0).prop_map(move |(u, v)| r.min + Vec2::new(u * size.x, v * size.y))
}

fn target() -> impl Strategy<Value = Vec2> {
    prop_oneof![
        2 => point_in(BUTTON),
        1 => point_in(TOGGLE),
        2 => point_in(rect(10.0, 60.0, 380.0, 32.0)),
        3 => point_in(EDIT),
        3 => point_in(PASSWORD),
        2 => point_in(PASSWORD_CLEAR),
        3 => point_in(DROPDOWN),
        3 => point_in(POPOVER),
        3 => point_in(OPEN_MODAL),
        2 => point_in(CONFIRM),
        2 => point_in(CANCEL),
        3 => point_in(SCROLL),
        2 => point_in(CHECKBOX),
        2 => point_in(HEADER),
        2 => point_in(NUMBER_DEC),
        2 => point_in(NUMBER_INC),
        3 => point_in(NUMBER_MID),
        2 => point_in(SCREEN),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    (
        target(),
        prop::bool::weighted(0.4),
        prop::bool::weighted(0.4),
        prop::collection::vec(prop::char::range(' ', '~'), 0..3),
        prop::bool::weighted(0.15),
        -3.0f32..3.0,
        0.0f32..0.05,
    )
        .prop_map(
            |(target, press, release, mut chars, backspace, scroll, dt)| {
                // A couple of non-ASCII printable characters keep the edits honest.
                if chars.len() == 2 && scroll > 2.0 {
                    chars[0] = 'é';
                }
                Step {
                    target,
                    press,
                    release,
                    chars,
                    backspace,
                    scroll: scroll.round(),
                    dt,
                }
            },
        )
}

/// Model of `edit_apply` from its contract: backspace drops one character,
/// then printable characters append up to `max_len` characters.
fn model_edit(
    buf: &str,
    chars: &[char],
    backspace: bool,
    max_len: usize,
    ascii_only: bool,
) -> String {
    let mut out: Vec<char> = buf.chars().collect();
    if backspace {
        out.pop();
    }
    for &c in chars {
        if c.is_control() || (ascii_only && !c.is_ascii()) {
            continue;
        }
        if out.len() >= max_len {
            break;
        }
        out.push(c);
    }
    out.into_iter().collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    None,
    Edit,
    Password,
    /// The numeric field opened for typing by a click on its middle.
    Number,
}

fn number_clamp(value: f32) -> f32 {
    value.clamp(NUMBER_OPTS.min, NUMBER_OPTS.max)
}

/// Defect: a button firing on release or while held, two widgets reacting
/// to one press, a click passing through a modal or a popover, focus or
/// text edits ignoring the field's bounds, a dropdown selection changing
/// without a row press, a scroll offset escaping its range, a checkbox or
/// header flipping without a press, or a numeric field whose value moves
/// without a button press / scrub / typed edit, escapes its range, or
/// opens for typing after a drag. Oracle: the client.md 7 rules applied by
/// hand per frame from the generated input.
#[test]
fn widget_tree_obeys_the_interaction_contract_under_random_input() {
    check(prop::collection::vec(step(), 1..120), |steps| {
        let mut assets: Assets<Texture> = Assets::new();
        let mut ui = Ui::new(&mut assets);
        let mut text = TextRenderer::new(include_bytes!("fixtures/IBMPlexSans-Regular.ttf"))
            .expect("fixture font");

        // Caller-owned widget state.
        let (mut toggled, mut radio, mut selected) = (false, 0u8, 0usize);
        let (mut edit, mut secret) = (String::new(), String::new());
        let (mut scroll_offset, mut modal_open) = (0.0f32, false);
        let (mut checked, mut open, mut number) = (false, true, 0.0f32);

        // Driver + model state.
        let mut down = false;
        let mut focus = Focus::None;
        // The popover is drawn on a frame when it was open at that frame's
        // start, and what was drawn last frame blocks this frame.
        let mut popover_open = false;
        let mut popover_blocking = false;
        let mut modal_blocking = false;
        // The numeric field's scrub: (press x, value at the press, moved past
        // the threshold), and the text it shows while typing.
        let mut number_drag: Option<(f32, f32, bool)> = None;
        let mut number_text = String::new();

        for (frame_index, step) in steps.iter().enumerate() {
            let pressed = step.press && !down;
            let released = !pressed && step.release && down;
            if pressed {
                down = true;
            }
            if released {
                down = false;
            }
            let input = UiInput {
                mouse_pos: step.target,
                mouse_pressed: pressed,
                mouse_released: released,
                mouse_down: down,
                chars: step.chars.clone(),
                backspace: step.backspace,
                scroll: Vec2::new(0.0, step.scroll),
                dt: step.dt,
                ..UiInput::default()
            };
            let p = step.target;
            let blocked = modal_blocking || (popover_blocking && contains(&POPOVER, p));
            let hit = |r: &Rect| contains(r, p) && !blocked;
            let popover_drawn = popover_open;

            // --- Expectations for this frame, from the contract.
            let press_edit = pressed && hit(&EDIT);
            let press_password = pressed && hit(&PASSWORD);
            // Exactly one field receives this frame's characters: the one
            // the press landed on, or the focused one when there was no press.
            let edit_focused = press_edit || (focus == Focus::Edit && !pressed);
            let password_focused = press_password || (focus == Focus::Password && !pressed);
            let clear_pressed = pressed && hit(&PASSWORD_CLEAR) && !secret.is_empty();
            let expected_edit = if edit_focused {
                model_edit(&edit, &step.chars, step.backspace, EDIT_MAX, false)
            } else {
                edit.clone()
            };
            let secret_base = if clear_pressed {
                String::new()
            } else {
                secret.clone()
            };
            let expected_secret = if password_focused {
                model_edit(
                    &secret_base,
                    &step.chars,
                    step.backspace,
                    PASSWORD_MAX,
                    true,
                )
            } else {
                secret_base
            };
            // The numeric field: a press on its middle while it is open for
            // typing keeps it open (a line edit); otherwise it starts a scrub.
            let press_mid = pressed && hit(&NUMBER_MID);
            let number_typing = focus == Focus::Number && (!pressed || press_mid);
            let mut expected_focus_after = if press_password {
                Focus::Password
            } else if press_edit {
                Focus::Edit
            } else if press_mid && focus == Focus::Number {
                Focus::Number
            } else if pressed {
                Focus::None
            } else {
                focus
            };
            let expected_checked = if pressed && hit(&CHECKBOX) {
                !checked
            } else {
                checked
            };
            let expected_open = if pressed && hit(&HEADER) { !open } else { open };
            let mut expected_number = number;
            let mut expected_number_text = number_text.clone();
            if number_typing {
                expected_number_text = model_edit(
                    &number_text,
                    &step.chars,
                    step.backspace,
                    NUMBER_EDIT_MAX,
                    false,
                );
                if expected_number_text != number_text
                    && let Ok(typed) = expected_number_text.trim().parse::<f32>()
                    && typed.is_finite()
                {
                    expected_number = number_clamp(typed);
                }
            } else {
                if pressed && hit(&NUMBER_DEC) {
                    expected_number = number_clamp(number - NUMBER_OPTS.step);
                }
                if pressed && hit(&NUMBER_INC) {
                    expected_number = number_clamp(number + NUMBER_OPTS.step);
                }
            }
            if press_mid && focus != Focus::Number {
                number_drag = Some((p.x, number, false));
            }
            if let Some((start_x, start_value, mut moved)) = number_drag {
                if down {
                    if (p.x - start_x).abs() >= NUMBER_DRAG_THRESHOLD {
                        moved = true;
                    }
                    if moved {
                        expected_number =
                            number_clamp(start_value + (p.x - start_x) * NUMBER_OPTS.speed);
                    }
                    number_drag = Some((start_x, start_value, moved));
                } else {
                    number_drag = None;
                    if !moved {
                        expected_focus_after = Focus::Number;
                        expected_number_text = format!("{expected_number:.2}");
                    }
                }
            }
            let expected_modal = if modal_open && pressed {
                if contains(&CONFIRM, p) {
                    Some(true)
                } else if contains(&CANCEL, p) {
                    Some(false)
                } else {
                    None
                }
            } else {
                None
            };
            // Popover rows are overlay content: they take a press even under
            // a modal, and the popover itself is never blocked.
            let row_pressed = (pressed && popover_drawn && contains(&POPOVER, p))
                .then(|| ((p.y - POPOVER.min.y) / 32.0) as usize);
            let expected_selected = row_pressed.unwrap_or(selected);
            let expected_popover_after = if pressed {
                !popover_open && hit(&DROPDOWN)
            } else {
                popover_open
            };

            // --- Run the frame.
            let mut list = DrawList::new();
            let mut frame = ui.begin(&mut text, &mut list, input);
            let mut fires: Vec<(&str, Rect)> = Vec::new();
            if frame.button("b", BUTTON, "B", PX) {
                fires.push(("button", BUTTON));
            }
            if frame.toggle_button("t", TOGGLE, "T", PX, toggled) {
                toggled = !toggled;
                fires.push(("toggle", TOGGLE));
            }
            for (i, r) in RADIOS.iter().enumerate() {
                if frame.radio(&format!("r{i}"), *r, "R", PX, i as u8, &mut radio) {
                    fires.push(("radio", *r));
                }
            }
            frame.line_edit("e", EDIT, &mut edit, EDIT_MAX, PX);
            frame.password_edit_clear("p", PASSWORD, &mut secret, PASSWORD_MAX, PX);
            let changed = frame.dropdown("d", DROPDOWN, &OPTIONS, &mut selected, PX);
            if frame.button("open", OPEN_MODAL, "Open", PX) {
                fires.push(("open", OPEN_MODAL));
                modal_open = true;
            }
            frame.scroll_area_begin("s", SCROLL, CONTENT_H, &mut scroll_offset);
            let offset_now = scroll_offset;
            for i in 0..INNER_Y.len() {
                let r = inner_button(i, offset_now);
                if frame.button(&format!("inner{i}"), r, "I", 16.0) {
                    fires.push(("inner", r));
                }
            }
            frame.scroll_area_end();
            if frame.checkbox("cb", CHECKBOX, "Check", PX, &mut checked) {
                fires.push(("checkbox", CHECKBOX));
            }
            if frame.collapsing_header("hd", HEADER, "Section", PX, &mut open) {
                fires.push(("header", HEADER));
            }
            let number_before = number;
            let number_changed = frame.number_field("num", NUMBER, &mut number, NUMBER_OPTS, PX);
            frame.tooltip(BUTTON, SCREEN, "tip", 16.0);
            let modal_result = if modal_open {
                frame.confirm_modal("m", SCREEN, "Title", "Body", "OK", "Cancel", PX)
            } else {
                None
            };
            if modal_result.is_some() {
                modal_open = false;
            }
            frame.end();
            text.end_frame(&mut assets, &mut list);

            // --- Check.
            let ctx = |what: &str| format!("frame {frame_index}: {what} (step {step:?})");
            prop_assert!(
                fires.len() <= 1,
                "{}",
                ctx(&format!("two widgets fired: {fires:?}"))
            );
            if let Some((name, r)) = fires.first() {
                prop_assert!(pressed, "{}", ctx(&format!("{name} fired without a press")));
                prop_assert!(
                    contains(r, p),
                    "{}",
                    ctx(&format!("{name} fired with the pointer outside {r:?}"))
                );
                prop_assert!(
                    !blocked,
                    "{}",
                    ctx(&format!("{name} fired through a modal or popover"))
                );
                if *name == "inner" {
                    prop_assert!(
                        contains(&SCROLL, p),
                        "{}",
                        ctx("clipped button fired outside its scroll area")
                    );
                }
            }
            prop_assert_eq!(modal_result, expected_modal, "{}", ctx("modal result"));
            prop_assert_eq!(edit.clone(), expected_edit, "{}", ctx("line edit text"));
            prop_assert!(edit.chars().count() <= EDIT_MAX);
            prop_assert_eq!(secret.clone(), expected_secret, "{}", ctx("password text"));
            prop_assert!(secret.chars().count() <= PASSWORD_MAX && secret.is_ascii());
            prop_assert_eq!(
                ui.has_focus(),
                expected_focus_after != Focus::None,
                "{}",
                ctx("focus")
            );
            prop_assert_eq!(selected, expected_selected, "{}", ctx("dropdown selection"));
            prop_assert_eq!(
                changed,
                row_pressed.is_some(),
                "{}",
                ctx("dropdown change flag")
            );
            prop_assert_eq!(
                ui.any_popup_open(),
                expected_popover_after,
                "{}",
                ctx("popover state")
            );
            prop_assert!(
                (0.0..=CONTENT_H - SCROLL.size().y).contains(&scroll_offset),
                "{}",
                ctx(&format!("scroll offset {scroll_offset} out of range"))
            );
            prop_assert_eq!(checked, expected_checked, "{}", ctx("checkbox"));
            prop_assert_eq!(open, expected_open, "{}", ctx("collapsing header"));
            prop_assert!(
                (number - expected_number).abs() < 1e-4,
                "{}",
                ctx(&format!("number {number}, expected {expected_number}"))
            );
            prop_assert!(
                (NUMBER_OPTS.min..=NUMBER_OPTS.max).contains(&number),
                "{}",
                ctx(&format!("number {number} out of range"))
            );
            prop_assert_eq!(
                number_changed,
                number != number_before,
                "{}",
                ctx("number change flag")
            );
            // Quads drawn for the clipped buttons carry the scroll area's clip;
            // nothing in the list has a non-finite position.
            for i in 0..INNER_Y.len() {
                let r = inner_button(i, offset_now);
                let center = (r.min + r.max) * 0.5;
                let bodies: Vec<_> = list
                    .screen
                    .iter()
                    .filter(|q| q.pos == center && q.scale == r.size())
                    .collect();
                prop_assert!(!bodies.is_empty(), "{}", ctx("clipped button body missing"));
                for q in bodies {
                    prop_assert_eq!(
                        q.clip,
                        Some(SCROLL),
                        "{}",
                        ctx("clipped button quad without the clip")
                    );
                }
            }
            prop_assert!(
                list.screen
                    .iter()
                    .all(|q| q.pos.is_finite() && q.scale.is_finite()),
                "{}",
                ctx("non-finite quad")
            );

            // --- Advance the model.
            number_text = expected_number_text;
            focus = expected_focus_after;
            popover_blocking = popover_drawn;
            popover_open = expected_popover_after;
            modal_blocking = modal_open;
        }
        Ok(())
    });
}

/// Defect: a cursor that walks past the end, a max length counted in bytes,
/// or a change flag that lies. Oracle: the contract restated as a model over
/// characters (including multi-byte ones).
#[test]
fn edit_apply_matches_its_character_model() {
    let strategy = (
        prop::collection::vec(prop::char::any(), 0..10),
        prop::collection::vec(prop::char::any(), 0..6),
        any::<bool>(),
        0usize..12,
    );
    check(strategy, |(initial, chars, backspace, max_len)| {
        let initial: String = initial.into_iter().filter(|c| !c.is_control()).collect();
        let mut buf = initial.clone();
        let changed = edit_apply(&mut buf, &chars, backspace, max_len);
        let expected = model_edit(&initial, &chars, backspace, max_len, false);
        // The model never grows past max_len; the implementation may keep an
        // over-long initial buffer as is (it only refuses to append).
        prop_assert_eq!(buf.clone(), expected);
        prop_assert_eq!(changed, buf != initial, "change flag");
        prop_assert!(buf.chars().count() <= max_len.max(initial.chars().count()));
        Ok(())
    });
}

/// Defect: the FPS label refreshing on frame count instead of elapsed time,
/// or reporting frames over the wrong window. Oracle: frames divided by the
/// seconds they took, refreshed once a second of `dt` has accumulated.
#[test]
fn fps_counter_reports_frames_per_accumulated_second() {
    check(prop::collection::vec(0.0f32..0.2, 1..200), |dts| {
        let mut counter = FpsCounter::new();
        let (mut accum, mut frames) = (0.0f32, 0u32);
        prop_assert_eq!(counter.text(), "0.00");
        for dt in dts {
            accum += dt;
            frames += 1;
            let refreshed = counter.tick(dt);
            prop_assert_eq!(refreshed, accum >= 1.0);
            if refreshed {
                prop_assert_eq!(counter.text(), format!("{:.2}", frames as f32 / accum));
                accum = 0.0;
                frames = 0;
            }
        }
        Ok(())
    });
}
