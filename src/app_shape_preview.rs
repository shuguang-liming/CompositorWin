//! Vector drafts share committed drag geometry without allocating a shape raster.
use super::*;
use crate::edit::ShapeKind;

pub(super) struct ShapeDraft {
    kind: ShapeKind,
    rect: [f64; 4],
    ends: Option<[[f64; 2]; 2]>,
    color: [u8; 4],
    radius: f64,
    smoothing: bool,
    line_width: f64,
}

impl ShapeDraft {
    pub(super) fn commit(self, document: &mut Document) -> anyhow::Result<Uuid> {
        crate::edit::add_shape_with_smoothing(
            document,
            self.kind,
            self.rect,
            self.color,
            self.radius,
            self.line_width,
            self.ends,
            self.smoothing,
        )
    }

    fn paint(&self, painter: &egui::Painter, origin: Pos2, zoom: f32) {
        if self.rect.iter().any(|value| !value.is_finite())
            || self.rect[2] < 1.
            || self.rect[3] < 1.
            || self.rect[2] * self.rect[3] > crate::model::MAX_PIXELS as f64
            || !zoom.is_finite()
            || zoom <= 0.
        {
            return;
        }
        // add_shape_with_smoothing rounds the raster size before drawing its
        // curves and normalized line endpoints. Match that exact native grid.
        let width = self.rect[2].round();
        let height = self.rect[3].round();
        let screen = |point: [f64; 2]| {
            origin
                + Vec2::new(
                    (self.rect[0] + point[0]) as f32,
                    (self.rect[1] + point[1]) as f32,
                ) * zoom
        };
        // The committed raster clips any subpixel cap overflow to its rounded
        // native box. Keep that same boundary for a fractional, snapped line.
        let painter = painter.with_clip_rect(Rect::from_min_max(
            screen([0., 0.]),
            screen([width, height]),
        ));
        let color = Color32::from_rgba_unmultiplied(
            self.color[0],
            self.color[1],
            self.color[2],
            self.color[3],
        );
        let tolerance = 0.25 / (zoom as f64 * painter.ctx().pixels_per_point() as f64);
        let points = match self.kind {
            ShapeKind::Rectangle => crate::smooth_corners::RoundedRectangle::new(
                width,
                height,
                self.radius,
                self.smoothing,
            )
            .outline(tolerance),
            ShapeKind::Ellipse => {
                painter.add(egui::Shape::ellipse_filled(
                    screen([width / 2., height / 2.]),
                    Vec2::new(width as f32, height as f32) * (zoom / 2.),
                    color,
                ));
                return;
            }
            ShapeKind::Line => {
                let ends = self
                    .ends
                    .unwrap_or([[0., 0.], [1., 1.]])
                    .map(|point| [point[0] * width, point[1] * height]);
                let angle = (ends[1][1] - ends[0][1]).atan2(ends[1][0] - ends[0][0]);
                let radius = self.line_width.clamp(0.1, 5000.) / 2.;
                let mut points = Vec::new();
                // A single filled capsule keeps round caps and translucent
                // foreground colors exact, without overlapping stroke/circles.
                for (center, start) in [
                    (ends[1], angle - std::f64::consts::FRAC_PI_2),
                    (ends[0], angle + std::f64::consts::FRAC_PI_2),
                ] {
                    crate::smooth_corners::append_arc(
                        &mut points,
                        center,
                        radius,
                        start,
                        start + std::f64::consts::PI,
                        tolerance,
                    );
                }
                points
            }
        };
        painter.add(egui::Shape::convex_polygon(
            points.into_iter().map(screen).collect(),
            color,
            Stroke::NONE,
        ));
    }
}

impl EditorApp {
    pub(super) fn shape_draft(&self, drag: &Drag, shift: bool, alt: bool) -> ShapeDraft {
        let kind = match self.shape_kind.as_str() {
            "Ellipse" => ShapeKind::Ellipse,
            "Line" => ShapeKind::Line,
            _ => ShapeKind::Rectangle,
        };
        let (rect, ends) = crate::edit::shape_drag_geometry(
            kind,
            drag.start,
            drag.last,
            shift,
            alt,
            self.line_width,
        );
        ShapeDraft {
            kind,
            rect,
            ends,
            color: self.foreground,
            radius: self.corner_radius,
            smoothing: self.shape_smoothing,
            line_width: self.line_width,
        }
    }

    pub(super) fn paint_shape_draft(
        &self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        canvas: Rect,
        zoom: f32,
    ) {
        if let Some(drag) = &self.drag
            && drag.mode == Tool::Shape
            && self.drag_is_current(drag)
        {
            let (shift, alt) = ui.input(|input| (input.modifiers.shift, input.modifiers.alt));
            self.shape_draft(drag, shift, alt).paint(
                &painter.with_clip_rect(canvas),
                canvas.min,
                zoom,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, epaint::ClippedShape};

    struct Frame {
        origin: Pos2,
        drafts: Vec<ClippedShape>,
    }

    fn editor(kind: &str, smoothing: bool) -> EditorApp {
        let mut app = EditorApp::create(None, None);
        app.tabs = vec![Session::new(Document::new(120, 100), None)];
        app.current = 0;
        app.tool = Tool::Shape;
        app.shape_kind = kind.into();
        app.shape_smoothing = smoothing;
        app.corner_radius = 12.;
        app.line_width = 7.;
        app.foreground = [37, 149, 211, 193];
        app.session_mut().zoom = 4.;
        app.welcome_clipboard_checked = true;
        app
    }

    fn frame(
        app: &mut EditorApp,
        ctx: &egui::Context,
        events: Vec<Event>,
        modifiers: Modifiers,
    ) -> Frame {
        let mut origin = Pos2::ZERO;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280., 860.))),
                events,
                modifiers,
                ..Default::default()
            },
            |ctx| {
                for command in app.shortcuts(ctx) {
                    app.command(command);
                }
                egui::CentralPanel::default().show(ctx, |ui| {
                    let ppp = ctx.pixels_per_point();
                    let zoom = app.session().zoom / ppp;
                    let size = Vec2::new(120., 100.) * zoom;
                    origin =
                        ui.available_rect_before_wrap().center() + app.session().pan - size / 2.;
                    if app.session().zoom >= 1. {
                        origin = ((origin.to_vec2() * ppp).round() / ppp).to_pos2();
                    }
                    app.canvas(ui);
                });
            },
        );
        let color = Color32::from_rgba_unmultiplied(37, 149, 211, 193);
        let drafts = output
            .shapes
            .into_iter()
            .filter(|clipped| match &clipped.shape {
                egui::Shape::Path(path) => path.fill == color,
                egui::Shape::Ellipse(ellipse) => ellipse.fill == color,
                _ => false,
            })
            .collect();
        Frame { origin, drafts }
    }

    fn pointer(
        app: &mut EditorApp,
        ctx: &egui::Context,
        position: Pos2,
        pressed: bool,
        modifiers: Modifiers,
    ) -> Frame {
        frame(
            app,
            ctx,
            vec![
                Event::PointerMoved(position),
                Event::PointerButton {
                    pos: position,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers,
                },
            ],
            modifiers,
        )
    }

    fn start_drag(
        app: &mut EditorApp,
        ctx: &egui::Context,
        start: [f64; 2],
        end: [f64; 2],
        modifiers: Modifiers,
    ) -> (Pos2, Pos2, ClippedShape) {
        frame(app, ctx, vec![], Modifiers::NONE);
        let origin = frame(app, ctx, vec![], Modifiers::NONE).origin;
        let screen = |point: [f64; 2]| origin + Vec2::new(point[0] as f32, point[1] as f32) * 4.;
        pointer(app, ctx, screen(start), true, modifiers);
        let mut moved = frame(app, ctx, vec![Event::PointerMoved(screen(end))], modifiers);
        assert!(
            app.drag
                .as_ref()
                .is_some_and(|drag| drag.mode == Tool::Shape)
        );
        assert_eq!(
            moved.drafts.len(),
            1,
            "A real pointer frame must paint one filled draft"
        );
        (origin, screen(end), moved.drafts.pop().unwrap())
    }

    fn distance_to_segment(point: Pos2, a: Pos2, b: Pos2) -> f32 {
        let ab = b - a;
        let length = ab.length_sq();
        let t = if length == 0. {
            0.
        } else {
            ((point - a).dot(ab) / length).clamp(0., 1.)
        };
        point.distance(a + ab * t)
    }

    // Compare the actual egui output with the committed native raster. Ignore
    // only pixels whose antialiasing footprint reaches the vector boundary.
    fn assert_preview_matches_raster(draft: &ClippedShape, origin: Pos2, layer: &Layer) {
        let raster = layer.image.as_ref().unwrap();
        let pixels = raster.to_rgba();
        let mut inside_count = 0;
        let mut outside_count = 0;
        for y in 0..raster.height {
            for x in 0..raster.width {
                let point = origin
                    + Vec2::new(
                        (layer.transform.origin[0] + x as f64 + 0.5) as f32,
                        (layer.transform.origin[1] + y as f64 + 0.5) as f32,
                    ) * 4.;
                let (inside, edge_distance) = match &draft.shape {
                    egui::Shape::Path(path) => {
                        let mut positive = false;
                        let mut negative = false;
                        let mut distance = f32::INFINITY;
                        for index in 0..path.points.len() {
                            let a = path.points[index];
                            let b = path.points[(index + 1) % path.points.len()];
                            let edge = b - a;
                            let delta = point - a;
                            let cross = edge.x * delta.y - edge.y * delta.x;
                            positive |= cross > 1e-4;
                            negative |= cross < -1e-4;
                            distance = distance.min(distance_to_segment(point, a, b));
                        }
                        (!(positive && negative), distance)
                    }
                    egui::Shape::Ellipse(ellipse) => {
                        let delta = point - ellipse.center;
                        let normalized =
                            Vec2::new(delta.x / ellipse.radius.x, delta.y / ellipse.radius.y)
                                .length();
                        // A conservative lower bound on Euclidean edge distance.
                        (
                            normalized <= 1.,
                            (normalized - 1.).abs() * ellipse.radius.min_elem(),
                        )
                    }
                    _ => panic!("Only the actual filled shape may be used as a draft"),
                };
                if edge_distance < 3. {
                    continue;
                }
                let inside = inside && draft.clip_rect.contains(point);
                let alpha = pixels[((y * raster.width + x) * 4 + 3) as usize];
                assert_eq!(
                    alpha,
                    if inside { 255 } else { 0 },
                    "Draft/raster mismatch at ({x}, {y})"
                );
                inside_count += usize::from(inside);
                outside_count += usize::from(!inside);
            }
        }
        assert!(
            inside_count > 0,
            "The preview must include the committed fill"
        );
        let style = layer.shape.as_ref().unwrap();
        if style["kind"] == "Ellipse"
            || (style["kind"] == "Line"
                && style["start"][0] != style["end"][0]
                && style["start"][1] != style["end"][1])
        {
            assert!(
                outside_count > 0,
                "Ellipse and diagonal line previews must leave their box corners empty"
            );
        }
    }

    fn finish_edit(app: &mut EditorApp, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while app.session().edit_busy {
            let event = app
                .receiver
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("The real shape worker must return its result");
            app.sender
                .send(event)
                .expect("The GUI worker receiver is live");
            app.poll(ctx);
        }
        assert!(
            app.message.is_none(),
            "Shape commit failed: {:?}",
            app.message
        );
    }

    #[test]
    fn shape_canvas_preview_matches_commit_for_round_smooth_ellipse_and_round_cap_lines() {
        let centered_square = Modifiers {
            shift: true,
            alt: true,
            ..Default::default()
        };
        for (kind, smoothing, start, end, modifiers) in [
            ("Rectangle", false, [20., 18.], [84., 70.], Modifiers::NONE),
            ("Rectangle", true, [65., 55.], [37., 37.], centered_square),
            ("Ellipse", false, [60., 50.], [84., 66.], centered_square),
            ("Line", false, [25., 20.], [80., 60.], Modifiers::NONE),
            ("Line", false, [84., 70.], [30., 25.], centered_square),
            ("Line", false, [24., 50.], [90., 50.], Modifiers::ALT),
            ("Line", false, [60., 20.], [60., 80.], Modifiers::NONE),
        ] {
            let mut app = editor(kind, smoothing);
            let before = app.session().doc.clone();
            let revision = app.session().history.revision();
            let ctx = egui::Context::default();
            let (origin, release, draft) = start_drag(&mut app, &ctx, start, end, modifiers);
            assert_eq!(
                app.session().doc,
                before,
                "Dragging must not change document pixels"
            );
            assert_eq!(app.session().history.revision(), revision);
            assert!(app.session().history.undo_name().is_none());
            let released = pointer(&mut app, &ctx, release, false, modifiers);
            assert!(released.drafts.is_empty());
            assert!(app.drag.is_none());
            finish_edit(&mut app, &ctx);
            assert_eq!(app.session().doc.layers.len(), before.layers.len() + 1);
            let layer = app.session().doc.active().unwrap();
            let style = layer.shape.as_ref().unwrap();
            assert_eq!(style["kind"], kind);
            assert_eq!(style["smoothing"].as_bool().unwrap_or(false), smoothing);
            assert_eq!(layer.opacity, 193. / 255.);
            if kind == "Ellipse" {
                assert!(matches!(&draft.shape, egui::Shape::Ellipse(_)));
                assert_eq!(layer.transform.origin, [36., 26.]);
                assert_eq!(layer.transform.size, [48., 48.]);
            }
            if kind == "Rectangle" && smoothing {
                assert_eq!(layer.transform.origin, [37., 27.]);
                assert_eq!(layer.transform.size, [56., 56.]);
            }
            if kind == "Line" {
                assert_eq!(style["lineWidth"], 7.);
                let start = [0, 1].map(|axis| {
                    style["start"][axis].as_f64().unwrap() * layer.transform.size[axis]
                });
                let end = [0, 1]
                    .map(|axis| style["end"][axis].as_f64().unwrap() * layer.transform.size[axis]);
                if modifiers.shift {
                    assert!(
                        start[0] > end[0] && start[1] > end[1],
                        "Reverse line direction must survive snapping"
                    );
                    assert!(((end[0] - start[0]).abs() - (end[1] - start[1]).abs()).abs() < 1e-9);
                } else if modifiers.alt {
                    assert_eq!(start[1], end[1]);
                    assert_eq!(layer.transform.origin, [20.5, 46.5]);
                    assert_eq!(layer.transform.size, [73., 7.]);
                }
            }
            assert_preview_matches_raster(&draft, origin, layer);
            assert_eq!(app.session().history.undo_name(), Some("New shape"));
            app.command(Command::Undo);
            assert_eq!(app.session().doc, before);
            assert!(
                app.session().history.undo_name().is_none(),
                "One drag must create exactly one undo entry"
            );
        }
    }

    #[test]
    fn shape_canvas_preview_updates_radius_smoothing_and_modifiers_without_pointer_lag() {
        let mut app = editor("Rectangle", false);
        app.corner_radius = 5.;
        let before = app.session().doc.clone();
        let ctx = egui::Context::default();
        let (origin, _, round) =
            start_drag(&mut app, &ctx, [65., 55.], [37., 37.], Modifiers::NONE);
        app.shape_smoothing = true;
        let smooth = frame(&mut app, &ctx, vec![], Modifiers::NONE)
            .drafts
            .pop()
            .unwrap();
        let (egui::Shape::Path(round), egui::Shape::Path(smooth)) = (&round.shape, &smooth.shape)
        else {
            panic!("Rounded rectangle previews must use their real filled contour");
        };
        assert_ne!(
            round.points, smooth.points,
            "Smooth Corners must change the live path"
        );
        app.corner_radius = 0.;
        let square = frame(
            &mut app,
            &ctx,
            vec![],
            Modifiers {
                shift: true,
                alt: true,
                ..Default::default()
            },
        )
        .drafts
        .pop()
        .unwrap();
        let egui::Shape::Path(path) = square.shape else {
            panic!("Rectangle draft must be filled")
        };
        assert_eq!(path.points.len(), 4);
        let bounds = Rect::from_points(&path.points);
        assert_eq!(bounds.min, origin + Vec2::new(37., 27.) * 4.);
        assert_eq!(bounds.size(), Vec2::splat(56. * 4.));
        assert_eq!(app.session().doc, before);
        assert!(app.session().history.undo_name().is_none());
    }

    #[test]
    fn shape_canvas_escape_removes_live_preview_without_pixels_history_or_late_commit() {
        let mut app = editor("Rectangle", true);
        let before = app.session().doc.clone();
        let ctx = egui::Context::default();
        let (_, release, _) = start_drag(&mut app, &ctx, [20., 18.], [84., 70.], Modifiers::NONE);
        let escaped = frame(
            &mut app,
            &ctx,
            vec![Event::Key {
                key: Key::Escape,
                physical_key: Some(Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            Modifiers::NONE,
        );
        assert!(escaped.drafts.is_empty());
        assert!(app.drag.is_none());
        assert!(!app.session().edit_busy);
        assert_eq!(app.session().doc, before);
        assert!(app.session().history.undo_name().is_none());
        let released = pointer(&mut app, &ctx, release, false, Modifiers::NONE);
        assert!(released.drafts.is_empty());
        assert!(!app.session().edit_busy);
        assert_eq!(app.session().doc, before);
        assert!(app.session().history.undo_name().is_none());
    }
}
