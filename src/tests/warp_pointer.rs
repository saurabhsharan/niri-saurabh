use niri_config::{Action, Config};
use niri_ipc::WindowGeometry;
use smithay::utils::Point;
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::{ClientId, PointerEvent};
use super::*;

// Build the smallest scene that can receive pointer motion events: one headless output and one
// mapped xdg-toplevel with a committed 100x100 buffer. This intentionally mirrors the
// simulate-click tests while staying local to this file, so future upstream rebases are less
// likely to turn these personal-fork tests into a shared helper merge conflict.
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

// Recompute the same global logical geometry reported by focused-window. The warp-pointer action
// accepts global logical screen coordinates, so tests derive target points from the real layout
// instead of relying on placement constants that could shift after an upstream rebase.
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

#[test]
fn warp_pointer_contract_emit_motion_true_enters_and_motions_without_buttons() {
    // warp-pointer eventful contract:
    // - the compositor cursor moves to the requested global logical coordinate
    // - the target surface receives wl_pointer.enter/motion
    // - no button events are synthesized
    //
    // Niri/Smithay implementation assumption guarded here:
    // `State::warp_pointer(..., true)` uses the same pointer.motion-based focus path as regular
    // pointer movement. If this fails after a rebase, inspect Smithay PointerHandle::motion focus
    // behavior and niri's `contents_under()` surface-location tuple first.
    let (mut f, id, surface) = set_up_window();
    let geometry = focused_window_global_geometry(&mut f);
    let local_x = 37.;
    let local_y = 42.;
    let target = Point::from((geometry.x + local_x, geometry.y + local_y));

    f.client(id).state.pointer_events.clear();
    f.niri_state().warp_pointer(target, true).unwrap();
    f.roundtrip(id);

    let events = &f.client(id).state.pointer_events;

    // Assumption: the first `pointer.motion()` into a new surface establishes wl_pointer focus
    // and converts the global warp point to surface-local coordinates. If these coordinates stop
    // matching, check output scale/global-space changes before changing the test.
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
        .expect("expected eventful warp to enter the target surface");

    // Assumption: eventful warp emits an explicit wl_pointer.motion in addition to enter. Smithay
    // historically emits enter, but not motion, for the first motion into a newly focused surface;
    // `warp_pointer(..., true)` sends a same-position second motion to make this contract stable
    // for clients that update hover/hit-testing only on motion.
    let motion_idx = events
        .iter()
        .position(|event| {
            matches!(
                event,
                PointerEvent::Motion { x, y } if *x == local_x && *y == local_y
            )
        })
        .expect("expected eventful warp to emit target-local pointer motion");

    // Assertion purpose: warp-pointer must not become a partial click during future refactors.
    // Any button event here means the action leaked simulate-click behavior into a pure cursor
    // movement operation.
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PointerEvent::Button { .. })),
        "expected warp-pointer to emit no button events; events: {events:#?}"
    );
    // Ordering assumption: clients receive focus before motion. If this flips, clients may reject
    // or ignore the motion because they do not yet own pointer focus.
    assert!(
        enter_idx < motion_idx,
        "expected pointer enter before pointer motion; events: {events:#?}"
    );
    // Assumption: eventful warp updates Smithay's compositor pointer location through
    // `pointer.motion()`, so rendering and follow-up input use the warped location.
    assert_eq!(
        f.niri().seat.get_pointer().unwrap().current_location(),
        target
    );
    // Assumption: this is visible user-like pointer movement, not a hidden keyboard focus warp.
    assert!(f.niri().pointer_visibility.is_visible());
    // Assumption: niri's cached pointer contents track the new visible pointer position when
    // motion is emitted. This matters for follow-up cursor-shape, constraint, and focus logic.
    assert!(f.niri().pointer_contents.window.is_some());
}

#[test]
fn warp_pointer_contract_emit_motion_false_silently_moves_visible_cursor() {
    // warp-pointer silent contract:
    // `emit_motion = false` updates the compositor cursor location but does not emit Wayland
    // pointer enter/motion/leave/button events and does not update niri's cached pointer focus.
    //
    // Niri/Smithay implementation assumption guarded here:
    // this path must keep using Smithay PointerHandle::set_location(), whose documented behavior is
    // to update current_location without sending events or changing focus. Replacing it with
    // pointer.motion() would make this test fail by delivering client events.
    let (mut f, id, _surface) = set_up_window();
    let geometry = focused_window_global_geometry(&mut f);
    let target = Point::from((geometry.x + 25., geometry.y + 30.));
    let old_pointer_contents = f.niri().pointer_contents.clone();

    f.client(id).state.pointer_events.clear();
    f.niri_state().warp_pointer(target, false).unwrap();
    f.roundtrip(id);

    let events = &f.client(id).state.pointer_events;
    // Assertion purpose: disabling motion means no Wayland pointer events at all. This protects
    // clients that want to move the compositor cursor as state, without changing toolkit hover
    // state, pointer focus, or triggering enter/leave handlers.
    assert!(
        events.is_empty(),
        "expected silent warp to emit no pointer events; events: {events:#?}"
    );
    // Assumption: Smithay `set_location()` updates the compositor pointer location even though no
    // client event was sent. If this fails, upstream may have changed set_location semantics and
    // this feature will need a new no-event cursor-position primitive.
    assert_eq!(
        f.niri().seat.get_pointer().unwrap().current_location(),
        target
    );
    // Assumption: the visible cursor is still shown after a silent warp. This keeps the action's
    // cursor side effect consistent with the eventful warp path.
    assert!(f.niri().pointer_visibility.is_visible());
    // Assumption: silent warp does not update niri's cached pointer contents. Updating this cache
    // without matching Wayland enter/leave/motion would desynchronize niri's internal pointer
    // focus model from clients, making later pointer side effects harder to reason about.
    assert!(f.niri().pointer_contents == old_pointer_contents);
}

#[test]
fn warp_pointer_contract_allows_output_background_targets() {
    // warp-pointer target contract:
    // unlike simulate-click, cursor warping only requires a valid output coordinate. A client
    // surface under the target is optional because there may be useful scripts that park the
    // cursor over the desktop background or a decoration-only area.
    let mut f = Fixture::with_config(Config::default());
    f.add_output(1, (1920, 1080));
    let target = Point::from((10., 10.));

    f.niri_state().warp_pointer(target, true).unwrap();
    // Assumption: an eventful warp to the background uses pointer.motion(None), so it updates the
    // compositor pointer location while clearing/keeping empty pointer contents instead of
    // incorrectly dispatching to a stale client focus.
    assert_eq!(
        f.niri().seat.get_pointer().unwrap().current_location(),
        target
    );
    assert!(f.niri().pointer_contents.window.is_none());

    let silent_target = Point::from((20., 20.));
    f.niri_state().warp_pointer(silent_target, false).unwrap();
    // Assumption: the silent path has the same output-bounds acceptance as the eventful path; the
    // only behavior difference is whether Wayland pointer events/focus updates are emitted.
    assert_eq!(
        f.niri().seat.get_pointer().unwrap().current_location(),
        silent_target
    );
}

#[test]
fn warp_pointer_contract_rejects_invalid_targets() {
    // warp-pointer error contract:
    // invalid coordinates fail before Smithay pointer state is touched, and coordinates must be
    // inside some output in global logical screen space.
    let mut f = Fixture::with_config(Config::default());
    f.add_output(1, (1920, 1080));

    let err = f
        .niri_state()
        .warp_pointer(Point::from((f64::NAN, 10.)), true)
        .unwrap_err();
    // Assumption: NaN/inf are rejected before hit testing or set_location. Geometry predicates can
    // behave surprisingly with non-finite floats, so failures should stay explicit here.
    assert!(
        err.contains("coordinates must be finite"),
        "unexpected error: {err}"
    );

    let err = f
        .niri_state()
        .warp_pointer(Point::from((5000., 5000.)), false)
        .unwrap_err();
    // Assumption: `Niri::output_under()` remains the action's boundary check for global logical
    // coordinates. If this starts passing, revisit output placement/scaling semantics before
    // loosening the action.
    assert!(
        err.contains("outside all outputs"),
        "unexpected error: {err}"
    );
}

#[test]
fn warp_pointer_action_assumption_dispatch_preserves_emit_motion_false() {
    // niri_config::Action dispatch contract:
    // the action path preserves `emit_motion = false` and calls the silent helper path. This is the
    // path used after IPC requests are converted into niri_config::Action for non-special cases.
    let (mut f, id, _surface) = set_up_window();
    let geometry = focused_window_global_geometry(&mut f);
    let target_x = geometry.x + 50.;
    let target_y = geometry.y + 50.;
    let target = Point::from((target_x, target_y));

    f.client(id).state.pointer_events.clear();
    f.niri_state().do_action(
        Action::WarpPointer {
            x: target_x,
            y: target_y,
            emit_motion: false,
        },
        false,
    );
    f.roundtrip(id);

    let events = &f.client(id).state.pointer_events;
    // Assumption: action dispatch does not accidentally fall back to the default eventful behavior
    // while crossing the niri_ipc -> niri_config -> State::do_action boundary.
    assert!(
        events.is_empty(),
        "expected Action::WarpPointer with emit_motion=false to emit no pointer events; \
         events: {events:#?}"
    );
    assert_eq!(
        f.niri().seat.get_pointer().unwrap().current_location(),
        target
    );
}
