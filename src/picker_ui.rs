//! Original 256-point color picker with live RGBA editing and canvas sampling.
use super::*;
use eframe::egui::{Key, Mesh, Pos2, Rect, Response, Stroke, StrokeKind};

const FIELD_SIZE: f32 = 256.;

fn opaque_color(hue: f32, saturation: f32, brightness: f32) -> Color32 {
    let rgb = egui::ecolor::rgb_from_hsv((hue, saturation, brightness));
    Color32::from_rgb(
        (rgb[0] * 255.).round() as u8,
        (rgb[1] * 255.).round() as u8,
        (rgb[2] * 255.).round() as u8,
    )
}

fn checker(ui: &egui::Ui, rect: Rect) {
    let colors = crate::chrome::theme::palette(ui.ctx(), ui.visuals());
    let painter = ui.painter();
    painter.rect_filled(rect, 3, colors.thumb_checker_light);
    let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    for y in 0..(rect.height() / 6.).ceil() as i32 {
        for x in 0..(rect.width() / 6.).ceil() as i32 {
            if (x + y) % 2 == 0 {
                painter.rect_filled(
                    Rect::from_min_size(
                        rect.min + Vec2::new(x as f32 * 6., y as f32 * 6.),
                        Vec2::splat(6.),
                    ),
                    0,
                    colors.thumb_checker_dark,
                );
            }
        }
    }
}

fn numeric_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    maximum: f64,
    suffix: &str,
) -> Response {
    ui.label(egui::RichText::new(label).size(11.));
    ui.add_sized(
        [52., 20.],
        egui::DragValue::new(value)
            .range(0.0..=maximum)
            .speed(1.)
            .max_decimals(0)
            .suffix(suffix),
    )
}

impl ColorPicker {
    pub(super) fn update_from_hsva(&mut self) {
        let rgb = self.hsva.to_rgb();
        self.color = [
            (rgb[0] * 255.).round() as u8,
            (rgb[1] * 255.).round() as u8,
            (rgb[2] * 255.).round() as u8,
            self.color[3],
        ];
        self.hsva.a = self.color[3] as f32 / 255.;
        if !self.hex_focused {
            self.hex = format_hex(self.color);
        }
    }

    fn saturation_brightness_ui(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(Vec2::splat(FIELD_SIZE), Sense::click_and_drag());
        response.widget_info(|| {
            egui::WidgetInfo::slider(ui.is_enabled(), self.hsva.s as f64 * 100., "饱和度与亮度")
        });
        let hue_color = opaque_color(self.hsva.h, 1., 1.);
        let mut field = Mesh::default();
        field.colored_vertex(rect.left_top(), Color32::WHITE);
        field.colored_vertex(rect.right_top(), hue_color);
        field.colored_vertex(rect.right_bottom(), hue_color);
        field.colored_vertex(rect.left_bottom(), Color32::WHITE);
        field.add_triangle(0, 1, 2);
        field.add_triangle(0, 2, 3);
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        painter.add(egui::Shape::mesh(field));
        let mut brightness = Mesh::default();
        brightness.colored_vertex(rect.left_top(), Color32::TRANSPARENT);
        brightness.colored_vertex(rect.right_top(), Color32::TRANSPARENT);
        brightness.colored_vertex(rect.right_bottom(), Color32::BLACK);
        brightness.colored_vertex(rect.left_bottom(), Color32::BLACK);
        brightness.add_triangle(0, 1, 2);
        brightness.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(brightness));
        painter.rect_stroke(
            rect,
            0,
            Stroke::new(1.0_f32, Color32::from_black_alpha(153)),
            StrokeKind::Inside,
        );
        if (response.clicked() || response.dragged() || response.is_pointer_button_down_on())
            && let Some(point) = response.interact_pointer_pos()
        {
            self.hsva.s = ((point.x - rect.left()) / FIELD_SIZE).clamp(0., 1.);
            self.hsva.v = 1. - ((point.y - rect.top()) / FIELD_SIZE).clamp(0., 1.);
            self.update_from_hsva();
        }
        let marker = Pos2::new(
            rect.left() + self.hsva.s * FIELD_SIZE,
            rect.top() + (1. - self.hsva.v) * FIELD_SIZE,
        );
        painter.circle_stroke(marker, 6.75, Stroke::new(0.75_f32, Color32::BLACK));
        painter.circle_stroke(marker, 6., Stroke::new(1.5_f32, Color32::WHITE));
        response.on_hover_text("水平调整饱和度，垂直调整亮度");
    }

    fn hue_ui(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(34., FIELD_SIZE), Sense::click_and_drag());
        response.widget_info(|| {
            egui::WidgetInfo::slider(ui.is_enabled(), self.hsva.h as f64 * 360., "色相")
        });
        let strip = Rect::from_min_max(rect.min + Vec2::new(7., 0.), rect.max - Vec2::new(7., 0.));
        let mut mesh = Mesh::default();
        for index in 0..6 {
            let top = index as f32 / 6.;
            let bottom = (index + 1) as f32 / 6.;
            let top_color = opaque_color(1. - top, 1., 1.);
            let bottom_color = opaque_color(1. - bottom, 1., 1.);
            let start = mesh.vertices.len() as u32;
            mesh.colored_vertex(
                Pos2::new(strip.left(), strip.top() + top * FIELD_SIZE),
                top_color,
            );
            mesh.colored_vertex(
                Pos2::new(strip.right(), strip.top() + top * FIELD_SIZE),
                top_color,
            );
            mesh.colored_vertex(
                Pos2::new(strip.right(), strip.top() + bottom * FIELD_SIZE),
                bottom_color,
            );
            mesh.colored_vertex(
                Pos2::new(strip.left(), strip.top() + bottom * FIELD_SIZE),
                bottom_color,
            );
            mesh.add_triangle(start, start + 1, start + 2);
            mesh.add_triangle(start, start + 2, start + 3);
        }
        ui.painter().add(egui::Shape::mesh(mesh));
        ui.painter().rect_stroke(
            strip,
            0,
            Stroke::new(1.0_f32, Color32::from_black_alpha(153)),
            StrokeKind::Inside,
        );
        if (response.clicked() || response.dragged() || response.is_pointer_button_down_on())
            && let Some(point) = response.interact_pointer_pos()
        {
            self.hsva.h = 1. - ((point.y - strip.top()) / FIELD_SIZE).clamp(0., 1.);
            self.update_from_hsva();
        }
        let marker_y = strip.top() + (1. - self.hsva.h) * FIELD_SIZE;
        for points in [
            vec![
                Pos2::new(rect.left(), marker_y - 5.),
                Pos2::new(strip.left(), marker_y),
                Pos2::new(rect.left(), marker_y + 5.),
            ],
            vec![
                Pos2::new(rect.right(), marker_y - 5.),
                Pos2::new(strip.right(), marker_y),
                Pos2::new(rect.right(), marker_y + 5.),
            ],
        ] {
            ui.painter().add(egui::Shape::convex_polygon(
                points,
                ui.visuals().text_color(),
                Stroke::NONE,
            ));
        }
        response.on_hover_text(format!("色相：{:.0}°", self.hsva.h * 360.));
    }

    fn previews_ui(&mut self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(64.), Sense::hover());
        for (color, new) in [(self.color, true), (self.original, false)] {
            let subrect = if new {
                Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.center().y))
            } else {
                Rect::from_min_max(Pos2::new(rect.left(), rect.center().y), rect.max)
            };
            checker(ui, subrect);
            ui.painter().rect_filled(
                subrect,
                0,
                Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]),
            );
            let response = ui.interact(
                subrect,
                ui.id()
                    .with(if new { "new-color" } else { "original-color" }),
                Sense::click(),
            );
            if response.clicked() {
                self.sample(color);
            }
            response.on_hover_text(if new {
                "新颜色"
            } else {
                "当前颜色：点击恢复打开时的颜色"
            });
        }
        ui.painter().hline(
            rect.x_range(),
            rect.center().y,
            Stroke::new(1.0_f32, Color32::from_black_alpha(153)),
        );
        ui.painter().rect_stroke(
            rect,
            4,
            Stroke::new(1.0_f32, Color32::from_black_alpha(153)),
            StrokeKind::Inside,
        );
    }

    fn channels_ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("picker-hsb-rgb")
            .spacing([8., 6.])
            .show(ui, |ui| {
                for index in 0..3 {
                    let label = ["H", "S", "B"][index];
                    let mut hsb = [
                        self.hsva.h as f64 * 360.,
                        self.hsva.s as f64 * 100.,
                        self.hsva.v as f64 * 100.,
                    ][index];
                    if numeric_field(
                        ui,
                        label,
                        &mut hsb,
                        if index == 0 { 360. } else { 100. },
                        if index == 0 { "°" } else { "%" },
                    )
                    .changed()
                    {
                        match index {
                            0 => self.hsva.h = hsb as f32 / 360.,
                            1 => self.hsva.s = hsb as f32 / 100.,
                            _ => self.hsva.v = hsb as f32 / 100.,
                        }
                        self.update_from_hsva();
                    }
                    let mut rgb = self.color[index] as f64;
                    if numeric_field(ui, ["R", "G", "B"][index], &mut rgb, 255., "").changed() {
                        let mut color = self.color;
                        color[index] = rgb.round() as u8;
                        self.sample(color);
                    }
                    ui.end_row();
                }
            });
        ui.add_space(6.);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("A").size(11.));
            let mut alpha = self.color[3] as f64;
            if ui
                .add_sized(
                    [52., 20.],
                    egui::DragValue::new(&mut alpha)
                        .range(0.0..=255.)
                        .speed(1.)
                        .max_decimals(0),
                )
                .changed()
            {
                self.color[3] = alpha.round() as u8;
                self.hsva.a = self.color[3] as f32 / 255.;
            }
            ui.label(egui::RichText::new("不透明度").size(10.).weak());
        });
        ui.add_space(6.);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("#").size(11.));
            let response = ui.add_sized(
                [126., 22.],
                egui::TextEdit::singleline(&mut self.hex)
                    .font(egui::TextStyle::Monospace)
                    .id_salt("picker-hex"),
            );
            self.hex_focused = response.has_focus();
            self.hex_dirty |= response.changed();
            if response.lost_focus()
                || response.has_focus() && ui.input(|input| input.key_pressed(Key::Enter))
            {
                if self.hex_dirty
                    && let Some(color) = parse_hex(&self.hex, self.color[3])
                {
                    self.sample(color);
                }
                self.hex = format_hex(self.color);
                self.hex_dirty = false;
                if ui.input(|input| input.key_pressed(Key::Enter)) {
                    response.surrender_focus();
                }
            }
            response.on_hover_text("RGB、RRGGBB 或 RRGGBBAA；保留原有透明度");
        });
    }

    pub(super) fn show_original_ui(&mut self, ctx: &egui::Context, title: &str) -> Option<bool> {
        let mut open = true;
        let mut finish = None;
        egui::Window::new(title)
            .id(egui::Id::new("palette-picker"))
            .open(&mut open)
            .resizable(false)
            .default_width(538.)
            .show(ctx, |ui| {
                egui::Frame::new().inner_margin(20).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 14.;
                    ui.horizontal_top(|ui| {
                        self.saturation_brightness_ui(ui);
                        self.hue_ui(ui);
                        ui.allocate_ui_with_layout(
                            Vec2::new(180., FIELD_SIZE),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(8., 6.);
                                ui.horizontal_top(|ui| {
                                    self.previews_ui(ui);
                                    ui.add_space(8.);
                                    ui.vertical(|ui| {
                                        if ui
                                            .add_sized([90., 28.], egui::Button::new("确定"))
                                            .clicked()
                                        {
                                            finish = Some(true);
                                        }
                                        ui.add_space(2.);
                                        if ui
                                            .add_sized([90., 28.], egui::Button::new("取消"))
                                            .clicked()
                                        {
                                            finish = Some(false);
                                        }
                                    });
                                });
                                ui.add_space(12.);
                                self.channels_ui(ui);
                                ui.add_space(8.);
                                ui.label(
                                    egui::RichText::new("单击或拖动画布可取样颜色。")
                                        .size(11.)
                                        .weak(),
                                );
                            },
                        );
                    });
                });
            });
        if !open {
            finish = Some(false);
        }
        finish
    }
}
