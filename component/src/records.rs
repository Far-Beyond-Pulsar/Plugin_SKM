//! The animator's legacy record migration, run on every level and class
//! prefab load with the engine's own (the plugin's root crate registers it as
//! a `pulsar_class::records::RecordMigrationRegistration`).
//!
//! Before the animator existed the skinned mesh was the player: a
//! `SkeletalMeshComponent` record held the animation graph, the old single
//! sequence, playback and root motion. [`split_animator`] moves those keys
//! into an `AnimationComponent` record placed right after the mesh's, at
//! the same place among the object's animators as the mesh among its
//! meshes (the animator at that place drives that mesh).

use serde_json::{Map, Value};

use crate::ANIMATION_COMPONENT;

/// The skinned mesh's class (the meshes'), as its saved records name it.
const SKELETAL_MESH: &str = "SkeletalMeshComponent";

/// The keys the skinned mesh held for its animation, now the animator's.
pub const ANIMATOR_KEYS: [&str; 9] = [
    "animation_graph",
    "animation",
    "looping",
    "playing",
    "playback_rate",
    "root_motion_mode",
    "root_motion_rotation",
    "root_motion_vertical",
    "root_motion_authority",
];

/// What [`split_animator`] did, for reports.
pub const SPLIT: &str = "moved a SkeletalMeshComponent's animation into an AnimationComponent";

/// Moves every skinned mesh record's animation keys into an animator record
/// after it. A mesh record without them is left alone (it shows its bind
/// pose, as a mesh with no animation did). Returns whether the list changed.
pub fn split_animator(entries: &mut Vec<Value>) -> bool {
    let mut changed = false;
    let mut index = 0;
    while index < entries.len() {
        let entry = &mut entries[index];
        index += 1;
        if entry.get("class_name").and_then(Value::as_str) != Some(SKELETAL_MESH) {
            continue;
        }
        let enabled = entry.get("enabled").cloned().unwrap_or(Value::Bool(true));
        let Some(data) = entry.get_mut("data").and_then(Value::as_object_mut) else {
            continue;
        };
        let mut animator = Map::new();
        for key in ANIMATOR_KEYS {
            if let Some(value) = data.remove(key) {
                animator.insert(key.to_string(), value);
            }
        }
        if animator.is_empty() {
            continue;
        }
        changed = true;
        entries.insert(
            index,
            serde_json::json!({
                "class_name": ANIMATION_COMPONENT,
                "enabled": enabled,
                "data": Value::Object(animator),
            }),
        );
        index += 1;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skinned_mesh_with_animation_gets_an_animator_after_it() {
        let mut entries = vec![
            serde_json::json!({"class_name": "SkeletalMeshComponent", "enabled": false, "data": {
                "mesh": "m", "animation_graph": "g", "playing": true, "playback_rate": 2.0,
                "root_motion_mode": "Everything"
            }}),
            serde_json::json!({"class_name": "SkeletalMeshComponent", "data": {"mesh": "n"}}),
        ];
        assert!(split_animator(&mut entries));
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["data"], serde_json::json!({"mesh": "m"}));
        assert_eq!(entries[1]["class_name"], ANIMATION_COMPONENT);
        assert_eq!(entries[1]["enabled"], false);
        assert_eq!(
            entries[1]["data"],
            serde_json::json!({
                "animation_graph": "g", "playing": true, "playback_rate": 2.0,
                "root_motion_mode": "Everything"
            })
        );
        // the mesh without animation stays as it was, and a second run changes nothing
        assert_eq!(entries[2]["data"], serde_json::json!({"mesh": "n"}));
        assert!(!split_animator(&mut entries));
    }
}
