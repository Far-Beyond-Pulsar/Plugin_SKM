//! The nine `NumberInput` fields (translation/rotation/scale × x/y/z) used by
//! the properties panel to display and edit the selected bone or keyframe's
//! local transform.

use gpui::*;
use std::cell::RefCell;
use ui::input::InputState;

use crate::core::{Transform, Vec3};

/// One of the three vector groups edited in the properties panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransformGroup {
    Translation,
    Rotation,
    Scale,
}

impl TransformGroup {
    /// Value every axis returns to on reset.
    pub fn default_value(self) -> f32 {
        match self {
            TransformGroup::Scale => 1.0,
            _ => 0.0,
        }
    }
}

pub struct TransformInputs {
    displayed_values: RefCell<[SharedString; 9]>,
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
            displayed_values: RefCell::new(std::array::from_fn(|i| {
                if i < 6 {
                    "0.000".into()
                } else {
                    "1.000".into()
                }
            })),
            translation: [axis(0.0, cx), axis(0.0, cx), axis(0.0, cx)],
            rotation: [axis(0.0, cx), axis(0.0, cx), axis(0.0, cx)],
            scale: [axis(1.0, cx), axis(1.0, cx), axis(1.0, cx)],
        }
    }

    /// The three axis inputs of `group`.
    pub fn group(&self, group: TransformGroup) -> &[Entity<InputState>; 3] {
        match group {
            TransformGroup::Translation => &self.translation,
            TransformGroup::Rotation => &self.rotation,
            TransformGroup::Scale => &self.scale,
        }
    }

    /// All nine input entities, in display order.
    pub fn all(&self) -> [&Entity<InputState>; 9] {
        [
            &self.translation[0],
            &self.translation[1],
            &self.translation[2],
            &self.rotation[0],
            &self.rotation[1],
            &self.rotation[2],
            &self.scale[0],
            &self.scale[1],
            &self.scale[2],
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
        let values = fields.map(|(_, value)| SharedString::from(format!("{value:.3}")));
        // set_value emits Change too; selection and undo must not round the asset's transforms.
        *self.displayed_values.borrow_mut() = values.clone();
        for ((input, _), value) in fields.into_iter().zip(values) {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
    }

    pub fn take_changed(&self, cx: &App) -> Option<[bool; 9]> {
        let values = self.all().map(|input| input.read(cx).value());
        let mut displayed = self.displayed_values.borrow_mut();
        let changed = std::array::from_fn(|i| displayed[i] != values[i]);
        *displayed = values;
        changed.iter().any(|changed| *changed).then_some(changed)
    }

    pub fn apply_changes(
        &self,
        mut transform: Transform,
        changed: [bool; 9],
        cx: &App,
    ) -> Option<Transform> {
        let inputs = self.all();
        for (input, changed) in inputs.iter().zip(changed) {
            if changed && !input.read(cx).value().parse::<f32>().ok()?.is_finite() {
                return None;
            }
        }
        let parsed = self.to_transform(cx);
        let values = [
            parsed.translation.x,
            parsed.translation.y,
            parsed.translation.z,
            parsed.rotation.x,
            parsed.rotation.y,
            parsed.rotation.z,
            parsed.scale.x,
            parsed.scale.y,
            parsed.scale.z,
        ];
        let fields = [
            &mut transform.translation.x,
            &mut transform.translation.y,
            &mut transform.translation.z,
            &mut transform.rotation.x,
            &mut transform.rotation.y,
            &mut transform.rotation.z,
            &mut transform.scale.x,
            &mut transform.scale.y,
            &mut transform.scale.z,
        ];
        for ((field, value), changed) in fields.into_iter().zip(values).zip(changed) {
            if changed {
                *field = value;
            }
        }
        Some(transform)
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
