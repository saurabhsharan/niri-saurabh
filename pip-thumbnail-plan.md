# PiP Window Thumbnail Feature

## Context

Add a `make-window-thumbnail-pip` bind action that creates a live Picture-in-Picture thumbnail of the currently focused window. The PiP thumbnail renders as a small overlay in the lower-right corner, mirrors the source window's content in real time with zero-copy rendering, can be dragged around within its output, and has a hover-visible close button.

This is a personal fork, so the design should prioritize:
- maintainability over generality
- modularity over deep integration
- a small merge surface against upstream `niri`

## Architecture Decision

**Store PiP state in `Niri` as a standalone compositor overlay**, similar to `hotkey_overlay` and `window_mru_ui`, not inside layout or floating-space internals.

Rationale:
- PiP should persist across workspace switches without becoming part of workspace state.
- This avoids modifying layout data structures that are large, complex, and likely to drift upstream.
- Most new logic can stay isolated in `src/ui/pip.rs` and `src/input/pip_grab.rs`.
- Existing-file edits stay small and localized to stable integration points.

**Use stable PiP IDs, not vector indices, across module boundaries.**

Rationale:
- Pointer grabs and click handlers should not depend on `Vec` indices that can shift after window unmap/destroy cleanup.
- A tiny `PipId(u64)` owned by `PipManager` keeps the module self-contained and avoids stale-index bugs.

**Mirror window contents using the MRU thumbnail rendering pattern, not just raw rescaling.**

Rationale:
- `Mapped::render_normal()` already gives zero-copy rendering of the source surfaces.
- However, the MRU UI does more than scale: it also wraps render elements to preserve rounded-corner clipping and blocked-out-window rendering.
- PiP should copy that local pattern into `src/ui/pip.rs` so the feature is visually correct without touching layout code.

**Keep PiP below transient compositor and layer-shell overlay UI, but above normal workspace content.**

Rationale:
- Hotkey overlay, MRU, and overlay-layer surfaces should remain authoritative and keep their existing input behavior.
- PiP still behaves like a compositor-level overlay during normal usage.
- This makes render order and hit-testing consistent with fewer special cases.

**Redraw PiPs from source-window commits and PiP interactions, not via permanent continuous redraw.**

Rationale:
- A PiP should not force the compositor into a forever-busy state just because one exists.
- Source-window commits already flow through stable handler paths.
- This preserves idle behavior and keeps the implementation maintainable.

## Files to Create

### 1. `src/ui/pip.rs` -- Core PiP module

This file owns all PiP state, hit-testing, render helpers, cleanup helpers, and output-local geometry logic.

**Types:**
- `type PipId = u64`
- `struct PipThumbnail`
- `struct PipManager`

**`PipThumbnail` fields:**
- `id: PipId`
- `source_id: MappedId`
- `output: Output`
- `position: Point<f64, Logical>` -- upper-left corner in output-local coordinates
- `size: Size<f64, Logical>` -- current PiP size used for hit-testing and rendering
- `source_size: Size<f64, Logical>` -- cached source geometry size, refreshed on source commits
- `is_hovered: bool`

**`PipManager` fields:**
- `thumbnails: Vec<PipThumbnail>`
- `next_id: PipId`

**Methods on `PipManager`:**
- `new() -> Self`
- `toggle_pip(mapped: &Mapped, output: &Output) -> Option<PipId>`
  - If a PiP already exists for `mapped.id()`, remove it.
  - Otherwise create one at 1/4 output width in the lower-right corner with 16 px margin.
- `remove(id: PipId) -> Vec<Output>`
- `remove_by_source(id: MappedId) -> Vec<Output>`
- `remove_by_output(output: &Output) -> Vec<Output>`
- `find(id: PipId) -> Option<&PipThumbnail>`
- `find_mut(id: PipId) -> Option<&mut PipThumbnail>`
- `pip_under(output: &Output, pos: Point<f64, Logical>) -> Option<PipId>`
  - Hit-test in reverse order so the most recently created PiP wins if they overlap.
- `close_button_rect(id: PipId) -> Option<Rectangle<f64, Logical>>`
- `update_hover(pointer: Option<(&Output, Point<f64, Logical>)>) -> bool`
  - Clears stale hover state on all non-target outputs too.
- `refresh_source_size(source: MappedId, size: Size<i32, Logical>)`
- `outputs_for_source(source: MappedId) -> Vec<Output>`
- `clamp_to_output(output: &Output, size: Size<f64, Logical>) -> bool`
  - Keeps PiPs on-screen after output resize.
- `render_for_output<R: NiriRenderer>(...)`

**Render element enums:**

Use a local copy of the MRU wrapping strategy so PiP rendering remains isolated inside this module:

```rust
niri_render_elements! {
    PipThumbnailRenderElement<R> => {
        LayoutElement = LayoutElementRenderElement<R>,
        ClippedSurface = ClippedSurfaceRenderElement<R>,
        Border = BorderRenderElement,
    }
}

niri_render_elements! {
    PipRenderElement<R> => {
        Thumbnail = RelocateRenderElement<RescaleRenderElement<PipThumbnailRenderElement<R>>>,
        SolidColor = SolidColorRenderElement,
    }
}
```

**`render_for_output()` implementation:**
1. Iterate PiPs belonging to the target output.
2. Resolve each source `Mapped` from `niri.layout.windows()` via `MappedId`.
3. If the source is missing, skip rendering it; cleanup paths remove stale PiPs quickly.
4. Use the same clipping / blocked-out-window wrapping pattern as MRU before scaling.
5. Rescale and relocate the wrapped render elements into PiP geometry.
6. If `is_hovered`, render a simple close button with `SolidColorRenderElement`.

This keeps the feature zero-copy while preserving rounded corners and blocked-out-window behavior.

### 2. `src/input/pip_grab.rs` -- Pointer grab for dragging

**`PipDragGrab` fields:**
- `start_data: PointerGrabStartData<State>`
- `pip_id: PipId`
- `output: Output`
- `grab_offset: Point<f64, Logical>`

**`PointerGrab<State>` behavior:**
- `motion()`
  - Convert pointer location to output-local coordinates using `output_under()`.
  - If the pointer is still on the PiP's output, update the PiP position to `pos - grab_offset`.
  - Clamp to output bounds.
  - If the PiP was removed externally, unset the grab.
  - Redraw the affected output.
- `relative_motion()`: forward to handle
- `button()`: on left-button release, unset the grab
- `axis()` / gesture methods: forward to handle
- `start_data()`: return `&self.start_data`
- `unset()`
  - restore the default cursor image
  - redraw the PiP output

For the initial implementation, dragging stays within the PiP's current output. That keeps the code simpler and avoids extra cross-output state churn.

## Files to Modify

### 3. `src/ui/mod.rs`

Add:

```rust
pub mod pip;
```

### 4. `src/input/mod.rs`

**Module declaration:**

```rust
pub mod pip_grab;
```

**In `do_action()` add:**

Use the focused window's output, not the cursor output:

```rust
Action::MakeWindowThumbnailPip => {
    if let Some((mapped, output)) = self.niri.layout.focus_with_output() {
        let mapped = mapped.clone();
        let output = output.clone();
        self.niri.pip_manager.toggle_pip(&mapped, &output);
        self.niri.queue_redraw(&output);
    }
}
```

This keeps keyboard-triggered behavior deterministic on multi-output setups.

**In `on_pointer_button()` add a PiP hit-test block after the MRU block and before mouse binds:**

Rationale:
- PiP is rendered above normal workspace content.
- PiP clicks should therefore win over normal window activation and mouse binds underneath it.

Use stable `PipId` lookups only; do not index directly into `thumbnails`.

Pseudo-flow:
- find `pip_id` with `pip_under(output, pos_within_output)`
- if the close button was hit, remove the PiP
- otherwise create `PipDragGrab::new(...)`
- set the cursor to `Grabbing`
- suppress the button release
- redraw the affected output

**In pointer motion paths update PiP hover state in all three places:**
- `on_pointer_motion()`
- `on_pointer_motion_absolute()`
- `on_tablet_tool_axis()`

Use a single helper-style call:

```rust
let hover = self.niri.output_under(pos).map(|(output, pos)| (output, pos));
if self.niri.pip_manager.update_hover(hover) {
    self.niri.queue_redraw_all();
}
```

Important detail:
- pass `None` when the pointer is not over any output so hover state clears correctly

### 5. `niri-config/src/binds.rs`

Add action variant:

```rust
MakeWindowThumbnailPip,
```

And in `From<niri_ipc::Action> for Action`:

```rust
niri_ipc::Action::MakeWindowThumbnailPip {} => Self::MakeWindowThumbnailPip,
```

Optional polish:
- add a friendly name in `src/ui/hotkey_overlay.rs` if you want this action to read nicely there instead of falling back to `"FIXME: Unknown"`

### 6. `niri-ipc/src/lib.rs`

Add IPC action variant:

```rust
MakeWindowThumbnailPip {},
```

### 7. `src/niri.rs`

**Add field to `Niri`:**

```rust
pub pip_manager: PipManager,
```

**Initialize it in the constructor:**

```rust
pip_manager: PipManager::new(),
```

**Add a render element variant:**

```rust
Pip = PipRenderElement<R>,
```

**Add a small redraw helper near `queue_redraw_mru_output()`:**

```rust
pub fn queue_redraw_pips_for_source(&mut self, source: MappedId) {
    for output in self.pip_manager.outputs_for_source(source) {
        self.queue_redraw(&output);
    }
}
```

This keeps redraw plumbing out of handlers.

**Add PiP render call in `render_inner()` after `Layer::Overlay` is rendered and before top/workspace content:**

This placement means:
- below hotkey overlay
- below MRU
- below layer-shell overlay surfaces
- above top/workspace/bottom/background content

That ordering is conservative and keeps transient UI authoritative.

**Do not add always-on redraw via `unfinished_animations_remain`.**

Instead, PiP redraws should come from:
- source-window commits
- PiP drag motion
- PiP create/remove actions
- hover changes
- output resize cleanup

**Add PiP check to `contents_under()` after overlay-layer hit-testing but before hot-corner / top-layer / layout hit-testing:**

This keeps input ordering aligned with render ordering.

Pseudo-shape:

```rust
let mut under =
    layer_popup_under(Layer::Overlay).or_else(|| layer_toplevel_under(Layer::Overlay));

if under.is_none() {
    if self.pip_manager.pip_under(output, pos_within_output).is_some() {
        return rv;
    }
}
```

Important detail:
- PiP must be checked before the hot corner so the overview does not trigger when the cursor is over a visible PiP in a corner.

**Add PiP check to `window_under()` after `output_under()` and sticky-obscured checks, before layout hit-testing:**

```rust
if self.pip_manager.pip_under(output, pos_within_output).is_some() {
    return None;
}
```

**Handle output lifecycle:**
- In `output_removed()`, remove PiPs on that output with `remove_by_output(output)`
- In `output_resized()`, clamp PiPs on that output and redraw if anything moved

This avoids orphaned or off-screen PiPs and keeps output-specific behavior contained in one place.

### 8. `src/handlers/compositor.rs`

Use existing window-commit paths to drive PiP updates instead of permanent redraw.

**On mapped toplevel commits:**
- after `window_mru_ui.update_window(...)`
- refresh cached PiP source size with `refresh_source_size(id, mapped.size())`
- call `queue_redraw_pips_for_source(id)`

Do this in both existing commit paths:
- previously-mapped toplevel root commits
- non-root / non-toplevel-root commits that still belong to a mapped window

**On unmap cleanup:**

After `self.niri.window_mru_ui.remove_window(id);`:

```rust
for output in self.niri.pip_manager.remove_by_source(id) {
    self.niri.queue_redraw(&output);
}
```

This is important because the PiP may live on a different output than the source window.

### 9. `src/handlers/xdg_shell.rs`

On toplevel destruction, mirror the same cleanup pattern:

```rust
for output in self.niri.pip_manager.remove_by_source(id) {
    self.niri.queue_redraw(&output);
}
```

## Rendering And Input Order

Final intended top-down stacking order:

```text
Pointer
Screen transition
Exit confirm dialog
Config error notification
Hotkey overlay
MRU / Alt-Tab UI
Layer::Overlay
PiP thumbnails
Layer::Top / interactive move / workspaces / Layer::Bottom / Layer::Background
Backdrop
```

This is intentional:
- compositor-global UI stays above PiP
- PiP stays above normal workspace content
- render order and click order match

## Close Button Design

Initial version:
- 24x24 logical px semi-transparent dark square at the PiP upper-left corner
- visible only while hovered
- hit-tested before drag start

This keeps the first implementation simple and self-contained.

Future polish, if wanted:
- render a nicer circular button and `X` glyph using a small texture, following patterns already used in `src/ui/exit_confirm_dialog.rs`

## Redraw Strategy

Do not set `state.unfinished_animations_remain = true` just because a PiP exists.

Instead:
- source-window commits redraw every output that hosts a PiP for that source
- drag motion redraws the PiP output
- hover transitions redraw affected outputs
- create/remove/output-resize events redraw affected outputs

This is a better fit for a personal fork too:
- less hidden power cost
- no compositor-wide busy loop
- behavior remains explicit and debuggable

If a future edge case needs frame-driven refresh, add it as a targeted fallback, not as the default behavior.

## Verification Plan

1. Build with `cargo build`.
2. Add a bind such as `Mod+P { make-window-thumbnail-pip; }`.
3. Open a window, press `Mod+P`, verify the PiP appears on the focused window's output, not the cursor's output.
4. Change the source window contents, verify the PiP updates without requiring permanent always-on redraw.
5. Hover the PiP and verify the close button appears and disappears correctly.
6. Click-drag the PiP and verify it moves smoothly and the cursor resets correctly on release.
7. Open MRU or another overlay-layer UI and verify PiP stays below it and does not steal its input.
8. Switch workspaces and verify PiP persists.
9. Close the source window and verify the PiP disappears immediately.
10. Resize the output and verify the PiP is clamped on-screen.
11. Disconnect an output with a PiP on it and verify the PiP is cleaned up cleanly.
12. Create PiPs for multiple windows and verify hit-testing and close/drag behavior remain correct.
13. Press `Mod+P` on the same focused window twice and verify the second invocation removes the PiP.
