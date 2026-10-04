//! Semantic colors in sRGB with straight alpha; games own theme selection.

/// Colors for functional surfaces and interaction states. `Default` preserves
/// the original translucent game UI. Tool presets use opaque surfaces, so scene
/// brightness and grid lines cannot compete with control labels.
#[derive(Debug, Clone, Copy)]
pub struct UiTheme {
    pub panel: [f32; 4],
    pub control: [f32; 4],
    pub hovered: [f32; 4],
    pub pressed: [f32; 4],
    pub text: [f32; 4],
    pub text_hovered: [f32; 4],
    pub text_pressed: [f32; 4],
    pub accent: [f32; 4],
    pub caret: [f32; 4],
    pub selection: [f32; 4],
    pub disabled: [f32; 4],
    pub disabled_text: [f32; 4],
    pub scroll_track: [f32; 4],
    pub scroll_thumb: [f32; 4],
    pub scroll_thumb_hovered: [f32; 4],
    pub popup: [f32; 4],
    pub modal_dim: [f32; 4],
    /// Black text outline width in logical points; tool presets use clean text.
    pub text_outline: f32,
}

impl Default for UiTheme {
    fn default() -> Self {
        Self {
            panel: super::PANEL_COLOR,
            control: super::BUTTON_NORMAL,
            hovered: super::BUTTON_HOVER,
            pressed: super::BUTTON_PRESSED,
            text: super::FONT_NORMAL,
            text_hovered: super::FONT_HOVER,
            text_pressed: super::FONT_PRESSED,
            accent: super::ACCENT,
            caret: super::CARET,
            selection: [0.44, 0.73, 0.98, 0.3],
            disabled: super::PANEL_COLOR,
            disabled_text: [0.45, 0.45, 0.45, 1.0],
            scroll_track: super::SCROLLBAR_TRACK,
            scroll_thumb: super::SCROLLBAR_THUMB,
            scroll_thumb_hovered: super::SCROLLBAR_THUMB_HOVER,
            popup: super::POPOVER_BG,
            modal_dim: super::MODAL_DIM,
            text_outline: super::WIDGET_OUTLINE,
        }
    }
}

impl UiTheme {
    /// Opaque dark tool chrome with distinct hover, press, focus and disabled
    /// roles. Selection marks and focus borders supplement color.
    #[must_use]
    pub fn tools_dark() -> Self {
        Self {
            panel: [0.10, 0.12, 0.15, 1.0],
            control: [0.16, 0.19, 0.23, 1.0],
            hovered: [0.23, 0.28, 0.34, 1.0],
            pressed: [0.07, 0.10, 0.14, 1.0],
            text: [0.91, 0.93, 0.96, 1.0],
            text_hovered: [1.0, 1.0, 1.0, 1.0],
            text_pressed: [1.0, 1.0, 1.0, 1.0],
            accent: [0.45, 0.76, 1.0, 1.0],
            caret: [1.0, 1.0, 1.0, 1.0],
            selection: [0.25, 0.55, 0.85, 0.4],
            disabled: [0.13, 0.15, 0.18, 1.0],
            disabled_text: [0.59, 0.63, 0.69, 1.0],
            scroll_track: [0.07, 0.09, 0.12, 1.0],
            scroll_thumb: [0.40, 0.45, 0.52, 1.0],
            scroll_thumb_hovered: [0.57, 0.64, 0.72, 1.0],
            popup: [0.13, 0.16, 0.20, 1.0],
            modal_dim: [0.0, 0.0, 0.0, 0.55],
            text_outline: 0.0,
        }
    }

    /// Opaque light tool chrome, suitable over either bright or dark artwork.
    #[must_use]
    pub fn tools_light() -> Self {
        Self {
            panel: [0.92, 0.93, 0.95, 1.0],
            control: [0.83, 0.86, 0.90, 1.0],
            hovered: [0.74, 0.80, 0.87, 1.0],
            pressed: [0.64, 0.72, 0.81, 1.0],
            text: [0.10, 0.13, 0.18, 1.0],
            text_hovered: [0.04, 0.07, 0.11, 1.0],
            text_pressed: [0.02, 0.04, 0.08, 1.0],
            accent: [0.08, 0.34, 0.62, 1.0],
            caret: [0.04, 0.07, 0.11, 1.0],
            selection: [0.10, 0.40, 0.75, 0.3],
            disabled: [0.87, 0.89, 0.92, 1.0],
            disabled_text: [0.40, 0.44, 0.50, 1.0],
            scroll_track: [0.81, 0.84, 0.88, 1.0],
            scroll_thumb: [0.45, 0.50, 0.58, 1.0],
            scroll_thumb_hovered: [0.30, 0.36, 0.44, 1.0],
            popup: [0.96, 0.97, 0.98, 1.0],
            modal_dim: [0.0, 0.0, 0.0, 0.40],
            text_outline: 0.0,
        }
    }
}
