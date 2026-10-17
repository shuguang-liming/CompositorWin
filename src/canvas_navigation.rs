//! Canvas pointer navigation, matching Compositor's MIT-licensed middle-button behavior.
//! Original: robbietilton/Compositor, Rendering/EditorCanvas.swift (v1.5.3).
use eframe::egui::{self, CursorIcon, PointerButton, Pos2, Rect, Response, Ui, Vec2};
use uuid::Uuid;

#[derive(Clone, Copy)]
struct Gesture {
    button: PointerButton,
    anchor: Pos2,
    last_y: f32,
    zoom: f32,
    pan: Vec2,
    zooming: bool,
    zooming_in: bool,
    moved: bool,
}

pub(super) struct View {
    pub zoom: f32,
    pub pan: Vec2,
    pub consumed: bool,
}

impl Gesture {
    fn view(&mut self, point: Pos2, center: Pos2) -> View {
        self.moved |= self.anchor.distance(point) > 3.;
        if !self.zooming {
            return View {
                zoom: self.zoom,
                pan: self.pan + (point - self.anchor),
                consumed: true,
            };
        }
        if point.y != self.last_y {
            self.zooming_in = point.y < self.last_y;
            self.last_y = point.y;
        }
        let zoom = (self.zoom * ((self.anchor.y - point.y) / 100.).exp2()).clamp(0.005, 64.);
        View {
            zoom,
            pan: anchored_pan(self.pan, self.anchor - center, zoom / self.zoom),
            consumed: true,
        }
    }

    fn cursor(self) -> CursorIcon {
        if !self.zooming {
            CursorIcon::Grabbing
        } else if self.zooming_in {
            CursorIcon::ZoomIn
        } else {
            CursorIcon::ZoomOut
        }
    }
}

fn anchored_pan(pan: Vec2, anchor: Vec2, ratio: f32) -> Vec2 {
    anchor - (anchor - pan) * ratio
}

#[allow(clippy::too_many_arguments)]
pub(super) fn update(
    ui: &Ui,
    response: &Response,
    space: Rect,
    session: Uuid,
    zoom: f32,
    pan: Vec2,
    zoom_tool: bool,
) -> View {
    let key = egui::Id::new("canvas-pointer-navigation");
    let mut active = ui.ctx().data(|data| data.get_temp::<(Uuid, Gesture)>(key));
    let mut result = View {
        zoom,
        pan,
        consumed: false,
    };
    let (point, focused) = ui.input(|input| (input.pointer.latest_pos(), input.focused));
    if !focused || active.is_some_and(|(id, _)| id != session) {
        active = None;
    }
    if focused
        && active.is_none()
        && response.contains_pointer()
        && let Some(point) = point
    {
        let pressed = ui.input(|input| {
            if input.pointer.button_pressed(PointerButton::Middle) {
                Some((
                    PointerButton::Middle,
                    input.modifiers.ctrl || input.modifiers.command,
                ))
            } else if zoom_tool && input.pointer.button_pressed(PointerButton::Primary) {
                Some((PointerButton::Primary, true))
            } else {
                None
            }
        });
        if let Some((button, zooming)) = pressed {
            active = Some((
                session,
                Gesture {
                    button,
                    anchor: point,
                    last_y: point.y,
                    zoom,
                    pan,
                    zooming,
                    zooming_in: true,
                    moved: false,
                },
            ));
        }
    }
    if let Some((_, mut gesture)) = active {
        result = gesture.view(point.unwrap_or(gesture.anchor), space.center());
        let down = ui.input(|input| input.pointer.button_down(gesture.button));
        let released = ui.input(|input| input.pointer.button_released(gesture.button));
        if released && gesture.button == PointerButton::Primary && !gesture.moved {
            result.zoom = (gesture.zoom
                * if ui.input(|i| i.modifiers.alt) {
                    0.5
                } else {
                    2.
                })
            .clamp(0.005, 64.);
            result.pan = anchored_pan(
                gesture.pan,
                gesture.anchor - space.center(),
                result.zoom / gesture.zoom,
            );
        }
        if down {
            ui.ctx().set_cursor_icon(gesture.cursor());
            active = Some((session, gesture));
        } else {
            active = None;
        }
    }
    ui.ctx().data_mut(|data| {
        if let Some(active) = active {
            data.insert_temp(key, active);
        } else {
            data.remove::<(Uuid, Gesture)>(key);
        }
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        session: Uuid,
        view: View,
        events: Vec<egui::Event>,
        ctrl: bool,
    ) -> View {
        let mut result = None;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600., 400.))),
                events,
                modifiers: egui::Modifiers {
                    ctrl,
                    ..Default::default()
                },
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let (space, response) =
                        ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
                    result = Some(update(
                        ui, &response, space, session, view.zoom, view.pan, false,
                    ));
                });
            },
        );
        result.unwrap()
    }

    #[test]
    fn actual_middle_events_capture_release_and_isolate_document_sessions() {
        let ctx = egui::Context::default();
        let session = Uuid::new_v4();
        let start = Pos2::new(180., 250.);
        let mut view = View {
            zoom: 1.25,
            pan: Vec2::ZERO,
            consumed: false,
        };
        view = frame(
            &ctx,
            session,
            view,
            vec![egui::Event::PointerMoved(start)],
            true,
        );
        view = frame(
            &ctx,
            session,
            view,
            vec![egui::Event::PointerButton {
                pos: start,
                button: PointerButton::Middle,
                pressed: true,
                modifiers: egui::Modifiers::CTRL,
            }],
            true,
        );
        assert!(view.consumed);
        view = frame(
            &ctx,
            session,
            view,
            vec![egui::Event::PointerMoved(start - Vec2::new(0., 100.))],
            true,
        );
        assert_eq!(view.zoom, 2.5);
        view = frame(
            &ctx,
            session,
            view,
            vec![egui::Event::PointerButton {
                pos: start - Vec2::new(0., 100.),
                button: PointerButton::Middle,
                pressed: false,
                modifiers: egui::Modifiers::CTRL,
            }],
            true,
        );
        assert!(view.consumed);
        view = frame(
            &ctx,
            session,
            view,
            vec![egui::Event::PointerMoved(start)],
            false,
        );
        assert!(!view.consumed);
        assert_eq!(view.zoom, 2.5);
        view = frame(
            &ctx,
            session,
            view,
            vec![egui::Event::PointerButton {
                pos: start,
                button: PointerButton::Middle,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            false,
        );
        assert!(view.consumed);
        view = frame(
            &ctx,
            Uuid::new_v4(),
            view,
            vec![egui::Event::PointerMoved(Pos2::new(600., 600.))],
            false,
        );
        assert!(!view.consumed);
    }

    fn gesture(zooming: bool) -> Gesture {
        Gesture {
            button: PointerButton::Middle,
            anchor: Pos2::new(180., 250.),
            last_y: 250.,
            zoom: 1.25,
            pan: Vec2::new(20., -10.),
            zooming,
            zooming_in: true,
            moved: false,
        }
    }

    #[test]
    fn zoom_reverses_cursor_before_crossing_start_and_keeps_stationary_direction() {
        let mut drag = gesture(true);
        let center = Pos2::new(300., 200.);
        assert_eq!(drag.view(Pos2::new(180., 150.), center).zoom, 2.5);
        assert_eq!(drag.cursor(), CursorIcon::ZoomIn);
        let reversed = drag.view(Pos2::new(180., 170.), center);
        assert!(reversed.zoom < 2.5 && reversed.zoom > 1.25);
        assert_eq!(drag.cursor(), CursorIcon::ZoomOut);
        drag.view(Pos2::new(180., 170.), center);
        assert_eq!(drag.cursor(), CursorIcon::ZoomOut);
        drag.view(Pos2::new(180., 350.), center);
        assert_eq!(drag.cursor(), CursorIcon::ZoomOut);
        drag.view(Pos2::new(180., 330.), center);
        assert_eq!(drag.cursor(), CursorIcon::ZoomIn);
    }

    #[test]
    fn zoom_keeps_the_pressed_document_point_and_clamps_extremes() {
        let mut drag = gesture(true);
        let center = Pos2::new(300., 200.);
        let before = (drag.anchor - center - drag.pan) / drag.zoom;
        for y in [150., 350., -100_000., 100_000.] {
            let view = drag.view(Pos2::new(180., y), center);
            assert!((0.005..=64.).contains(&view.zoom));
            let after = (drag.anchor - center - view.pan) / view.zoom;
            assert!((before - after).length() < 0.001);
        }
    }

    #[test]
    fn middle_pan_preserves_zoom_and_uses_original_view() {
        let mut drag = gesture(false);
        let view = drag.view(Pos2::new(210., 210.), Pos2::ZERO);
        assert_eq!(view.zoom, 1.25);
        assert_eq!(view.pan, Vec2::new(50., -50.));
        assert_eq!(drag.cursor(), CursorIcon::Grabbing);
        assert_eq!(drag.view(drag.anchor, Pos2::ZERO).pan, Vec2::new(20., -10.));
    }
}
