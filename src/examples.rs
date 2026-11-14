//! The plugin's `examples/` folder: one small, consistent set of example
//! animation assets of every file type it registers (`.skeleton`,
//! `.skmesh`, `.animseq`, `.blendspace`, `.montage`, `.animgraph`, `.ikrig`,
//! `.animretarget`, `.ikretarget`, `.posedb`), served read-only in the
//! plugin's virtual folder and embedded from `examples_assets/`.
//!
//! The files refer to each other by stable id (a humanoid skeleton, a block
//! mesh skinned to it, clips for it, and the assets built on the clips), so a
//! copy of the whole folder is a working character; see `examples/README.md`.

use std::sync::Arc;

use engine_fs::{FolderSource, InGame, MemoryFolder, PluginFolder};

/// The example files, embedded; each path is where it sits in the plugin's
/// folder.
pub const FILES: &[engine_fs::EmbeddedFile] = engine_fs::embedded_files!("examples_assets":
    "examples/README.md",
    "examples/skeleton/humanoid.skeleton",
    "examples/skeleton/stylized.skeleton",
    "examples/meshes/humanoid_blockout.skmesh",
    "examples/meshes/stylized_blockout.skmesh",
    "examples/animations/humanoid_idle.animseq",
    "examples/animations/humanoid_walk.animseq",
    "examples/animations/humanoid_run.animseq",
    "examples/animations/humanoid_jump.animseq",
    "examples/animations/humanoid_wave.animseq",
    "examples/animations/humanoid_slash.animseq",
    "examples/blend_spaces/humanoid_locomotion.blendspace",
    "examples/blend_spaces/humanoid_strafe.blendspace",
    "examples/montages/sword_slash.montage",
    "examples/montages/wave_twice.montage",
    "examples/animation_graphs/humanoid_locomotion.animgraph",
    "examples/animation_graphs/humanoid_upper_body_layer.animgraph",
    "examples/ik_rigs/humanoid.ikrig",
    "examples/ik_rigs/stylized.ikrig",
    "examples/retargeting/humanoid_to_stylized.animretarget",
    "examples/retargeting/humanoid_to_stylized.ikretarget",
    "examples/pose_search/humanoid_locomotion.posedb",
);

fn open(folder: PluginFolder) -> Arc<dyn FolderSource> {
    Arc::new(MemoryFolder::with_files(folder, FILES))
}

/// The plugin's one file provider.
pub(crate) fn provider(origin: &'static str) -> engine_fs::FileProvider {
    engine_fs::FileProvider {
        origin,
        open,
        // A game does not ask the plugin: it serves the packager's snapshot.
        in_game: InGame::Snapshot,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use skeletal_assets::dependencies::{extract, kind_of_extension, KINDS};
    use skeletal_assets::diagnostics::Diagnostic;
    use skeletal_assets::report::asset_report;

    use super::*;

    fn extension(file: &engine_fs::EmbeddedFile) -> Option<&str> {
        file.path.rsplit('.').next().filter(|e| !e.contains('/'))
    }

    /// Every asset (not the README), as the project's file index would
    /// serve it: by kind and stable id.
    fn project() -> HashMap<(String, String), Vec<u8>> {
        let mut files = HashMap::new();
        for file in FILES {
            let Some(kind) = extension(file).and_then(kind_of_extension) else {
                continue;
            };
            let extracted = extract(kind, file.bytes).unwrap_or_else(|e| panic!("{}: {e}", file.path));
            assert_eq!(extracted.kind, kind, "{}", file.path);
            let was = files.insert((kind.to_string(), extracted.id.clone()), file.bytes.to_vec());
            assert!(was.is_none(), "{}: id {} is used twice", file.path, extracted.id);
        }
        files
    }

    #[test]
    fn every_file_type_the_plugin_registers_has_examples() {
        let kinds: HashSet<_> = project().keys().map(|(kind, _)| kind.clone()).collect();
        for kind in KINDS {
            assert!(kinds.contains(kind), "no example of {kind}");
        }
        // The few extension-only checks: the legacy `.skelanim` folder type
        // is deliberately absent (new files are made as skeleton + sequence).
        assert!(FILES.iter().all(|f| !f.path.ends_with(".skelanim")));
    }

    #[test]
    fn every_example_loads_with_its_own_codec_and_has_no_findings() {
        let project = project();
        let mut checked = 0;
        let mut problems = Vec::new();
        for file in FILES {
            let Some(kind) = extension(file).and_then(kind_of_extension) else {
                continue;
            };
            // The codec read, the asset's own checks against its skeleton or
            // rigs, and the dependency closure: every reference resolves to
            // a file of the right kind for the same skeleton, no cycles.
            let findings: Vec<Diagnostic> = asset_report(&project, kind, file.bytes);
            if !findings.is_empty() {
                problems.push(format!("{}: {findings:#?}", file.path));
            }
            checked += 1;
        }
        assert!(problems.is_empty(), "{}", problems.join("
"));
        assert!(checked >= 18, "only {checked} examples");
    }

    #[test]
    fn the_readme_is_there() {
        assert!(FILES.iter().any(|f| f.path == "examples/README.md"));
    }
}
