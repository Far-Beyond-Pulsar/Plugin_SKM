//! GPU-side data structures for the 3D bone viewport.

use crate::core::Mat4;
use crate::style::{mix, with_alpha, Palette};

/// Uploaded once per frame. Mirrors the `Uniforms` block in
/// `shaders/common.wgsl` field for field (288 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ViewportUniforms {
    /// Column-major camera view-projection matrix.
    pub view_proj: [f32; 16],
    /// Inverse of the *unjittered* view-projection matrix.
    pub inv_view_proj: [f32; 16],
    /// Camera position in `xyz`; `w` is the distance the ground grid fades out at.
    pub eye: [f32; 4],
    /// Render-target size in pixels.
    pub viewport: [f32; 2],
    pub time: f32,
    pub _pad: f32,
    /// Sub-pixel clip-space offset applied this frame for TAA sample
    /// jittering. Zero for passes that should not be jittered (e.g. gizmo).
    pub jitter: [f32; 2],
    pub _pad2: [f32; 2],
    pub sky_top: [f32; 4],
    pub sky_bottom: [f32; 4],
    pub grid_minor: [f32; 4],
    pub grid_major: [f32; 4],
    pub axis_x: [f32; 4],
    pub axis_z: [f32; 4],
    /// Contact shadow: `x`, `z` center on the ground, `y` radius, `w` strength.
    pub shadow: [f32; 4],
}

impl ViewportUniforms {
    /// Uniforms for a frame, with every color taken from `palette`.
    pub fn new(
        palette: &Palette,
        view_proj: Mat4,
        eye: [f32; 3],
        size: [f32; 2],
        grid_fade: f32,
        shadow: [f32; 4],
    ) -> Self {
        let black = [0.0, 0.0, 0.0, 1.0];
        let white = [1.0, 1.0, 1.0, 1.0];
        let (sky_top, sky_bottom, minor_alpha) = if palette.dark {
            (
                mix(palette.background, black, 0.32),
                mix(palette.background, palette.text, 0.055),
                0.13,
            )
        } else {
            (
                mix(palette.background, palette.text, 0.10),
                mix(palette.background, white, 0.35),
                0.16,
            )
        };
        Self {
            view_proj: view_proj.0,
            inv_view_proj: view_proj.inverse().0,
            eye: [eye[0], eye[1], eye[2], grid_fade],
            viewport: size,
            time: 0.0,
            _pad: 0.0,
            jitter: [0.0, 0.0],
            _pad2: [0.0, 0.0],
            sky_top,
            sky_bottom,
            grid_minor: with_alpha(palette.text, minor_alpha),
            grid_major: with_alpha(palette.text, 0.34),
            axis_x: with_alpha(palette.axis_x, 0.9),
            axis_z: with_alpha(palette.axis_z, 0.9),
            shadow,
        }
    }
}

/// Uniforms for the TAA/upscale resolve pass.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ResolveUniforms {
    /// Inverse of this frame's *unjittered* camera view-projection matrix,
    /// used to reconstruct world-space positions from depth.
    pub inv_view_proj: [f32; 16],
    /// Previous frame's *unjittered* camera view-projection matrix, used to
    /// reproject world-space positions into last frame's history buffer.
    pub prev_view_proj: [f32; 16],
    /// Size of the offscreen scene render target (post render-scale).
    pub render_size: [f32; 2],
    /// Size of the final output (swapchain) target.
    pub output_size: [f32; 2],
    /// Weight of the reprojected history sample (0 = current frame only).
    pub blend: f32,
    /// 0.0 on the first frame or after a resize, when the history buffer
    /// holds no useful data yet.
    pub history_valid: f32,
    pub _pad: [f32; 2],
}

/// One shaded triangle vertex of a bone mesh.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 4],
    /// Emissive boost used for selected / hovered bones.
    pub glow: f32,
}

/// One anti-aliased line segment of constant pixel width, with a color
/// gradient from `color_a` to `color_b`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RibbonSegment {
    pub a: [f32; 3],
    /// Width in pixels.
    pub width: f32,
    pub b: [f32; 3],
    /// NDC depth the line is pulled toward the camera by.
    pub bias: f32,
    pub color_a: [f32; 4],
    pub color_b: [f32; 4],
}

impl RibbonSegment {
    pub fn solid(a: [f32; 3], b: [f32; 3], width: f32, color: [f32; 4]) -> Self {
        Self {
            a,
            width,
            b,
            bias: 0.0,
            color_a: color,
            color_b: color,
        }
    }

    pub fn with_bias(mut self, bias: f32) -> Self {
        self.bias = bias;
        self
    }
}

/// Joint marker styles (see `fs_joint`).
pub const JOINT_PIVOT: f32 = 0.0;
pub const JOINT_SELECTED: f32 = 1.0;
pub const JOINT_HOVER: f32 = 2.0;
pub const JOINT_KEY: f32 = 3.0;

/// One instanced screen-facing marker.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct JointInstance {
    pub center: [f32; 3],
    /// Diameter in pixels.
    pub size: f32,
    pub color: [f32; 4],
    pub style: f32,
}

/// Gizmo bubble kinds (see `fs_bubble`).
pub const BUBBLE_FILLED: f32 = 0.0;
pub const BUBBLE_HOLLOW: f32 = 1.0;
pub const BUBBLE_BACKDROP: f32 = 2.0;

/// One instanced billboard circle of the orientation gizmo.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GizmoBubbleInstance {
    pub center: [f32; 3],
    /// Diameter in pixels.
    pub size: f32,
    pub color: [f32; 4],
    /// Axis label glyph: 0 = X, 1 = Y, 2 = Z, -1 = none.
    pub letter: f32,
    pub kind: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_block_matches_wgsl_layout() {
        // 2 matrices + 16 floats of header + 7 vec4 of colors/params.
        assert_eq!(std::mem::size_of::<ViewportUniforms>(), 288);
        assert_eq!(std::mem::size_of::<ResolveUniforms>(), 160);
        assert_eq!(std::mem::size_of::<MeshVertex>(), 44);
        assert_eq!(std::mem::size_of::<RibbonSegment>(), 64);
        assert_eq!(std::mem::size_of::<JointInstance>(), 36);
        assert_eq!(std::mem::size_of::<GizmoBubbleInstance>(), 40);
    }
}
