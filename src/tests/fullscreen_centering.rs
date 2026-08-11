//! A client that commits smaller than its fullscreen output is centred in it
//! rather than left pinned to the top-left corner the park mapped it at.
//! Centring moves the *mapped* stage position, so hit-testing has to follow it
//! for free, and the settled fullscreen-cull predicate has to keep reading the
//! output as covered even though the window no longer sits exactly on the
//! camera origin.
//!
//! Every "answers with a smaller size" scenario here picks an answer size that
//! differs from the size the window was mapped at, never a bare re-commit of
//! the same buffer: `WindowAnimations::on_window_commit` only resolves the
//! fullscreen-entry chase's outstanding request on a commit whose size
//! actually changes (or matches the offer exactly) — a size-for-size identical
//! re-commit reads as "nothing new happened" and never registers as an answer.
//! A real client that never changes its buffer size across the whole fullscreen
//! cycle would see the same real-time endpoint-hold degrade every other frozen
//! resize does; that path is exercised elsewhere and is not what these
//! scenarios are pinning.

use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::Point;

use super::client::ClientId;
use super::{Fixture, adopt_last_configure, map_window, tick_until_settled, window_by_app_id};

/// A window mapped at `map_size` on a fresh `1920x1080` output, then made
/// fullscreen on it. The client has not yet answered the fullscreen configure.
fn fullscreen_window(
    f: &mut Fixture,
    map_size: (u16, u16),
) -> (
    ClientId,
    wayland_client::protocol::wl_surface::WlSurface,
    Output,
    Window,
) {
    let output = f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let surface = map_window(f, id, "fs", map_size);
    let window = window_by_app_id(f, "fs").unwrap();
    f.state().enter_fullscreen(&window, Some(output.clone()));
    f.double_roundtrip(id);
    (id, surface, output, window)
}

/// Ack the fullscreen configure but commit at `size` instead of the offered
/// one — the client-chosen size a fixed-aspect-ratio game or dialog answers
/// with.
fn ack_fullscreen_at(
    f: &mut Fixture,
    id: ClientId,
    surface: &wayland_client::protocol::wl_surface::WlSurface,
    size: (u16, u16),
) {
    f.double_roundtrip(id);
    let window = f.client(id).window(surface);
    window.set_size(size.0, size.1);
    window.attach_new_buffer();
    window.ack_last_and_commit();
    f.double_roundtrip(id);
}

/// A plain redraw at a new size, with no configure to ack — a client resizing
/// itself after it has already answered the fullscreen offer.
fn commit_at(
    f: &mut Fixture,
    id: ClientId,
    surface: &wayland_client::protocol::wl_surface::WlSurface,
    size: (u16, u16),
) {
    let window = f.client(id).window(surface);
    window.set_size(size.0, size.1);
    window.attach_new_buffer();
    window.commit();
    f.double_roundtrip(id);
}

/// A client that acks the fullscreen configure but takes a size smaller than
/// the output is moved off the parked corner by half the shortfall on each
/// axis — and the settled predicate the fullscreen render cull gates on must
/// still read the output as covered, because it backs the offset out of the
/// position to recover the park.
#[test]
fn a_smaller_fullscreen_commit_is_centred_and_still_reads_as_covering_the_output() {
    let mut f = Fixture::new();
    let (id, surface, output, window) = fullscreen_window(&mut f, (700, 500));
    let camera = f.state().camera().to_i32_round();

    ack_fullscreen_at(&mut f, id, &surface, (800, 600));
    tick_until_settled(&mut f);

    let position = f.state().stage.position_of(&window).expect("staged");
    assert_eq!(
        position - camera,
        Point::from((560, 240)),
        "an 800x600 answer on a 1920x1080 output sits half the shortfall in on each axis"
    );
    assert!(
        f.state().is_output_visually_fullscreen(&output),
        "the cull gate must still read the output as covered once the window is centred"
    );

    f.state().exit_fullscreen_on(&output);
}

/// The centring moves the mapped stage position, so a pointer at the visual
/// centre of the output resolves to the centred window, and a pointer in the
/// black band it left uncovered resolves to no window at all.
#[test]
fn a_pointer_at_the_output_centre_hits_the_centred_window_and_the_band_hits_nothing() {
    let mut f = Fixture::new();
    let (id, surface, output, window) = fullscreen_window(&mut f, (700, 500));
    let camera = f.state().camera();

    ack_fullscreen_at(&mut f, id, &surface, (800, 600));

    let viewport = crate::state::output_logical_size(&output).to_f64();
    let centre = Point::from((camera.x + viewport.w / 2.0, camera.y + viewport.h / 2.0));
    let hit = f.state().element_under(centre).map(|(w, _)| w.clone());
    assert_eq!(
        hit,
        Some(window.clone()),
        "the output's visual centre hits the centred window"
    );

    let band = Point::from((camera.x + 5.0, camera.y + 5.0));
    assert!(
        f.state().element_under(band).is_none(),
        "the black band the centring left uncovered hits no window"
    );

    f.state().exit_fullscreen_on(&output);
}

/// A client that commits at exactly the offered size is untouched: it keeps
/// sitting on the plain camera-origin park and records no centring offset —
/// the ordinary case must see no behaviour change.
#[test]
fn a_compliant_fullscreen_commit_keeps_the_plain_park_with_a_zero_offset() {
    let mut f = Fixture::new();
    let (id, surface, output, window) = fullscreen_window(&mut f, (800, 600));
    let camera = f.state().camera().to_i32_round();

    adopt_last_configure(&mut f, id, &surface);

    let position = f.state().stage.position_of(&window).expect("staged");
    assert_eq!(
        position, camera,
        "a client that takes the offered size stays parked, not centred"
    );
    assert_eq!(
        f.state()
            .stage
            .fullscreen_on(&output.name())
            .unwrap()
            .centre_offset,
        Point::default(),
        "no offset is recorded for a client that took the offered size"
    );

    f.state().exit_fullscreen_on(&output);
}

/// A commit that answers with a size smaller than the output but never acks
/// the fullscreen configure must not centre the window: the compositor still
/// owes the client an answer to a live configure, so acting on the geometry
/// now would fling the window to the middle and slide it back once the real
/// ack lands.
#[test]
fn an_unacked_fullscreen_commit_does_not_centre_the_window() {
    let mut f = Fixture::new();
    let (id, surface, output, window) = fullscreen_window(&mut f, (700, 500));
    let camera = f.state().camera().to_i32_round();

    // A differently-sized commit with no ack: registers as an answer to the
    // *chase*, but the fullscreen configure itself is still outstanding.
    f.double_roundtrip(id);
    let client_window = f.client(id).window(&surface);
    client_window.set_size(800, 600);
    client_window.attach_new_buffer();
    client_window.commit();
    f.double_roundtrip(id);

    assert_eq!(
        window.geometry().size,
        smithay::utils::Size::from((800, 600)),
        "precondition: the commit actually landed a new size"
    );
    let position = f.state().stage.position_of(&window).expect("staged");
    assert_eq!(
        position, camera,
        "an unacked commit must not move the window off the park"
    );
    assert_eq!(
        f.state()
            .stage
            .fullscreen_on(&output.name())
            .unwrap()
            .centre_offset,
        Point::default(),
        "no offset is recorded while the fullscreen configure is unacked"
    );

    f.state().exit_fullscreen_on(&output);
}

/// The fixed-size client — the whole reason this exists. It answers the
/// fullscreen offer by re-committing the size it already had, which the
/// fullscreen-entry chase reads as no answer at all
/// (`WindowAnimations::on_window_commit` resolves its outstanding request only
/// on a size *change*, or a match to the offer). Centring must not be gated on
/// that chase, or the clients it is for are the ones it never reaches.
#[test]
fn a_fixed_size_client_that_re_commits_its_own_size_is_still_centred() {
    let mut f = Fixture::new();
    let (id, surface, output, window) = fullscreen_window(&mut f, (800, 600));
    let camera = f.state().camera().to_i32_round();

    ack_fullscreen_at(&mut f, id, &surface, (800, 600));

    let position = f.state().stage.position_of(&window).expect("staged");
    assert_eq!(
        position - camera,
        Point::from((560, 240)),
        "a client that answers with the size it already had is centred like any other"
    );

    f.state().exit_fullscreen_on(&output);
}

/// A second answer at a different size re-centres from the position the first
/// one already moved: the write backs the stored offset out before adding the
/// new one, so the offsets telescope rather than accumulating.
#[test]
fn a_second_smaller_commit_re_centres_rather_than_stacking_offsets() {
    let mut f = Fixture::new();
    let (id, surface, output, window) = fullscreen_window(&mut f, (700, 500));
    let camera = f.state().camera().to_i32_round();

    ack_fullscreen_at(&mut f, id, &surface, (800, 600));
    tick_until_settled(&mut f);
    commit_at(&mut f, id, &surface, (1000, 700));

    let position = f.state().stage.position_of(&window).expect("staged");
    assert_eq!(
        position - camera,
        Point::from((460, 190)),
        "the second answer is centred from the park, not from where the first left it"
    );
    assert!(
        f.state().is_output_visually_fullscreen(&output),
        "and the cull gate survives a re-centre"
    );

    f.state().exit_fullscreen_on(&output);
}

/// Exiting fullscreen from a centred window restores the exact pre-fullscreen
/// position, not the centred one — `saved_location` was captured before the
/// centring ever ran, from the same pre-fullscreen geometry every plain exit
/// restores from.
#[test]
fn exiting_fullscreen_from_a_centred_window_restores_the_pre_fullscreen_position() {
    let mut f = Fixture::new();
    let output = f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let surface = map_window(&mut f, id, "fs", (600, 400));
    let window = window_by_app_id(&mut f, "fs").unwrap();
    let pre = f.state().stage.position_of(&window).expect("staged");

    f.state().enter_fullscreen(&window, Some(output.clone()));
    f.double_roundtrip(id);
    ack_fullscreen_at(&mut f, id, &surface, (650, 450));

    assert_ne!(
        f.state()
            .stage
            .fullscreen_on(&output.name())
            .unwrap()
            .centre_offset,
        Point::default(),
        "the scenario needs a real centring offset to prove the exit undoes it"
    );

    f.state().exit_fullscreen_on(&output);
    assert_eq!(
        f.state().stage.position_of(&window),
        Some(pre),
        "exit restores the exact pre-fullscreen position, not the centred one"
    );
}

/// A commit larger than the output clamps to a zero offset rather than a
/// negative one: `max(0)` on each axis, not a window pushed past the origin.
#[test]
fn an_oversized_fullscreen_commit_yields_a_zero_offset() {
    let mut f = Fixture::new();
    let (id, surface, output, window) = fullscreen_window(&mut f, (400, 300));
    let camera = f.state().camera().to_i32_round();

    ack_fullscreen_at(&mut f, id, &surface, (2200, 1300));

    let position = f.state().stage.position_of(&window).expect("staged");
    assert_eq!(
        position, camera,
        "a commit bigger than the output is not offset at all"
    );
    assert_eq!(
        f.state()
            .stage
            .fullscreen_on(&output.name())
            .unwrap()
            .centre_offset,
        Point::default(),
        "an over-sized commit clamps to a zero offset, not a negative one"
    );

    f.state().exit_fullscreen_on(&output);
}
