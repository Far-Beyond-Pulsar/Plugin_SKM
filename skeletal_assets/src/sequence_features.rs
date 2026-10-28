//! sequence-owned metadata; bone animation stays independent of these channels.

use super::{curves::AnimationCurve, notifies::AnimationNotify, root_motion::RootMotionSettings};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnimationFeatures {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notifies: Vec<AnimationNotify>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curves: Vec<AnimationCurve>,
    #[serde(default, skip_serializing_if = "RootMotionSettings::is_default")]
    pub root_motion: RootMotionSettings,
}

impl AnimationFeatures {
    pub fn is_empty(&self) -> bool {
        self.notifies.is_empty() && self.curves.is_empty() && self.root_motion.is_default()
    }
    pub fn validate(&self, duration: f32) -> Result<(), String> {
        if !duration.is_finite() || duration < 0.0 {
            return Err("Clip duration must be finite and nonnegative".into());
        }
        let mut ids = std::collections::HashSet::new();
        for notify in &self.notifies {
            super::notifies::validate_notify(notify, duration)?;
            if !ids.insert(&notify.id) {
                return Err("Notify IDs must be unique".into());
            }
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
