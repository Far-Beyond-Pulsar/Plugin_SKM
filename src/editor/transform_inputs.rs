//! The nine `NumberInput` fields (translation/rotation/scale × x/y/z) used by
//! the properties panel to display and edit the selected bone or keyframe's
//! local transform.

use gpui::*;
use ui::input::InputState;

use crate::core::{Transform, Vec3};

pub struct TransformInputs {
    pub translation: [Entity<InputState>; 3],
    pub rotation: [Entity<InputState>; 3],
    pub scale: [Entity<InputState>; 3],
}

impl TransformInputs {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let mut axis = |default: f32, cx: &mut App| {
            cx.new(|cx| InputState::new(window, cx).default_value(format!("{:.3}", default)))
        };

        Self {
            translation: [axis(0.0, cx), axis(0.0, cx), axis(0.0, cx)],
            rotation: [axis(0.0, cx), axis(0.0, cx), axis(0.0, cx)],
            scale: [axis(1.0, cx), axis(1.0, cx), axis(1.0, cx)],
        }
    }

    /// All nine input entities, in display order.
    pub fn all(&self) -> [&Entity<InputState>; 9] {
        [
            &self.translation[0], &self.translation[1], &self.translation[2],
            &self.rotation[0], &self.rotation[1], &self.rotation[2],
            &self.scale[0], &self.scale[1], &self.scale[2],
        ]
    }

    /// Push the given transform's components into the nine inputs.
    pub fn set_from_transform(&self, transform: &Transform, window: &mut Window, cx: &mut App) {
        let fields = [
            (&self.translation[0], transform.translation.x),
            (&self.translation[1], transform.translation.y),
            (&self.translation[2], transform.translation.z),
            (&self.rotation[0], transform.rotation.x),
            (&self.rotation[1], transform.rotation.y),
            (&self.rotation[2], transform.rotation.z),
            (&self.scale[0], transform.scale.x),
            (&self.scale[1], transform.scale.y),
            (&self.scale[2], transform.scale.z),
        ];
        for (input, value) in fields {
            input.update(cx, |input, cx| {
                input.set_value(format!("{:.3}", value), window, cx);
            });
        }
    }

    /// Parse the nine inputs into a [`Transform`], falling back to `0.0` (or
    /// `1.0` for scale) on invalid input.
    pub fn to_transform(&self, cx: &App) -> Transform {
        let parse = |input: &Entity<InputState>, default: f32| {
            input.read(cx).value().parse::<f32>().unwrap_or(default)
        };

        Transform {
            translation: Vec3::new(
                parse(&self.translation[0], 0.0),
                parse(&self.translation[1], 0.0),
                parse(&self.translation[2], 0.0),
            ),
            rotation: Vec3::new(
                parse(&self.rotation[0], 0.0),
                parse(&self.rotation[1], 0.0),
                parse(&self.rotation[2], 0.0),
            ),
            scale: Vec3::new(
                parse(&self.scale[0], 1.0),
                parse(&self.scale[1], 1.0),
                parse(&self.scale[2], 1.0),
            ),
        }
    }
}
