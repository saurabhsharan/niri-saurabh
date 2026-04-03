use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use smithay::backend::input::{Axis, AxisRelativeDirection, AxisSource};
use smithay::input::pointer::AxisFrame;

use crate::niri::State;
use crate::utils::get_monotonic_time;

const KEYBOARD_SCROLL_INTERVAL: Duration = Duration::from_nanos(8_333_333);
pub const DEFAULT_KEYBOARD_SCROLL_PIXELS_PER_SECOND: f64 = 150.;

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

#[derive(Debug, Clone, Copy)]
pub struct ActiveKeyboardScroll {
    pub direction: KeyboardScrollDirection,
    pub speed: f64,
    pub last_tick_time: Duration,
}

impl State {
    pub(super) fn start_keyboard_scroll(&mut self, direction: KeyboardScrollDirection, speed: f64) {
        self.niri.active_keyboard_scroll = Some(ActiveKeyboardScroll {
            direction,
            speed,
            // Prime the first immediate tick with one interval's worth of scroll.
            last_tick_time: get_monotonic_time().saturating_sub(KEYBOARD_SCROLL_INTERVAL),
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
                    TimeoutAction::Drop
                }
            })
            .unwrap();

        self.niri.keyboard_scroll_timer = Some(token);
    }

    pub(super) fn stop_keyboard_scroll(&mut self) {
        if self.niri.active_keyboard_scroll.is_none() {
            return;
        }

        if let Some(token) = self.niri.keyboard_scroll_timer.take() {
            self.niri.event_loop.remove(token);
        }

        self.niri.active_keyboard_scroll = None;
    }

    fn tick_keyboard_scroll(&mut self) {
        let Some(mut scroll) = self.niri.active_keyboard_scroll.take() else {
            return;
        };

        let now = get_monotonic_time();
        let delta = now.saturating_sub(scroll.last_tick_time);
        scroll.last_tick_time = now;

        if delta.is_zero() {
            self.niri.active_keyboard_scroll = Some(scroll);
            return;
        }

        let amount = scroll.direction.amount(scroll.speed * delta.as_secs_f64());

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
