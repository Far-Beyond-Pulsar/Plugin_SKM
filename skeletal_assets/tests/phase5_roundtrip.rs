//! Phase 5: every current and new animation format, read from the committed
//! fixtures, survives repeated read/write cycles: ids stay put, text settles to
//! a fixed point, fields this version does not know are carried through, and no
//! file serializes a reverse reference or names itself.

use std::path::PathBuf;

use serde_json::{json, Value};
use skeletal_assets::anim_graph::AnimationGraphAsset;
use skeletal_assets::assets::AnimSeqAsset;
use skeletal_assets::blend_space::BlendSpaceAsset;
use skeletal_assets::ik_retarget::IkRetargetAsset;
use skeletal_assets::ik_rig::IkRigAsset;
use skeletal_assets::montage::AnimationMontageAsset;
use skeletal_assets::pose_db::PoseDbAsset;
use skeletal_assets::retarget::RetargetProfileAsset;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// `(file, parse -> id and canonical text)` for every fixture, old and new.
/// `to_text` returns a `String` for some codecs and a `Result` for others.
trait AsText {
    fn as_text(self) -> Result<String, String>;
}

impl AsText for String {
    fn as_text(self) -> Result<String, String> {
        Ok(self)
    }
}

impl<E: std::fmt::Display> AsText for Result<String, E> {
    fn as_text(self) -> Result<String, String> {
        self.map_err(|e| e.to_string())
    }
}

type Reader = fn(&str) -> Result<(String, String), String>;

fn readers() -> Vec<(&'static str, Reader)> {
    macro_rules! reader {
        ($ty:ty, $id:expr) => {
            |text| {
                let asset = <$ty>::parse(text).map_err(|e| e.to_string())?;
                let id: String = $id(&asset);
                Ok((id, AsText::as_text(asset.to_text())?))
            }
        };
    }
    vec![
        ("v1/aim.blendspace", reader!(BlendSpaceAsset, |a: &BlendSpaceAsset| a.id.as_str().to_string())),
        ("v1/locomotion.blendspace", reader!(BlendSpaceAsset, |a: &BlendSpaceAsset| a.id.as_str().to_string())),
        ("v1/reload.montage", reader!(AnimationMontageAsset, |a: &AnimationMontageAsset| a.id.as_str().to_string())),
        ("v1/locomotion.animgraph", reader!(AnimationGraphAsset, |a: &AnimationGraphAsset| a.id.as_str().to_string())),
        ("v1/walk_v1.animseq", reader!(AnimSeqAsset, |a: &AnimSeqAsset| a.id.to_string())),
        ("v1/footstep_v3.animseq", reader!(AnimSeqAsset, |a: &AnimSeqAsset| a.id.to_string())),
        ("v2/locomotion.blendspace", reader!(BlendSpaceAsset, |a: &BlendSpaceAsset| a.id.as_str().to_string())),
        ("v2/reload.montage", reader!(AnimationMontageAsset, |a: &AnimationMontageAsset| a.id.as_str().to_string())),
        ("v2/locomotion.animgraph", reader!(AnimationGraphAsset, |a: &AnimationGraphAsset| a.id.as_str().to_string())),
        ("v2/humanoid_a.ikrig", reader!(IkRigAsset, |a: &IkRigAsset| a.id.as_str().to_string())),
        ("v2/humanoid_b.ikrig", reader!(IkRigAsset, |a: &IkRigAsset| a.id.as_str().to_string())),
        ("v2/a_to_b.animretarget", reader!(RetargetProfileAsset, |a: &RetargetProfileAsset| a.id.as_str().to_string())),
        ("v2/a_to_b.ikretarget", reader!(IkRetargetAsset, |a: &IkRetargetAsset| a.id.as_str().to_string())),
        ("v2/locomotion.posedb", reader!(PoseDbAsset, |a: &PoseDbAsset| a.id.as_str().to_string())),
    ]
}

fn text_ids(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) if uuid::Uuid::parse_str(s).is_ok() => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|v| text_ids(v, out)),
        Value::Object(o) => o.values().for_each(|v| text_ids(v, out)),
        _ => {}
    }
}

#[test]
fn every_fixture_reads_keeps_its_id_and_settles_to_a_fixed_point() {
    for (name, read) in readers() {
        let (id1, text1) = read(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (id2, text2) = read(&text1).unwrap_or_else(|e| panic!("{name} (second read): {e}"));
        let (id3, text3) = read(&text2).unwrap_or_else(|e| panic!("{name} (third read): {e}"));
        assert!(!id1.is_empty(), "{name} has an id");
        assert_eq!((&id1, &id2), (&id2, &id3), "{name}: the id is stable across read/write cycles");
        let (a, b): (Value, Value) = (serde_json::from_str(&text2).unwrap(), serde_json::from_str(&text3).unwrap());
        assert_eq!(a, b, "{name}: the second write equals the third");
        assert_eq!(text2, text3, "{name}: the text is byte-stable once migrated");
    }
}

#[test]
fn fields_a_newer_minor_writer_added_survive_a_read_and_write() {
    for (name, read) in readers() {
        let mut value: Value = serde_json::from_str(&read(&fixture(name)).unwrap().1).unwrap();
        value.as_object_mut().unwrap().insert("x_future_field".into(), json!({ "keep": [1, 2, 3] }));
        let (_, rewritten) = read(&value.to_string()).unwrap_or_else(|e| panic!("{name} with an unknown field: {e}"));
        let back: Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(back.get("x_future_field"), Some(&json!({ "keep": [1, 2, 3] })), "{name} dropped an unknown field");
    }
}

#[test]
fn no_file_serializes_a_reverse_reference_or_names_itself() {
    for (name, read) in readers() {
        let (id, text) = read(&fixture(name)).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        let mut keys = Vec::new();
        collect_keys(&value, &mut keys);
        for forbidden in ["used_by", "referenced_by", "referrers", "dependants", "component", "entity"] {
            assert!(!keys.iter().any(|k| k == forbidden), "{name} stores `{forbidden}`: reverse refs are derived, never saved");
        }
        let mut ids = Vec::new();
        if let Value::Object(o) = &value {
            for (key, v) in o {
                if key != "id" {
                    text_ids(v, &mut ids);
                }
            }
        }
        assert!(!ids.contains(&id), "{name} references its own id");
    }
}

fn collect_keys(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(o) => {
            for (k, v) in o {
                out.push(k.clone());
                collect_keys(v, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|v| collect_keys(v, out)),
        _ => {}
    }
}

#[test]
fn an_asset_without_an_id_is_given_one_by_the_migration_and_keeps_it() {
    let (first, text) = {
        let asset = BlendSpaceAsset::parse_migrating(&fixture("v1/no_id.blendspace")).expect("v1 without an id migrates").0;
        (asset.id.as_str().to_string(), asset.to_text().unwrap())
    };
    let again = BlendSpaceAsset::parse(&text).unwrap();
    assert_eq!(again.id.as_str(), first, "the id assigned once is the id forever after");
}
