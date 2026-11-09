pub mod timeline;
pub mod viewport;

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
        let mut sources: Vec<(&str, String)> = vec![(
            "timeline",
            include_str!("timeline/shaders/timeline.wgsl").to_string(),
        )];
        sources.extend(super::viewport::shaders::all());
        for (name, source) in sources {
            let module = naga::front::wgsl::parse_str(&source)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&source)));
            Validator::new(ValidationFlags::all(), Capabilities::all())
                .validate(&module)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&source)));
        }
    }
}
