# Animation examples

Read-only examples of every file type the Animation plugin registers. They
are one consistent set: a humanoid skeleton, a mesh skinned to it, clips for
it, and one of each asset built on the clips. Assets refer to each other by
stable id (never by name or path), so you can copy the whole folder into a
project, or any file with the files it names, and the references still hold.

All files are JSON except `.skmesh` (binary, see below). In the JSON assets
that allow it, a free-form `description` field says what the example shows;
it is a note the engine ignores and the editors keep when they save. The
`.skeleton` format has no such field, so its notes are here.

## The set

| Folder | File | Type | Shows |
|---|---|---|---|
| `skeleton/` | `humanoid.skeleton` | Skeleton | 21 bones in T-pose (Y up, metres, arms along X), three sockets (weapon grip, hat, back holster) and four named masks (UpperBody, RightArm, LowerBody, HeadSoft) the montages and graphs use by name |
| `skeleton/` | `stylized.skeleton` | Skeleton | a shorter character with different bone names, the target of the retargeting examples |
| `meshes/` | `humanoid_blockout.skmesh` | Skeletal Mesh | a box per bone, each rigidly weighted to its joint, skinned to the humanoid skeleton |
| `meshes/` | `stylized_blockout.skmesh` | Skeletal Mesh | the same idea for the stylized skeleton |
| `animations/` | `humanoid_idle.animseq` | Animation Sequence | a looping idle |
| `animations/` | `humanoid_walk.animseq` | Animation Sequence | root motion, notifies with a typed payload schema, a step curve |
| `animations/` | `humanoid_run.animseq` | Animation Sequence | a faster cycle for blending with the walk |
| `animations/` | `humanoid_jump.animseq` | Animation Sequence | a one-shot with notifies, a notify state (begin, tick, end) and a float curve |
| `animations/` | `humanoid_wave.animseq` | Animation Sequence | only the right arm keyed, an overlay |
| `animations/` | `humanoid_slash.animseq` | Animation Sequence | an upper-body attack |
| `blend_spaces/` | `humanoid_locomotion.blendspace` | Blend Space | 1D by speed, phase sync |
| `blend_spaces/` | `humanoid_strafe.blendspace` | Blend Space | 2D by direction (wrapping) and speed |
| `montages/` | `sword_slash.montage` | Animation Montage | a masked upper-body slot track, sections that chain, blend in and out, notifies |
| `montages/` | `wave_twice.montage` | Animation Montage | an inline mask, a trimmed clip looped twice, overlap interruption |
| `animation_graphs/` | `humanoid_locomotion.animgraph` | Animation Graph | a state machine (Idle, Locomotion, Jump) with transition rules driven by parameters, then a slot for montages |
| `animation_graphs/` | `humanoid_upper_body_layer.animgraph` | Animation Graph | a layered blend per bone through a skeleton mask |
| `ik_rigs/` | `humanoid.ikrig` | IK Rig | chains, goals, two-bone and FABRIK solvers, joint limits |
| `ik_rigs/` | `stylized.ikrig` | IK Rig | the same chain names on a different skeleton |
| `retargeting/` | `humanoid_to_stylized.animretarget` | Retarget Profile | bone-to-bone mappings between the two skeletons (a profile maps by bone or by rig chain, not both) |
| `retargeting/` | `humanoid_to_stylized.ikretarget` | IK Retargeter | per-chain FK and IK settings between the two rigs |
| `pose_search/` | `humanoid_locomotion.posedb` | Pose Search Database | motion matching channels over the clips |

## Notes on the formats

* **Keys are local transforms.** A track's keyframe holds the bone's whole
  local transform (translation, Euler rotation in degrees XYZ, scale), so the
  translation of a track starts at the bone's bind translation. A sequence
  need not key every bone.
* **`.skmesh` is binary**: a short magic and version, a JSON header (mesh and
  skeleton ids, joint table with inverse bind matrices, material slots, LODs)
  and then packed vertices and indices. Open it in the Skeletal Mesh editor.
* **A `.skelanim` folder** (the old format) is not provided: new files are
  made as a skeleton plus sequences.
* **The ids are fixed**, so copying a file does not make a new asset: two
  files with one id make a reference ambiguous. Give a copy you will edit its
  own `id` (a new UUID) first, or let the editor's duplicate command do it.
* The Physics plugin's example ragdoll (`skeletal_physics/humanoid_ragdoll.physasset`
  in its examples) is made for `humanoid_blockout.skmesh`: it carries that
  mesh's id and names bones of this skeleton.
