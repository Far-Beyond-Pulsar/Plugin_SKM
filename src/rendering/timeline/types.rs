//! GPU-side data structures for the keyframe timeline.

/// Uploaded once per frame.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TimelineUniforms {
    /// Render-target size in pixels.
    pub viewport: [f32; 2],
    /// Horizontal scroll offset, in pixels.
    pub scroll_x: f32,
    /// Pixels per second along the time axis.
    pub px_per_sec: f32,
}

/// One instanced rectangle: track row backgrounds, keyframe diamonds, and
/// the playhead all share this layout.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RectInstance {
    /// Top-left, in screen pixels (before scroll is applied by the shader).
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub color: [f32; 4],
    /// 0 = plain rect, 1 = diamond (keyframe marker), 2 = fixed (ignores scroll, e.g. row backgrounds/playhead),
    /// 3 = greyed-out diagonal hatch (out-of-range track area)
    pub kind: u32,
    pub _pad: [u32; 3],
}
