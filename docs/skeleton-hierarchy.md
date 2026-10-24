# Skeleton hierarchy editing (#1335)

The skeleton editor owns its bones and sockets. Sequence and mesh editors show
copies resolved from the referenced skeleton and cannot edit either collection.
Bone IDs and socket IDs are stable strings; names are display labels. Renaming a
bone never rewrites an animation track or mesh joint. New editor-created IDs use
the existing UUID dependency. The public `Bone` and `Skeleton` fields remain
available for importers; `Skeleton::validate`, asset parsing, asset saving and
every hierarchy mutation reject duplicate IDs, missing parents, cycles and
non-finite transforms. Empty skeletons and multiple roots are valid.

## Editing

Use Add Root, Add Child and Add Socket in the hierarchy. Select an item to rename
it, change its parent, or delete it. The parent menu excludes the selected bone
and its descendants. Bone rows also have context menus. Insert adds a child (or
a root with no selection), F2 focuses the name, Enter applies the name, and Delete removes a
leaf or socket. Delete Subtree confirms the number of bones and sockets removed.
Sockets appear below their bones, have local transform controls in Properties,
and draw attachment markers in the viewport; a selected socket draws local axes.

All these edits use `plugin_editor_api::History`, capped at 100 entries, with
sparse ordered bone and socket deltas. Transform edits capture one target;
bone-only edits do not copy sockets. History stores no mesh geometry, animation clips, renderer or engine state. Consecutive transform
input changes merge within an input session. Focus, blur, Enter, selection,
another command, undo/redo and Save end the run. Reset applies exact model
defaults in its own transaction and then refreshes the displayed inputs. Undo/redo restores order, parents, names,
transforms, sockets and primary selection. Expanded nodes are pruned and the restored selection's ancestors are
expanded. The same Undo/Redo actions and Edit menu as other Pulsar editors apply;
text fields retain their own text history.

## Bind pose and dependent assets

Reparenting preserves the moved bone's world-space bind matrix and therefore the
world matrices of its descendants. Composition is `parent_world * local`, with
column-major matrices. The new local transform is
`inverse(new_parent_world) * old_world`. A singular parent or a result that cannot
be represented by the existing translation/Euler rotation/scale model (shear or
singular axes) returns an error without mutation. Normalized basis dot products
must be at most `2e-5`; each reconstructed
basis column must agree within `2e-5` of its original length, without an absolute
unit-scale floor. Lengths use f64 arithmetic to avoid overflow/underflow during
measurement. Axes below `1e-8` are conservatively unsupported. Reflections are
represented by a signed scale. The recomposed candidate world matrix and every
descendant are checked using runtime f32 arithmetic before mutation. Translation
error is limited per component to `2e-5 * (1 + abs(original))` engine units; this
allows small roundoff without scaling the allowance by a distant new parent.
This is a bounded numerical preservation guarantee, not bitwise equivalence.
Wider local arithmetic cannot recover offsets that the persisted f32 TRS cannot
represent, so those changes are rejected. A second comparison uses the original
inverse world matrix, after verifying its f32 inverse reconstructs identity:
candidate and original bind-space matrices must also agree
within the same tolerance. Large world coordinates therefore cannot hide a
substantial skin-matrix change. The `2e-5` allowance is roughly 168 f32 epsilons,
allowing Euler conversion and composition roundoff while remaining well below
the three-decimal display precision near the origin. Existing inverse bind
matrices and mesh joint ordering are unchanged.

Local animation keys cannot in general be preserved by changing a bind
transform: the old and new animated parent chains can differ at every time.
Reparenting is therefore rejected when a project sequence for this skeleton has
any authored keys. An explicit animation baking/migration tool would be a
separate feature. Empty tracks retain their bone IDs.

Before deleting bones, the editor checks project `.animseq` tracks, `.skmesh`
joints and `.physasset` bodies/constraints. References to removed bones cause a
clear rejection; dependent files are never rewritten. Sockets on removed bones
are deleted in the same confirmed transaction and restored by undo. Deleting a
non-leaf through the leaf command is rejected rather than implicitly promoting
children. Unreadable dependencies or failed/oversized scans reject destructive
edits. Command and UI Save scans run on the existing GPUI background executor and
discard results if the skeleton revision changed while scanning. Save also checks
the path, asset identity and preview reference before writing. Save compares the
last saved hierarchy through an indexed lookup, then checks dependencies again,
which also covers undo/redo and assets created after the initial edit. Save
errors retain the pending edits and appear in the editor banner. Mesh reference
checks read bounded headers and joint tables without decoding geometry. Valid
hidden files and directories are included; `.pulsar` metadata, VCS directories
and the provider's established build/cache directories are excluded. The public
synchronous `save` method remains available to explicit blocking callers; UI
handlers use the asynchronous path.

Checks cover persisted assets in the project, not unsaved documents in other
windows, external projects, or script-authored string references. The filesystem
is not locked against another process changing files between validation and
writing. As with existing asset saving, the write itself is atomic.

## Socket format proposal (maintainer agreement needed)

The proposed additive version 1 field is an optional `sockets` array on
`SkeletonAsset`:

```json
{
  "sockets": [{
    "id": "03a785c4-fc9d-446e-9530-7d4c55c77b95",
    "name": "Weapon",
    "bone_id": "hand_r",
    "local_transform": {
      "translation": {"x": 0.0, "y": 0.0, "z": 0.0},
      "rotation": {"x": 0.0, "y": 0.0, "z": 0.0},
      "scale": {"x": 1.0, "y": 1.0, "z": 1.0}
    }
  }]
}
```

The field defaults to empty and is omitted when empty. Bone and skeleton asset
IDs are unchanged. Socket IDs and names are unique within the skeleton (names
are case-sensitive); bone names may repeat. A socket must reference an existing
bone and have finite local transform components. `Socket::world_transform`
resolves an attachment as `bone_world * socket_local`, using the current pose.
Changing a socket's bone retains its local offset; unlike a bone reparent, this
is an intentional change to attachment space.

`Skeleton` and `Bone` keep their existing Rust struct shapes. The separate
`Sockets` collection lives in the asset and editor; no SceneDB component or
runtime attachment protocol is introduced. Existing version 1 files load as
empty socket collections. Older editors ignore unknown JSON fields and could
drop sockets when saving; maintainers must decide whether that warrants a format
version bump or an unknown-field preservation policy before merge.

The public `SkeletonAsset` Rust struct gains `sockets`: external struct literals
must supply it, even though old serialized files still load. This is a source
compatibility break; linked hosts and DLL plugins must rebuild against the same
asset crate. `Skeleton` and `Bone` retain their field layouts.

Construction audit (`rg` across tracked and untracked Rust sources, including
submodules): the editor save path is the only external literal and supplies the
collection. `SkeletonAsset::new` initializes empty sockets for the importer,
legacy `.skelanim` conversion, samples and test fixtures. Library loads parse
and validate the collection. Legacy conversion has no authored sockets to copy.
The host importer reconstructs existing assets and therefore needs the separate
follow-up patch to preserve sockets and reject lost parents before any writes.
No other production constructors were found in this checkout.

## Draft issue comment (do not post automatically)

For Pulsar-Native#1335, these policies remain proposals requiring maintainer agreement:

* Versioning: sockets currently use an optional version 1 array. Existing files
  load empty; older editors can silently discard the array on save. Choose a new
  format version or an explicit preservation policy before merge. This draft does
  not approve additive v1 as the release policy.
* Reparent: preserve the world bind pose within the documented f32 tolerance;
  reject shear, singular/precision-losing results and skeletons with authored
  animation keys until explicit track migration exists. IDs and inverse binds stay fixed.
* Delete: reject persisted animation-track, mesh-joint and applicable physics
  references, including hidden assets. Confirmed subtree deletion removes attached
  sockets in one undoable edit. No dependent asset is rewritten implicitly.
  Unsaved other documents, external projects and script strings are outside this scan.
* Compatibility: stable socket IDs, unique case-sensitive names and bone-local
  offsets are proposed. Adding `SkeletonAsset::sockets` breaks downstream Rust
  literals and requires rebuilding consumers. Reimport must preserve sockets and
  reject missing parents before writes. This adds no runtime attachment contract.

Merge Plugin_SKM upstream first; then land the separate host reimport/provider
patch and pin Pulsar-Native to the merged upstream-main commit. No feature-branch
pin or policy approval is implied. This comment is a draft and has not been posted.

## Dependency-first host follow-up

Merge Plugin_SKM first. Then update Pulsar-Native's submodule pin to that upstream
main commit, following `.agents/CRATES.md`. The current working tree deliberately
leaves the parent pin unchanged. The host's skeletal built-in provider should
call `SkeletalAnimationPlugin::create_editor_with_undo(file_path,
Some(&editor_context.services.undo), window, cx)` so host-level history inspection
sees the document. The existing `create_editor` API still works and retains local
history using the same shared implementation; dynamic factories already receive
the service through `register_fn_with_settings`.

The concrete follow-up is in `docs/pulsar-native-followup.patch`. It includes
socket-preserving reimport, rejection before writes when a socket parent would
be lost, importer regression coverage, and the built-in undo-service wiring.
The patch is kept separate from the plugin changes and must accompany the host
pin update after the plugin merges.
