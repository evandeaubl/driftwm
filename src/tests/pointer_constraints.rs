//! Pointer lock coordinate space, and staying quiet while the lock holds.
//!
//! A locked client keeps its own cursor and tells the compositor where it is
//! with `set_cursor_position_hint`, in surface-local coordinates. Convert that
//! from the wrong origin, or hand the client an absolute motion it never made,
//! and a client reading absolute positions reads the difference as camera
//! movement.

use smithay::utils::{Logical, Point};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_locked_pointer_v1::ZwpLockedPointerV1;
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use super::client::{ClientId, LayerConfigureProps};
use super::input_backend::{FakeDevice, pointer_relative_motion, pointer_to};
use super::{Fixture, map_window, window_by_app_id};

const SHADOW: Point<i32, Logical> = Point::new(26, 23);
const GEOMETRY: (i32, i32) = (800, 600);

/// A mapped window whose surface reaches [`SHADOW`] beyond its geometry on
/// every side, like a client drawing its own shadows. Returns the client
/// surface and the canvas-space *surface* origin, which sits [`SHADOW`] above
/// and left of the geometry origin the stage positions the window by.
fn shadowed_window(f: &mut Fixture, id: ClientId) -> (WlSurface, Point<f64, Logical>) {
    let surface = map_window(
        f,
        id,
        "game",
        (
            (GEOMETRY.0 + SHADOW.x * 2) as u16,
            (GEOMETRY.1 + SHADOW.y * 2) as u16,
        ),
    );
    f.client(id)
        .window(&surface)
        .set_geometry(SHADOW.x, SHADOW.y, GEOMETRY.0, GEOMETRY.1);
    f.client(id).window(&surface).commit();
    f.double_roundtrip(id);

    let window = window_by_app_id(f, "game").unwrap();
    assert_eq!(
        window.geometry().loc,
        SHADOW,
        "the window must carry the shadow offset, or this scenario tests nothing"
    );
    let position = f.state().stage.position_of(&window).unwrap();
    (surface, (position - SHADOW).to_f64())
}

/// Put the pointer over the window's center and lock it there.
fn lock_pointer_over(f: &mut Fixture, id: ClientId, surface: &WlSurface) -> ZwpLockedPointerV1 {
    let window = window_by_app_id(f, "game").unwrap();
    let position = f.state().stage.position_of(&window).unwrap().to_f64();
    pointer_to(
        f,
        &FakeDevice::mouse(),
        position + Point::from((GEOMETRY.0 as f64 / 2.0, GEOMETRY.1 as f64 / 2.0)),
    );
    f.roundtrip(id);

    let lock = f.client(id).lock_pointer(surface);
    f.double_roundtrip(id);
    assert!(
        f.state().pointer_constraint_active(),
        "the lock must activate with the pointer over its surface, or this \
         scenario tests nothing"
    );
    lock
}

/// The hint is surface-local, so it must be measured from the surface origin,
/// not the geometry origin the stage positions the window by.
#[test]
fn cursor_position_hint_lands_at_the_surface_origin() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let (surface, surface_origin) = shadowed_window(&mut f, id);
    let lock = lock_pointer_over(&mut f, id, &surface);

    lock.set_cursor_position_hint(400.0, 300.0);
    f.client(id).window(&surface).commit();
    f.double_roundtrip(id);

    assert_eq!(
        f.state().seat.get_pointer().unwrap().current_location(),
        surface_origin + Point::from((400.0, 300.0)),
        "a surface-local hint must be measured from the surface origin"
    );
}

/// The pointer never moved — the client moved its own cursor — so re-creating
/// the lock must not replay the hinted position back as a `wl_pointer.motion`.
#[test]
fn relocking_does_not_replay_the_hint_as_motion() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let (surface, _) = shadowed_window(&mut f, id);
    let lock = lock_pointer_over(&mut f, id, &surface);

    lock.set_cursor_position_hint(400.0, 300.0);
    f.client(id).window(&surface).commit();
    f.double_roundtrip(id);

    f.client(id).state.pointer_positions.clear();
    lock.destroy();
    let _relock = f.client(id).lock_pointer(&surface);
    f.double_roundtrip(id);

    assert_eq!(
        f.client(id).state.pointer_positions,
        Vec::new(),
        "tearing the lock down and putting it back must not deliver absolute \
         motion — the pointer is frozen and the client moved its own cursor"
    );
}

/// A scene change away from the cursor must not re-seat pointer focus through
/// the locked surface: the re-seat carries an absolute motion the locked client
/// cannot tell from a real one.
#[test]
fn a_layer_dying_elsewhere_sends_the_locked_client_nothing() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let (surface, _) = shadowed_window(&mut f, id);
    let _lock = lock_pointer_over(&mut f, id, &surface);

    let notification =
        f.client(id)
            .create_layer(None, zwlr_layer_shell_v1::Layer::Top, "notification");
    let layer_surface = notification.surface.clone();
    notification.set_configure_props(LayerConfigureProps {
        size: Some((300, 100)),
        anchor: Some(zwlr_layer_surface_v1::Anchor::Top),
        exclusive_zone: Some(0),
        ..Default::default()
    });
    notification.commit();
    f.roundtrip(id);
    let notification = f.client(id).layer(&layer_surface);
    notification.set_size(300, 100);
    notification.attach_new_buffer();
    notification.ack_last_and_commit();
    f.double_roundtrip(id);

    f.client(id).state.pointer_positions.clear();
    f.client(id).layer(&layer_surface).layer_surface.destroy();
    f.client(id).layer(&layer_surface).surface.destroy();
    f.double_roundtrip(id);

    assert_eq!(
        f.client(id).state.pointer_positions,
        Vec::new(),
        "a layer teardown elsewhere on screen must not reach a locked client"
    );
}

/// Fullscreening a window that already holds the cursor lock must leave the
/// lock alone: dropping and re-arming it sends an absolute motion.
#[test]
fn fullscreening_a_locked_game_leaves_its_lock_untouched() {
    let mut f = Fixture::new();
    let output = f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let (surface, _) = shadowed_window(&mut f, id);
    let _lock = lock_pointer_over(&mut f, id, &surface);

    let frozen = f.state().seat.get_pointer().unwrap().current_location();
    f.client(id).state.pointer_positions.clear();

    let window = window_by_app_id(&mut f, "game").unwrap();
    f.state().enter_fullscreen(&window, Some(output));
    f.double_roundtrip(id);

    assert!(
        f.state().pointer_constraint_active(),
        "the game's lock must survive the fullscreen entry"
    );
    assert_eq!(
        f.state().seat.get_pointer().unwrap().current_location(),
        frozen,
        "a locked pointer must not be moved by the fullscreen entry"
    );
    assert_eq!(
        f.client(id).state.pointer_positions,
        Vec::new(),
        "fullscreening must not hand the locked client an absolute jump"
    );
}

#[test]
fn a_locked_pointer_neither_moves_nor_reports_absolute_motion() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let (surface, _) = shadowed_window(&mut f, id);
    let _lock = lock_pointer_over(&mut f, id, &surface);

    let frozen = f.state().seat.get_pointer().unwrap().current_location();
    f.client(id).state.pointer_positions.clear();

    pointer_relative_motion(&mut f, &FakeDevice::mouse(), Point::from((40.0, 25.0)));
    f.double_roundtrip(id);

    assert_eq!(
        f.state().seat.get_pointer().unwrap().current_location(),
        frozen,
        "a locked pointer must not move"
    );
    assert_eq!(
        f.client(id).state.pointer_positions,
        Vec::new(),
        "a locked client must see relative motion only"
    );
}
