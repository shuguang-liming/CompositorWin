//! Canvas presentation derived from the original checkerboard and rulers.
use crate::chrome::{metrics, theme};
use eframe::egui::{self, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};

pub fn document_backdrop(painter: &Painter, rect: Rect, visible: Rect) {
    if !visible.is_positive() {
        return;
    }
    let style = painter.ctx().style();
    let colors = theme::palette(painter.ctx(), &style.visuals);
    painter.add(theme::document_shadow(&style.visuals).as_shape(rect, 0.));
    let id = egui::Id::new(("original-canvas-checker", style.visuals.dark_mode));
    let cached = painter
        .ctx()
        .data(|data| data.get_temp::<egui::TextureHandle>(id));
    let texture = cached.unwrap_or_else(|| {
        let texture = painter.ctx().load_texture(
            "Original canvas checker",
            egui::ColorImage::new(
                [2, 2],
                vec![
                    colors.checker_light,
                    colors.checker_dark,
                    colors.checker_dark,
                    colors.checker_light,
                ],
            ),
            egui::TextureOptions::NEAREST_REPEAT,
        );
        painter
            .ctx()
            .data_mut(|data| data.insert_temp(id, texture.clone()));
        texture
    });
    // A two-pixel repeating texture keeps 10-point cells with one quad per viewport.
    let uv = Rect::from_min_max(
        ((visible.min - rect.min) / 20.).to_pos2(),
        ((visible.max - rect.min) / 20.).to_pos2(),
    );
    painter.image(texture.id(), visible, uv, Color32::WHITE);
}

pub fn document_edge(painter: &Painter, rect: Rect) {
    painter.rect_stroke(
        rect,
        0.,
        Stroke::new(1.0_f32, theme::overlay(&painter.ctx().style().visuals, 33)),
        egui::StrokeKind::Middle,
    );
}

pub fn rulers(painter: &Painter, viewport: Rect, document: Rect, zoom: f32) {
    let style = painter.ctx().style();
    let colors = theme::palette(painter.ctx(), &style.visuals);
    let thickness = metrics::RULER_THICKNESS;
    let top = Rect::from_min_size(viewport.min, Vec2::new(viewport.width(), thickness));
    let left = Rect::from_min_size(viewport.min, Vec2::new(thickness, viewport.height()));
    painter.rect_filled(top, 0., colors.ruler_fill);
    painter.rect_filled(left, 0., colors.ruler_fill);
    let target = 70. / zoom.max(0.0001);
    let major = [
        1., 2., 5., 10., 20., 25., 50., 100., 200., 250., 500., 1000., 2000., 2500., 5000., 10000.,
        20000., 25000.,
    ]
    .into_iter()
    .find(|step| *step >= target)
    .unwrap_or(50000.);
    let minor = major / 10.;
    let hairline = 1. / painter.ctx().pixels_per_point().max(1.);
    for horizontal in [true, false] {
        let strip = if horizontal { top } else { left };
        let axis = if horizontal {
            [
                viewport.left() + thickness,
                viewport.right(),
                document.left(),
            ]
        } else {
            [
                viewport.top() + thickness,
                viewport.bottom(),
                document.top(),
            ]
        };
        let first = (((axis[0] - axis[2]) / zoom) / minor).floor() as i64;
        let last = (((axis[1] - axis[2]) / zoom) / minor).ceil() as i64;
        let clipped = painter.with_clip_rect(strip.intersect(painter.clip_rect()));
        for index in first..=last.min(first + 2000) {
            let coordinate = axis[2] + index as f32 * minor * zoom;
            let is_major = index.rem_euclid(10) == 0;
            let length = if is_major {
                8.
            } else if index.rem_euclid(5) == 0 {
                5.
            } else {
                3.
            };
            let segment = if horizontal {
                [
                    Pos2::new(coordinate, top.bottom() - length),
                    Pos2::new(coordinate, top.bottom()),
                ]
            } else {
                [
                    Pos2::new(left.right() - length, coordinate),
                    Pos2::new(left.right(), coordinate),
                ]
            };
            clipped.line_segment(segment, Stroke::new(hairline, colors.ruler_tick));
            if is_major {
                let label = format!("{}", (index as f32 * minor).round() as i64);
                let galley =
                    clipped.layout_no_wrap(label, FontId::monospace(8.), colors.ruler_text);
                if horizontal {
                    clipped.galley(
                        Pos2::new(coordinate + 2., top.top()),
                        galley,
                        colors.ruler_text,
                    );
                } else {
                    let mut text = egui::epaint::TextShape::new(
                        Pos2::new(left.left() + 1. + galley.size().y, coordinate + 2.),
                        galley,
                        colors.ruler_text,
                    );
                    text.angle = std::f32::consts::FRAC_PI_2;
                    clipped.add(Shape::Text(text));
                }
            }
        }
    }
    painter.line_segment(
        [top.left_bottom(), top.right_bottom()],
        Stroke::new(hairline, colors.ruler_divider),
    );
    painter.line_segment(
        [left.right_top(), left.right_bottom()],
        Stroke::new(hairline, colors.ruler_divider),
    );
    let corner = Rect::from_min_size(viewport.min, Vec2::splat(thickness));
    painter.rect_filled(corner, 0., colors.ruler_fill);
    painter.line_segment(
        [
            corner.min + Vec2::new(5., thickness - 4.),
            corner.min + Vec2::new(thickness - 4., 5.),
        ],
        Stroke::new(1.0_f32, theme::overlay(&style.visuals, 71)),
    );
}
