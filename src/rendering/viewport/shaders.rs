//! WGSL sources for the viewport. Every pass except the TAA resolve shares the
//! uniform block in `common.wgsl`, which is prepended here so the layout lives
//! in exactly one place.

const COMMON: &str = include_str!("shaders/common.wgsl");

fn with_common(body: &str) -> String {
    format!("{COMMON}\n{body}")
}

pub fn backdrop() -> String {
    with_common(include_str!("shaders/backdrop.wgsl"))
}

pub fn scene() -> String {
    with_common(include_str!("shaders/scene.wgsl"))
}

pub fn ribbon() -> String {
    with_common(include_str!("shaders/ribbon.wgsl"))
}

pub fn gizmo() -> String {
    with_common(include_str!("shaders/gizmo.wgsl"))
}

pub fn resolve() -> String {
    include_str!("shaders/resolve.wgsl").to_string()
}

/// Every shader with a name, for validation.
#[cfg(test)]
pub fn all() -> Vec<(&'static str, String)> {
    vec![
        ("backdrop", backdrop()),
        ("scene", scene()),
        ("ribbon", ribbon()),
        ("gizmo", gizmo()),
        ("resolve", resolve()),
    ]
}
