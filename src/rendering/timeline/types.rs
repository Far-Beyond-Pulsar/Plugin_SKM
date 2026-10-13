//! GPU-side data structures for the keyframe timeline.

/// Uploaded once per frame.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TimelineUniforms {
    /// Render-target size in pixels.
    pub viewport: [f32; 2],
    /// Horizontal scroll offset, in pixels.
    pub scroll_x: f32,
    pub _pad: f32,
}

/// Shape selector, stored in the low byte of [`RectInstance::kind`].
pub const SHAPE_RECT: u32 = 0;
/// Rhombus (keyframe marker).
pub const SHAPE_DIAMOND: u32 = 1;
/// Diagonal hatch over the instance's rectangle (out-of-range area).
pub const SHAPE_HATCH: u32 = 2;
/// Downward-pointing triangle (playhead cap).
pub const SHAPE_TRIANGLE_DOWN: u32 = 3;
/// Vertical gradient from `color` (top) to `border_color` (bottom).
pub const SHAPE_GRADIENT: u32 = 4;

/// OR-ed into `kind`: the instance ignores horizontal scroll (positions are
/// already in canvas pixels).
pub const FLAG_FIXED: u32 = 0x100;

/// One instanced shape. Everything the timeline draws (row backgrounds, grid
/// lines, spans, keyframe markers, the playhead, the marquee) is one of these.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RectInstance {
    /// Top-left, in canvas pixels (before scroll is applied, unless
    /// [`FLAG_FIXED`] is set).
    pub pos: [f32; 2],
    pub size: [f32; 2],
    /// Fill color, premultiplication is done in the shader.
    pub color: [f32; 4],
    /// Outline (and glow) color.
    pub border_color: [f32; 4],
    /// Shape in the low byte, flags above it.
    pub kind: u32,
    /// Corner radius in pixels.
    pub radius: f32,
    /// Outline width in pixels, drawn inside the shape.
    pub border: f32,
    /// Soft outer glow radius in pixels (0 = none).
    pub glow: f32,
}

impl RectInstance {
    pub fn new(pos: [f32; 2], size: [f32; 2], color: [f32; 4]) -> Self {
        Self {
            pos,
            size,
            color,
            border_color: [0.0; 4],
            kind: SHAPE_RECT,
            radius: 0.0,
            border: 0.0,
            glow: 0.0,
        }
    }

    pub fn shape(mut self, shape: u32) -> Self {
        self.kind = (self.kind & !0xFF) | shape;
        self
    }

    pub fn fixed(mut self) -> Self {
        self.kind |= FLAG_FIXED;
        self
    }

    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }

    pub fn outline(mut self, color: [f32; 4], width: f32) -> Self {
        self.border_color = color;
        self.border = width;
        self
    }
}
