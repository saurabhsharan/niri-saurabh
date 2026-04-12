use niri_config::{Action, Config};
use niri_ipc::{ClickButton, WindowGeometry};
use smithay::utils::Point;
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::{ClientId, PointerButtonState, PointerEvent};
use super::*;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

// Build the smallest scene that can receive pointer events: one headless output and one mapped
// xdg-toplevel with a committed 100x100 buffer. Keeping this local avoids coupling these
// personal-fork tests to broader window-opening snapshot helpers.
fn set_up_window() -> (Fixture, ClientId, WlSurface) {
    let mut f = Fixture::with_config(Config::default());
    f.add_output(1, (1920, 1080));

    let id = f.add_client();
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(100, 100);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    (f, id, surface)
}

// Recompute the same global logical geometry that `niri msg focused-window` reports. The
// simulate-click action accepts this coordinate space, so tests use it to choose target points
// without hard-coding current layout placement details such as default gaps or centering.
fn focused_window_global_geometry(f: &mut Fixture) -> WindowGeometry {
    let niri = f.niri();
    let (_, output, _, layout) = niri
        .layout
        .find_window_with_ipc_layout(|mapped| mapped.is_focused())
        .unwrap();
    let output = output.unwrap();
    let output_geometry = niri.global_space.output_geometry(output).unwrap();
    let (tile_x, tile_y) = layout.tile_pos_in_workspace_view.unwrap();
    let (window_offset_x, window_offset_y) = layout.window_offset_in_tile;
    let (width, height) = layout.window_size;

    WindowGeometry {
        x: f64::from(output_geometry.loc.x) + tile_x + window_offset_x,
        y: f64::from(output_geometry.loc.y) + tile_y + window_offset_y,
        width,
        height,
    }
}

// Return the press/release positions in the client's chronological pointer event log. Keeping this
// as a helper makes each test name and comment focus on the implementation assumption it protects.
fn button_event_indices(events: &[PointerEvent], expected_button: u32) -> (usize, usize) {
    // Assumption: both direct helper use and Action dispatch eventually call Smithay's
    // `pointer.button()` with Linux evdev button codes. If this fails, first check whether the
    // helper still maps niri_ipc::ClickButton to the same button code or whether a future
    // Smithay/niri input abstraction changed the button code expected by clients.
    let press_idx = events
        .iter()
        .position(|event| {
            matches!(
                event,
                PointerEvent::Button {
                    button,
                    state: PointerButtonState::Pressed,
                } if *button == expected_button
            )
        })
        .expect("expected simulated click to emit the requested button press");

    // Assumption: every synthetic press has a matching synthetic release. A missing release leaves
    // clients believing the mouse button is still held, which is worse than a dropped click.
    // For the Action path, this also checks that the calloop timer actually fired.
    let release_idx = events
        .iter()
        .position(|event| {
            matches!(
                event,
                PointerEvent::Button {
                    button,
                    state: PointerButtonState::Released,
                } if *button == expected_button
            )
        })
        .expect("expected simulated click to emit a matching button release");

    (press_idx, release_idx)
}

#[test]
fn simulate_click_contract_motion_enter_precedes_buttons_and_uses_target_local_coords() {
    // simulate-click contract:
    // - global logical coordinates are converted to surface-local pointer coordinates
    // - the target surface receives wl_pointer.enter/motion before wl_pointer.button
    //
    // Niri/Smithay implementation assumption guarded here:
    // `State::simulate_click_press()` calls `contents_under()` and then `pointer.motion()` with
    // the target surface and its global location. If either detail changes, button events may be
    // delivered to the old pointer focus or with stale local coordinates.
    let (mut f, id, surface) = set_up_window();
    let geometry = focused_window_global_geometry(&mut f);
    let local_x = 37.;
    let local_y = 42.;
    let target = Point::from((geometry.x + local_x, geometry.y + local_y));

    f.client(id).state.pointer_events.clear();
    f.niri_state()
        .simulate_click_press(target, ClickButton::Left)
        .unwrap();
    f.niri_state().simulate_click_release(ClickButton::Left);
    f.roundtrip(id);

    let events = &f.client(id).state.pointer_events;

    // Assumption: `contents_under(target)` returns the actual client WlSurface under the global
    // logical target, not merely the focused window or tile. The enter coordinates prove that niri
    // passed Smithay the target surface plus the surface's global location, allowing Smithay to
    // convert global coordinates to surface-local coordinates correctly.
    //
    // If this fails after a rebase, inspect hit testing and surface-location computation first:
    // `Niri::contents_under()`, layout window offsets, subsurface handling, and any changes in
    // Smithay's `PointerHandle::motion()` focus tuple.
    let enter_idx = events
        .iter()
        .position(|event| {
            matches!(
                event,
                PointerEvent::Enter {
                    surface: entered,
                    x,
                    y,
                } if entered == &surface && *x == local_x && *y == local_y
            )
        })
        .expect("expected pointer enter on the clicked surface at target-local coordinates");

    // Assumption: enter alone is not enough for this fork's contract. Some clients/toolkits update
    // hover or widget hit-testing only on wl_pointer.motion, so simulate-click intentionally sends
    // an explicit same-position motion after establishing pointer focus.
    //
    // This test originally caught that Smithay's first `pointer.motion()` into a new surface emits
    // wl_pointer.enter with coordinates but not a separate wl_pointer.motion. If this assertion
    // fails, check whether the second same-position motion in `simulate_click_press()` was removed
    // or whether Smithay changed when it emits motion after enter.
    let motion_idx = events
        .iter()
        .position(|event| {
            matches!(
                event,
                PointerEvent::Motion { x, y } if *x == local_x && *y == local_y
            )
        })
        .expect("expected pointer motion at target-local coordinates before the click");

    let (press_idx, release_idx) = button_event_indices(events, BTN_LEFT);

    // Ordering assumption: clients must learn pointer focus before they receive motion. This is
    // mostly Smithay protocol sequencing, but a future refactor could accidentally bypass
    // `pointer.motion()` and synthesize only internal cursor state.
    assert!(
        enter_idx < motion_idx,
        "expected pointer enter before pointer motion; events: {events:#?}"
    );
    // Ordering assumption: the explicit target motion must be delivered before button press.
    // Without this, the button may be delivered with stale client-side hover/hit-test state, which
    // is exactly the failure mode this feature was added to avoid for OCR-driven clicks.
    assert!(
        motion_idx < press_idx,
        "expected pointer motion before button press; events: {events:#?}"
    );
    // Ordering assumption: the synthetic button sequence remains a normal click. If this fails,
    // check both direct `simulate_click_release()` callers and the timer-based Action path.
    assert!(
        press_idx < release_idx,
        "expected button press before button release; events: {events:#?}"
    );
}

#[test]
fn simulate_click_assumption_warp_updates_pointer_location_and_visible_pointer_state() {
    // simulate-click side-effect contract:
    // the compositor cursor is warped to the requested global logical coordinate and remains
    // there after release.
    //
    // Niri implementation assumption guarded here:
    // the action updates Smithay's pointer location through `pointer.motion()` and explicitly
    // treats the result as regular visible pointer movement rather than a hidden programmatic
    // focus warp.
    let (mut f, _id, _surface) = set_up_window();
    let geometry = focused_window_global_geometry(&mut f);
    let target = Point::from((geometry.x + 25., geometry.y + 30.));

    f.niri_state()
        .simulate_click_press(target, ClickButton::Left)
        .unwrap();
    f.niri_state().simulate_click_release(ClickButton::Left);

    let pointer_location = f.niri().seat.get_pointer().unwrap().current_location();
    // Assumption: `pointer.motion()` is the operation that actually warps Smithay's compositor
    // pointer location. If this fails while clients still receive events, a future Smithay change
    // may have decoupled event dispatch from `current_location()`, and rendering/cursor placement
    // code may need a separate update.
    assert_eq!(pointer_location, target);
    // Assumption: simulate-click is a visible user-like pointer movement, not a hidden
    // keyboard-focus warp. This protects the explicit `PointerVisibility::Visible` assignment in
    // `simulate_click_press()`.
    assert!(f.niri().pointer_visibility.is_visible());
    // Assumption: niri's cached pointer contents are updated to match the visible pointer
    // location. A stale `pointer_contents` can affect follow-up clicks, pointer constraints,
    // cursor shape refreshes, and focus/layer side effects.
    assert!(f.niri().pointer_contents.window.is_some());
}

#[test]
fn simulate_click_button_flag_contract_maps_buttons_to_evdev_codes() {
    // simulate-click button flag contract:
    // `--button left|right|middle` changes only the button code sent to clients; the motion/warp
    // path remains the same.
    //
    // Niri implementation assumption guarded here:
    // niri_ipc::ClickButton is mapped directly to Linux evdev BTN_LEFT/BTN_RIGHT/BTN_MIDDLE before
    // calling Smithay's `pointer.button()`. If future Niri introduces a richer mouse-button type,
    // this is the mapping to preserve for this personal fork.
    let cases = [
        (ClickButton::Left, BTN_LEFT),
        (ClickButton::Right, BTN_RIGHT),
        (ClickButton::Middle, BTN_MIDDLE),
    ];

    for (button, expected_button_code) in cases {
        let (mut f, id, _surface) = set_up_window();
        let geometry = focused_window_global_geometry(&mut f);
        let target = Point::from((geometry.x + 20., geometry.y + 20.));

        f.client(id).state.pointer_events.clear();
        f.niri_state().simulate_click_press(target, button).unwrap();
        f.niri_state().simulate_click_release(button);
        f.roundtrip(id);

        let events = &f.client(id).state.pointer_events;
        let (press_idx, release_idx) = button_event_indices(events, expected_button_code);

        // Assertion purpose: the requested logical button must be the exact evdev code clients see
        // in both press and release. This catches accidental fallbacks to left-click when adding
        // new CLI/IPC fields or rebasing the enum through niri_config::Action.
        assert!(
            press_idx < release_idx,
            "expected {button:?} press/release with evdev button {expected_button_code}; \
             events: {events:#?}"
        );
    }
}

#[test]
fn simulate_click_contract_rejects_invalid_targets_before_button_dispatch() {
    // simulate-click error contract:
    // bad targets fail before any button event can be sent.
    //
    // Niri implementation assumption guarded here:
    // `output_under()` and `contents_under()` are the authoritative checks for "inside an output"
    // and "a client surface exists under this point".
    let mut f = Fixture::with_config(Config::default());
    f.add_output(1, (1920, 1080));

    let err = f
        .niri_state()
        .simulate_click_press(Point::from((f64::NAN, 10.)), ClickButton::Left)
        .unwrap_err();
    // Assumption: invalid floating-point inputs are rejected before any hit test. This avoids
    // accidentally feeding NaN through geometry comparisons whose behavior may be surprising.
    assert!(
        err.contains("coordinates must be finite"),
        "unexpected error: {err}"
    );

    let err = f
        .niri_state()
        .simulate_click_press(Point::from((5000., 5000.)), ClickButton::Left)
        .unwrap_err();
    // Assumption: `Niri::output_under()` is the boundary check for global logical screen space.
    // If this starts passing, revisit whether output geometry, output scaling, or global-space
    // placement semantics changed upstream.
    assert!(
        err.contains("outside all outputs"),
        "unexpected error: {err}"
    );

    let err = f
        .niri_state()
        .simulate_click_press(Point::from((10., 10.)), ClickButton::Left)
        .unwrap_err();
    // Assumption: this feature intentionally clicks client surfaces only. A coordinate on the
    // desktop/background or decoration-only area must not dispatch a button to the previous
    // pointer focus. If this changes, document whether background clicks became an intentional
    // behavior.
    assert!(
        err.contains("no surface under point"),
        "unexpected error: {err}"
    );
}

#[test]
fn simulate_click_action_assumption_release_is_scheduled_on_calloop_timer() {
    // simulate-click action contract:
    // dispatching the niri_config action sends a press immediately and the matching release from
    // the 5 ms calloop timer. Use a middle click rather than the default left click so this also
    // verifies that the niri_config::Action path preserves the requested button across the timer
    // closure.
    //
    // Niri implementation assumption guarded here:
    // the bind/action path shares the same press helper as IPC, but does not wait synchronously
    // for release. If the timer scheduling changes, this test should catch missing or reordered
    // release events.
    let (mut f, id, _surface) = set_up_window();
    let geometry = focused_window_global_geometry(&mut f);
    let target_x = geometry.x + 50.;
    let target_y = geometry.y + 50.;

    f.client(id).state.pointer_events.clear();
    f.niri_state().do_action(
        Action::SimulateClick {
            x: target_x,
            y: target_y,
            button: ClickButton::Middle,
        },
        false,
    );
    f.state
        .server
        .event_loop
        .dispatch(
            std::time::Duration::from_millis(20),
            &mut f.state.server.state,
        )
        .unwrap();
    f.state.server.state.refresh_and_flush_clients();
    f.roundtrip(id);

    let events = &f.client(id).state.pointer_events;
    let (press_idx, release_idx) = button_event_indices(events, BTN_MIDDLE);
    // Assumption: the bind/action path must not return a press-only state. The release is delayed
    // by a calloop timer rather than emitted inline, so this assertion protects both timer
    // registration and the closure that calls `simulate_click_release()`.
    //
    // If this fails after a rebase, inspect changes around `State::do_action()`, calloop timer
    // source lifetimes, and whether tests still dispatch the same event loop that owns niri's
    // timer sources.
    assert!(
        press_idx < release_idx,
        "expected delayed release after press; events: {events:#?}"
    );
}
