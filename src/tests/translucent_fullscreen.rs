//! A fullscreen window carrying a rule `opacity` below 1.0 is drawn see-through,
//! so the canvas behind it must keep rendering. `fullscreen_conceals_canvas` is
//! the predicate that decides it, and it is deliberately *narrower* than
//! `is_output_visually_fullscreen` rather than a widening of it: the layer
//! buckets, the other windows and the pinned ones keep culling on coverage, and
//! only the background, the canvas layers and the outlines answer to
//! concealment.
//!
//! Backend is `None`, so nothing here composes a frame. Every scenario asserts
//! the predicate the composer gates those three buckets on.

use driftwm::config::Action;
use driftwm::desktop_entry::DesktopEntryCache;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::{Point, SERIAL_COUNTER};

use crate::ipc::dispatch;
use crate::ipc::protocol::{Request, Response};
use crate::state::StageWindow;

use super::client::ClientId;
use super::real::TempDir;
use super::{Fixture, config, map_window, tick_until_settled, window_by_app_id};

/// A `[[window_rules]]` block seeding app_id `fs` with `opacity`, or no rule at
/// all when `opacity` is `None`.
fn config_with_opacity(opacity: Option<f64>) -> driftwm::config::Config {
    match opacity {
        Some(value) => config(&format!(
            "[[window_rules]]\napp_id = \"fs\"\nopacity = {value}\n"
        )),
        None => config(""),
    }
}

/// A window mapped at `size`, driven fullscreen by the client and settled, on a
/// fresh `1920x1080` output at zoom 1.
fn settled_fullscreen(
    f: &mut Fixture,
    size: (u16, u16),
) -> (
    ClientId,
    wayland_client::protocol::wl_surface::WlSurface,
    Output,
    Window,
) {
    let output = f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let surface = map_window(f, id, "fs", size);
    let window = window_by_app_id(f, "fs").unwrap();
    f.client(id).window(&surface).set_fullscreen(None);
    f.double_roundtrip(id);
    super::adopt_last_configure(f, id, &surface);
    tick_until_settled(f);
    (id, surface, output, window)
}

/// The headline: a rule opacity below 1.0 stops the fullscreen picture
/// concealing the canvas, while the output stays *covered*. Both halves matter —
/// the second is what catches a fix that widened the coverage predicate instead
/// of adding a narrower one beside it, which would pop the panels and every
/// other window back in over the fullscreen frame.
#[test]
fn a_translucent_fullscreen_window_does_not_conceal_the_canvas() {
    let mut f = Fixture::with_config(config_with_opacity(Some(0.5)));
    let (_id, _surface, output, _window) = settled_fullscreen(&mut f, (800, 600));

    assert!(
        f.state().is_output_visually_fullscreen(&output),
        "the picture still covers the output — only the canvas stops being culled"
    );
    assert!(
        !f.state().fullscreen_conceals_canvas(&output),
        "a translucent fullscreen window has to leave the canvas drawn behind it"
    );

    f.state().exit_fullscreen_on(&output);
}

/// The control, both ways an opaque window reads as opaque: an explicit
/// `opacity = 1.0` rule, and no rule at all (the `unwrap_or(1.0)` path).
#[test]
fn an_opaque_fullscreen_window_conceals_the_canvas() {
    for rule in [Some(1.0), None] {
        let mut f = Fixture::with_config(config_with_opacity(rule));
        let (_id, _surface, output, _window) = settled_fullscreen(&mut f, (800, 600));

        assert!(
            f.state().fullscreen_conceals_canvas(&output),
            "an opaque fullscreen window conceals the canvas (rule {rule:?})"
        );

        f.state().exit_fullscreen_on(&output);
    }
}

/// Under-fill is not a trigger. A client that answers the fullscreen configure
/// smaller than the output is centred in it and leaves a band of clear colour
/// around itself — but while it is opaque it still conceals, so that band stays
/// black rather than turning into a window-shaped hole onto the canvas. Inverse
/// of `a_smaller_fullscreen_commit_is_centred_and_still_reads_as_covering_the_output`:
/// fold under-fill into the predicate and this one flips.
#[test]
fn an_under_filling_fullscreen_window_still_conceals_the_canvas() {
    let mut f = Fixture::with_config(config_with_opacity(None));
    let output = f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let surface = map_window(&mut f, id, "fs", (700, 500));
    let window = window_by_app_id(&mut f, "fs").unwrap();
    f.state().enter_fullscreen(&window, Some(output.clone()));
    f.double_roundtrip(id);
    let camera = f.state().camera().to_i32_round();

    // Ack the fullscreen offer at a smaller size than the one configured, the
    // way a fixed-aspect-ratio client does.
    f.double_roundtrip(id);
    f.client(id).window(&surface).set_size(800, 600);
    f.client(id).window(&surface).attach_new_buffer();
    f.client(id).window(&surface).ack_last_and_commit();
    f.double_roundtrip(id);
    tick_until_settled(&mut f);

    let position = f.state().stage.position_of(&window).expect("staged");
    assert_eq!(
        position - camera,
        Point::from((560, 240)),
        "precondition: the window really under-fills, centred in the output"
    );
    assert!(
        f.state().fullscreen_conceals_canvas(&output),
        "under-fill alone must never uncover the canvas — the band around an \
         opaque window stays the clear colour"
    );

    f.state().exit_fullscreen_on(&output);
}

/// The runtime path: the `opacity` IPC verb writes the same applied rule the
/// predicate reads, so the answer changes on the spot — no animation tick, no
/// stage change, no re-entry into fullscreen.
#[test]
fn setting_opacity_over_ipc_stops_concealing_the_canvas() {
    let mut f = Fixture::with_config(config_with_opacity(None));
    let (_id, _surface, output, _window) = settled_fullscreen(&mut f, (800, 600));
    assert!(
        f.state().fullscreen_conceals_canvas(&output),
        "precondition: it starts opaque, so it conceals"
    );

    let set = dispatch(
        Request::Opacity {
            window: None,
            value: Some(0.5),
        },
        f.state(),
    );
    assert_eq!(set, Ok(Response::Opacity(0.5)));

    assert!(
        f.state().is_output_visually_fullscreen(&output),
        "the picture is unchanged — it still covers the output"
    );
    assert!(
        !f.state().fullscreen_conceals_canvas(&output),
        "but the canvas has to come back the moment the value is written"
    );

    f.state().exit_fullscreen_on(&output);
}

/// The transition half. A fullscreen exit lets go of stage membership at the
/// action but holds the fullscreen picture on screen for the length of its
/// freeze, and the rule has to hold across that: the output stays *covered* so
/// the panels do not pop back in, while the canvas it is see-through onto stays
/// drawn.
///
/// The coverage assertion is the one that fails if the freeze is ever taught to
/// stop claiming coverage for a translucent picture — a change that would
/// uncover every bucket, not just the canvas.
#[test]
fn a_frozen_exit_of_a_translucent_fullscreen_window_still_covers_but_conceals_nothing() {
    let mut f = Fixture::with_config(config_with_opacity(Some(0.5)));
    let (id, surface, output, window) = settled_fullscreen(&mut f, (800, 600));
    let eid = f.state().stage.id_of(&window).expect("staged");

    f.state().exit_fullscreen_on(&output);
    f.double_roundtrip(id);
    assert!(
        f.state().window_animations.start_held(eid),
        "precondition: the exit is frozen, waiting for the client's windowed redraw"
    );

    assert!(
        f.state().is_output_visually_fullscreen(&output),
        "the frozen picture has not moved, so the output stays covered"
    );
    assert_eq!(
        f.state().visually_fullscreen_windows_on(&output),
        vec![window.clone()],
        "and the window on its way out is the one drawing it"
    );
    assert!(
        !f.state().fullscreen_conceals_canvas(&output),
        "and it is still translucent, so the canvas stays drawn under it"
    );

    super::adopt_last_configure(&mut f, id, &surface);
    tick_until_settled(&mut f);
}

/// A cover with no client window behind it — a suspended stand-in — has no
/// surface, so no rule to carry an opacity, and it is drawn fully opaque. The
/// window list is empty there and `all` on an empty list answers `true`, which
/// is the right answer for accident-shaped reasons; pin it.
///
/// The state is seated on the stage directly because no production path reaches
/// it today: `convert_to_suspended` drops the outgoing window's animation entry,
/// so the exit freeze that would have named the stand-in is already gone by the
/// time the stand-in exists, and `Stage::set_fullscreen`'s one production caller
/// takes a live `Window`. Either of those changing makes this reachable, and
/// this is the assertion saying what it must answer then.
#[test]
fn a_stand_in_fullscreen_cover_still_conceals_the_canvas() {
    let tmp = TempDir::new();
    let mut f = Fixture::with_config(config_with_opacity(Some(0.5)));
    let output = f.add_output(1, (1920, 1080));
    std::fs::write(
        tmp.path().join("fs.desktop"),
        "[Desktop Entry]\nType=Application\nName=fs\nExec=fs\n",
    )
    .unwrap();
    f.state().desktop_entry_cache = Some(DesktopEntryCache::new(vec![tmp.path().to_path_buf()]));

    let id = f.add_client();
    let surface = map_window(&mut f, id, "fs", (800, 600));
    let window = window_by_app_id(&mut f, "fs").unwrap();
    let serial = SERIAL_COUNTER.next_serial();
    f.state().raise_and_focus(&window, serial);
    f.state().execute_action(&Action::SuspendWindow);
    f.client(id).window(&surface).destroy();
    f.roundtrip(id);
    f.dispatch();

    let stand_in = f
        .state()
        .stage
        .windows()
        .find_map(|w| w.suspended().cloned())
        .expect("the window converted to a stand-in");
    let sid = stand_in.id;
    let element = StageWindow::Suspended(stand_in);
    let size = element.geometry().size;
    let position = f.state().stage.position_of(&element).expect("staged");
    f.state()
        .stage
        .set_fullscreen(&output.name(), element, position, size);
    // The coverage claim is "the viewport still sits where the entry parked it",
    // so park it on the stand-in's own position rather than a canvas constant.
    f.state().with_output_state(|os| {
        os.camera = position.to_f64();
        os.zoom = 1.0;
    });
    f.state().update_output_from_camera();

    assert!(
        f.state().is_output_visually_fullscreen(&output),
        "precondition: the stand-in's picture covers the output"
    );
    assert!(
        f.state().visually_fullscreen_windows_on(&output).is_empty(),
        "precondition: a stand-in has no client window, so the list is empty"
    );
    assert!(
        f.state().fullscreen_conceals_canvas(&output),
        "a stand-in is drawn opaque, so its cover conceals the canvas"
    );

    f.state().stage.take_fullscreen(&output.name());
    f.state().dismiss_suspended(sid);
    tick_until_settled(&mut f);
}

/// The animated background has to keep ticking on an output showing its canvas
/// through a translucent fullscreen window: this filter gates the idle
/// due-check, the udev tick-timer arming and the per-frame dirty marking alike,
/// so dropping the output here draws the wallpaper frozen rather than not at
/// all. Turning the opacity back up drops it out again, so eligibility really
/// tracks the rule rather than having stopped filtering at all.
#[test]
fn a_translucent_fullscreen_output_stays_background_render_eligible() {
    let mut f = Fixture::with_config(config_with_opacity(Some(0.5)));
    let (_id, _surface, output, _window) = settled_fullscreen(&mut f, (800, 600));
    // `active_outputs` is the udev backend's set; the fixture has no backend, so
    // seat it by hand or the filter has nothing to filter.
    f.state().active_outputs.insert(output.clone());

    assert!(
        f.state()
            .background_render_eligible_outputs()
            .any(|o| *o == output),
        "the output still renders its background while the window is see-through"
    );

    let set = dispatch(
        Request::Opacity {
            window: None,
            value: Some(1.0),
        },
        f.state(),
    );
    assert_eq!(set, Ok(Response::Opacity(1.0)));
    assert!(
        !f.state()
            .background_render_eligible_outputs()
            .any(|o| *o == output),
        "and drops back out of the set the moment the window turns opaque"
    );

    f.state().exit_fullscreen_on(&output);
}
