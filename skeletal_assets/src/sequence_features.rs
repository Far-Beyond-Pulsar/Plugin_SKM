//! sequence-owned metadata; bone animation stays independent of these channels.

use super::{
    curves::AnimationCurve,
    notifies::AnimationNotify,
    notify_schema::{check_payload, AnimationNotifyState, NotifyPayloadSchema},
    root_motion::RootMotionSettings,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnimationFeatures {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notifies: Vec<AnimationNotify>,
    /// Ranged events (version 4). Kept apart from `notifies` so instant
    /// notifies stay byte-stable for older readers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_states: Vec<AnimationNotifyState>,
    /// Payload schemas the notifies and states above may reference (version 4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_schemas: Vec<NotifyPayloadSchema>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curves: Vec<AnimationCurve>,
    #[serde(default, skip_serializing_if = "RootMotionSettings::is_default")]
    pub root_motion: RootMotionSettings,
}

impl AnimationFeatures {
    pub fn is_empty(&self) -> bool {
        self.notifies.is_empty()
            && self.notify_states.is_empty()
            && self.notify_schemas.is_empty()
            && self.curves.is_empty()
            && self.root_motion.is_default()
    }

    /// Whether any field needs the version 4 reader.
    pub fn needs_v4(&self) -> bool {
        !self.notify_states.is_empty()
            || !self.notify_schemas.is_empty()
            || self.notifies.iter().any(|n| n.schema.is_some())
    }

    pub fn validate(&self, duration: f32) -> Result<(), String> {
        if !duration.is_finite() || duration < 0.0 {
            return Err("Clip duration must be finite and nonnegative".into());
        }
        let mut ids = std::collections::HashSet::new();
        for notify in &self.notifies {
            super::notifies::validate_notify(notify, duration)?;
            if !ids.insert(notify.id.as_str()) {
                return Err("Notify IDs must be unique".into());
            }
        }
        // Notifies and states share one id space: the Blueprint event
        // descriptor `animseq::<sequence>.<id>` must name exactly one of them.
        for state in &self.notify_states {
            state.validate(duration)?;
            if !ids.insert(state.id.as_str()) {
                return Err("Notify and notify state IDs must be unique".into());
            }
        }
        let mut schema_ids = std::collections::HashSet::new();
        for schema in &self.notify_schemas {
            schema.validate()?;
            if !schema_ids.insert(schema.id.as_str()) {
                return Err("Payload schema IDs must be unique".into());
            }
        }
        for notify in &self.notifies {
            check_payload(&self.notify_schemas, &notify.schema, &notify.fields)?;
        }
        for state in &self.notify_states {
            check_payload(&self.notify_schemas, &state.schema, &state.fields)?;
        }
        let mut ids = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for curve in &self.curves {
            curve.validate(duration)?;
            if !ids.insert(curve.id()) || !names.insert(curve.name().trim()) {
                return Err("Curve IDs and names must be unique".into());
            }
        }
        self.root_motion.validate()
    }
}
