//! The animation plugin's events for scripts: the notifies and states the
//! animation assets an `AnimationComponent` names declare.
//!
//! An `AnimationComponent` names its Animation Graph through an
//! `AssetRef<AnimationGraphAssetKind>` property, so the graph's kind answers
//! for it (`engine_backend::AssetEventSource`): the graph's own state events
//! and, through it, the sequences' and Montages' notifies. The scripting
//! editor draws an entry point from each [`CatalogEvent`]; how they are
//! grouped and described is decided here, not there.

use std::collections::HashSet;

use animation_component::component_animation_notify_events;
use engine_backend::{AssetEventSource, CatalogEvent, ComponentEventCatalog, EventView, SourceCx};
use pulsar_events::gamma::EventDescriptor;
use pulsar_reflection::AnimationGraphAssetKind;

/// The animation graph kind's source.
pub(crate) fn source(origin: &'static str) -> AssetEventSource {
    AssetEventSource::for_kind::<AnimationGraphAssetKind>(origin, declare)
        .with_node_title(node_title)
}

/// The title of a saved animation event node, by the event's name (the part
/// after the component: `montage::<id>.hit`).
fn node_title(event: &str) -> Option<String> {
    let title = if event.starts_with("animseq::") {
        "On Animation Notify"
    } else if event.starts_with("animgraph::") {
        "On Animation State"
    } else if event.starts_with("montage::") {
        if event.contains(".lifecycle_") {
            "On Montage Event"
        } else {
            "On Montage Notify"
        }
    } else {
        return None;
    };
    Some(title.to_owned())
}

fn declare(cx: &mut SourceCx<'_>, asset: &str) -> ComponentEventCatalog {
    // The notifies come from the graph's sequences and Montages, which the
    // catalog reads through its own project source: watch their files by
    // extension rather than naming each.
    for extension in ["animgraph", "animseq", "montage"] {
        cx.watch_extension(extension);
    }
    let assets = HashSet::from([asset.to_owned()]);
    let catalog = component_animation_notify_events(cx.root(), &assets);
    ComponentEventCatalog {
        families: Vec::new(),
        events: catalog
            .events
            .into_iter()
            .map(|(label, descriptor)| present(label, descriptor))
            .collect(),
        diagnostics: catalog
            .diagnostics
            .into_iter()
            .map(|diagnostic| (diagnostic.path, diagnostic.error))
            .collect(),
    }
}

/// How an animation event is filed and described. The event's name is
/// `<source asset id>.<item>` in the namespace of the asset kind that owns it
/// (`animseq`, `montage`, `animgraph`).
pub fn present(label: String, descriptor: EventDescriptor) -> CatalogEvent {
    let source_id = descriptor
        .name
        .split_once('.')
        .map(|(source, _)| source)
        .unwrap_or_default()
        .to_owned();
    let source_label: String = source_id.chars().take(8).collect();
    let kind = descriptor
        .namespace
        .as_deref()
        .unwrap_or("animseq")
        .to_owned();
    let (group, noun) = match kind.as_str() {
        "montage" => ("Montage", "Montage"),
        "animgraph" => ("Animation Graph", "Animation Graph"),
        _ => ("Animation", "animation sequence"),
    };
    let name = match descriptor.namespace.as_deref() {
        Some(namespace) => format!("{namespace}::{}", descriptor.name),
        None => descriptor.name.clone(),
    };
    CatalogEvent {
        view: EventView {
            group: format!("{group}/{source_label}"),
            description: format!(
                "Runs for this component reference when {noun} {source_id} emits {label}."
            ),
            documentation: format!(
                "Asset-owned event `{name}` of {noun} `{source_id}`. Connect the component \
                 reference to `component_ref` to subscribe to that SceneDB component instance. \
                 The event is delivered on the referenced actor's entity channel; it is not a \
                 global broadcast. Notify states raise it for each phase: check `notify_phase` \
                 (`begin`, `tick`, `end`) and `end_reason`."
            ),
            properties: vec![
                ("animation_sequence_id".to_owned(), source_id),
                ("animation_source_kind".to_owned(), kind),
            ],
        },
        label,
        descriptor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulsar_events::gamma::FieldType;

    fn descriptor(namespace: &str, item: &str) -> EventDescriptor {
        EventDescriptor::dynamic_in(
            namespace,
            format!("11111111-2222-4333-8444-555555555555.{item}"),
            [("entity".to_owned(), FieldType::U64)],
        )
        .expect("a valid namespace")
    }

    #[test]
    fn events_are_filed_under_the_asset_that_owns_them() {
        let cases = [
            ("montage", "Montage/11111111"),
            ("animgraph", "Animation Graph/11111111"),
            ("animseq", "Animation/11111111"),
        ];
        for (namespace, group) in cases {
            let event = present("Footstep".into(), descriptor(namespace, "n1"));
            assert_eq!(event.view.group, group, "{namespace}");
            assert!(event
                .view
                .properties
                .contains(&("animation_source_kind".to_owned(), namespace.to_owned())));
            assert!(event.view.properties.contains(&(
                "animation_sequence_id".to_owned(),
                "11111111-2222-4333-8444-555555555555".to_owned()
            )));
            assert!(event.view.description.contains("Footstep"));
        }
    }

    #[test]
    fn the_source_is_the_animation_graph_kind() {
        assert_eq!(source("test").kind, "anim_graph");
    }
}

#[cfg(test)]
mod title_tests {
    use super::node_title;

    #[test]
    fn saved_animation_nodes_are_titled_by_what_they_listen_to() {
        let title = |event| node_title(event).unwrap();
        assert_eq!(title("montage::m1.hit"), "On Montage Notify");
        assert_eq!(title("montage::m1.lifecycle_ended"), "On Montage Event");
        assert_eq!(title("animseq::s1.step"), "On Animation Notify");
        assert_eq!(title("animgraph::g1.enter"), "On Animation State");
        assert_eq!(node_title("Door.opened"), None, "not an animation event");
    }
}
