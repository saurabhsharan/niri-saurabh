# Expose Feature Implementation Plan

## Overview

Implement a macOS Mission Control-inspired "Expose" mode for Niri that displays all windows from the **current workspace** as scaled thumbnails in a dynamic grid layout. Users can click a window to focus it.

**Key differences from Overview:**
- Overview zooms out to show all workspaces with windows at their real positions (just scaled down). Expose rearranges windows into a grid layout optimized for visibility.
- Overview doesn't guarantee all windows are visible (off-screen scrolling columns are not shown). Expose shows every window on the active workspace.
- Overview preserves exact relative positions. Expose uses a grid layout with position-affinity assignment.

---

## Architecture Principles

Since this is a personal fork that must be rebased on upstream releases:

1. **Self-contained core logic** in a new file `src/layout/expose.rs` — the grid layout algorithm, per-window state, and animation interpolation all live here.
2. **Minimal surgical changes** to existing files — limited to: adding action variants, adding state fields, hooking into render/input paths, and adding config.
3. **Document all integration points** with `// EXPOSE INTEGRATION` comments so they're easy to find during rebases.
4. **No changes to core rendering primitives** — reuse existing `RescaleRenderElement`, `RelocateRenderElement`, `CropRenderElement`, and the `Tile::render()` pipeline.

---

## 1. Grid Layout Algorithm

### 1.1 Algorithm Description

The layout algorithm arranges N windows into a grid that fits the monitor's working area with padding.

**Grid dimensioning:**
```
Given:
  N = number of windows
  W, H = working area dimensions (output size minus exclusive zones)
  aspect = W / H

Compute:
  cols = ceil(sqrt(N * aspect))
  rows = ceil(N / cols)

  // Clamp to avoid degenerate layouts
  if cols > N { cols = N; rows = 1; }
```

This naturally adapts to ultrawide monitors: a 21:9 display with 6 windows produces a 4×2 grid (wider than tall), while a 16:9 display produces 3×2.

**Cell sizing:**
```
padding = 48px logical (outer margin on all sides)
gap     = 24px logical (space between cells)

available_w = W - 2 * padding
available_h = H - 2 * padding
cell_w = (available_w - (cols - 1) * gap) / cols
cell_h = (available_h - (rows - 1) * gap) / rows
```

**Slot centers:**
```
For row r, col c:
  slot_center_x = padding + c * (cell_w + gap) + cell_w / 2
  slot_center_y = padding + r * (cell_h + gap) + cell_h / 2
```

**Last-row centering:** If the last row is not full (N % cols != 0), offset the last row's slots horizontally to center them. This prevents a ragged left-aligned bottom row.

```
last_row_count = N - (rows - 1) * cols
last_row_offset = (cols - last_row_count) * (cell_w + gap) / 2
```

### 1.2 Window-to-Slot Assignment (Position Affinity)

To approximate macOS's behavior of preserving relative window positions:

1. Compute each window's **original center point** in workspace-relative coordinates. For scrolling windows, use their tile position from `tiles_with_render_positions()`. For floating windows, use their logical position.
2. Sort slots left-to-right, top-to-bottom.
3. Sort windows by their original center (primary: y-coordinate quantized to rows, secondary: x-coordinate).
4. Assign windows to slots in order.

This is simpler than Hungarian assignment and produces intuitive results: windows that were on the left stay on the left, windows that were higher stay higher.

**Alternative for better position preservation:** Use a greedy nearest-slot assignment:
1. Build a list of (window, original_center) pairs.
2. Build a list of (slot_index, slot_center) pairs.
3. Compute all pairwise distances.
4. Repeatedly assign the closest unassigned (window, slot) pair.

Start with the simpler sorted assignment; upgrade to greedy if results look wrong in practice.

### 1.3 Per-Window Scaling

Each window is scaled to fit within its cell while preserving aspect ratio:

```
window_w, window_h = tile.tile_size() (includes border)
scale_x = cell_w / window_w
scale_y = cell_h / window_h
scale = min(scale_x, scale_y, 1.0)  // Never upscale

// Minimum scale to prevent tiny thumbnails
scale = max(scale, 0.15)

scaled_w = window_w * scale
scaled_h = window_h * scale

// Center within cell
pos_x = slot_center_x - scaled_w / 2
pos_y = slot_center_y - scaled_h / 2
```

### 1.4 Special Cases

- **0 windows:** Expose shows just the backdrop. Click or press Escape to close.
- **1 window:** Centered in the working area, scaled to ~75% of screen width/height (whichever is limiting).
- **Many windows (>16):** The algorithm handles this naturally — cells just get smaller. No special casing needed, though the minimum scale of 0.15 prevents them from becoming illegibly tiny.

---

## 2. Data Structures

### 2.1 New File: `src/layout/expose.rs`

```rust
use smithay::utils::{Logical, Point, Size, Rectangle};
use std::collections::HashMap;

/// Describes the computed position and scale of one window in expose layout.
#[derive(Debug, Clone)]
pub struct ExposedWindow<Id> {
    pub id: Id,
    /// Top-left position in workspace-logical coordinates.
    pub target_pos: Point<f64, Logical>,
    /// Scale factor applied to the tile (0.0–1.0).
    pub scale: f64,
    /// Original position before expose (for animation interpolation).
    pub original_pos: Point<f64, Logical>,
    /// Original scale (always 1.0 in normal layout).
    pub original_scale: f64,
}

/// Configuration for expose layout.
#[derive(Debug, Clone, Copy)]
pub struct ExposeConfig {
    pub padding: f64,       // Outer padding in logical px (default 48)
    pub gap: f64,           // Gap between cells in logical px (default 24)
    pub min_scale: f64,     // Minimum window scale (default 0.15)
    pub backdrop_color: [f32; 4],
}

/// The complete expose layout for one workspace.
#[derive(Debug, Clone)]
pub struct ExposeLayout<Id> {
    pub windows: Vec<ExposedWindow<Id>>,
    pub grid_cols: usize,
    pub grid_rows: usize,
}

impl<Id: Clone + PartialEq> ExposeLayout<Id> {
    /// Compute the expose grid layout.
    ///
    /// `windows_with_positions`: iterator of (id, original_pos, tile_size).
    /// `view_size`: the output/workspace size.
    /// `config`: layout parameters.
    pub fn compute(
        windows_with_positions: impl Iterator<Item = (Id, Point<f64, Logical>, Size<f64, Logical>)>,
        view_size: Size<f64, Logical>,
        config: &ExposeConfig,
    ) -> Self {
        // ... implementation as described in section 1
    }

    /// Find which window (if any) is under the given point, considering
    /// the expose layout positions and scales.
    pub fn window_at(&self, point: Point<f64, Logical>, tile_sizes: &HashMap<Id, Size<f64, Logical>>) -> Option<&Id> {
        // Iterate in reverse (topmost first) and check if point is within
        // the window's scaled bounding rect.
    }
}
```

### 2.2 Expose State in Layout (additions to `src/layout/mod.rs`)

```rust
// New fields in the Layout struct (near overview_open/overview_progress):
pub(super) expose_open: bool,
pub(super) expose_progress: Option<ExposeProgress>,
```

`ExposeProgress` follows the same pattern as `OverviewProgress`:
```rust
pub(super) enum ExposeProgress {
    Animation(Animation),
    Gesture(ExposeGesture),
    Open,
}
```

### 2.3 Expose State in Monitor (additions to `src/layout/monitor.rs`)

```rust
// New fields in Monitor:
pub(super) expose_open: bool,
pub(super) expose_progress: Option<MonitorExposeProgress>,
/// Cached expose layout for the active workspace, recomputed when
/// expose opens or windows change.
pub(super) expose_layout: Option<ExposeLayout<W::Id>>,
```

The `MonitorExposeProgress` mirrors the monitor-level `OverviewProgress`:
```rust
pub(super) enum MonitorExposeProgress {
    Animation { value: f64 },
    Gesture { value: f64 },
    Open,
}
```

---

## 3. Rendering

### 3.1 Strategy

During expose, instead of calling the normal `ws.render_scrolling()` and `ws.render_floating()` pipeline which renders windows at their layout positions, we render each tile individually at its interpolated expose position with its interpolated scale.

**Interpolation formula** (applied per-window):
```
t = expose_progress (0.0 = closed, 1.0 = fully open)
current_pos   = lerp(original_pos,   target_pos,   t)
current_scale = lerp(original_scale,  target_scale, t)
```

### 3.2 New Render Method on Monitor

Add `Monitor::render_expose()` in `monitor.rs` (or preferably keep most logic in `expose.rs` and call it from monitor):

```rust
pub fn render_expose<R: NiriRenderer>(
    &self,
    renderer: &mut R,
    target: RenderTarget,
    push: &mut dyn FnMut(MonitorRenderElement<R>),
) {
    let Some(layout) = &self.expose_layout else { return };
    let progress = self.expose_progress_value(); // 0.0–1.0

    let active_ws = &self.workspaces[self.active_workspace_idx];
    let scale = self.scale.fractional_scale();

    for exposed in &layout.windows {
        // Find the tile in the workspace
        let Some((tile, _orig_pos, _visible)) = active_ws
            .tiles_with_render_positions()
            .find(|(t, _, _)| t.window().id() == &exposed.id)
        else {
            continue;
        };

        // Interpolate position and scale
        let pos = lerp_point(exposed.original_pos, exposed.target_pos, progress);
        let tile_scale = lerp(exposed.original_scale, exposed.scale, progress);

        // Render tile at computed position with computed scale
        tile.render(renderer, pos, /* focus_ring */ true, target, &mut |elem| {
            // Apply per-window scale via RescaleRenderElement
            let elem = RescaleRenderElement::from_element(
                elem,
                pos.to_physical_precise_round(scale),
                tile_scale,
            );
            push(elem.into());
        });
    }
}
```

**Note:** The exact rendering approach needs care to avoid double-scaling. The tile's own `render()` positions elements relative to a tile origin. We need to:
1. Render the tile as if at position (0,0) and normal scale
2. Apply `RescaleRenderElement` with the expose scale
3. Apply `RelocateRenderElement` to move to the expose position

This matches the pattern already used in `render_workspaces()` with the `scale_relocate` closure.

### 3.3 Integration in Main Render Pipeline

In `src/niri.rs`, `render_inner()` (~line 4197), add a branch:

```rust
// EXPOSE INTEGRATION: When expose is active, render expose view
// instead of normal workspace rendering.
if monitor.expose_progress.is_some() {
    monitor.render_expose(renderer, target, &mut |elem| {
        push(OutputRenderElements::Monitor(elem));
    });
} else if monitor.overview_progress.is_some() {
    // ... existing overview rendering ...
} else {
    // ... existing normal rendering ...
}
```

The backdrop, overlay layers, and top layers render exactly as in overview mode (dark backdrop behind, overlay/top layers above). We reuse the same `backdrop_color` or add a separate one.

### 3.4 What Surfaces to Show/Hide

Match overview behavior exactly:
- **Show:** Overlay layer, Top layer (above expose), expose window thumbnails
- **Hide/dim:** Bottom layer, Background layer (rendered within backdrop)
- **Backdrop:** Solid color behind everything (same as overview or configurable)

---

## 4. Input Handling

### 4.1 Keyboard

Add `KeyboardFocus::Expose` variant to the `KeyboardFocus` enum in `src/niri.rs`.

Hardcoded keybindings when in expose mode (similar to overview, in `src/input/mod.rs` around line 4631):
- `Escape` → Close expose
- `Return` → Focus highlighted window and close expose
- Arrow keys → Navigate between windows in the grid (left/right move within row, up/down move between rows)

### 4.2 Mouse Click

In `on_pointer_button()` (src/input/mod.rs ~line 2879):

```rust
// EXPOSE INTEGRATION: Handle click in expose mode
if is_expose_open {
    if let Some(window_id) = monitor.expose_window_at(pointer_pos) {
        // Focus this window and close expose
        layout.focus_window_in_expose(&window_id);
        layout.close_expose();
    } else {
        // Clicked on empty space — just close expose
        layout.close_expose();
    }
    return;
}
```

Hit testing uses `ExposeLayout::window_at()` which checks if the pointer is within any window's scaled bounding rectangle.

### 4.3 Hover Highlight

When the pointer moves over a window in expose mode, highlight it (e.g., brighter border or slight scale-up). This provides visual feedback before clicking.

Implementation: Track `expose_hovered_window: Option<Id>` in the monitor/layout. On pointer motion, update this by calling `expose_window_at()`. When rendering, the hovered window gets a focus ring or a subtle scale boost (e.g., 1.05× additional scale).

---

## 5. Actions and Configuration

### 5.1 New Actions

Add to `niri-config/src/binds.rs` `Action` enum:
```rust
ToggleExpose,
OpenExpose,
CloseExpose,
```

Add corresponding IPC actions in `niri-ipc/src/lib.rs`.

### 5.2 Configuration

Add to `niri-config/src/misc.rs`:
```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Expose {
    pub backdrop_color: Color,
    pub padding: f64,     // Default 48.0
    pub gap: f64,         // Default 24.0
}
```

Add to `niri-config/src/animations.rs`:
```rust
pub struct ExposeOpenCloseAnim(pub Animation);
// Default: same spring params as overview (damping 1.0, stiffness 800)
```

### 5.3 Config File Syntax

```kdl
expose {
    backdrop-color 0.15 0.15 0.15 1.0
    padding 48
    gap 24
}

animations {
    expose-open-close {
        spring damping-ratio=1.0 stiffness=800 epsilon=0.0001
    }
}
```

---

## 6. State Machine and Lifecycle

### 6.1 States

```
Closed (expose_open=false, expose_progress=None)
  │
  ├─ toggle_expose() ──► Opening (expose_open=true, expose_progress=Animation(0→1))
  │                         │
  │                         ├─ animation complete ──► Open (expose_open=true, expose_progress=Open)
  │                         │                           │
  │                         │                           ├─ toggle_expose() ──► Closing
  │                         │                           ├─ click window ──► Closing (with focus change)
  │                         │                           └─ Escape ──► Closing
  │                         │
  │                         └─ toggle_expose() ──► Closing (expose_open=false, expose_progress=Animation(current→0))
  │
  └─ gesture_begin() ──► Gesture (expose_open=true, expose_progress=Gesture)
                            │
                            └─ gesture_end() ──► Opening or Closing animation
```

### 6.2 Mutual Exclusion with Overview

Expose and Overview are mutually exclusive. Opening Expose while Overview is open should close Overview first (and vice versa). Implementation:

```rust
pub fn toggle_expose(&mut self) {
    // Close overview if open
    if self.overview_open {
        self.close_overview();
    }
    // ... proceed with expose toggle
}
```

Similarly, `toggle_overview()` should close expose if open.

### 6.3 Layout Recomputation

The expose layout (`ExposeLayout`) is computed:
1. When expose opens (initial computation).
2. When a window is added or removed while expose is open (recompute with animation).
3. When output size changes while expose is open.

The layout is **not** recomputed continuously — it's a snapshot taken at open time with updates only for structural changes.

---

## 7. Animation Details

### 7.1 Enter Animation

When expose opens:
1. Snapshot all window positions from the current layout state via `tiles_with_render_positions()`.
2. Compute the `ExposeLayout` grid.
3. Start a spring animation from 0.0 to 1.0.
4. Each frame, interpolate every window between its original position/scale and its grid position/scale using the animation progress.

### 7.2 Exit Animation (Normal Close)

When expose closes without selecting a window:
1. Animate from current progress back to 0.0.
2. Windows return to their original positions.
3. When animation completes, clear expose state.

### 7.3 Exit Animation (Window Selected)

When the user clicks a window in expose:
1. **Before starting the close animation:** Change focus to the selected window in the layout. This may cause the scrolling view to shift (view offset changes).
2. Recompute "original positions" based on the **new** layout state (after focus change). This ensures windows animate to where they'll actually be, not where they were before.
3. Animate from current progress to 0.0.
4. When animation completes, clear expose state.

This produces a smooth animation where the selected window flies to its new focused position and other windows return to their (updated) positions.

**Simplification if the above is too complex:** Skip recomputing original positions. Just close expose instantly (no animation) and let the normal layout take over. The focus change itself may have its own animation (view offset scrolling). This is less polished but much simpler and still looks acceptable.

### 7.4 Gesture Support (Optional, Phase 2)

Support 3-finger or 4-finger swipe to progressively enter/exit expose, similar to overview gestures. Reuse the same `SwipeTracker` infrastructure. This can be added later since keybind toggle is sufficient for v1.

---

## 8. IPC

### 8.1 State Query

Add `Request::ExposeState` → `Response::ExposeState(Expose { is_open: bool })` in `niri-ipc`.

### 8.2 Event

Add `Event::ExposeOpenedOrClosed { is_open: bool }` for event stream subscribers.

### 8.3 Actions

The `ToggleExpose`, `OpenExpose`, `CloseExpose` actions are already dispatchable via IPC through the standard `Request::Action(action)` mechanism.

---

## 9. File-by-File Change List

### New Files

| File | Purpose |
|------|---------|
| `src/layout/expose.rs` | Core algorithm: `ExposeLayout`, `ExposedWindow`, `compute()`, `window_at()`, render helpers, interpolation utilities |

### Modified Files

| File | Changes | Scope |
|------|---------|-------|
| `src/layout/mod.rs` | Add `expose_open`, `expose_progress` fields to `Layout`. Add `toggle_expose()`, `open_expose()`, `close_expose()`, `focus_window_in_expose()` methods. Add `mod expose;`. Sync expose state to monitors in `set_monitors_overview_state()` (rename or add parallel method). | ~80 lines |
| `src/layout/monitor.rs` | Add `expose_open`, `expose_progress`, `expose_layout` fields to `Monitor`. Add `render_expose()`, `expose_window_at()` methods. Add expose progress advancement in `advance_animations()`. | ~120 lines |
| `src/niri.rs` | Add `KeyboardFocus::Expose` variant. In `render_inner()`, add expose rendering branch. In `update_keyboard_focus()`, handle expose focus. Add expose backdrop rendering. Add `ipc_refresh_expose()`. | ~40 lines |
| `src/input/mod.rs` | Handle `Action::ToggleExpose/OpenExpose/CloseExpose` in `do_action()`. Add expose click handling in `on_pointer_button()`. Add hardcoded keys for expose mode. Add expose hover tracking in pointer motion. | ~60 lines |
| `niri-config/src/binds.rs` | Add `ToggleExpose`, `OpenExpose`, `CloseExpose` to `Action` enum. | ~6 lines |
| `niri-config/src/misc.rs` | Add `Expose` struct with defaults. Add `ExposePart` for config parsing. | ~30 lines |
| `niri-config/src/animations.rs` | Add `ExposeOpenCloseAnim` with default spring params. | ~15 lines |
| `niri-ipc/src/lib.rs` | Add expose action variants and state/event types. | ~15 lines |
| `src/ipc/server.rs` | Handle `Request::ExposeState`. Add `ipc_refresh_expose()`. Emit expose events. | ~15 lines |

### Total Estimated Changes
- **New code:** ~400 lines in `expose.rs`
- **Integration changes:** ~380 lines across existing files
- **Clearly marked:** All integration points use `// EXPOSE INTEGRATION` comments

---

## 10. Implementation Order

### Phase 1: Core (Minimum Viable Expose)
1. Add `Action::ToggleExpose/OpenExpose/CloseExpose` to config and IPC.
2. Create `src/layout/expose.rs` with `ExposeLayout::compute()` and `ExposedWindow`.
3. Add expose state fields to `Layout` and `Monitor`.
4. Implement `toggle_expose()`, `open_expose()`, `close_expose()` on `Layout`.
5. Implement `Monitor::render_expose()` — render tiles at interpolated positions.
6. Hook into `niri.rs` render pipeline — when expose active, render expose instead of normal workspace.
7. Add `KeyboardFocus::Expose` and hardcoded Escape to close.
8. Add click-to-focus: hit test expose layout, focus window, close expose.

### Phase 2: Polish
9. Add enter/exit spring animations.
10. Implement exit-with-focus animation (recompute targets after focus change).
11. Add hover highlight (focus ring on hovered window).
12. Arrow key navigation in the grid.
13. Add expose config (`padding`, `gap`, `backdrop_color`).
14. Add IPC state query and events.

### Phase 3: Optional Enhancements
15. Gesture support (swipe to enter/exit).
16. Window close button on hover (×) — click to close window from expose.
17. Window title labels below thumbnails.
18. Animated layout recomputation when windows open/close during expose.

---

## 11. Assumptions and Upstream Risks

These are implementation assumptions that could break on upstream Niri updates. Check these during rebases.

| Assumption | Where Used | Risk |
|------------|-----------|------|
| `Workspace::tiles_with_render_positions()` returns all tiles with their current render positions | Expose layout computation | Low — stable API used throughout codebase |
| `Tile::render()` accepts arbitrary position and renders at it | Expose rendering | Low — fundamental tile rendering interface |
| `RescaleRenderElement` and `RelocateRenderElement` can be composed for per-window scaling | Expose rendering | Low — already used this way in overview |
| `OverviewProgress` enum pattern (Animation/Gesture/Open) is the standard for modal state | Expose state machine | Low — established pattern |
| `KeyboardFocus` enum can be extended with new variants | Expose input handling | Medium — new variants upstream could conflict |
| `Action` enum can be extended with new variants | Config/IPC | Medium — upstream may add similar feature with different names |
| `Monitor` struct fields are directly accessible from `Layout` | State synchronization | Low — established pattern with overview |
| `on_pointer_button()` in `input/mod.rs` handles modal click dispatch in a specific order | Click handling | Medium — upstream may restructure input handling |
| `render_inner()` in `niri.rs` has clear branching for overview | Render pipeline hookup | Medium — render pipeline may be refactored |
| `WorkspaceShadow` config exists and is reusable | Expose window shadows | Low — stable config type |

---

## 12. Testing Strategy

1. **Manual testing matrix:**
   - 1, 2, 3, 6, 9, 12, 16+ windows
   - Regular 16:9 display
   - Ultrawide 21:9 display (test with `WINIT_BACKEND` and custom resolution)
   - Mixed tiling + floating windows
   - Fullscreen windows in expose
   - Opening/closing windows while expose is active

2. **Edge cases:**
   - Expose with 0 windows (empty workspace)
   - Expose with 1 window
   - Very small windows (terminal) vs very large windows (fullscreen)
   - Toggling expose rapidly
   - Switching focus via expose, verifying correct window receives input after close

3. **Animation verification:**
   - Smooth enter/exit transitions
   - No visual glitches at animation start/end
   - Windows land at correct positions after close animation

---

## 13. Design Rationale: Why Not Modify Overview?

We could add an "expose mode" to the existing Overview. However:

1. **Overview is workspace-centric** (shows all workspaces). Expose is **window-centric** (shows all windows on one workspace). The rendering and layout logic are fundamentally different.
2. **Overview preserves window positions** (just zooms out). Expose **rearranges** windows into a grid. The animation model is different (overview interpolates a single zoom factor; expose interpolates per-window position and scale).
3. **Separation reduces rebase conflicts.** Adding expose as a separate system means overview changes upstream don't break expose and vice versa.
4. **The overview code is already complex** (gesture tracking, workspace switching, view offset, insert hints). Adding expose logic would make it harder to maintain.
