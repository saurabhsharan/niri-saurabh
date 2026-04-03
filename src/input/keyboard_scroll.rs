use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use smithay::backend::input::{Axis, AxisRelativeDirection, AxisSource};
use smithay::input::pointer::AxisFrame;

use crate::niri::State;
use crate::utils::get_monotonic_time;

const KEYBOARD_SCROLL_INTERVAL: Duration = Duration::from_nanos(8_333_333);
pub const DEFAULT_KEYBOARD_SCROLL_PIXELS_PER_SECOND: f64 = 150.;
const KEYBOARD_SCROLL_DECAY_TIME_CONSTANT: Duration = Duration::from_millis(80);
const KEYBOARD_SCROLL_MIN_VELOCITY: f64 = 1.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardScrollDirection {
    Up,
    Down,
}

impl KeyboardScrollDirection {
    fn amount(self, amount: f64) -> f64 {
        match self {
            Self::Up => -amount,
            Self::Down => amount,
        }
    }
}

// Snapshot of the scroll target at scroll start. If any of these change while scrolling, treat
// it as leaving the original context and stop immediately instead of continuing or decaying into
// a different output/window/focus target.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyboardScrollTargets {
    output_name: Option<String>,
    window_under_cursor_id: Option<u64>,
    focused_window_id: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ActiveKeyboardScroll {
    pub direction: KeyboardScrollDirection,
    pub speed: f64,
    pub current_velocity: f64,
    pub decay: bool,
    pub is_decaying: bool,
    pub last_tick_time: Duration,
    targets: KeyboardScrollTargets,
}

impl State {
    pub(super) fn start_keyboard_scroll(
        &mut self,
        direction: KeyboardScrollDirection,
        speed: f64,
        decay: bool,
    ) {
        self.niri.active_keyboard_scroll = Some(ActiveKeyboardScroll {
            direction,
            speed,
            current_velocity: speed,
            decay,
            is_decaying: false,
            // Prime the first immediate tick with one interval's worth of scroll.
            last_tick_time: get_monotonic_time().saturating_sub(KEYBOARD_SCROLL_INTERVAL),
            // Capture the initial target so later ticks can detect whether the scroll context
            // changed underneath us and should therefore stop immediately.
            targets: self.keyboard_scroll_targets(),
        });

        if let Some(token) = self.niri.keyboard_scroll_timer.take() {
            self.niri.event_loop.remove(token);
        }

        let timer = Timer::immediate();
        let token = self
            .niri
            .event_loop
            .insert_source(timer, |_, _, state| {
                state.tick_keyboard_scroll();

                if state.niri.active_keyboard_scroll.is_some() {
                    TimeoutAction::ToDuration(KEYBOARD_SCROLL_INTERVAL)
                } else {
                    state.niri.keyboard_scroll_timer = None;
                    TimeoutAction::Drop
                }
            })
            .unwrap();

        self.niri.keyboard_scroll_timer = Some(token);
    }

    // Hard-stop scrolling right now. Use this for non-release interruptions where decaying would
    // be misleading or unsafe, like session/output/focus changes.
    pub(super) fn stop_keyboard_scroll_immediately(&mut self) {
        if self.niri.active_keyboard_scroll.is_none() {
            return;
        }

        if let Some(token) = self.niri.keyboard_scroll_timer.take() {
            self.niri.event_loop.remove(token);
        }

        self.niri.active_keyboard_scroll = None;
    }

    // Handle a normal key-release stop. This keeps the scroll alive briefly if decay is enabled,
    // otherwise it falls back to the same hard stop as stop_keyboard_scroll_immediately().
    pub(super) fn stop_keyboard_scroll_on_release(&mut self) {
        let Some(scroll) = &mut self.niri.active_keyboard_scroll else {
            return;
        };

        if !scroll.decay {
            // This is still a release-driven stop, but the bind explicitly disabled decay.
            self.stop_keyboard_scroll_immediately();
            return;
        }

        scroll.is_decaying = true;
        scroll.last_tick_time = get_monotonic_time();
    }

    fn keyboard_scroll_targets(&self) -> KeyboardScrollTargets {
        // Recompute the current scroll context so ticks can compare it against the snapshot taken
        // at scroll start.
        KeyboardScrollTargets {
            output_name: self.niri.output_under_cursor().map(|output| output.name()),
            window_under_cursor_id: self
                .niri
                .window_under_cursor()
                .map(|window| window.id().get()),
            focused_window_id: self.niri.layout.focus().map(|window| window.id().get()),
        }
    }

    fn tick_keyboard_scroll(&mut self) {
        let Some(mut scroll) = self.niri.active_keyboard_scroll.take() else {
            return;
        };

        // If the pointer/focus context changed, don't continue or decay into a different target.
        if self.keyboard_scroll_targets() != scroll.targets {
            return;
        }

        let now = get_monotonic_time();
        let delta = now.saturating_sub(scroll.last_tick_time);
        scroll.last_tick_time = now;

        if delta.is_zero() {
            self.niri.active_keyboard_scroll = Some(scroll);
            return;
        }

        if scroll.is_decaying {
            let delta_secs = delta.as_secs_f64();
            let decay_factor =
                (-delta_secs / KEYBOARD_SCROLL_DECAY_TIME_CONSTANT.as_secs_f64()).exp();
            scroll.current_velocity *= decay_factor;

            if scroll.current_velocity < KEYBOARD_SCROLL_MIN_VELOCITY {
                return;
            }
        }

        let amount = scroll
            .direction
            .amount(scroll.current_velocity * delta.as_secs_f64());

        let pointer = self.niri.seat.get_pointer().unwrap();
        let frame = AxisFrame::new(now.as_millis() as u32)
            .source(AxisSource::Continuous)
            .relative_direction(Axis::Vertical, AxisRelativeDirection::Identical)
            .value(Axis::Vertical, amount);

        pointer.axis(self, frame);
        pointer.frame(self);

        self.niri.active_keyboard_scroll = Some(scroll);
    }
}
