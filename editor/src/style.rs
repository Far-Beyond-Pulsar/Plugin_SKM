//! Shared look of the editor.
//!
//! Both GPU canvases (the 3D viewport and the keyframe timeline) paint with
//! colors taken from the active engine theme, so they stay in step with the
//! rest of the UI whatever theme is loaded. The surfaces are `*Srgb` formats,
//! so shader output is linear: theme colors (authored in sRGB) are converted
//! once here instead of in every shader.

use gpui::Hsla;
use ui::Theme;

/// Per-axis colors shared by the viewport gizmo, the ground grid axes and the
/// X/Y/Z labels in the properties panel (sRGB).
pub const AXIS_X: Hsla = Hsla {
    h: 0.0,
    s: 0.72,
    l: 0.58,
    a: 1.0,
};
pub const AXIS_Y: Hsla = Hsla {
    h: 0.36,
    s: 0.62,
    l: 0.52,
    a: 1.0,
};
pub const AXIS_Z: Hsla = Hsla {
    h: 0.60,
    s: 0.76,
    l: 0.62,
    a: 1.0,
};

pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// A theme color as linear RGBA, ready to hand to a shader.
pub fn linear(color: Hsla) -> [f32; 4] {
    let rgb = color.to_rgb();
    [
        srgb_to_linear(rgb.r),
        srgb_to_linear(rgb.g),
        srgb_to_linear(rgb.b),
        rgb.a,
    ]
}

pub fn with_alpha(mut color: [f32; 4], alpha: f32) -> [f32; 4] {
    color[3] = alpha;
    color
}

pub fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

/// Theme colors the GPU canvases need, resolved once per frame.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub background: [f32; 4],
    /// Panel/sidebar surface, the color the dock panels around the canvas use.
    pub panel: [f32; 4],
    pub surface: [f32; 4],
    pub border: [f32; 4],
    pub text: [f32; 4],
    pub text_muted: [f32; 4],
    pub primary: [f32; 4],
    pub info: [f32; 4],
    pub danger: [f32; 4],
    pub warning: [f32; 4],
    pub success: [f32; 4],
    pub axis_x: [f32; 4],
    pub axis_y: [f32; 4],
    pub axis_z: [f32; 4],
}

impl Palette {
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            dark: theme.is_dark(),
            background: linear(theme.background),
            panel: linear(theme.sidebar),
            surface: linear(theme.secondary),
            border: linear(theme.border),
            text: linear(theme.foreground),
            text_muted: linear(theme.muted_foreground),
            primary: linear(theme.primary),
            info: linear(theme.info),
            danger: linear(theme.danger),
            warning: linear(theme.warning),
            success: linear(theme.success),
            axis_x: linear(AXIS_X),
            axis_y: linear(AXIS_Y),
            axis_z: linear(AXIS_Z),
        }
    }

    /// A fixed light palette, used by headless renders and tests.
    #[cfg(test)]
    pub fn test_light() -> Self {
        let c =
            |r: f32, g: f32, b: f32| [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), 1.0];
        Self {
            dark: false,
            background: c(0.94, 0.945, 0.955),
            panel: c(0.91, 0.915, 0.93),
            surface: c(0.88, 0.885, 0.90),
            border: c(0.78, 0.79, 0.82),
            text: c(0.13, 0.14, 0.17),
            text_muted: c(0.42, 0.44, 0.50),
            primary: c(0.15, 0.42, 0.90),
            info: c(0.15, 0.50, 0.90),
            danger: c(0.85, 0.22, 0.25),
            warning: c(0.90, 0.50, 0.08),
            success: c(0.15, 0.62, 0.38),
            axis_x: linear(AXIS_X),
            axis_y: linear(AXIS_Y),
            axis_z: linear(AXIS_Z),
        }
    }

    /// A fixed dark palette, used by headless renders and tests.
    #[cfg(test)]
    pub fn test_dark() -> Self {
        let c =
            |r: f32, g: f32, b: f32| [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), 1.0];
        Self {
            dark: true,
            background: c(0.105, 0.110, 0.125),
            panel: c(0.135, 0.140, 0.155),
            surface: c(0.17, 0.175, 0.195),
            border: c(0.25, 0.26, 0.285),
            text: c(0.90, 0.91, 0.93),
            text_muted: c(0.58, 0.60, 0.65),
            primary: c(0.25, 0.52, 0.96),
            info: c(0.35, 0.65, 0.98),
            danger: c(0.93, 0.30, 0.33),
            warning: c(0.98, 0.70, 0.22),
            success: c(0.30, 0.78, 0.50),
            axis_x: linear(AXIS_X),
            axis_y: linear(AXIS_Y),
            axis_z: linear(AXIS_Z),
        }
    }
}
