//! independent named scalar and vector channels with validated, sorted keys.

use super::math::Vec3;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CurveInterpolation {
    Step,
    #[default]
    Linear,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurveKey<T> {
    pub id: String,
    pub time: f32,
    pub value: T,
    #[serde(default)]
    pub interpolation: CurveInterpolation,
}

impl<T> CurveKey<T> {
    pub fn new(time: f32, value: T) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            time,
            value,
            interpolation: CurveInterpolation::Linear,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "keys")]
pub enum CurveData {
    Float(Vec<CurveKey<f32>>),
    Vector(Vec<CurveKey<Vec3>>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurveValue {
    Float(f32),
    Vector(Vec3),
}

/// keys are sorted once at load/edit boundaries, never during sampling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "CurveRecord", into = "CurveRecord")]
pub struct AnimationCurve {
    id: String,
    name: String,
    data: CurveData,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CurveRecord {
    id: String,
    name: String,
    data: CurveData,
}

impl TryFrom<CurveRecord> for AnimationCurve {
    type Error = String;
    fn try_from(record: CurveRecord) -> Result<Self, String> {
        Self::from_data(record.id, record.name, record.data)
    }
}
impl From<AnimationCurve> for CurveRecord {
    fn from(curve: AnimationCurve) -> Self {
        Self {
            id: curve.id,
            name: curve.name,
            data: curve.data,
        }
    }
}

impl AnimationCurve {
    pub fn new(name: String, vector: bool) -> Result<Self, String> {
        Self::from_data(
            uuid::Uuid::new_v4().to_string(),
            name,
            if vector {
                CurveData::Vector(Vec::new())
            } else {
                CurveData::Float(Vec::new())
            },
        )
    }
    pub fn from_data(id: String, name: String, mut data: CurveData) -> Result<Self, String> {
        match &mut data {
            CurveData::Float(keys) => keys.sort_by(|a, b| a.time.total_cmp(&b.time)),
            CurveData::Vector(keys) => keys.sort_by(|a, b| a.time.total_cmp(&b.time)),
        }
        let curve = Self { id, name, data };
        curve.validate(f32::MAX)?;
        Ok(curve)
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn data(&self) -> &CurveData {
        &self.data
    }
    pub fn is_vector(&self) -> bool {
        matches!(self.data, CurveData::Vector(_))
    }
    pub fn len(&self) -> usize {
        match &self.data {
            CurveData::Float(k) => k.len(),
            CurveData::Vector(k) => k.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn rename(&mut self, name: String) -> Result<(), String> {
        if name.trim().is_empty() {
            return Err("Curve name cannot be empty".into());
        }
        self.name = name;
        Ok(())
    }
    pub fn key(&self, index: usize) -> Option<(&str, f32, CurveValue, CurveInterpolation)> {
        match &self.data {
            CurveData::Float(keys) => keys.get(index).map(|k| {
                (
                    k.id.as_str(),
                    k.time,
                    CurveValue::Float(k.value),
                    k.interpolation,
                )
            }),
            CurveData::Vector(keys) => keys.get(index).map(|k| {
                (
                    k.id.as_str(),
                    k.time,
                    CurveValue::Vector(k.value),
                    k.interpolation,
                )
            }),
        }
    }
    pub fn key_index(&self, id: &str) -> Option<usize> {
        (0..self.len()).find(|&i| self.key(i).is_some_and(|k| k.0 == id))
    }
    /// empty curves have no value; endpoints hold, and invalid sample times return None.
    pub fn sample(&self, time: f32) -> Option<CurveValue> {
        if !time.is_finite() {
            return None;
        }
        match &self.data {
            CurveData::Float(keys) => {
                let (a, b, t) = sample_span(keys, time)?;
                Some(CurveValue::Float(
                    if a.interpolation == CurveInterpolation::Step {
                        a.value
                    } else {
                        interpolate_float(a.value, b.value, t)
                    },
                ))
            }
            CurveData::Vector(keys) => {
                let (a, b, t) = sample_span(keys, time)?;
                Some(CurveValue::Vector(
                    if a.interpolation == CurveInterpolation::Step {
                        a.value
                    } else {
                        Vec3::new(
                            interpolate_float(a.value.x, b.value.x, t),
                            interpolate_float(a.value.y, b.value.y, t),
                            interpolate_float(a.value.z, b.value.z, t),
                        )
                    },
                ))
            }
        }
    }
    /// replaces only the key with this ID, or inserts a new one. collisions are errors.
    pub fn set_key(
        &mut self,
        id: String,
        time: f32,
        value: CurveValue,
        interpolation: CurveInterpolation,
        duration: f32,
    ) -> Result<(), String> {
        if !duration.is_finite()
            || duration < 0.0
            || id.trim().is_empty()
            || !time.is_finite()
            || time < 0.0
            || time > duration
        {
            return Err("Key time must be within the clip and ID cannot be empty".into());
        }
        if (0..self.len()).any(|i| self.key(i).is_some_and(|k| k.0 != id && k.1 == time)) {
            return Err("A curve key already exists at this time".into());
        }
        match (&mut self.data, value) {
            (CurveData::Float(keys), CurveValue::Float(value)) if value.is_finite() => {
                keys.retain(|k| k.id != id);
                let index = keys.partition_point(|k| k.time < time);
                keys.insert(
                    index,
                    CurveKey {
                        id,
                        time,
                        value,
                        interpolation,
                    },
                );
            }
            (CurveData::Vector(keys), CurveValue::Vector(value))
                if value.to_array().iter().all(|v| v.is_finite()) =>
            {
                keys.retain(|k| k.id != id);
                let index = keys.partition_point(|k| k.time < time);
                keys.insert(
                    index,
                    CurveKey {
                        id,
                        time,
                        value,
                        interpolation,
                    },
                );
            }
            _ => return Err("Key value must be finite and match the curve type".into()),
        }
        Ok(())
    }
    pub fn remove_key(&mut self, id: &str) -> bool {
        let before = self.len();
        match &mut self.data {
            CurveData::Float(k) => k.retain(|k| k.id != id),
            CurveData::Vector(k) => k.retain(|k| k.id != id),
        }
        self.len() != before
    }
    pub fn validate(&self, duration: f32) -> Result<(), String> {
        if self.id.trim().is_empty() || self.name.trim().is_empty() {
            return Err("Curve ID and name cannot be empty".into());
        }
        let mut ids = std::collections::HashSet::new();
        let mut previous = None;
        for i in 0..self.len() {
            let (id, time, value, _) = self.key(i).unwrap();
            let finite = match value {
                CurveValue::Float(v) => v.is_finite(),
                CurveValue::Vector(v) => v.to_array().iter().all(|v| v.is_finite()),
            };
            if id.trim().is_empty() || !ids.insert(id) {
                return Err("Curve key IDs must be unique and nonempty".into());
            }
            if !finite || !time.is_finite() || time < 0.0 || time > duration {
                return Err("Curve keys need finite values and times within the clip".into());
            }
            if previous.is_some_and(|p| time <= p) {
                return Err("Curve key times must be distinct and sorted".into());
            }
            previous = Some(time);
        }
        Ok(())
    }
}

fn sample_span<T>(keys: &[CurveKey<T>], time: f32) -> Option<(&CurveKey<T>, &CurveKey<T>, f32)> {
    let first = keys.first()?;
    let last = keys.last()?;
    if time <= first.time {
        return Some((first, first, 0.0));
    }
    if time >= last.time {
        return Some((last, last, 0.0));
    }
    let upper = keys.partition_point(|k| k.time <= time);
    let (a, b) = (&keys[upper - 1], &keys[upper]);
    Some((a, b, (time - a.time) / (b.time - a.time)))
}

fn interpolate_float(a: f32, b: f32, t: f32) -> f32 {
    // use a wider intermediate so opposite finite extremes do not overflow.
    ((1.0 - t as f64) * a as f64 + t as f64 * b as f64) as f32
}
