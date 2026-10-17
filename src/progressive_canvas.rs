//! Progressive Blur's canvas handles share document coordinates with preview and commit.
use super::*;
use std::sync::Weak;

const HANDLE_ID: &str = "progressive-blur-handle";

#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    session: Uuid,
    layer: Uuid,
    mask: bool,
}

#[derive(Clone)]
struct Handle {
    target: Target,
    edit: Weak<Mutex<crate::preview::PreviewCache>>,
    index: usize,
    anchor: [f64; 2],
}

struct Canvas<'a> {
    target: Target,
    edit: &'a Arc<Mutex<crate::preview::PreviewCache>>,
    rect: Rect,
    size: [f64; 2],
    points: [[f64; 2]; 2],
    hand: bool,
    blocked: bool,
}

#[derive(Default)]
struct Outcome {
    consumed: bool,
    wheel: f32,
}

pub(super) fn clear(ctx: &egui::Context) {
    ctx.data_mut(|data| data.remove::<Handle>(egui::Id::new(HANDLE_ID)));
}

fn document_point(rect: Rect, size: [f64; 2], point: Pos2) -> [f64; 2] {
    [
        ((point.x - rect.left()) as f64 / rect.width() as f64 * size[0]).clamp(-1e9, 1e9),
        ((point.y - rect.top()) as f64 / rect.height() as f64 * size[1]).clamp(-1e9, 1e9),
    ]
}

fn constrained(point: [f64; 2], anchor: [f64; 2]) -> [f64; 2] {
    let delta = [point[0] - anchor[0], point[1] - anchor[1]];
    let length = delta[0].hypot(delta[1]);
    let step = std::f64::consts::FRAC_PI_4;
    let angle = (delta[1].atan2(delta[0]) / step).round() * step;
    [
        anchor[0] + angle.cos() * length,
        anchor[1] + angle.sin() * length,
    ]
}

/// One cache allocation identifies one modal edit. Weak preserves its identity across
/// cancel/reopen without retaining the old raster cache or allowing address reuse.
fn update(
    ui: &egui::Ui,
    response: &egui::Response,
    canvas: Canvas<'_>,
    settings: &mut Value,
) -> Outcome {
    let key = egui::Id::new(HANDLE_ID);
    let mut active = ui.ctx().data(|data| data.get_temp::<Handle>(key));
    let edit = Arc::downgrade(canvas.edit);
    if active
        .as_ref()
        .is_some_and(|handle| handle.target != canvas.target || !handle.edit.ptr_eq(&edit))
    {
        active = None;
    }
    let invalid = canvas.rect.width() <= 0.
        || canvas.rect.height() <= 0.
        || !canvas.rect.is_finite()
        || canvas.size.iter().any(|v| !v.is_finite() || *v <= 0.);
    let (focus_lost, middle, space, escape, point) = ui.input(|input| {
        (
            !input.focused
                || input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::WindowFocused(false))),
            input.pointer.button_down(egui::PointerButton::Middle)
                || input.pointer.button_released(egui::PointerButton::Middle),
            input.key_down(Key::Space),
            input.key_pressed(Key::Escape),
            input.pointer.interact_pos(),
        )
    });
    if invalid || focus_lost || escape || canvas.blocked {
        clear(ui.ctx());
        return Outcome {
            consumed: true,
            wheel: 0.,
        };
    }
    let screen = |point: [f64; 2]| {
        canvas.rect.min
            + Vec2::new(
                (point[0] / canvas.size[0]) as f32 * canvas.rect.width(),
                (point[1] / canvas.size[1]) as f32 * canvas.rect.height(),
            )
    };
    let ends = canvas.points.map(screen);
    let painter = ui.painter().with_clip_rect(response.rect);
    painter.line_segment(ends, Stroke::new(3_f32, Color32::BLACK));
    painter.line_segment(ends, Stroke::new(1_f32, Color32::WHITE));
    painter.circle_filled(ends[0], 5., Color32::WHITE);
    painter.circle_stroke(ends[0], 6., Stroke::new(1_f32, Color32::BLACK));
    painter.circle_stroke(ends[1], 6., Stroke::new(3_f32, Color32::BLACK));
    painter.circle_stroke(ends[1], 6., Stroke::new(1_f32, Color32::WHITE));
    if crate::filters::boolean(settings, "progressiveRadial", false) {
        painter.circle_stroke(
            ends[0],
            ends[0].distance(ends[1]),
            Stroke::new(1_f32, Color32::WHITE),
        );
    }
    if middle || space || canvas.hand {
        clear(ui.ctx());
        return Outcome::default();
    }
    let wheel = if response.hovered() {
        ui.input(|input| input.smooth_scroll_delta.y)
    } else {
        0.
    };
    if wheel != 0. {
        clear(ui.ctx());
        return Outcome {
            consumed: true,
            wheel,
        };
    }
    if response.contains_pointer() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        if ui.input(|input| input.pointer.primary_pressed())
            && let Some(point) = point
        {
            let press = ui
                .input(|input| {
                    input.events.iter().find_map(|event| match event {
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            ..
                        } => Some(*pos),
                        _ => None,
                    })
                })
                .unwrap_or(point);
            if response.rect.contains(press) {
                // Prefer the end when both endpoints coincide, as the original does.
                let index = [1, 0].into_iter().find(|&i| ends[i].distance(press) <= 12.);
                let (index, anchor) = if let Some(index) = index {
                    (index, canvas.points[1 - index])
                } else {
                    let point = document_point(canvas.rect, canvas.size, press);
                    settings["progressiveStart"] = json!({"x":point[0],"y":point[1]});
                    settings["progressiveEnd"] = json!({"x":point[0],"y":point[1]});
                    (1, point)
                };
                active = Some(Handle {
                    target: canvas.target,
                    edit,
                    index,
                    anchor,
                });
            }
        }
    }
    if let Some(handle) = &active
        && let Some(point) = point
        && ui.input(|input| input.pointer.primary_down() || input.pointer.primary_released())
    {
        let mut point = document_point(canvas.rect, canvas.size, point);
        if ui.input(|input| input.modifiers.shift) {
            point = constrained(point, handle.anchor);
        }
        settings[["progressiveStart", "progressiveEnd"][handle.index]] =
            json!({"x":point[0],"y":point[1]});
    }
    if !ui.input(|input| input.pointer.primary_down() && input.pointer.has_pointer()) {
        active = None;
    }
    ui.ctx().data_mut(|data| {
        if let Some(handle) = active {
            data.insert_temp(key, handle);
        } else {
            data.remove::<Handle>(key);
        }
    });
    Outcome {
        consumed: true,
        wheel: 0.,
    }
}

impl EditorApp {
    pub(super) fn progressive_canvas_ui(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: Rect,
    ) -> bool {
        if !matches!(&self.dialog,Some(Dialog::Filter{kind})if kind=="Progressive Blur") {
            clear(ui.ctx());
            return false;
        }
        let Some((session, Some(id))) = self.dialog_target else {
            clear(ui.ctx());
            return false;
        };
        if session != self.session().id {
            clear(ui.ctx());
            return false;
        }
        self.prepare_progressive_line();
        let Some(points) = crate::filter_edit::progressive_line(
            &self.session().doc,
            id,
            self.dialog_mask,
            &self.dialog_value,
        ) else {
            clear(ui.ctx());
            return true;
        };
        let size = [
            self.session().doc.width as f64,
            self.session().doc.height as f64,
        ];
        let outcome = update(
            ui,
            response,
            Canvas {
                target: Target {
                    session,
                    layer: id,
                    mask: self.dialog_mask,
                },
                edit: &self.dialog_source_cache,
                rect,
                size,
                points,
                hand: self.tool == Tool::Hand,
                blocked: self.command_palette.is_open(),
            },
            &mut self.dialog_value,
        );
        if outcome.wheel != 0.
            && let Some(position) = ui.input(|input| input.pointer.hover_pos())
        {
            let old_zoom = self.session().zoom;
            let new_zoom = (old_zoom * (outcome.wheel * 0.002).exp()).clamp(0.005, 64.);
            let delta = position - response.rect.center();
            self.session_mut().pan = delta - (delta - self.session().pan) * (new_zoom / old_zoom);
            self.session_mut().zoom = new_zoom;
        }
        outcome.consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct State {
        target: Target,
        cache: Arc<Mutex<crate::preview::PreviewCache>>,
        settings: Value,
        hand: bool,
        blocked: bool,
        focused: bool,
    }
    impl State {
        fn new() -> Self {
            Self {
                target: Target {
                    session: Uuid::new_v4(),
                    layer: Uuid::new_v4(),
                    mask: false,
                },
                cache: Arc::new(Mutex::new(crate::preview::PreviewCache::default())),
                settings: json!({"progressiveFrom":0,"progressiveTo":6,
                "progressiveStart":{"x":50.,"y":20.},"progressiveEnd":{"x":50.,"y":90.}}),
                hand: false,
                blocked: false,
                focused: true,
            }
        }
        fn points(&self) -> [[f64; 2]; 2] {
            ["progressiveStart", "progressiveEnd"].map(|key| {
                [
                    self.settings[key]["x"].as_f64().unwrap(),
                    self.settings[key]["y"].as_f64().unwrap(),
                ]
            })
        }
    }
    fn frame(
        ctx: &egui::Context,
        state: &mut State,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
    ) -> Outcome {
        let mut output = Outcome::default();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(700., 500.))),
                events,
                modifiers,
                focused: state.focused,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let (_, response) =
                        ui.allocate_exact_size(Vec2::new(600., 440.), Sense::click_and_drag());
                    output = update(
                        ui,
                        &response,
                        Canvas {
                            target: state.target,
                            edit: &state.cache,
                            rect: Rect::from_min_size(Pos2::new(100., 50.), Vec2::new(400., 400.)),
                            size: [100., 100.],
                            points: state.points(),
                            hand: state.hand,
                            blocked: state.blocked,
                        },
                        &mut state.settings,
                    );
                });
            },
        );
        output
    }
    fn pointer(pos: Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn begin(ctx: &egui::Context, state: &mut State) {
        frame(
            ctx,
            state,
            vec![egui::Event::PointerMoved(Pos2::new(300., 410.))],
            egui::Modifiers::NONE,
        );
        frame(
            ctx,
            state,
            vec![pointer(
                Pos2::new(300., 410.),
                egui::PointerButton::Primary,
                true,
            )],
            egui::Modifiers::NONE,
        );
    }
    fn captured(ctx: &egui::Context) -> bool {
        ctx.data(|data| data.get_temp::<Handle>(egui::Id::new(HANDLE_ID)).is_some())
    }
    #[test]
    fn real_frames_move_release_and_shift_constrain_a_document_endpoint() {
        let ctx = egui::Context::default();
        let mut state = State::new();
        begin(&ctx, &mut state);
        assert!(captured(&ctx));
        frame(
            &ctx,
            &mut state,
            vec![egui::Event::PointerMoved(Pos2::new(420., 350.))],
            egui::Modifiers::SHIFT,
        );
        let [start, end] = state.points();
        assert!((end[0] - start[0] - (end[1] - start[1])).abs() < 1e-8);
        frame(
            &ctx,
            &mut state,
            vec![pointer(
                Pos2::new(440., 330.),
                egui::PointerButton::Primary,
                false,
            )],
            egui::Modifiers::NONE,
        );
        assert_eq!(state.points()[1], [85., 70.]);
        assert!(!captured(&ctx));
        frame(
            &ctx,
            &mut state,
            vec![egui::Event::PointerMoved(Pos2::new(200., 200.))],
            egui::Modifiers::NONE,
        );
        assert_eq!(state.points()[1], [85., 70.]);
    }
    #[test]
    fn real_frame_fast_press_move_release_creates_a_line_from_the_press_position() {
        let ctx = egui::Context::default();
        let mut state = State::new();
        frame(
            &ctx,
            &mut state,
            vec![egui::Event::PointerMoved(Pos2::new(180., 170.))],
            egui::Modifiers::NONE,
        );
        frame(
            &ctx,
            &mut state,
            vec![
                pointer(Pos2::new(180., 170.), egui::PointerButton::Primary, true),
                egui::Event::PointerMoved(Pos2::new(420., 290.)),
                pointer(Pos2::new(420., 290.), egui::PointerButton::Primary, false),
            ],
            egui::Modifiers::NONE,
        );
        assert_eq!(state.points(), [[20., 30.], [80., 60.]]);
        assert!(!captured(&ctx));
    }
    #[test]
    fn navigation_and_palette_interrupt_before_changing_any_endpoint() {
        for interruption in ["middle", "space", "hand", "palette"] {
            let ctx = egui::Context::default();
            let mut state = State::new();
            begin(&ctx, &mut state);
            let before = state.settings.clone();
            let mut events = vec![egui::Event::PointerMoved(Pos2::new(450., 300.))];
            match interruption {
                "middle" => events.push(pointer(
                    Pos2::new(450., 300.),
                    egui::PointerButton::Middle,
                    true,
                )),
                "space" => events.push(egui::Event::Key {
                    key: Key::Space,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }),
                "hand" => state.hand = true,
                _ => state.blocked = true,
            }
            let outcome = frame(&ctx, &mut state, events, egui::Modifiers::NONE);
            assert_eq!(state.settings, before, "{interruption}");
            assert!(!captured(&ctx));
            assert_eq!(outcome.consumed, interruption == "palette");
        }
    }
    #[test]
    fn focus_loss_escape_and_pointer_exit_never_continue_a_stale_drag() {
        for interruption in ["focus", "focus-cycle", "escape", "gone"] {
            let ctx = egui::Context::default();
            let mut state = State::new();
            begin(&ctx, &mut state);
            let before = state.settings.clone();
            let events = match interruption {
                "focus" => {
                    state.focused = false;
                    vec![
                        egui::Event::PointerMoved(Pos2::new(450., 300.)),
                        egui::Event::WindowFocused(false),
                    ]
                }
                "focus-cycle" => vec![
                    egui::Event::WindowFocused(false),
                    egui::Event::WindowFocused(true),
                    egui::Event::PointerMoved(Pos2::new(450., 300.)),
                ],
                "escape" => vec![egui::Event::Key {
                    key: Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                _ => vec![egui::Event::PointerGone],
            };
            frame(&ctx, &mut state, events, egui::Modifiers::NONE);
            assert_eq!(state.settings, before, "{interruption}");
            assert!(!captured(&ctx));
            state.focused = true;
            frame(
                &ctx,
                &mut state,
                vec![
                    egui::Event::WindowFocused(true),
                    egui::Event::PointerMoved(Pos2::new(200., 200.)),
                ],
                egui::Modifiers::NONE,
            );
            assert_eq!(
                state.settings, before,
                "{interruption} must require a new press"
            );
        }
    }
    #[test]
    fn cancel_reopen_and_target_changes_discard_capture_without_an_intermediate_frame() {
        for change in ["reopen", "session", "layer", "mask"] {
            let ctx = egui::Context::default();
            let mut state = State::new();
            begin(&ctx, &mut state);
            let before = state.settings.clone();
            match change {
                "reopen" => {
                    state.cache = Arc::new(Mutex::new(crate::preview::PreviewCache::default()))
                }
                "session" => state.target.session = Uuid::new_v4(),
                "layer" => state.target.layer = Uuid::new_v4(),
                _ => state.target.mask = true,
            }
            frame(
                &ctx,
                &mut state,
                vec![egui::Event::PointerMoved(Pos2::new(400., 200.))],
                egui::Modifiers::NONE,
            );
            assert_eq!(state.settings, before, "{change}");
            assert!(!captured(&ctx));
        }
    }
    #[test]
    fn real_scroll_events_preserve_settings_and_release_drag_for_viewport_zoom() {
        let ctx = egui::Context::default();
        let mut state = State::new();
        begin(&ctx, &mut state);
        let before = state.settings.clone();
        let outcome = frame(
            &ctx,
            &mut state,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0., 40.),
                modifiers: egui::Modifiers::NONE,
            }],
            egui::Modifiers::NONE,
        );
        assert!(outcome.consumed);
        assert_ne!(outcome.wheel, 0.);
        assert_eq!(state.settings, before);
        assert!(!captured(&ctx));
    }
    #[test]
    fn pointer_settings_are_shared_by_reduced_preview_commit_and_single_undo() {
        let ctx = egui::Context::default();
        let mut state = State::new();
        begin(&ctx, &mut state);
        frame(
            &ctx,
            &mut state,
            vec![
                egui::Event::PointerMoved(Pos2::new(300., 370.)),
                pointer(Pos2::new(300., 370.), egui::PointerButton::Primary, false),
            ],
            egui::Modifiers::NONE,
        );
        let source = Raster::from_rgba(
            100,
            100,
            (0..10000)
                .flat_map(|i| {
                    let v = if i % 4 < 2 { 0 } else { 255 };
                    [v, v, v, 255]
                })
                .collect(),
        );
        let mut original = Document::new(100, 100);
        original.layers = vec![Layer::raster("Stripes", source)];
        let id = original.layers[0].id;
        original.active_layer_id = Some(id);
        let points =
            crate::filter_edit::progressive_line(&original, id, false, &state.settings).unwrap();
        assert_eq!(points, state.points());
        let mut reduced = original.clone();
        let small = crate::preview::area_resize(original.layers[0].image.as_ref().unwrap(), 50, 50);
        reduced.layers[0].image = Some(Arc::new(small));
        let mut settings = state.settings.clone();
        crate::preview::scale_filter(&mut settings, "Progressive Blur", 0.5);
        assert_eq!(settings["progressiveTo"], json!(3.));
        assert_eq!(
            crate::filter_edit::progressive_line(&reduced, id, false, &settings),
            Some(points)
        );
        crate::filter_edit::apply(&mut reduced, id, false, "Progressive Blur", &settings).unwrap();
        let mut committed = original.clone();
        crate::filter_edit::apply(
            &mut committed,
            id,
            false,
            "Progressive Blur",
            &state.settings,
        )
        .unwrap();
        let mut history = History::default();
        assert!(history.record("Progressive Blur", original.clone(), &committed));
        assert_ne!(committed, original);
        assert!(history.undo(&mut committed));
        assert_eq!(committed, original);
        assert_eq!(state.settings["progressiveTo"], json!(6));
        assert!(state.settings.get("_progressiveEndPixels").is_none());
    }
}
