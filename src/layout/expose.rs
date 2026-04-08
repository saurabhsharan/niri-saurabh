//! Expose layout algorithm.
//!
//! Computes a grid layout that shows all windows from a workspace as scaled thumbnails,
//! inspired by macOS Mission Control.
//!
//! EXPOSE INTEGRATION: This is a self-contained module. The only integration points are
//! in layout/mod.rs, layout/monitor.rs, niri.rs, and input/mod.rs.

use std::cmp;

use smithay::utils::{Logical, Point, Size};

/// Outer padding around the grid in logical pixels.
const PADDING: f64 = 48.0;
/// Gap between grid cells in logical pixels.
const GAP: f64 = 24.0;
/// Minimum scale factor for a window thumbnail.
const MIN_SCALE: f64 = 0.15;

/// Describes the computed position and scale of one window in the expose layout.
#[derive(Debug, Clone)]
pub struct ExposedWindow<Id> {
    /// Window identifier.
    pub id: Id,
    /// Top-left position in output-logical coordinates (expose target).
    pub target_pos: Point<f64, Logical>,
    /// Scale factor applied to the tile (0.0–1.0).
    pub target_scale: f64,
    /// Original render position before expose (for animation interpolation).
    pub original_pos: Point<f64, Logical>,
    /// Original tile size (unscaled).
    pub tile_size: Size<f64, Logical>,
}

/// Direction for keyboard navigation in the expose grid.
#[derive(Debug, Clone, Copy)]
pub enum ExposeDirection {
    Up,
    Down,
    Left,
    Right,
}

/// The complete expose layout for one workspace.
#[derive(Debug, Clone)]
pub struct ExposeLayout<Id> {
    pub windows: Vec<ExposedWindow<Id>>,
    /// Number of columns in the grid.
    pub cols: usize,
    /// Number of rows in the grid.
    pub rows: usize,
    /// Currently selected window index (for keyboard navigation).
    /// Independent of Niri's focused window state.
    pub selected_idx: usize,
}

impl<Id: Clone + PartialEq> ExposeLayout<Id> {
    /// Compute the expose grid layout.
    ///
    /// `windows`: iterator of (id, original_render_pos, tile_size).
    /// `view_size`: the output size in logical pixels.
    pub fn compute(
        windows: impl Iterator<Item = (Id, Point<f64, Logical>, Size<f64, Logical>)>,
        view_size: Size<f64, Logical>,
    ) -> Self {
        let mut items: Vec<(Id, Point<f64, Logical>, Size<f64, Logical>)> =
            windows.collect();
        let n = items.len();

        if n == 0 {
            return Self {
                windows: Vec::new(),
                cols: 0,
                rows: 0,
                selected_idx: 0,
            };
        }

        // Compute grid dimensions based on window count and screen aspect ratio.
        let aspect = view_size.w / view_size.h;
        let cols = if n == 1 {
            1usize
        } else {
            let c = (((n as f64) * aspect).sqrt()).ceil() as usize;
            cmp::min(c, n)
        };
        let rows = (n + cols - 1) / cols;

        // Compute cell sizes with padding and gaps.
        let available_w = view_size.w - 2.0 * PADDING;
        let available_h = view_size.h - 2.0 * PADDING;
        let cell_w = (available_w - (cols as f64 - 1.0) * GAP) / cols as f64;
        let cell_h = (available_h - (rows as f64 - 1.0) * GAP) / rows as f64;

        if cell_w <= 0.0 || cell_h <= 0.0 {
            // Screen too small for padding — just return empty.
            return Self {
                windows: Vec::new(),
                cols: 0,
                rows: 0,
                selected_idx: 0,
            };
        }

        // Generate slot centers.
        let mut slots: Vec<Point<f64, Logical>> = Vec::with_capacity(n);
        for r in 0..rows {
            // How many windows in this row.
            let cols_in_row = if r == rows - 1 {
                let last = n - (rows - 1) * cols;
                if last == 0 { cols } else { last }
            } else {
                cols
            };

            // Center the last row if it has fewer items.
            let row_offset_x = if cols_in_row < cols {
                ((cols - cols_in_row) as f64 * (cell_w + GAP)) / 2.0
            } else {
                0.0
            };

            for c in 0..cols_in_row {
                let x = PADDING + row_offset_x + c as f64 * (cell_w + GAP) + cell_w / 2.0;
                let y = PADDING + r as f64 * (cell_h + GAP) + cell_h / 2.0;
                slots.push(Point::from((x, y)));
            }
        }

        // Sort windows by their original position: primary by Y (quantized to rows),
        // secondary by X. This preserves spatial relationships.
        //
        // We quantize Y into row buckets so that windows at similar heights stay in
        // the same row.
        let row_height = if rows > 1 {
            view_size.h / rows as f64
        } else {
            view_size.h
        };
        items.sort_by(|a, b| {
            let a_row = (a.1.y / row_height) as i64;
            let b_row = (b.1.y / row_height) as i64;
            a_row.cmp(&b_row).then_with(|| {
                a.1.x
                    .partial_cmp(&b.1.x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });

        // Assign windows to slots in order.
        let exposed: Vec<ExposedWindow<Id>> = items
            .into_iter()
            .zip(slots.into_iter())
            .map(|((id, original_pos, tile_size), slot_center)| {
                // Scale to fit within cell, preserving aspect ratio, never upscaling.
                let scale_x = cell_w / tile_size.w;
                let scale_y = cell_h / tile_size.h;
                let scale = scale_x.min(scale_y).min(1.0).max(MIN_SCALE);

                let scaled_w = tile_size.w * scale;
                let scaled_h = tile_size.h * scale;

                let target_pos = Point::from((
                    slot_center.x - scaled_w / 2.0,
                    slot_center.y - scaled_h / 2.0,
                ));

                ExposedWindow {
                    id,
                    target_pos,
                    target_scale: scale,
                    original_pos,
                    tile_size,
                }
            })
            .collect();

        Self {
            windows: exposed,
            cols,
            rows,
            selected_idx: 0,
        }
    }

    /// Set the selected index to the window matching `id`, or 0 if not found.
    pub fn select_by_id(&mut self, id: &Id) {
        self.selected_idx = self
            .windows
            .iter()
            .position(|w| w.id == *id)
            .unwrap_or(0);
    }

    /// Get the id of the currently selected window.
    pub fn selected_id(&self) -> Option<&Id> {
        self.windows.get(self.selected_idx).map(|w| &w.id)
    }

    /// Navigate in the given direction, wrapping around edges.
    pub fn navigate(&mut self, direction: ExposeDirection) {
        let n = self.windows.len();
        if n == 0 {
            return;
        }

        let row = self.selected_idx / self.cols;
        let col = self.selected_idx % self.cols;

        let new_idx = match direction {
            ExposeDirection::Left => {
                if self.selected_idx == 0 {
                    n - 1
                } else {
                    self.selected_idx - 1
                }
            }
            ExposeDirection::Right => {
                if self.selected_idx + 1 >= n {
                    0
                } else {
                    self.selected_idx + 1
                }
            }
            ExposeDirection::Up => {
                // Go to the same column in the previous row, wrapping to bottom.
                let mut target_row = if row == 0 { self.rows - 1 } else { row - 1 };
                loop {
                    let idx = target_row * self.cols + col;
                    if idx < n {
                        break idx;
                    }
                    // Last row might not have this column; go up one more.
                    if target_row == 0 {
                        target_row = self.rows - 1;
                    } else {
                        target_row -= 1;
                    }
                    if target_row == row {
                        // Wrapped all the way around; stay put.
                        break self.selected_idx;
                    }
                }
            }
            ExposeDirection::Down => {
                // Go to the same column in the next row, wrapping to top.
                let mut target_row = if row + 1 >= self.rows { 0 } else { row + 1 };
                loop {
                    let idx = target_row * self.cols + col;
                    if idx < n {
                        break idx;
                    }
                    // Last row might not have this column; wrap to top.
                    if target_row + 1 >= self.rows {
                        target_row = 0;
                    } else {
                        target_row += 1;
                    }
                    if target_row == row {
                        break self.selected_idx;
                    }
                }
            }
        };

        self.selected_idx = new_idx;
    }

    /// Find which window is under the given point (in output-logical coordinates).
    ///
    /// Returns the window id if found. Iterates in reverse order so topmost
    /// (last-rendered) windows are hit first.
    pub fn window_at(&self, point: Point<f64, Logical>, progress: f64) -> Option<&Id> {
        for exposed in self.windows.iter().rev() {
            let pos = lerp_point(exposed.original_pos, exposed.target_pos, progress);
            let scale = lerp(1.0, exposed.target_scale, progress);
            let w = exposed.tile_size.w * scale;
            let h = exposed.tile_size.h * scale;

            if point.x >= pos.x && point.x < pos.x + w && point.y >= pos.y && point.y < pos.y + h {
                return Some(&exposed.id);
            }
        }
        None
    }
}

/// Linear interpolation between two f64 values.
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Linear interpolation between two points.
pub fn lerp_point(a: Point<f64, Logical>, b: Point<f64, Logical>, t: f64) -> Point<f64, Logical> {
    Point::from((lerp(a.x, b.x, t), lerp(a.y, b.y, t)))
}
