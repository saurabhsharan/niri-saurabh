use smithay::input::pointer::{
    AxisFrame, ButtonEvent, CursorImageStatus, GestureHoldBeginEvent, GestureHoldEndEvent,
    GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
    GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData as PointerGrabStartData,
    MotionEvent, PointerGrab, PointerInnerHandle, RelativeMotionEvent,
};
use smithay::input::SeatHandler;
use smithay::output::Output;
use smithay::utils::{Logical, Point};

use crate::niri::State;
use crate::ui::pip::PipId;
use crate::utils::ResizeEdge;

pub enum PipGrabMode {
    Move { grab_offset: Point<f64, Logical> },
    Resize { edges: ResizeEdge },
}

pub struct PipPointerGrab {
    start_data: PointerGrabStartData<State>,
    pip_id: PipId,
    output: Output,
    mode: PipGrabMode,
}

impl PipPointerGrab {
    pub fn new_move(
        start_data: PointerGrabStartData<State>,
        pip_id: PipId,
        output: Output,
        grab_offset: Point<f64, Logical>,
    ) -> Self {
        Self {
            start_data,
            pip_id,
            output,
            mode: PipGrabMode::Move { grab_offset },
        }
    }

    pub fn new_resize(
        start_data: PointerGrabStartData<State>,
        pip_id: PipId,
        output: Output,
        edges: ResizeEdge,
    ) -> Self {
        Self {
            start_data,
            pip_id,
            output,
            mode: PipGrabMode::Resize { edges },
        }
    }

    fn queue_redraw_if_output_alive(&self, data: &mut State) {
        if data.niri.output_state.contains_key(&self.output) {
            data.niri.queue_redraw(&self.output);
        }
    }

    fn pos_within_output(
        &self,
        data: &State,
        location: Point<f64, Logical>,
    ) -> Option<Point<f64, Logical>> {
        let geo = data.niri.global_space.output_geometry(&self.output)?;
        Some(location - geo.loc.to_f64())
    }
}

impl PointerGrab<State> for PipPointerGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);

        if data.niri.pip_manager.find(self.pip_id).is_none()
            || !data.niri.output_state.contains_key(&self.output)
        {
            handle.unset_grab(self, data, event.serial, event.time, true);
            return;
        }

        let Some(pos_within_output) = self.pos_within_output(data, event.location) else {
            return;
        };

        let changed = match self.mode {
            PipGrabMode::Move { grab_offset } => data
                .niri
                .pip_manager
                .move_pip(self.pip_id, pos_within_output - grab_offset),
            PipGrabMode::Resize { edges } => {
                data.niri
                    .pip_manager
                    .resize_pip(self.pip_id, edges, pos_within_output)
            }
        };
        if changed.is_some() {
            self.queue_redraw_if_output_alive(data);
        }
    }

    fn relative_motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    fn button(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);

        if !handle.current_pressed().contains(&self.start_data.button) {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &PointerGrabStartData<State> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut State) {
        data.niri
            .cursor_manager
            .set_cursor_image(CursorImageStatus::default_named());
        self.queue_redraw_if_output_alive(data);
    }
}
