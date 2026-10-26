https://github.com/user-attachments/assets/1ab9e9ba-8750-49b3-b6ad-d74473f0b969

<img width="2496" height="1310" alt="Screenshot 2026-06-12 at 9 00 12 PM" src="https://github.com/user-attachments/assets/29d52036-3191-4922-9df3-4091381c9c3e" />
<img width="2158" height="1182" alt="Screenshot 2026-06-13 at 12 14 39 AM" src="https://github.com/user-attachments/assets/c62099b6-105a-4835-a461-7b66b659e21e" />

## Controls

**Viewport** — RMB drag: look · RMB held + WASD / Space / Shift: fly · MMB drag: pan · Alt+LMB drag: orbit ·
Alt+RMB drag: dolly · scroll: zoom · click a joint: select · double-click: frame it · click a gizmo axis: snap
the view · `F` / `Home`: frame selection / everything · `Space`: play.

**Timeline** — click or drag the ruler: scrub (snaps to frames) · click / drag keys: select / move ·
Shift+click: add to selection · drag empty space: marquee · scroll: zoom · Ctrl+scroll or MMB: pan ·
Shift+scroll: scroll tracks · `←` `→`: step a frame (Shift = 10) · `[` `]`: previous / next key · `K`: key the
selected bone · `Ctrl+A/C/X/V`: select all / copy / cut / paste · `Del`: delete · `F`: fit the play range ·
`Esc`: cancel a key drag.

**Hierarchy** — arrow keys navigate, `←`/`→` collapse/expand, the filter box narrows the tree, hovering a row
highlights the bone in the viewport.

**Skeleton editing** — Add Root / Add Child build the hierarchy. Select a bone or socket to rename,
change its parent, edit its local transform, or delete it. `Insert` adds a bone, `F2` focuses its name,
`Enter` applies the name, and `Delete` removes a leaf or socket. Delete Subtree asks for confirmation.
The Edit menu and standard undo/redo shortcuts restore hierarchy and socket edits. Referenced skeletons
in sequence and mesh editors are read-only. See [the hierarchy design and compatibility notes](docs/skeleton-hierarchy.md)
for bind-pose preservation, dependency checks, socket persistence and the proposed host follow-up.

## Rendering

Both canvases take their colors from the active engine theme (`style.rs`). The viewport draws an analytic
anti-aliased ground grid, lit bones with an x-ray ghost for occluded parts, onion-skin poses, a motion path for
the selected bone, thick anti-aliased ribbon lines, and a temporally anti-aliased resolve. The timeline is a
single SDF shape pipeline (diamonds, rounded rects, glow, hatch) with GPUI text on top for crisp labels.

## Previewing the GPU output headlessly

The renderers can be run without opening the editor. These tests render into an offscreen texture and skip
themselves when no GPU adapter is available:

```
SKM_PREVIEW_DIR=./previews cargo test -p skeletal_animation_plugin --lib
```

With `SKM_PREVIEW_DIR` set, each render is also written out as a PNG (`timeline.png`, `viewport_dark.png`,
`viewport_light.png`, …).

### Checking a real rigged model

Two tests check the importer and the skinned viewport against a real rigged, animated glTF. They skip unless
pointed at one, and nothing made from the model is committed (check its license before sharing the outputs):

```
# 1. import it (asset_import) into a scratch folder
SKM_SAMPLE_DIR=<folder with BrainStem.glb> SKM_OUT_DIR=<scratch> cargo test -p asset_import --test real_sample
# 2. render it, optionally comparing skinned vertices with an independent reference (SKM_REF_JSON)
SKM_SAMPLE_OUT=<scratch> SKM_PREVIEW_DIR=<pngs> cargo test -p skeletal_animation_plugin --lib render_real_sample
```
