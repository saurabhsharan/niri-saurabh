use smithay::output::{Mode, Output};
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;
use smithay::utils::{Logical, Point, Size};
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::{ClientId, LayerConfigureProps};
use super::*;
use crate::layout::LayoutElement as _;
use crate::render_helpers::{RenderCtx, RenderTarget};
use crate::utils::output_size;
use crate::window::mapped::MappedId;

fn set_up_window(
    outputs: &[(u8, (u16, u16))],
    window_size: (u16, u16),
) -> (Fixture, ClientId, WlSurface, MappedId) {
    let mut f = Fixture::new();
    for (n, size) in outputs {
        f.add_output(*n, *size);
    }

    let id = f.add_client();
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(window_size.0, window_size.1);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    let mapped_id = f.niri().layout.windows().next().unwrap().1.id();
    (f, id, surface, mapped_id)
}

fn add_pip(f: &mut Fixture, mapped_id: MappedId, output: &Output) {
    let source_size = {
        let niri = f.niri();
        niri.layout
            .windows()
            .find(|(_, mapped)| mapped.id() == mapped_id)
            .unwrap()
            .1
            .size()
    };

    let redraws = f
        .niri()
        .pip_manager
        .toggle_pip_for_source(mapped_id, source_size, output);
    assert_eq!(redraws, vec![output.clone()]);
}

fn pip_thumbnail(f: &mut Fixture, mapped_id: MappedId) -> crate::ui::pip::PipThumbnail {
    f.niri()
        .pip_manager
        .find_by_source(mapped_id)
        .unwrap()
        .clone()
}

fn pip_center_global(f: &mut Fixture, mapped_id: MappedId) -> Point<f64, Logical> {
    let pip = pip_thumbnail(f, mapped_id);
    let output_loc = f
        .niri()
        .global_space
        .output_geometry(&pip.output)
        .unwrap()
        .loc
        .to_f64();
    output_loc + pip.position + pip.size.downscale(2.).to_point()
}

fn map_fullscreen_overlay_layer(f: &mut Fixture, id: ClientId) -> WlSurface {
    let layer = f.client(id).create_layer(None, Layer::Overlay, "");
    let surface = layer.surface.clone();
    layer.set_configure_props(LayerConfigureProps {
        anchor: Some(Anchor::Left | Anchor::Right | Anchor::Top | Anchor::Bottom),
        size: Some((0, 0)),
        ..Default::default()
    });
    layer.commit();
    f.roundtrip(id);

    let layer = f.client(id).layer(&surface);
    let (_, configure) = layer.configures_received.last().unwrap();
    layer.attach_new_buffer();
    layer.set_size(configure.size.0 as u16, configure.size.1 as u16);
    layer.ack_last_and_commit();
    f.double_roundtrip(id);

    surface
}

fn render_output_smoke(f: &mut Fixture, output: &Output) -> usize {
    let output = output.clone();
    let state = f.niri_state();
    let niri = &state.niri;
    let backend = &mut state.backend;
    backend
        .with_primary_renderer(|renderer| {
            let ctx = RenderCtx {
                renderer,
                target: RenderTarget::Output,
                xray: None,
            };
            niri.render_to_vec(ctx, &output, false).len()
        })
        .unwrap()
}

// Verifies that a visible PiP blocks normal window hit-testing.
// Assumption guarded: render/input ordering keeps PiP above regular layout content.
#[test]
fn pip_occludes_window_hits_when_visible() {
    let (mut f, _id, _surface, mapped_id) = set_up_window(&[(1, (1920, 1080))], (100, 100));
    let output = f.niri_output(1);
    add_pip(&mut f, mapped_id, &output);

    let point = pip_center_global(&mut f, mapped_id);

    assert!(f.niri().window_under(point).is_none());

    let contents = f.niri().contents_under(point);
    assert_eq!(contents.output.as_ref(), Some(&output));
    assert!(contents.surface.is_none());
    assert!(contents.window.is_none());
    assert!(contents.layer.is_none());
    assert!(!contents.hot_corner);
}

// Verifies that overlay-layer surfaces still win hit-testing above PiP.
// Assumption guarded: overlay-layer ordering stays above PiP in both rendering and input paths.
#[test]
fn overlay_layer_beats_pip_hit_testing() {
    let (mut f, _id, _surface, mapped_id) = set_up_window(&[(1, (1920, 1080))], (100, 100));
    let output = f.niri_output(1);
    add_pip(&mut f, mapped_id, &output);

    let layer_client = f.add_client();
    let _surface = map_fullscreen_overlay_layer(&mut f, layer_client);

    let point = pip_center_global(&mut f, mapped_id);
    let contents = f.niri().contents_under(point);
    assert!(contents.layer.is_some());
    assert!(contents.window.is_none());
}

// Verifies that PiP state is cleaned up when the source window unmaps.
// Assumption guarded: mapped-window unmap hooks continue removing PiP entries.
#[test]
fn pip_removed_when_source_unmaps() {
    let (mut f, id, surface, mapped_id) = set_up_window(&[(1, (1920, 1080))], (100, 100));
    let output = f.niri_output(1);
    add_pip(&mut f, mapped_id, &output);

    let window = f.client(id).window(&surface);
    window.attach_null();
    window.commit();
    f.double_roundtrip(id);

    assert!(f.niri().pip_manager.find_by_source(mapped_id).is_none());
}

// Verifies that removing an output also removes any PiPs assigned to it.
// Assumption guarded: output teardown continues cleaning up PiP overlay state.
#[test]
fn pip_removed_when_output_is_removed() {
    let (mut f, _id, _surface, mapped_id) =
        set_up_window(&[(1, (1920, 1080)), (2, (1280, 720))], (100, 100));
    let pip_output = f.niri_output(2);
    add_pip(&mut f, mapped_id, &pip_output);

    f.niri().remove_output(&pip_output);

    assert!(f.niri().pip_manager.find_by_source(mapped_id).is_none());
}

// Verifies that output resize keeps existing PiPs inside the output bounds.
// Assumption guarded: output resize handling continues clamping PiP geometry after layout changes.
#[test]
fn pip_is_clamped_after_output_resize() {
    let (mut f, _id, _surface, mapped_id) = set_up_window(&[(1, (1920, 1080))], (100, 100));
    let output = f.niri_output(1);
    add_pip(&mut f, mapped_id, &output);

    let mode = Mode {
        size: Size::from((1280, 720)),
        refresh: 60_000,
    };
    output.change_current_state(Some(mode), None, None, None);
    output.set_preferred(mode);
    f.niri().output_resized(&output);

    let pip = pip_thumbnail(&mut f, mapped_id);
    let output_size = output_size(&output);
    let eps = 1e-6;
    assert!(pip.position.x >= -eps);
    assert!(pip.position.y >= -eps);
    assert!(pip.position.x + pip.size.w <= output_size.w + eps);
    assert!(pip.position.y + pip.size.h <= output_size.h + eps);
}

// Verifies that source commits still refresh the PiP's cached source size metadata.
// Assumption guarded: mapped-window commit hooks continue propagating size changes to PiP state.
#[test]
fn source_commit_refreshes_pip_source_size() {
    let (mut f, id, surface, mapped_id) = set_up_window(&[(1, (1920, 1080))], (100, 100));
    let output = f.niri_output(1);
    add_pip(&mut f, mapped_id, &output);

    let window = f.client(id).window(&surface);
    window.set_size(200, 150);
    window.commit();
    f.double_roundtrip(id);

    let pip = pip_thumbnail(&mut f, mapped_id);
    assert_eq!(pip.source_size, Size::from((200., 150.)));
}

// Verifies that the core render path survives PiP lifecycle changes without panicking.
// Assumption guarded: PiP render elements remain compatible with headless rendering and teardown.
#[test]
fn render_smoke_with_pip_survives_lifecycle_changes() {
    let (mut f, id, surface, mapped_id) = set_up_window(&[(1, (1920, 1080))], (100, 100));
    f.niri_state().backend.headless().add_renderer().unwrap();

    let output = f.niri_output(1);
    add_pip(&mut f, mapped_id, &output);
    assert!(render_output_smoke(&mut f, &output) > 0);

    let layer_client = f.add_client();
    let _layer_surface = map_fullscreen_overlay_layer(&mut f, layer_client);
    assert!(render_output_smoke(&mut f, &output) > 0);

    let window = f.client(id).window(&surface);
    window.set_size(220, 160);
    window.commit();
    f.double_roundtrip(id);
    assert!(render_output_smoke(&mut f, &output) > 0);

    let resized_mode = Mode {
        size: Size::from((1280, 720)),
        refresh: 60_000,
    };
    output.change_current_state(Some(resized_mode), None, None, None);
    output.set_preferred(resized_mode);
    f.niri().output_resized(&output);
    assert!(render_output_smoke(&mut f, &output) > 0);

    let window = f.client(id).window(&surface);
    window.attach_null();
    window.commit();
    f.double_roundtrip(id);
    assert!(render_output_smoke(&mut f, &output) > 0);
}
