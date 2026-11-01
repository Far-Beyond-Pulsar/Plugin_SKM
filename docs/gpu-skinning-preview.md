# Skeletal preview skinning

The Sequence, Skeleton, Mesh, Montage and Blend Space previews skin indexed
bind-pose vertices in the viewport vertex shader. Pose evaluation remains on
the CPU. No deformed vertex array is built or uploaded during playback.

## Data and ownership

`PreparedSkin` retains the immutable source asset, valid triangle indices and
per-joint mesh-space bounds. The panel caches it by mesh `Arc` identity and
`mesh_revision`; the offscreen runtime preview uses the same cache. Holding the
source `Arc` makes editor mutations copy-on-write and prevents address reuse
from disguising an asset replacement.

Each viewport renderer owns its buffers and bindings. It uploads a LOD's static
vertices and indices on first use and retains visited LODs for its active mesh.
Replacing the mesh drops that owner's LODs, palette and bind group together.
Resizing the viewport retains mesh buffers. The existing device-loss path
constructs a fresh renderer; immutable preparation can be reused on the new
device. Separate previews never share a mutable palette.

A changed pose uploads an 80-byte entry per joint. An unchanged palette uploads
nothing. Weight selection changes per-joint flags instead of rebuilding vertex
colors. Queue writes precede submission of the frame using those matrices.
Packed static CPU vertices are discarded after upload.

## Skinning contract

- File joint indices remain `u16`, with four influences per vertex. The GPU
  vertex layout explicitly widens them to `Uint32x4`; the source Rust struct is
  not treated as a byte layout. Positions, normals, UVs, joints and weights use
  a 64-byte stride. Indices are `Uint32`.
- Palette order is the mesh's joint table, including separate entries that
  follow the same bone. Matrices are column-major `bone_world * inverse_bind`.
  A bone absent from the pose gets identity, matching the CPU reference.
- Nonpositive weights and out-of-range joints are skipped. Positions divide by
  the sum of usable weights. No usable weight leaves position and normal in
  bind space. The heat map sums raw weights for matching joints.
- Normals retain the reference's normalized upper-3x3 blend and its `1e-8`
  threshold. This is exact for rotation and uniform scale; it preserves the
  existing approximation under nonuniform scale. The preview has no tangent
  attribute or normal-map shading. Cancelled normals produce finite lighting.
- Invalid triangles and incomplete trailing indices are omitted as before.
  Non-finite vertex data is rejected at preparation; a non-finite palette omits
  the skin for that frame. Device binding and buffer limits are checked before
  allocation; unsupported or oversized skins are omitted with a diagnostic.

Bounds are conservative: transform the eight corners of each occupied joint's
cached box and union them with uninfluenced vertices. Positive normalized
blends lie inside that union. Pose-dependent work scales with occupied joints,
not vertices, and requires no readback. These bounds can widen camera framing
and the approximate contact shadow compared with exact CPU-skinned bounds.
Camera zoom and clipping limits now accommodate large posed bounds.

The asset format, manual LOD selection, flat material shading, skeleton overlay,
selection, transform handles, timeline, notifies and reload policy are preserved.
No engine submodule pointer change is included in this plugin PR.

## Validation commands

Run inside a current Pulsar-Native workspace with this plugin at
`plugins/vendor/skm_editor`:

```sh
cargo check -p skeletal_assets -p skeletal_animation_plugin --all-targets --locked
cargo test -p skeletal_assets -p skeletal_animation_plugin --locked
cargo clippy -p skeletal_assets -p skeletal_animation_plugin --all-targets --no-deps
cargo test -p skeletal_animation_plugin --lib -- --ignored \
  --skip skinning_geometry_benchmark --skip skinning_render_benchmark \
  --nocapture --test-threads=1
```

Explicit GPU tests fail if no adapter is available. They execute shared skinning
WGSL through readback, compare the actual indexed vertex/lighting/TAA pipeline
with CPU-skinned renders, and check buffer uploads, LOD switches, replacements,
empty palettes, independent previews and recreation on a second device. CI
installs Mesa Vulkan and explicitly runs these correctness tests.

The readback tolerance is `2e-5 + 2e-6 * abs(reference)` for positions and `3e-6`
for normal/color components. Render comparisons allow mean channel error below
0.2 byte values and fewer than 0.5% of pixels differing by over three byte values.
The CPU reference uses `skeletal_assets::skinning`, not a second invocation of
the GPU math.

Set `SKM_SAMPLE_OUT` to an imported rig directory to include it in GPU readback
parity. The existing `asset_import --test real_sample` and plugin
`render_real_sample` tests import and render BrainStem; source and generated
assets remain outside the repository. Set `SKM_PREVIEW_DIR` to save render PNGs.

For optimized benchmarks, select the two ignored benchmark tests separately:

```sh
cargo test -p skeletal_animation_plugin \
  --config 'profile.dev.package.skeletal_animation_plugin.opt-level=3' \
  --config 'profile.dev.package.skeletal_assets.opt-level=3' \
  skinning_geometry_benchmark -- --ignored --nocapture
cargo test -p skeletal_animation_plugin \
  --config 'profile.dev.package.skeletal_animation_plugin.opt-level=3' \
  --config 'profile.dev.package.skeletal_assets.opt-level=3' \
  skinning_render_benchmark -- --ignored --nocapture
```

The CPU benchmark compares the old deformation, triangle expansion and bounds
work with palette/bounds preparation at 10k/1, 60k/64 and 500k/128 vertices/joints.
It reports static and pose-dependent upload payloads from their actual layouts.
The rendering benchmark uses a stable 256x256 offscreen target, five warmup
frames and fifteen measured frames, waiting for completion of each submission.
It reports preparation time, synchronized offscreen frame time, and scene-pass
GPU timestamps when supported. This excludes native UI and presentation;
software-renderer timings are not hardware GPU performance measurements.

## Measured results

Measured on 2026-11-01, Linux, Intel N150 / Intel ADL-N integrated GPU,
Mesa 26.2.5 and Rust 1.99.1, against Pulsar `1b2e75737` and Plugin_SKM `0ef0b7e`.
Final compatibility was checked with Pulsar `ed7be2f58`, whose intervening
changes do not alter the affected packages or their pins. Only the two affected
packages use the optimization overrides above. No other
build or benchmark ran during the measurements.

Standalone playback preparation (pre-evaluated world poses; 30 old-path calls
and 1,000 palette/bounds calls after five warmups):

| Vertices / joints | CPU geometry, µs | Palette + bounds, µs | Old bytes / pose | Palette bytes / pose |
| --- | ---: | ---: | ---: | ---: |
| 10,000 / 1 | 892.801 | 0.465 | 1,319,736 | 80 |
| 60,000 / 64 | 5,693.476 | 12.740 | 7,919,736 | 5,120 |
| 500,000 / 128 | 55,415.880 | 25.232 | 65,999,736 | 10,240 |

One-time bounds/index preparation took 0.627 / 3.471 / 30.302 ms respectively;
this excludes first-upload packing and driver work. Static upload payloads are
759,976 / 4,559,976 / 37,999,976 bytes. Stationary palettes upload zero bytes.
These are payload counts, not measurements of physical bus traffic.

The synchronized offscreen benchmark separately measures per-frame preparation,
submission and completion. The scene timestamp covers the sky and mesh pass;
the full offscreen frame also includes the TAA/resolve work:

| Vertices / joints | CPU-path frame, ms | GPU-path frame, ms | CPU-path scene GPU, ms | GPU-skin scene GPU, ms |
| --- | ---: | ---: | ---: | ---: |
| 10,000 / 1 | 3.947 | 2.146 | 0.271 | 0.218 |
| 60,000 / 64 | 13.418 | 4.916 | 1.432 | 2.462 |
| 500,000 / 128 | 98.998 | 16.261 | 5.391 | 11.955 |

The large fixture's GPU pass costs more with vertex skinning. Removing CPU
geometry preparation and expanded-vertex uploads still reduces its synchronized
offscreen time. These results do not measure native editor input, UI layout or
presentation and are not a claim about whole-editor frame rate.

The complete affected-package run passed 441 tests plus doc tests. Ten multi-skin
import regressions and the real-rig import test passed. Explicit GPU tests passed
on both Intel Vulkan and llvmpipe, including the 34,159-vertex, 18-joint imported
BrainStem rig at bind pose and three animation times. Native render parity had
mean channel error below 0.00002 byte values; llvmpipe stayed below 0.00005.

Changed viewport files pass Rustfmt and the patch passes whitespace checks.
Clippy completed with existing warnings; crate-wide formatting still reports
unchanged upstream files. Scoped Desloppify scan/review/rescans addressed the
new test-harness layout and CI coverage; its embedded rustdoc detector reported
a parser error, while direct compilation and doc tests passed. Analysis state
is excluded from the patch.

Metal/DX12, native mouse/keyboard interaction and forced device-loss recovery
were not exercised. Resource recreation on a separately created device was
executed on both Vulkan adapters.
