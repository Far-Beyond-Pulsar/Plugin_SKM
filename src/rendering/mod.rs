pub mod timeline;
pub mod viewport;

#[cfg(test)]
pub(crate) mod preview;

pub use timeline::TimelinePanel;
pub use viewport::ViewportPanel;
#[cfg(test)]
mod shader_tests {
    use naga::valid::{Capabilities, ValidationFlags, Validator};

    /// wgpu validates shaders when the editor opens a window, where a mistake
    /// is a panic. Validate every shader with the same front end here.
    #[test]
    fn every_shader_validates() {
        for (name, source) in [
            ("timeline", include_str!("timeline/shaders/timeline.wgsl")),
            ("scene", include_str!("viewport/shaders/scene.wgsl")),
            ("gizmo", include_str!("viewport/shaders/gizmo.wgsl")),
            ("resolve", include_str!("viewport/shaders/resolve.wgsl")),
        ] {
            let module = naga::front::wgsl::parse_str(source)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(source)));
            Validator::new(ValidationFlags::all(), Capabilities::all())
                .validate(&module)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(source)));
        }
    }
}
