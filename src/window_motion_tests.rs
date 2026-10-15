use super::*;

#[test]
fn a_minimized_window_closes_without_waiting_for_unpresented_frames() {
    let ctx = egui::Context::default();
    let mut motion = WindowMotion::with_animations(true);
    motion.begin_close(0.0);
    let mut input = egui::RawInput::default();
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .minimized = Some(true);
    let _ = ctx.run(input, |ctx| assert!(motion.paint(ctx)));
    assert!(!motion.is_closing());
}

fn visible_motion() -> WindowMotion {
    let mut motion = WindowMotion::with_animations(true);
    motion.advance(0.0, 0);
    motion.advance(0.0, 1);
    motion.advance(OPEN_SECONDS, 2);
    motion
}

#[test]
fn startup_waits_for_a_native_frame_instead_of_a_discarded_pass() {
    let mut motion = WindowMotion::with_animations(true);
    let first = motion.advance(1.0, 10);
    assert_eq!(first.opacity, 1.0);
    assert!(first.repaint);
    let repeated = motion.advance(4.0, 10);
    assert_eq!(repeated.opacity, 1.0);
    let visible = motion.advance(7.0, 11);
    assert_eq!(visible.opacity, 1.0);
    let next = motion.advance(7.08, 12);
    assert!((next.opacity - 0.125).abs() < 0.0001);
    assert!(next.repaint);
}

#[test]
fn startup_opacity_decreases_and_repainting_stops_at_completion() {
    let mut motion = WindowMotion::with_animations(true);
    motion.advance(0.0, 0);
    motion.advance(0.0, 1);
    let mut previous = 1.0;
    for frame in 2_u32..=20 {
        let current = motion.advance(f64::from(frame) * 0.01, frame.into());
        assert!(current.opacity >= 0.0 && current.opacity <= previous);
        assert!(!current.close_now);
        previous = current.opacity;
    }
    let idle = motion.advance(500.0, 100);
    assert_eq!(idle.opacity, 0.0);
    assert!(!idle.repaint);
}

#[test]
fn waiting_for_unsaved_confirmation_does_not_start_closing() {
    let mut motion = visible_motion();
    // The app can keep its save/cancel dialog open without approving a close.
    for frame in 3_u32..20 {
        let sample = motion.advance(f64::from(frame), frame.into());
        assert!(!motion.is_closing());
        assert_eq!(sample.opacity, 0.0);
        assert!(!sample.close_now);
        assert!(!sample.repaint);
    }
    motion.cancel_close();
    assert!(!motion.advance(30.0, 20).close_now);
}

#[test]
fn close_has_ease_in_and_emits_one_close_command() {
    let mut motion = visible_motion();
    motion.begin_close(1.0);
    let start = motion.advance(1.0, 3);
    assert_eq!(start.opacity, 0.0);
    assert!(start.repaint);
    let middle = motion.advance(1.06, 4);
    assert!((middle.opacity - 0.125).abs() < 0.0001);
    assert!(!middle.close_now);
    let last = motion.advance(1.121, 5);
    assert_eq!(last.opacity, 1.0);
    assert!(last.close_now);
    assert!(!last.repaint);
    for frame in 6_u32..20 {
        let extra = motion.advance(f64::from(frame), frame.into());
        assert!(!extra.close_now);
        assert!(!extra.repaint);
    }
}

#[test]
fn repeated_close_requests_do_not_restart_the_animation() {
    let mut motion = visible_motion();
    motion.begin_close(1.0);
    motion.advance(1.1, 3);
    motion.begin_close(1.1);
    assert!(motion.advance(1.121, 4).close_now);
    motion.begin_close(10.0);
    assert!(!motion.advance(11.0, 5).close_now);
}

#[test]
fn canceled_close_restores_the_editor_without_a_delayed_close() {
    let mut motion = visible_motion();
    motion.begin_close(1.0);
    assert!(motion.advance(1.08, 3).opacity > 0.0);
    motion.cancel_close();
    assert!(!motion.is_closing());
    let restored = motion.advance(1.2, 4);
    assert_eq!(restored.opacity, 0.0);
    assert!(!restored.repaint);
    assert!(!restored.close_now);
    motion.begin_close(2.0);
    assert!(motion.advance(2.121, 5).close_now);
}

#[test]
fn close_during_opening_preserves_the_current_visible_opacity() {
    let mut motion = WindowMotion::with_animations(true);
    motion.advance(0.0, 0);
    motion.advance(0.0, 1);
    let opening = motion.advance(0.08, 2);
    motion.begin_close(0.08);
    let closing = motion.advance(0.08, 2);
    assert_eq!(opening.opacity, closing.opacity);
    let midway = motion.advance(0.14, 3);
    assert!(midway.opacity > closing.opacity && midway.opacity < 1.0);
    assert!(motion.advance(0.201, 4).close_now);
}

#[test]
fn reduced_motion_opens_and_closes_immediately() {
    let mut motion = WindowMotion::with_animations(false);
    let start = motion.advance(0.0, 0);
    assert_eq!(start.opacity, 0.0);
    assert!(!start.repaint);
    motion.begin_close(0.0);
    let closed = motion.advance(0.0, 0);
    assert!(closed.close_now);
    assert!(!closed.repaint);
    assert!(!motion.advance(1.0, 1).close_now);
}

#[test]
fn disabling_motion_during_a_transition_finishes_immediately() {
    let mut opening = WindowMotion::with_animations(true);
    opening.advance(0.0, 0);
    opening.advance(0.0, 1);
    opening.set_animations_enabled(false);
    let shown = opening.advance(0.01, 2);
    assert_eq!(shown.opacity, 0.0);
    assert!(!shown.repaint);
    opening.set_animations_enabled(true);
    assert_eq!(opening.advance(0.02, 3).opacity, 0.0);

    opening.begin_close(1.0);
    opening.set_animations_enabled(false);
    assert!(opening.advance(1.0, 4).close_now);
    opening.set_animations_enabled(true);
    assert!(!opening.advance(1.0, 5).close_now);
}

#[test]
fn long_frame_stalls_finish_without_extending_the_animation() {
    let mut motion = visible_motion();
    motion.begin_close(1.0);
    let completed = motion.advance(30.0, 3);
    assert!(completed.close_now);
    assert!(!completed.repaint);
}

#[test]
fn closing_suppresses_editor_events_but_preserves_native_close_and_geometry() {
    let ctx = egui::Context::default();
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        events: vec![
            egui::Event::PointerMoved(egui::pos2(100.0, 100.0)),
            egui::Event::PointerButton {
                pos: egui::pos2(100.0, 100.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::CTRL,
            },
            egui::Event::Key {
                key: egui::Key::N,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL,
            },
        ],
        dropped_files: vec![egui::DroppedFile {
            path: Some("unrequested-import.png".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .events
        .push(egui::ViewportEvent::Close);
    let mut motion = visible_motion();
    motion.begin_close(1.0);
    let _ = ctx.run(input, |ctx| {
        motion.suppress_input(ctx);
        ctx.input(|input| {
            assert!(input.events.is_empty());
            assert!(input.keys_down.is_empty());
            assert!(input.raw.dropped_files.is_empty());
            assert!(!input.pointer.any_down());
            assert!(input.viewport().close_requested());
            assert_eq!(input.content_rect().size(), egui::vec2(800.0, 600.0));
        });
    });
}
