//! GPU-side data structures for the 3D bone viewport.

/// Uploaded once per frame.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ViewportUniforms {
    /// Column-major camera view-projection matrix.
    pub view_proj: [f32; 16],
    /// Render-target size in pixels.
    pub viewport: [f32; 2],
    pub time: f32,
    pub _pad: f32,
}

/// One vertex of a `LineList` segment (grid lines and bone segments).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

/// One instanced billboard quad marking a joint.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct JointInstance {
    pub center: [f32; 3],
    /// Diameter in pixels.
    pub size: f32,
    pub color: [f32; 4],
}

/// One vertex of a shaded `TriangleList` mesh (used for the octahedral bone shapes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 4],
}

/// One instanced billboard circle for the orientation gizmo: an axis-end
/// bubble, or (with `letter < -1.5`) the flat backdrop disc behind the whole
/// widget.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GizmoBubbleInstance {
    pub center: [f32; 3],
    /// Diameter in pixels.
    pub size: f32,
    pub color: [f32; 4],
    /// Axis label glyph: 0 = X, 1 = Y, 2 = Z, -1 = no label, -2 = backdrop disc.
    pub letter: f32,
}
