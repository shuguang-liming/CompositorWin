//! Typed parameter controls for filters, adjustment layers, Camera Raw, and layer effects.
use crate::{filters, locale::text, model::Raster};
use eframe::egui::{self, Color32, Pos2, Sense, Stroke, Vec2};
use rayon::prelude::*;
use serde_json::{Value, json};
use std::ops::RangeInclusive;

fn number(
    ui: &mut egui::Ui,
    s: &mut Value,
    key: &str,
    label: &str,
    range: RangeInclusive<f64>,
    default: f64,
) -> bool {
    number_control(ui, s, key, label, range, default).0
}

#[derive(Clone, Copy)]
enum SliderTrack {
    Plain,
    Temperature,
    Tint,
    Chroma,
    Hue(f64),
    Saturation(f64),
    Luminance(f64),
    Spectrum(f64),
    Opposing(Color32, Color32),
}

fn hsv_color(hue: f64, saturation: f64, value: f64) -> Color32 {
    egui::ecolor::Hsva::new(
        (hue / 360.).rem_euclid(1.) as f32,
        saturation as f32,
        value as f32,
        1.,
    )
    .into()
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let a = a.to_array();
    let b = b.to_array();
    Color32::from_rgba_premultiplied(
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t).round() as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t).round() as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t).round() as u8,
        (a[3] as f32 + (b[3] as f32 - a[3] as f32) * t).round() as u8,
    )
}

impl SliderTrack {
    fn color(self, t: f32, plain: Color32) -> Color32 {
        let endpoints = match self {
            Self::Plain => return plain,
            Self::Temperature => (
                Color32::from_rgb(56, 117, 242),
                Color32::from_rgb(250, 209, 46),
            ),
            Self::Tint => (
                Color32::from_rgb(71, 179, 87),
                Color32::from_rgb(179, 102, 163),
            ),
            Self::Chroma => (
                Color32::from_rgb(158, 158, 163),
                Color32::from_rgb(219, 46, 51),
            ),
            Self::Hue(hue) => (
                hsv_color(hue - 50., 0.85, 0.9),
                hsv_color(hue + 50., 0.85, 0.9),
            ),
            Self::Saturation(hue) => (Color32::from_rgb(140, 140, 143), hsv_color(hue, 0.9, 0.9)),
            Self::Luminance(hue) => (hsv_color(hue, 0.55, 0.18), hsv_color(hue, 0.35, 0.95)),
            Self::Spectrum(center) => {
                return hsv_color(center + (t as f64 * 360. - 180.), 0.85, 0.9);
            }
            Self::Opposing(a, b) => (a, b),
        };
        lerp_color(endpoints.0, endpoints.1, t)
    }
}

fn paint_gradient(painter: &egui::Painter, rect: egui::Rect, track: SliderTrack, plain: Color32) {
    let mut mesh = egui::epaint::Mesh::default();
    // Thirteen stops also retain the source CameraRaw spectrum's 30-degree segments.
    for i in 0..=12 {
        let t = i as f32 / 12.;
        let x = rect.left() + rect.width() * t;
        let color = track.color(t, plain);
        mesh.colored_vertex(Pos2::new(x, rect.top()), color);
        mesh.colored_vertex(Pos2::new(x, rect.bottom()), color);
        if i > 0 {
            let index = i * 2;
            mesh.add_triangle(index - 2, index - 1, index);
            mesh.add_triangle(index - 1, index, index + 1);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

fn value_track(
    ui: &mut egui::Ui,
    value: &mut f64,
    range: &RangeInclusive<f64>,
    default: f64,
    decimals: usize,
    track: SliderTrack,
) -> egui::Response {
    let (mut response, painter) = ui.allocate_painter(
        Vec2::new(ui.available_width().max(24.), 22.),
        Sense::click_and_drag(),
    );
    let rect = response.rect.shrink2(Vec2::new(6., 0.));
    let span = range.end() - range.start();
    let before = *value;
    if response.double_clicked() {
        *value = default;
    } else if (response.clicked() || response.dragged())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let t = ((pointer.x - rect.left()) / rect.width().max(1.)).clamp(0., 1.) as f64;
        let factor = 10_f64.powi(decimals as i32);
        *value = ((*range.start() + t * span) * factor).round() / factor;
    }
    if response.clicked() || response.drag_started() {
        response.request_focus();
    }
    if response.has_focus() {
        let delta = ui.input_mut(|input| {
            let mut steps = 0;
            for key in [egui::Key::ArrowRight, egui::Key::ArrowUp] {
                if input.consume_key(egui::Modifiers::NONE, key) {
                    steps += 1;
                }
            }
            for key in [egui::Key::ArrowLeft, egui::Key::ArrowDown] {
                if input.consume_key(egui::Modifiers::NONE, key) {
                    steps -= 1;
                }
            }
            steps
        });
        *value += delta as f64 / 10_f64.powi(decimals as i32);
    }
    if before != *value {
        *value = value.clamp(*range.start(), *range.end());
        response.mark_changed();
    }
    let rail = egui::Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 4.));
    paint_gradient(&painter, rail, track, ui.visuals().widgets.inactive.bg_fill);
    let fraction = ((*value - range.start()) / span.max(f64::EPSILON)).clamp(0., 1.) as f32;
    let center = Pos2::new(rect.left() + rect.width() * fraction, rect.center().y);
    let visual = ui.style().interact(&response);
    painter.circle_filled(center, 6., visual.bg_fill);
    painter.circle_stroke(center, 6., visual.fg_stroke);
    response.widget_info(|| egui::WidgetInfo::new(egui::WidgetType::Slider));
    response.on_hover_text("拖动调整；双击恢复默认值；方向键微调")
}

fn numeric_label(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    range: &RangeInclusive<f64>,
    default: f64,
    step: f64,
) {
    let response = ui
        .add(egui::Label::new(label).sense(Sense::click_and_drag()))
        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    let id = response.id.with("numeric-scrub");
    if response.drag_started() {
        ui.data_mut(|data| data.insert_temp(id, *value));
    }
    if response.double_clicked() {
        *value = default;
    } else if response.dragged() {
        let start = ui.data(|data| data.get_temp::<f64>(id)).unwrap_or(*value);
        let speed = if ui.input(|i| i.modifiers.shift) {
            0.1
        } else {
            1.
        };
        *value = (start + response.total_drag_delta().unwrap_or_default().x as f64 * step * speed)
            .clamp(*range.start(), *range.end());
    }
    if response.drag_stopped() {
        ui.data_mut(|data| data.remove::<f64>(id));
    }
}

fn numeric_field(
    ui: &mut egui::Ui,
    value: &mut f64,
    range: RangeInclusive<f64>,
    decimals: usize,
    width: f32,
) -> egui::Response {
    ui.add_sized(
        [width, 22.],
        egui::DragValue::new(value)
            .range(range)
            .speed(1. / 10_f64.powi(decimals as i32))
            .fixed_decimals(decimals),
    )
}

fn parameter_track(s: &Value, key: &str, range: &RangeInclusive<f64>) -> SliderTrack {
    match key {
        "temperature" => SliderTrack::Temperature,
        "tint" => SliderTrack::Tint,
        "vibrance" | "saturation" => SliderTrack::Chroma,
        "hue" if *range.start() == 0. && *range.end() == 360. => SliderTrack::Spectrum(180.),
        "hueShift" => SliderTrack::Hue(filters::number(s, "hue", 0.)),
        "saturationShift" => SliderTrack::Saturation(filters::number(s, "hue", 0.)),
        "luminanceShift" => SliderTrack::Luminance(filters::number(s, "hue", 0.)),
        _ => SliderTrack::Plain,
    }
}

fn number_control(
    ui: &mut egui::Ui,
    s: &mut Value,
    key: &str,
    label: &str,
    range: RangeInclusive<f64>,
    default: f64,
) -> (bool, bool) {
    let mut value = filters::number(s, key, default);
    let before = value;
    let decimals = if key == "exposure" || range.end() - range.start() < 20. {
        2
    } else {
        0
    };
    let track = parameter_track(s, key, &range);
    let mut active = false;
    ui.push_id(key, |ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.;
            ui.horizontal(|ui| {
                numeric_label(
                    ui,
                    label,
                    &mut value,
                    &range,
                    default,
                    1. / 10_f64.powi(decimals as i32),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    active |= numeric_field(ui, &mut value, range.clone(), decimals, 56.)
                        .is_pointer_button_down_on();
                });
            });
            active |= value_track(ui, &mut value, &range, default, decimals, track)
                .is_pointer_button_down_on();
        });
    });
    let changed = value != before;
    if changed {
        s[key] = json!(value);
    }
    (changed, active)
}

fn toggle(ui: &mut egui::Ui, s: &mut Value, key: &str, label: &str, default: bool) -> bool {
    let mut value = filters::boolean(s, key, default);
    let changed = ui.checkbox(&mut value, label).changed();
    if changed {
        s[key] = json!(value);
    }
    changed
}
fn choice(ui: &mut egui::Ui, s: &mut Value, key: &str, label: &str, values: &[&str]) -> bool {
    let current = filters::string(s, key, values[0]).to_owned();
    let mut next = current.clone();
    ui.horizontal(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(key)
            .selected_text(text(&next))
            .show_ui(ui, |ui| {
                for &value in values {
                    ui.selectable_value(&mut next, value.to_owned(), text(value));
                }
            });
    });
    if next != current {
        s[key] = json!(next);
        true
    } else {
        false
    }
}
fn color(ui: &mut egui::Ui, s: &mut Value, label: &str, default: [f64; 3]) -> bool {
    let mut color = filters::color(s, default).map(|x| x as f32);
    let changed = ui
        .horizontal(|ui| {
            ui.label(label);
            ui.color_edit_button_rgb(&mut color).changed()
        })
        .inner;
    if changed {
        for (key, value) in ["red", "green", "blue"].into_iter().zip(color) {
            s[key] = json!(value);
        }
    }
    changed
}
fn section<'a>(s: &'a mut Value, key: &str) -> &'a mut Value {
    if s.get(key).is_some() { &mut s[key] } else { s }
}
fn controls(ui: &mut egui::Ui, s: &mut Value, fields: &[(&str, &str, f64, f64, f64)]) -> bool {
    let mut changed = false;
    for &(key, label, min, max, default) in fields {
        changed |= number(ui, s, key, label, min..=max, default);
    }
    changed
}

fn raw_controls(
    ui: &mut egui::Ui,
    s: &mut Value,
    fields: &[(&str, &str, f64, f64, f64)],
    diagnostic: &mut i64,
) -> bool {
    let mut changed = false;
    for &(key, label, min, max, default) in fields {
        let (edited, active) = number_control(ui, s, key, label, min..=max, default);
        changed |= edited;
        if edited && matches!(key, "temperature" | "tint") {
            s["whiteBalance"] = json!("Custom");
        }
        if active && ui.input(|i| i.modifiers.alt) {
            *diagnostic = match key {
                "exposure" | "highlights" | "whites" => 1,
                "shadows" | "blacks" => 2,
                "sharpenMasking" => 3,
                _ => 0,
            };
        }
    }
    changed
}

fn set_raw_diagnostic(s: &mut Value, diagnostic: i64) -> bool {
    if diagnostic == 0 {
        s.as_object_mut()
            .and_then(|object| object.remove("_altDiagnostic"))
            .is_some()
    } else if s["_altDiagnostic"].as_i64() != Some(diagnostic) {
        s["_altDiagnostic"] = json!(diagnostic);
        true
    } else {
        false
    }
}

/// Returns true only when an actual parameter was changed by the user.
pub fn show(ui: &mut egui::Ui, kind: &str, settings: &mut Value, sample: Option<&Raster>) -> bool {
    show_with_original_sample(ui, kind, settings, sample, sample)
}

/// Scopes use the clean graded preview; automatic white balance uses the original image.
pub fn show_with_original_sample(
    ui: &mut egui::Ui,
    kind: &str,
    settings: &mut Value,
    sample: Option<&Raster>,
    original_sample: Option<&Raster>,
) -> bool {
    let mut changed = false;
    ui.push_id(kind, |ui| match kind {
        "Gaussian Blur" => changed |= number(ui, settings, "blurRadius", "半径", 0.1..=250.0, 10.0),
        "Progressive Blur" => {
            changed |= toggle(ui, settings, "progressiveRadial", "径向模糊", false);
            changed |= number(
                ui,
                settings,
                "progressiveFrom",
                "起点模糊",
                0.0..=250.0,
                0.0,
            );
            changed |= number(ui, settings, "progressiveTo", "终点模糊", 0.0..=250.0, 20.0);
            changed |= toggle(ui, settings, "progressiveInverted", "反转方向", false);
            ui.separator();
            ui.label("在画布拖动控制线，或调整起点、终点的文档坐标。");
            for (key, label) in [("progressiveStart", "起点"), ("progressiveEnd", "终点")] {
                if settings[key].is_object() {
                    ui.horizontal(|ui| {
                        ui.label(label);
                        for axis in ["x", "y"] {
                            let mut coordinate = filters::number(&settings[key], axis, 0.);
                            if ui
                                .add(
                                    egui::DragValue::new(&mut coordinate)
                                        .prefix(format!("{axis}: "))
                                        .speed(1.)
                                        .range(-1_000_000_000.0..=1_000_000_000.0),
                                )
                                .changed()
                            {
                                settings[key][axis] = json!(coordinate);
                                changed = true;
                            }
                        }
                    });
                }
            }
        }
        "Motion Blur" => {
            changed |= controls(
                ui,
                settings,
                &[
                    ("motionAngle", "角度", -90., 90., 0.),
                    ("motionDistance", "距离", 1., 2000., 10.),
                ],
            )
        }
        "Add Noise" => {
            changed |= number(ui, settings, "noiseAmount", "数量", 0.1..=400.0, 10.0);
            changed |= toggle(ui, settings, "noiseGaussian", "高斯分布", false);
            changed |= toggle(ui, settings, "noiseMonochromatic", "单色", false);
            changed |= number(ui, settings, "noiseSeed", "随机种子", 0.0..=65535.0, 0.0);
        }
        "Invert" => {
            ui.label("反转 RGB 颜色，保留透明度。");
        }
        "Exposure" => {
            changed |= controls(
                ui,
                section(settings, "exposureSettings"),
                &[
                    ("exposure", "曝光（档）", -10., 10., 0.),
                    ("offset", "偏移", -0.5, 0.5, 0.),
                    ("gamma", "伽马", 0.1, 9.99, 1.),
                ],
            )
        }
        "Levels" => changed |= levels(ui, section(settings, "levels"), sample),
        "Curves" => changed |= curves(ui, section(settings, "curves"), 255.0),
        "Hue/Saturation" => changed |= hue_saturation(ui, section(settings, "hsvSettings")),
        "Gradient Map" => {
            let s = section(settings, "gradientMapSettings");
            changed |= color(ui, &mut s["shadows"], "阴影", [0.; 3]);
            changed |= color(ui, &mut s["highlights"], "高光", [1.; 3]);
            changed |= toggle(ui, s, "reversed", "反向", false);
        }
        "Grain" => {
            changed |= controls(
                ui,
                section(settings, "grainSettings"),
                &[
                    ("amount", "数量", 0., 100., 25.),
                    ("size", "大小", 0.5, 20., 1.5),
                    ("roughness", "粗糙度", 0., 100., 50.),
                    ("seed", "随机种子", 0., 65535., 0.),
                ],
            )
        }
        "Black & White" => {
            let s = section(settings, "blackWhiteSettings");
            changed |= controls(
                ui,
                s,
                &[
                    ("reds", "红色", -200., 300., 40.),
                    ("yellows", "黄色", -200., 300., 60.),
                    ("greens", "绿色", -200., 300., 40.),
                    ("cyans", "青色", -200., 300., 60.),
                    ("blues", "蓝色", -200., 300., 20.),
                    ("magentas", "洋红色", -200., 300., 80.),
                ],
            );
            changed |= toggle(ui, s, "tint", "色调", false);
            if filters::boolean(s, "tint", false) {
                changed |= controls(
                    ui,
                    s,
                    &[
                        ("tintHue", "色相", 0., 360., 40.),
                        ("tintSaturation", "饱和度", 0., 100., 20.),
                    ],
                );
            }
        }
        "Color Balance" => {
            let s = section(settings, "colorBalanceSettings");
            for (prefix, title) in [
                ("shadow", "Shadows"),
                ("mid", "Midtones"),
                ("highlight", "Highlights"),
            ] {
                egui::CollapsingHeader::new(match title {
                    "Shadows" => "阴影",
                    "Midtones" => "中间调",
                    _ => "高光",
                })
                .id_salt(title)
                .show(ui, |ui| {
                    for (suffix, label) in [
                        ("CyanRed", "青色 ↔ 红色"),
                        ("MagentaGreen", "洋红色 ↔ 绿色"),
                        ("YellowBlue", "黄色 ↔ 蓝色"),
                    ] {
                        changed |= number(
                            ui,
                            s,
                            &format!("{prefix}{suffix}"),
                            label,
                            -100.0..=100.0,
                            0.,
                        );
                    }
                });
            }
            changed |= toggle(ui, s, "preserveLuminosity", "保持明度", true);
        }
        "Vignette" => {
            changed |= controls(
                ui,
                settings,
                &[
                    ("vignetteAmount", "数量", 0., 100., 35.),
                    ("vignetteMidpoint", "中点", 0., 100., 50.),
                    ("vignetteRoundness", "圆度", -100., 100., 100.),
                    ("vignetteFeather", "羽化", 0., 100., 60.),
                    ("vignetteHighlights", "高光", 0., 100., 25.),
                ],
            );
            changed |= color(ui, &mut settings["vignetteColor"], "颜色", [0.; 3]);
        }
        "Bloom / Glow" => {
            changed |= controls(
                ui,
                settings,
                &[
                    ("bloomAmount", "数量", 0., 100., 40.),
                    ("bloomRadius", "半径", 1., 150., 24.),
                    ("bloomThreshold", "阈值", 0., 100., 0.),
                ],
            )
        }
        "Tonal Contrast" => {
            changed |= controls(
                ui,
                settings,
                &[
                    ("tonalAmount", "数量", 0., 100., 50.),
                    ("tonalRadius", "半径", 1., 100., 16.),
                    ("tonalShadows", "阴影", -100., 100., 40.),
                    ("tonalMidtones", "中间调", -100., 100., 60.),
                    ("tonalHighlights", "高光", -100., 100., 30.),
                ],
            )
        }
        "Lens Correction" => {
            changed |= number(ui, settings, "distortion", "畸变", -100.0..=100.0, 0.0)
        }
        "Dither" => changed |= dither(ui, section(settings, "dither")),
        "Scanlines" => changed |= scanlines(ui, section(settings, "scanlines")),
        "Camera Raw Filter" => {
            changed |= camera_raw(ui, section(settings, "cameraRaw"), sample, original_sample)
        }
        _ => {
            ui.label(format!("未知滤镜：{}", text(kind)));
        }
    });
    changed
}

pub fn show_effect(ui: &mut egui::Ui, kind: &str, s: &mut Value) -> bool {
    let mut changed = false;
    changed |= toggle(ui, s, "enabled", "启用", true);
    let white = matches!(kind, "outerGlow" | "innerGlow");
    changed |= color(ui, s, "颜色", if white { [1.; 3] } else { [0.; 3] });
    changed |= number(
        ui,
        s,
        "opacity",
        "不透明度",
        0.0..=1.0,
        if white {
            0.75
        } else if kind.contains("hadow") {
            0.5
        } else {
            1.
        },
    );
    match kind {
        "stroke" => {
            changed |= number(ui, s, "size", "宽度", 0.0..=500.0, 4.);
            changed |= toggle(ui, s, "inside", "内侧", false);
        }
        "shadow" | "innerShadow" => {
            let d = if kind == "shadow" { 20. } else { 10. };
            changed |= controls(
                ui,
                s,
                &[
                    ("angle", "光照角度", -360., 360., 90.),
                    ("distance", "距离", 0., 5000., d),
                    ("blur", "模糊", 0., 500., d),
                ],
            );
        }
        "outerGlow" | "innerGlow" => {
            changed |= number(
                ui,
                s,
                "size",
                "大小",
                0.0..=500.0,
                if kind == "outerGlow" { 20. } else { 10. },
            )
        }
        _ => {}
    }
    changed
}

/// The open dialog owns these transient controls; they never enter saved filter JSON.
pub struct FilterUiState {
    pub preview: bool,
    pub pointer: Option<FilterPointer>,
    hue_drag: Option<crate::filter_interactions::HueTargetDrag>,
}

impl Default for FilterUiState {
    fn default() -> Self {
        Self {
            preview: true,
            pointer: None,
            hue_drag: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterPointer {
    Levels(crate::filter_interactions::LevelsSample),
    Hue(crate::filter_interactions::HueSampleMode),
    HueTargeted,
}

impl FilterUiState {
    pub fn stop_pointer(&mut self) {
        self.pointer = None;
        self.hue_drag = None;
    }

    /// Coordinates are source pixels. Levels needs the original target raster, while
    /// Hue/Saturation needs the document composite before this edit's own preview.
    pub fn pointer_start(
        &mut self,
        settings: &mut Value,
        sample: &Raster,
        pixel: [f64; 2],
        pointer_x: f64,
    ) -> bool {
        use crate::filter_interactions::{HueTargetDrag, apply_hue_sample, apply_levels_sample};
        match self.pointer {
            Some(FilterPointer::Levels(mode)) => apply_levels_sample(settings, sample, pixel, mode),
            Some(FilterPointer::Hue(mode)) => apply_hue_sample(settings, sample, pixel, mode),
            Some(FilterPointer::HueTargeted) => {
                let before = settings.clone();
                self.hue_drag = HueTargetDrag::begin(settings, sample, pixel, pointer_x);
                before != *settings
            }
            None => false,
        }
    }

    pub fn pointer_drag(
        &mut self,
        settings: &mut Value,
        pointer_x: f64,
        adjusts_hue: bool,
    ) -> bool {
        if self.pointer == Some(FilterPointer::HueTargeted) {
            self.hue_drag
                .as_ref()
                .is_some_and(|drag| drag.apply(settings, pointer_x, adjusts_hue))
        } else {
            false
        }
    }

    pub fn pointer_color_start(
        &mut self,
        settings: &mut Value,
        color: crate::raw_interaction::ColorSample,
        pointer_x: f64,
    ) -> bool {
        use crate::filter_interactions::{HueTargetDrag, apply_hue_color};
        match self.pointer {
            Some(FilterPointer::Hue(mode)) => apply_hue_color(settings, color, mode),
            Some(FilterPointer::HueTargeted) => {
                let before = settings.clone();
                self.hue_drag = HueTargetDrag::begin_color(settings, color, pointer_x);
                before != *settings
            }
            _ => false,
        }
    }

    pub fn pointer_end(&mut self) {
        self.hue_drag = None;
    }
}

/// Dialog-only state is explicit, so an inline adjustment panel has no empty sampling
/// buttons and independent dialogs cannot leak modes or preview choices into each other.
pub fn show_dialog_controls(
    ui: &mut egui::Ui,
    kind: &str,
    settings: &mut Value,
    sample: Option<&Raster>,
    original_sample: Option<&Raster>,
    state: &mut FilterUiState,
) -> bool {
    match kind {
        "Levels" => {
            ui.push_id(kind, |ui| {
                levels_panel(ui, section(settings, "levels"), sample, Some(state))
            })
            .inner
        }
        "Hue/Saturation" => {
            ui.push_id(kind, |ui| {
                hue_panel(
                    ui,
                    section(settings, "hsvSettings"),
                    sample.is_some(),
                    Some(state),
                )
            })
            .inner
        }
        _ => show_with_original_sample(ui, kind, settings, sample, original_sample),
    }
}

fn toggle_pointer(
    ui: &mut egui::Ui,
    state: &mut FilterUiState,
    mode: FilterPointer,
    label: &str,
    help: &str,
    enabled: bool,
) {
    let clicked = ui
        .add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                let icon = if mode == FilterPointer::HueTargeted {
                    crate::chrome::Icon::Hand
                } else {
                    crate::chrome::Icon::Eyedropper
                };
                let response = crate::chrome::icon_button_sized(
                    ui,
                    icon,
                    state.pointer == Some(mode),
                    help,
                    Vec2::new(24., 20.),
                );
                if matches!(mode, FilterPointer::Levels(_)) {
                    ui.label(label);
                } else if matches!(
                    mode,
                    FilterPointer::Hue(
                        crate::filter_interactions::HueSampleMode::Add
                            | crate::filter_interactions::HueSampleMode::Remove
                    )
                ) {
                    ui.painter().text(
                        response.rect.right_bottom(),
                        egui::Align2::RIGHT_BOTTOM,
                        label,
                        egui::FontId::proportional(9.),
                        ui.visuals().text_color(),
                    );
                }
                response.clicked()
            })
            .inner
        })
        .inner;
    if clicked {
        let next = (state.pointer != Some(mode)).then_some(mode);
        state.stop_pointer();
        state.pointer = next;
    }
}

fn levels(ui: &mut egui::Ui, s: &mut Value, sample: Option<&Raster>) -> bool {
    levels_panel(ui, s, sample, None)
}

fn filter_channel_picker(
    ui: &mut egui::Ui,
    s: &mut Value,
    key: &str,
    values: &[&str],
    width: f32,
) -> bool {
    let current = filters::string(s, key, values[0]).to_owned();
    let mut next = current.clone();
    egui::ComboBox::from_id_salt(key)
        .width(width)
        .selected_text(text(&next))
        .show_ui(ui, |ui| {
            for &name in values {
                ui.selectable_value(&mut next, name.to_owned(), text(name));
            }
        });
    if next != current {
        s[key] = json!(next);
        true
    } else {
        false
    }
}

fn level_values(s: &Value) -> [f64; 5] {
    [
        filters::number(s, "black", 0.),
        filters::number(s, "gamma", 1.),
        filters::number(s, "white", 255.),
        filters::number(s, "outputBlack", 0.),
        filters::number(s, "outputWhite", 255.),
    ]
}

fn level_handle(values: &mut [f64; 5], output: bool, index: usize, position: f64) {
    let position = position.clamp(0., 255.);
    if output {
        values[if index == 0 { 3 } else { 4 }] = position.round();
    } else {
        match index {
            0 => values[0] = position.round().min(values[2] - 1.).max(0.),
            2 => values[2] = position.round().max(values[0] + 1.).min(255.),
            _ => {
                let fraction =
                    ((position - values[0]) / (values[2] - values[0]).max(1.)).clamp(0.001, 0.999);
                values[1] = (fraction.ln() / 0.5_f64.ln()).clamp(0.1, 9.99);
            }
        }
    }
}

fn level_handles(ui: &mut egui::Ui, values: &mut [f64; 5], output: bool) -> bool {
    let (response, painter) = ui.allocate_painter(
        Vec2::new(ui.available_width(), 20.),
        Sense::click_and_drag(),
    );
    let rect = response.rect.shrink2(Vec2::new(6., 0.));
    let positions = if output {
        vec![values[3], values[4]]
    } else {
        vec![
            values[0],
            values[0] + (values[2] - values[0]) * 0.5_f64.powf(values[1]),
            values[2],
        ]
    };
    let id = response.id.with("handle");
    if (response.drag_started() || response.clicked())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let position =
            ((pointer.x - rect.left()) / rect.width().max(1.)).clamp(0., 1.) as f64 * 255.;
        let nearest = positions
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (**a - position).abs().total_cmp(&(**b - position).abs()))
            .map_or(0, |(i, _)| i);
        ui.data_mut(|d| d.insert_temp(id, nearest));
    }
    let before = *values;
    if (response.clicked() || response.dragged())
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(index) = ui.data(|d| d.get_temp::<usize>(id))
    {
        level_handle(
            values,
            output,
            index,
            (pointer.x - rect.left()) as f64 / rect.width().max(1.) as f64 * 255.,
        );
    }
    if response.drag_stopped() || response.clicked() {
        ui.data_mut(|d| d.remove::<usize>(id));
    }
    let positions = if output {
        vec![values[3], values[4]]
    } else {
        vec![
            values[0],
            values[0] + (values[2] - values[0]) * 0.5_f64.powf(values[1]),
            values[2],
        ]
    };
    for (index, position) in positions.iter().enumerate() {
        let x = rect.left() + rect.width() * (*position / 255.).clamp(0., 1.) as f32;
        let color = if index == 0 {
            Color32::BLACK
        } else if index == positions.len() - 1 {
            Color32::WHITE
        } else {
            Color32::GRAY
        };
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(x, rect.top() + 1.),
                Pos2::new(x - 6., rect.top() + 12.),
                Pos2::new(x + 6., rect.top() + 12.),
            ],
            color,
            Stroke::new(0.75_f32, Color32::GRAY),
        ));
    }
    before != *values
}

fn histogram_scale(bins: &[u64; 256]) -> f64 {
    let peak = bins.iter().copied().max().unwrap_or(0) as f64;
    let mut interior = bins[1..255]
        .iter()
        .copied()
        .filter(|&n| n > 0)
        .collect::<Vec<_>>();
    if interior.is_empty() {
        return peak;
    }
    interior.sort_unstable();
    peak.min(interior[((interior.len() - 1) as f64 * 0.95).floor() as usize] as f64 * 4.)
}

fn cached_histogram_scales(ui: &egui::Ui, bins: &std::sync::Arc<[[u64; 256]; 4]>) -> [f64; 4] {
    let id = egui::Id::new("filter-histogram-display-scales");
    if let Some((previous, scales)) =
        ui.data(|data| data.get_temp::<(std::sync::Arc<[[u64; 256]; 4]>, [f64; 4])>(id))
        && std::sync::Arc::ptr_eq(&previous, bins)
    {
        return scales;
    }
    let scales = std::array::from_fn(|index| histogram_scale(&bins[index]));
    ui.data_mut(|data| data.insert_temp(id, (bins.clone(), scales)));
    scales
}

fn levels_histogram(ui: &mut egui::Ui, sample: Option<&Raster>, channel: usize) {
    let (response, painter) =
        ui.allocate_painter(Vec2::new(ui.available_width(), 150.), Sense::hover());
    let rect = response.rect.shrink2(Vec2::new(6., 0.));
    let colors = crate::chrome::theme::palette(ui.ctx(), ui.visuals());
    painter.rect_filled(rect, 0., colors.field);
    let Some(sample) = sample else {
        painter.text(
            rect.left_top() + Vec2::splat(8.),
            egui::Align2::LEFT_TOP,
            "打开图像以显示原始直方图",
            egui::FontId::proportional(11.),
            colors.secondary_text,
        );
        return;
    };
    let bins = cached_histogram_bins(ui, sample);
    let counts = &bins[if channel == 0 { 3 } else { channel - 1 }];
    let peak = cached_histogram_scales(ui, &bins)[if channel == 0 { 3 } else { channel - 1 }];
    if peak == 0. {
        return;
    }
    let color = [
        colors.secondary_text,
        Color32::RED,
        Color32::GREEN,
        Color32::BLUE,
    ][channel];
    for (index, &count) in counts.iter().enumerate() {
        let height = rect.height() * (count as f64 / peak).clamp(0., 1.) as f32;
        painter.rect_filled(
            egui::Rect::from_min_size(
                Pos2::new(
                    rect.left() + index as f32 * rect.width() / 256.,
                    rect.bottom() - height,
                ),
                Vec2::new(rect.width() / 256. + 0.1, height),
            ),
            0.,
            color,
        );
    }
}

fn level_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    range: RangeInclusive<f64>,
    decimals: usize,
    default: f64,
) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 5.;
        numeric_label(
            ui,
            label,
            value,
            &range,
            default,
            1. / 10_f64.powi(decimals as i32),
        );
        numeric_field(ui, value, range, decimals, 80.);
    });
}

fn levels_panel(
    ui: &mut egui::Ui,
    s: &mut Value,
    sample: Option<&Raster>,
    mut state: Option<&mut FilterUiState>,
) -> bool {
    use crate::filter_interactions::{LevelsAuto, LevelsSample, apply_levels_auto_bins};
    let mut changed =
        filter_channel_picker(ui, s, "channel", &["RGB", "Red", "Green", "Blue"], 180.);
    let index = match filters::string(s, "channel", "RGB") {
        "Red" => 1,
        "Green" => 2,
        "Blue" => 3,
        _ => 0,
    };
    ui.add_space(8.);
    levels_histogram(ui, sample, index);
    if let Some(range) = s
        .get_mut("ranges")
        .and_then(Value::as_array_mut)
        .and_then(|ranges| ranges.get_mut(index))
    {
        let mut values = level_values(range);
        let before = values;
        level_handles(ui, &mut values, false);
        ui.add_space(8.);
        ui.horizontal(|ui| {
            level_field(ui, "输入黑场", &mut values[0], 0. ..=254., 0, 0.);
            ui.add_space(((ui.available_width() - 160.) * 0.5).max(0.));
            level_field(ui, "伽马", &mut values[1], 0.1..=9.99, 2, 1.);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                level_field(ui, "输入白场", &mut values[2], 1. ..=255., 0, 255.);
            });
        });
        ui.add_space(16.);
        let (response, painter) =
            ui.allocate_painter(Vec2::new(ui.available_width(), 14.), Sense::hover());
        paint_gradient(
            &painter,
            response.rect.shrink2(Vec2::new(6., 0.)),
            SliderTrack::Opposing(Color32::BLACK, Color32::WHITE),
            Color32::GRAY,
        );
        level_handles(ui, &mut values, true);
        ui.add_space(8.);
        ui.horizontal(|ui| {
            level_field(ui, "输出黑场", &mut values[3], 0. ..=255., 0, 0.);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                level_field(ui, "输出白场", &mut values[4], 0. ..=255., 0, 255.);
            });
        });
        if values != before {
            if values[0] != before[0] {
                values[0] = values[0].min(values[2] - 1.);
            } else if values[2] != before[2] {
                values[2] = values[2].max(values[0] + 1.);
            }
            for (key, value) in ["black", "gamma", "white", "outputBlack", "outputWhite"]
                .into_iter()
                .zip(values)
            {
                range[key] = json!(value);
            }
            changed = true;
        }
    }
    if let Some(state) = state.as_deref_mut() {
        ui.add_space(16.);
        ui.horizontal(|ui| {
            ui.weak("取样");
            for (mode, label) in [
                (LevelsSample::Black, "黑场"),
                (LevelsSample::Gray, "灰场"),
                (LevelsSample::White, "白场"),
            ] {
                toggle_pointer(
                    ui,
                    state,
                    FilterPointer::Levels(mode),
                    label,
                    "点击原始图层以设置黑场、灰场或白场；再次点击结束取样",
                    sample.is_some(),
                );
            }
        });
        if matches!(state.pointer, Some(FilterPointer::Levels(_))) {
            ui.weak("点击原始图层取样；再次点击吸管结束。");
        }
    }
    ui.add_space(12.);
    ui.weak("自动");
    ui.horizontal(|ui| {
        for (mode, label) in [
            (LevelsAuto::Contrast, "对比度"),
            (LevelsAuto::Color, "颜色"),
            (LevelsAuto::Neutral, "颜色与中性"),
        ] {
            if ui
                .add_enabled(sample.is_some(), egui::Button::new(label))
                .clicked()
                && let Some(sample) = sample
            {
                changed |= apply_levels_auto_bins(s, &cached_histogram_bins(ui, sample), mode);
                if let Some(state) = state.as_deref_mut() {
                    state.stop_pointer();
                }
            }
        }
    });
    ui.add_space(12.);
    ui.horizontal(|ui| {
        if let Some(state) = state.as_deref_mut() {
            ui.checkbox(&mut state.preview, "预览");
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("重置").clicked() {
                *s = filters::default_settings("Levels")["levels"].clone();
                if let Some(state) = state {
                    state.stop_pointer();
                }
                changed = true;
            }
        });
    });
    ui.weak("原始像素 · 透明度加权直方图");
    changed
}

fn hue_saturation(ui: &mut egui::Ui, s: &mut Value) -> bool {
    hue_panel(ui, s, false, None)
}

fn hue_defaults(colorize: bool) -> Value {
    let mut value = filters::default_settings("Hue/Saturation")["hsvSettings"].clone();
    if colorize {
        value["colorize"] = json!(true);
        value["adjustments"]["Master"]["saturation"] = json!(25.);
    }
    value
}

fn hue_adjustment<'a>(s: &'a mut Value, range: &str) -> &'a mut Value {
    if s.get("adjustments").is_some() {
        &mut s["adjustments"][range]
    } else {
        s
    }
}

fn hue_row(
    ui: &mut egui::Ui,
    s: &mut Value,
    key: &str,
    label: &str,
    range: RangeInclusive<f64>,
    defaults: (f64, SliderTrack),
) -> bool {
    let (default, track) = defaults;
    let mut value = filters::number(s, key, default);
    let before = value;
    ui.push_id(key, |ui| {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(76., 22.),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    numeric_label(ui, label, &mut value, &range, default, 1.);
                },
            );
            let width = (ui.available_width() - 58.).max(24.);
            ui.allocate_ui_with_layout(
                Vec2::new(width, 22.),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    value_track(ui, &mut value, &range, default, 0, track);
                },
            );
            numeric_field(ui, &mut value, range, 0, 48.);
        });
    });
    if value != before {
        s[key] = json!(value);
        true
    } else {
        false
    }
}

fn hue_band_handles(band: &Value) -> [f64; 4] {
    ["falloffStart", "rangeStart", "rangeEnd", "falloffEnd"]
        .map(|key| filters::number(band, key, 0.))
}

fn family_hue_band(s: &Value, name: &str) -> Value {
    let values = family_hue_handles(s, name);
    json!({"falloffStart":values[0],"rangeStart":values[1],"rangeEnd":values[2],"falloffEnd":values[3]})
}

fn family_hue_handles(s: &Value, name: &str) -> [f64; 4] {
    let center = match name {
        "Yellows" => 60.,
        "Greens" => 120.,
        "Cyans" => 180.,
        "Blues" => 240.,
        "Magentas" => 300.,
        _ => 0.,
    };
    let defaults = [center - 45_f64, center - 15., center + 15., center + 45.]
        .map(|degree| degree.rem_euclid(360.));
    let keys = ["falloffStart", "rangeStart", "rangeEnd", "falloffEnd"];
    std::array::from_fn(|index| filters::number(&s["bands"][name], keys[index], defaults[index]))
}

fn set_hue_handle(band: &mut Value, index: usize, degrees: f64) -> bool {
    let mut values = hue_band_handles(band);
    if index >= values.len() || !degrees.is_finite() {
        return false;
    }
    values[index] = degrees.rem_euclid(360.);
    let forward = |a: f64, b: f64| (b - a).rem_euclid(360.);
    let span = forward(values[0], values[3]);
    let start = forward(values[0], values[1]);
    let end = forward(values[0], values[2]);
    if !(span > 1. && span <= 350. && start <= end && end <= span) {
        return false;
    }
    let key = ["falloffStart", "rangeStart", "rangeEnd", "falloffEnd"][index];
    if filters::number(band, key, 0.) == values[index] {
        false
    } else {
        band[key] = json!(values[index]);
        true
    }
}

// This display-only response uses the same range weights and evaluation order as the
// existing pixel filter; saturation and lightness do not alter the source hue legend.
fn shifted_spectrum_hue(hue: f64, s: &Value) -> f64 {
    if filters::boolean(s, "colorize", false) {
        return filters::number(hue_adjustment_read(s), "hue", 0.);
    }
    let selected = filters::string(s, "range", "Master");
    let Some(adjustments) = s["adjustments"].as_object() else {
        return (hue + filters::number(s, "hue", 0.)).rem_euclid(360.);
    };
    let mut shift = 0.;
    for name in [
        "Master", "Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas",
    ] {
        let Some(adjustment) = adjustments.get(name) else {
            continue;
        };
        let mut weight = 1.;
        if name != "Master" {
            let [a, b, c, d] = family_hue_handles(s, name);
            let span = (d - a).rem_euclid(360.);
            let pos = (hue - a).rem_euclid(360.);
            let lead = (b - a).rem_euclid(360.);
            let plateau = (c - a).rem_euclid(360.);
            weight = if span == 0. {
                1.
            } else if pos > span {
                0.
            } else if pos < lead {
                pos / lead.max(1e-10)
            } else if pos <= plateau {
                1.
            } else {
                (span - pos) / (span - plateau).max(1e-10)
            };
            if name == selected && filters::boolean(s, "invertRange", false) {
                weight = 1. - weight;
            }
        }
        shift += filters::number(adjustment, "hue", 0.) * weight;
    }
    (hue + shift).rem_euclid(360.)
}

fn hue_adjustment_read(s: &Value) -> &Value {
    if s.get("adjustments").is_some() {
        &s["adjustments"][filters::string(s, "range", "Master")]
    } else {
        s
    }
}

fn hue_spectrum(ui: &mut egui::Ui, s: &mut Value, range: &str) -> bool {
    let mut band = family_hue_band(s, range);
    let spectrum = |ui: &mut egui::Ui, s: &Value, after: bool| {
        let (response, painter) =
            ui.allocate_painter(Vec2::new(ui.available_width(), 16.), Sense::hover());
        let width = response.rect.width() / 72.;
        for index in 0..72 {
            let hue = index as f64 / 72. * 360.;
            let hue = if after {
                shifted_spectrum_hue(hue, s)
            } else {
                hue
            };
            painter.rect_filled(
                egui::Rect::from_min_size(
                    response.rect.left_top() + Vec2::new(index as f32 * width, 0.),
                    Vec2::new(width + 0.5, 16.),
                ),
                0.,
                hsv_color(hue, 1., 1.),
            );
        }
    };
    spectrum(ui, s, false);
    let (response, painter) = ui.allocate_painter(
        Vec2::new(ui.available_width(), 12.),
        Sense::click_and_drag(),
    );
    let rect = response.rect;
    let id = response.id.with("hue-handle");
    let degrees = |pointer: Pos2| {
        ((pointer.x - rect.left()) / rect.width().max(1.)).clamp(0., 1.) as f64 * 360.
    };
    if (response.drag_started() || response.clicked())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let d = degrees(pointer);
        let handles = hue_band_handles(&band);
        let distance = |h: f64| {
            let gap = (h - d).abs().rem_euclid(360.);
            gap.min(360. - gap)
        };
        let index = handles
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| distance(**a).total_cmp(&distance(**b)))
            .map_or(0, |(index, _)| index);
        ui.data_mut(|data| data.insert_temp(id, index));
    }
    let mut changed = false;
    if (response.dragged() || response.clicked())
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(index) = ui.data(|data| data.get_temp::<usize>(id))
    {
        changed |= set_hue_handle(&mut band, index, degrees(pointer));
    }
    if response.drag_stopped() || response.clicked() {
        ui.data_mut(|data| data.remove::<usize>(id));
    }
    let handles = hue_band_handles(&band);
    if changed {
        s["bands"][range] = band;
    }
    for (index, &degree) in handles.iter().enumerate() {
        let x = rect.left() + rect.width() * (degree / 360.) as f32;
        let inner = index == 1 || index == 2;
        painter.rect_filled(
            egui::Rect::from_center_size(
                Pos2::new(x, rect.center().y),
                if inner {
                    Vec2::new(2., 12.)
                } else {
                    Vec2::new(7., 5.)
                },
            ),
            0.,
            ui.visuals().text_color(),
        );
    }
    spectrum(ui, s, true);
    ui.label(
        egui::RichText::new(handles.map(|x| format!("{x:.0}°")).join("   "))
            .monospace()
            .small()
            .weak(),
    );
    changed
}

fn hue_panel(
    ui: &mut egui::Ui,
    s: &mut Value,
    can_sample: bool,
    mut state: Option<&mut FilterUiState>,
) -> bool {
    use crate::filter_interactions::HueSampleMode;
    let mut changed = false;
    let colorize = filters::boolean(s, "colorize", false);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!colorize, |ui| {
            changed |= filter_channel_picker(
                ui,
                s,
                "range",
                &[
                    "Master", "Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas",
                ],
                160.,
            );
        });
        if let Some(state) = state.as_deref_mut()
            && !colorize
        {
            let family = filters::string(s, "range", "Master") != "Master";
            if family {
                for (mode, label, help) in [
                    (
                        HueSampleMode::Replace,
                        "取样",
                        "点击图像，把范围中心设到取样颜色",
                    ),
                    (HueSampleMode::Add, "+", "点击图像，扩大范围以包含取样颜色"),
                    (
                        HueSampleMode::Remove,
                        "−",
                        "点击图像，缩小范围以排除取样颜色",
                    ),
                ] {
                    toggle_pointer(ui, state, FilterPointer::Hue(mode), label, help, can_sample);
                }
            }
            toggle_pointer(
                ui,
                state,
                FilterPointer::HueTargeted,
                "目标",
                "在图像上拖动调整该颜色的饱和度；按 Ctrl 拖动调整色相",
                can_sample,
            );
        }
    });
    let range = filters::string(s, "range", "Master").to_owned();
    let center = match range.as_str() {
        "Yellows" => 60.,
        "Greens" => 120.,
        "Cyans" => 180.,
        "Blues" => 240.,
        "Magentas" => 300.,
        _ => 0.,
    };
    let adjustment = hue_adjustment(s, &range);
    ui.add_space(16.);
    changed |= hue_row(
        ui,
        adjustment,
        "hue",
        "色相",
        if colorize { 0. ..=360. } else { -180. ..=180. },
        (
            0.,
            SliderTrack::Spectrum(if colorize { 180. } else { center }),
        ),
    );
    let current_hue = filters::number(adjustment, "hue", 0.);
    ui.add_space(12.);
    changed |= hue_row(
        ui,
        adjustment,
        "saturation",
        "饱和度",
        if colorize { 0. ..=100. } else { -100. ..=100. },
        (
            if colorize { 25. } else { 0. },
            if colorize {
                SliderTrack::Saturation(current_hue)
            } else if range == "Master" {
                SliderTrack::Chroma
            } else {
                SliderTrack::Saturation(center)
            },
        ),
    );
    ui.add_space(12.);
    changed |= hue_row(
        ui,
        adjustment,
        "lightness",
        "明度",
        -100. ..=100.,
        (0., SliderTrack::Opposing(Color32::BLACK, Color32::WHITE)),
    );
    if range != "Master" && !colorize {
        ui.add_space(16.);
        changed |= hue_spectrum(ui, s, &range);
        changed |= toggle(ui, s, "invertRange", "应用到此范围以外", false);
    }
    ui.add_space(16.);
    ui.horizontal(|ui| {
        let mut next = colorize;
        if ui.checkbox(&mut next, "着色").changed() {
            *s = hue_defaults(next);
            changed = true;
            if let Some(state) = state.as_deref_mut() {
                state.stop_pointer();
            }
        }
        if let Some(state) = state.as_deref_mut() {
            ui.checkbox(&mut state.preview, "预览");
        }
        if ui.button("重置").clicked() {
            *s = hue_defaults(colorize);
            changed = true;
            if let Some(state) = state {
                state.stop_pointer();
            }
        }
    });
    changed
}

fn curves(ui: &mut egui::Ui, s: &mut Value, extent: f64) -> bool {
    let mut changed = choice(ui, s, "channel", "通道", &["RGB", "Red", "Green", "Blue"]);
    let index = match filters::string(s, "channel", "RGB") {
        "Red" => 1,
        "Green" => 2,
        "Blue" => 3,
        _ => 0,
    };
    if let Some(channels) = s.get_mut("channels").and_then(Value::as_array_mut)
        && let Some(points) = channels.get_mut(index)
    {
        changed |= curve_editor(ui, points, extent);
    }
    changed
}
fn curve_editor(ui: &mut egui::Ui, points: &mut Value, extent: f64) -> bool {
    let Some(data) = points.as_array() else {
        return false;
    };
    let positions = data
        .iter()
        .map(|p| [filters::number(p, "x", 0.), filters::number(p, "y", 0.)])
        .collect::<Vec<_>>();
    let (response, painter) = ui.allocate_painter(
        Vec2::new(ui.available_width().min(320.), 200.),
        Sense::click_and_drag(),
    );
    let rect = response.rect.shrink(9.);
    let colors = crate::chrome::theme::palette(ui.ctx(), ui.visuals());
    painter.rect_filled(rect, 2., colors.field);
    for i in 0..=4 {
        let t = i as f32 / 4.;
        let x = rect.left() + rect.width() * t;
        let y = rect.bottom() - rect.height() * t;
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1_f32, colors.divider),
        );
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1_f32, colors.divider),
        );
    }
    let position = |p: [f64; 2]| {
        Pos2::new(
            rect.left() + rect.width() * (p[0] / extent) as f32,
            rect.bottom() - rect.height() * (p[1] / extent) as f32,
        )
    };
    let line = (0..=128)
        .map(|i| {
            let x = i as f64 / 128. * extent;
            position([x, filters::curve(x, points, extent)])
        })
        .collect::<Vec<_>>();
    painter.add(egui::Shape::line(
        line,
        Stroke::new(2_f32, Color32::from_rgb(90, 185, 240)),
    ));
    for &p in &positions {
        painter.circle_filled(position(p), 4., colors.text);
    }
    let mut changed = false;
    if response.double_clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let x = ((pointer.x - rect.left()) / rect.width()).clamp(0., 1.) as f64 * extent;
        let y = ((rect.bottom() - pointer.y) / rect.height()).clamp(0., 1.) as f64 * extent;
        if let Some(data) = points.as_array_mut() {
            data.push(json!({"x":x,"y":y}));
            data.sort_by(|a, b| {
                filters::number(a, "x", 0.).total_cmp(&filters::number(b, "x", 0.))
            });
            changed = true;
        }
    }
    if response.drag_started()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let nearest = positions
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                position(**a)
                    .distance_sq(pointer)
                    .total_cmp(&position(**b).distance_sq(pointer))
            })
            .map(|(i, _)| i);
        ui.ctx().data_mut(|d| {
            d.insert_temp(response.id.with("point"), nearest);
        });
    }
    if response.dragged()
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(Some(index)) = ui
            .ctx()
            .data(|d| d.get_temp::<Option<usize>>(response.id.with("point")))
        && let Some(data) = points.as_array_mut()
    {
        let previous = if index == 0 {
            0.
        } else {
            filters::number(&data[index - 1], "x", 0.) + extent / 1000.
        };
        let next = if index + 1 == data.len() {
            extent
        } else {
            filters::number(&data[index + 1], "x", extent) - extent / 1000.
        };
        let x = if index == 0 {
            0.
        } else if index + 1 == data.len() {
            extent
        } else {
            (((pointer.x - rect.left()) / rect.width()) as f64 * extent)
                .clamp(previous, next.max(previous))
        };
        let y = ((rect.bottom() - pointer.y) / rect.height()).clamp(0., 1.) as f64 * extent;
        data[index] = json!({"x":x,"y":y});
        changed = true;
    }
    ui.label("拖动控制点；双击添加控制点。");
    if ui.small_button("重置曲线").clicked() {
        *points = json!([{"x":0.,"y":0.},{"x":extent,"y":extent}]);
        changed = true;
    }
    if let Some(data) = points.as_array_mut() {
        let mut remove = None;
        for (i, p) in data.iter_mut().enumerate() {
            ui.push_id(i, |ui| {
                ui.horizontal(|ui| {
                    let mut x = filters::number(p, "x", 0.);
                    let mut y = filters::number(p, "y", 0.);
                    let a = ui.add(
                        egui::DragValue::new(&mut x)
                            .range(0.0..=extent)
                            .prefix("输入 "),
                    );
                    let b = ui.add(
                        egui::DragValue::new(&mut y)
                            .range(0.0..=extent)
                            .prefix("输出 "),
                    );
                    if a.changed() || b.changed() {
                        *p = json!({"x":x,"y":y});
                        changed = true;
                    }
                    if i > 0 && i + 1 < positions.len() && ui.small_button("×").clicked() {
                        remove = Some(i);
                    }
                });
            });
        }
        if let Some(i) = remove {
            data.remove(i);
            changed = true;
        }
        if changed {
            data.sort_by(|a, b| {
                filters::number(a, "x", 0.).total_cmp(&filters::number(b, "x", 0.))
            });
        }
    }
    changed
}
fn scanlines(ui: &mut egui::Ui, s: &mut Value) -> bool {
    let mut changed = controls(
        ui,
        s,
        &[
            ("lineSpacing", "扫描线间距", 2., 32., 4.),
            ("thickness", "线条厚度", 5., 100., 70.),
            ("glow", "辉光", 0., 100., 0.),
            ("dots", "点状纹理", 0., 100., 0.),
            ("displace", "亮度位移", -100., 100., 0.),
            ("smoothness", "位移平滑", 0., 100., 50.),
            ("threshold", "亮度阈值", 0., 100., 0.),
            ("wobble", "波动", 0., 64., 0.),
            ("split", "颜色分离", 0., 16., 0.),
            ("density", "密度", -100., 100., 0.),
            ("contrast", "对比度", -100., 100., 0.),
            ("blackLevel", "黑电平", 0., 100., 0.),
        ],
    );
    changed |= choice(
        ui,
        s,
        "colors",
        "配色",
        &["Black & White", "Two Colors", "Original"],
    );
    if filters::string(s, "colors", "Black & White") == "Two Colors" {
        changed |= color(ui, &mut s["dark"], "屏幕颜色", [0.; 3]);
        changed |= color(ui, &mut s["light"], "线条颜色", [1.; 3]);
    }
    changed
}

fn dither(ui: &mut egui::Ui, s: &mut Value) -> bool {
    let mut changed = choice(
        ui,
        s,
        "style",
        "算法",
        &[
            "Atkinson (Classic Mac)",
            "Floyd–Steinberg",
            "Bayer 2 × 2",
            "Bayer 4 × 4",
            "Bayer 8 × 8",
            "Halftone Dots",
            "Halftone Lines",
            "Halftone Diamonds",
            "Mac Patterns",
            "ASCII",
            "Scanlines (CRT)",
        ],
    );
    let style = filters::string(s, "style", "Atkinson (Classic Mac)").to_owned();
    let diffuses = matches!(style.as_str(), "Atkinson (Classic Mac)" | "Floyd–Steinberg");
    let has_tones = diffuses
        || matches!(
            style.as_str(),
            "Bayer 2 × 2" | "Bayer 4 × 4" | "Bayer 8 × 8"
        );
    let halftone = matches!(
        style.as_str(),
        "Halftone Dots" | "Halftone Lines" | "Halftone Diamonds"
    );
    let ascii = style == "ASCII";
    let scanlines = style == "Scanlines (CRT)";
    if !ascii && !scanlines {
        changed |= number(ui, s, "pixelSize", "像素大小", 1.0..=32.0, 2.0);
    }
    if ascii {
        changed |= number(ui, s, "textSize", "字符大小", 6.0..=64.0, 14.0);
        let mut text = filters::string(s, "characters", " .:-=+*#%@").to_owned();
        ui.label("字符集");
        if ui.text_edit_singleline(&mut text).changed() {
            s["characters"] = json!(
                text.chars()
                    .filter(|character| !character.is_control())
                    .take(64)
                    .collect::<String>()
            );
            changed = true;
        }
    }
    if scanlines {
        changed |= controls(
            ui,
            s,
            &[
                ("lineSpacing", "扫描线间距", 2., 32., 4.),
                ("glow", "辉光", 0., 100., 35.),
                ("dots", "点状纹理", 0., 100., 0.),
                ("wobble", "波动", 0., 64., 0.),
            ],
        );
    }
    if halftone {
        changed |= controls(
            ui,
            s,
            &[
                ("cellSize", "网点大小", 4., 64., 8.),
                ("angle", "角度", -90., 90., 45.),
            ],
        );
    }
    if has_tones {
        changed |= number(ui, s, "levels", "色阶数", 2.0..=8.0, 2.0);
    }
    if diffuses {
        changed |= number(ui, s, "diffusion", "扩散", 0.0..=100.0, 100.0);
    }
    changed |= controls(
        ui,
        s,
        &[
            ("density", "密度", -100., 100., 0.),
            ("contrast", "对比度", -100., 100., 0.),
        ],
    );
    changed |= choice(
        ui,
        s,
        "colors",
        "配色",
        &["Black & White", "Two Colors", "Original"],
    );
    if filters::string(s, "colors", "Black & White") == "Two Colors" {
        changed |= color(ui, &mut s["dark"], "暗色", [0.; 3]);
        changed |= color(ui, &mut s["light"], "亮色", [1.; 3]);
    }
    if !ascii && !scanlines && filters::number(s, "pixelSize", 2.).round() > 1. {
        changed |= choice(ui, s, "pixelShape", "像素形状", &["Square", "Dot"]);
    }
    if halftone || matches!(style.as_str(), "Mac Patterns" | "ASCII") {
        changed |= toggle(ui, s, "lightOnDark", "暗底亮字", true);
    }
    changed
}

/// Panel eye states are transient; disabled groups render neutral values while retaining slider values.
/// A wrapper is preserved so callers may pass either direct settings or a v11 filter record.
pub fn effective_camera_raw_settings(settings: &Value) -> Value {
    let mut result = settings.clone();
    let raw = if result.get("cameraRaw").is_some() {
        &mut result["cameraRaw"]
    } else {
        &mut result
    };
    let enabled = raw.get("_enabledSections").cloned().unwrap_or(Value::Null);
    let defaults = filters::camera_raw_defaults();
    for (section, fields) in [
        (
            "light",
            &[
                "exposure",
                "contrast",
                "highlights",
                "shadows",
                "whites",
                "blacks",
            ][..],
        ),
        (
            "color",
            &["temperature", "tint", "vibrance", "saturation"][..],
        ),
        (
            "effects",
            &[
                "texture",
                "clarity",
                "dehaze",
                "glow",
                "vignetteAmount",
                "grainAmount",
            ][..],
        ),
    ] {
        if !filters::boolean(&enabled, section, true) {
            for field in fields {
                raw[*field] = json!(0.);
            }
        }
    }
    for section in [
        "curve",
        "mixer",
        "grading",
        "detail",
        "optics",
        "geometry",
        "calibration",
    ] {
        if !filters::boolean(&enabled, section, true) {
            raw[section] = defaults[section].clone();
        }
    }
    match raw["_altDiagnostic"].as_i64() {
        Some(1 | 2) => raw["clipping"] = raw["_altDiagnostic"].clone(),
        Some(3) => raw["detail"]["showSharpenMask"] = json!(true),
        _ => {}
    }
    if let Some(object) = raw.as_object_mut() {
        object.remove("_enabledSections");
        object.remove("_altDiagnostic");
    }
    result
}

fn raw_section_title(key: &str) -> &str {
    match key {
        "rgb" => "RGB 复合通道",
        "red" => "红通道",
        "green" => "绿通道",
        "blue" => "蓝通道",
        "hue" => "色相",
        "saturation" => "饱和度",
        "luminance" => "亮度",
        "shadows" => "阴影",
        "midtones" => "中间调",
        "highlights" => "高光",
        "global" => "全局",
        _ => key,
    }
}

const CAMERA_RAW_RESET_FIELDS: &[(&str, &[&str])] = &[
    (
        "light",
        &[
            "exposure",
            "contrast",
            "highlights",
            "shadows",
            "whites",
            "blacks",
        ],
    ),
    (
        "color",
        &[
            "whiteBalance",
            "temperature",
            "tint",
            "vibrance",
            "saturation",
        ],
    ),
    (
        "effects",
        &[
            "texture",
            "clarity",
            "dehaze",
            "glow",
            "glowStyle",
            "glowRange",
            "glowSpread",
            "glowWarmth",
            "vignetteAmount",
            "vignetteStyle",
            "vignetteMidpoint",
            "vignetteRoundness",
            "vignetteFeather",
            "vignetteHighlights",
            "grainAmount",
            "grainSize",
            "grainRoughness",
            "seed",
        ],
    ),
    ("curve", &["curve"]),
    ("mixer", &["mixer"]),
    ("grading", &["grading"]),
    ("detail", &["detail"]),
    ("optics", &["optics"]),
    ("geometry", &["geometry"]),
    ("calibration", &["calibration"]),
];

fn raw_default_matches(value: &Value, default: &Value) -> bool {
    match (value, default) {
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::Object(left), Value::Object(right)) => right.iter().all(|(key, default)| {
            left.get(key)
                .is_none_or(|value| raw_default_matches(value, default))
        }),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(value, default)| raw_default_matches(value, default))
        }
        _ => value == default,
    }
}

fn camera_raw_section_is_default(raw: &Value, section: &str, defaults: &Value) -> bool {
    CAMERA_RAW_RESET_FIELDS
        .iter()
        .find(|(key, _)| *key == section)
        .is_none_or(|(_, fields)| {
            fields.iter().all(|field| {
                raw.get(*field)
                    .is_none_or(|value| raw_default_matches(value, &defaults[*field]))
            })
        })
}

/// Restore a section's choices and midpoint defaults as well as its amount sliders.
pub fn reset_camera_raw_section(settings: &mut Value, section: &str) -> bool {
    let Some((_, fields)) = CAMERA_RAW_RESET_FIELDS
        .iter()
        .find(|(key, _)| *key == section)
    else {
        return false;
    };
    let raw = if settings.get("cameraRaw").is_some() {
        &mut settings["cameraRaw"]
    } else {
        settings
    };
    if !raw.is_object() {
        return false;
    }
    let before = raw.clone();
    let defaults = filters::camera_raw_defaults();
    for field in *fields {
        raw[*field] = defaults[*field].clone();
    }
    if !filters::boolean(&raw["_enabledSections"], section, true) {
        raw["_enabledSections"][section] = json!(true);
    }
    *raw != before
}

/// Consume one panel reset so the editor can clear a selected color point or guide draft.
pub fn take_camera_raw_section_reset(ctx: &egui::Context) -> Option<String> {
    ctx.data_mut(|data| data.remove_temp(egui::Id::new("camera-raw-section-reset")))
}

fn camera_raw(
    ui: &mut egui::Ui,
    s: &mut Value,
    sample: Option<&Raster>,
    original_sample: Option<&Raster>,
) -> bool {
    let mut changed = false;
    let mut diagnostic = 0;
    let defaults = filters::camera_raw_defaults();
    egui::CollapsingHeader::new("启用分区")
        .id_salt("Enabled sections")
        .show(ui, |ui| {
            egui::Grid::new("camera-raw-enabled-sections")
                .num_columns(2)
                .show(ui, |ui| {
                    for (i, (key, label)) in [
                        ("light", "Light"),
                        ("color", "Color"),
                        ("effects", "Effects"),
                        ("curve", "Tone Curve"),
                        ("mixer", "Color Mixer"),
                        ("grading", "Color Grading"),
                        ("detail", "Detail"),
                        ("optics", "Optics"),
                        ("geometry", "Geometry"),
                        ("calibration", "Calibration"),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let mut enabled = filters::boolean(&s["_enabledSections"], key, true);
                        ui.horizontal(|ui| {
                            if ui
                                .checkbox(
                                    &mut enabled,
                                    match key {
                                        "light" => "光线",
                                        "color" => "颜色",
                                        "effects" => "效果",
                                        "curve" => "色调曲线",
                                        "mixer" => "混色器",
                                        "grading" => "颜色分级",
                                        "detail" => "细节",
                                        "optics" => "光学",
                                        "geometry" => "几何",
                                        "calibration" => "校准",
                                        _ => label,
                                    },
                                )
                                .changed()
                            {
                                s["_enabledSections"][key] = json!(enabled);
                                changed = true;
                            }
                            if ui
                                .add_enabled(
                                    !enabled || !camera_raw_section_is_default(s, key, &defaults),
                                    egui::Button::new("重置").small(),
                                )
                                .on_hover_text("将这个分区恢复默认值，保留其他分区的调整")
                                .clicked()
                            {
                                changed |= reset_camera_raw_section(s, key);
                                ui.data_mut(|data| {
                                    data.insert_temp(
                                        egui::Id::new("camera-raw-section-reset"),
                                        key.to_owned(),
                                    )
                                });
                            }
                        });
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });
        });
    if let Some(sample) = sample {
        let id = ui.make_persistent_id("camera-raw-scope-mode");
        let mut vector = ui.data(|data| data.get_temp::<bool>(id)).unwrap_or(false);
        let response = ui
            .scope(|ui| {
                if vector {
                    vectorscope(ui, sample);
                } else {
                    histogram(ui, sample);
                }
            })
            .response
            .interact(Sense::click());
        response.context_menu(|ui| {
            if ui.selectable_value(&mut vector, false, "直方图").clicked() {
                ui.close();
            }
            if ui
                .selectable_value(&mut vector, true, "矢量示波器")
                .clicked()
            {
                ui.close();
            }
        });
        ui.data_mut(|data| data.insert_temp(id, vector));
    }
    egui::CollapsingHeader::new("光线与白平衡")
        .id_salt("Light and White Balance")
        .default_open(true)
        .show(ui, |ui| {
            let balance_changed = choice(ui, s, "whiteBalance", "白平衡", &["Custom", "Auto"]);
            changed |= balance_changed;
            if filters::string(s, "whiteBalance", "Custom") == "Auto" {
                let recalculate = ui
                    .add_enabled(original_sample.is_some(), egui::Button::new("计算白平衡"))
                    .clicked();
                if (balance_changed || recalculate)
                    && let Some([temperature, tint]) = original_sample.and_then(auto_white_balance)
                {
                    s["temperature"] = json!(temperature.clamp(-100., 100.));
                    s["tint"] = json!(tint.clamp(-100., 100.));
                    changed = true;
                }
            }
            changed |= raw_controls(
                ui,
                s,
                &[
                    ("temperature", "色温", -100., 100., 0.),
                    ("tint", "色调", -100., 100., 0.),
                    ("exposure", "曝光", -5., 5., 0.),
                    ("contrast", "对比度", -100., 100., 0.),
                    ("highlights", "高光", -100., 100., 0.),
                    ("shadows", "阴影", -100., 100., 0.),
                    ("whites", "白色", -100., 100., 0.),
                    ("blacks", "黑色", -100., 100., 0.),
                    ("vibrance", "自然饱和度", -100., 100., 0.),
                    ("saturation", "饱和度", -100., 100., 0.),
                ],
                &mut diagnostic,
            );
            changed |= toggle(ui, s, "showShadowClipping", "显示阴影剪切", false);
            changed |= toggle(ui, s, "showHighlightClipping", "显示高光剪切", false);
        });
    egui::CollapsingHeader::new("效果")
        .id_salt("Effects")
        .show(ui, |ui| {
            changed |= controls(
                ui,
                s,
                &[
                    ("texture", "纹理", -100., 100., 0.),
                    ("clarity", "清晰度", -100., 100., 0.),
                    ("dehaze", "去朦胧", -100., 100., 0.),
                    ("glow", "辉光", 0., 100., 0.),
                    ("glowRange", "辉光范围", -100., 100., 0.),
                    ("glowSpread", "辉光扩散", -100., 100., 0.),
                    ("glowWarmth", "辉光暖度", -100., 100., 0.),
                ],
            );
            changed |= choice(
                ui,
                s,
                "glowStyle",
                "辉光样式",
                &["Diffusion", "Bloom", "Halation"],
            );
            changed |= choice(
                ui,
                s,
                "vignetteStyle",
                "暗角样式",
                &["Highlight Priority", "Color Priority", "Paint Overlay"],
            );
            changed |= controls(
                ui,
                s,
                &[
                    ("vignetteAmount", "暗角", -100., 100., 0.),
                    ("vignetteMidpoint", "中点", 0., 100., 50.),
                    ("vignetteRoundness", "圆度", -100., 100., 0.),
                    ("vignetteFeather", "羽化", 0., 100., 50.),
                    ("vignetteHighlights", "高光", 0., 100., 0.),
                    ("grainAmount", "颗粒数量", 0., 100., 0.),
                    ("grainSize", "颗粒大小", 0., 100., 25.),
                    ("grainRoughness", "粗糙度", 0., 100., 50.),
                    ("seed", "随机种子", 0., 65535., 0.),
                ],
            );
        });
    egui::CollapsingHeader::new("色调曲线")
        .id_salt("Tone Curve")
        .show(ui, |ui| {
            let c = &mut s["curve"];
            changed |= controls(
                ui,
                c,
                &[
                    ("shadows", "阴影", -100., 100., 0.),
                    ("darks", "暗调", -100., 100., 0.),
                    ("lights", "亮调", -100., 100., 0.),
                    ("highlights", "高光", -100., 100., 0.),
                    ("shadowSplit", "阴影分界", 0., 100., 25.),
                    ("darkSplit", "中间调分界", 0., 100., 50.),
                    ("lightSplit", "高光分界", 0., 100., 75.),
                    ("refineSaturation", "饱和度优化", -100., 100., 0.),
                ],
            );
            for key in ["rgb", "red", "green", "blue"] {
                ui.push_id(key, |ui| {
                    egui::CollapsingHeader::new(raw_section_title(key))
                        .id_salt(key)
                        .show(ui, |ui| {
                            changed |= curve_editor(ui, &mut c[key], 1.);
                        });
                });
            }
        });
    egui::CollapsingHeader::new("混色器").id_salt("Color Mixer").show(ui,|ui|{
        for key in ["hue","saturation","luminance"]{ui.push_id(key,|ui|{egui::CollapsingHeader::new(raw_section_title(key)).id_salt(key).show(ui,|ui|{
            if let Some(values)=s["mixer"][key].as_array_mut(){for (i,value) in values.iter_mut().enumerate(){let mut n=value.as_f64().unwrap_or(0.);if ui.add(egui::Slider::new(&mut n,-100.0..=100.0).text(["红色","橙色","黄色","绿色","浅绿色","蓝色","紫色","洋红色"][i.min(7)])).changed(){*value=json!(n);changed=true;}}}
        });});}
        if let Some(points)=s["mixer"]["points"].as_array_mut(){
            let mut remove=None;
            for (index,point) in points.iter_mut().enumerate(){ui.push_id(index,|ui|{egui::CollapsingHeader::new(format!("点颜色 {}",index+1)).id_salt(format!("Point color {}",index+1)).show(ui,|ui|{
                changed|=controls(ui,point,&[("hue","样本色相",0.,360.,0.),("saturation","样本饱和度",0.,1.,0.),("luminance","样本亮度",0.,1.,0.),("hueShift","色相",-100.,100.,0.),("saturationShift","饱和度",-100.,100.,0.),("luminanceShift","亮度",-100.,100.,0.),("hueRange","色相范围",0.,180.,30.),("saturationRange","饱和度范围",0.,1.,0.4),("luminanceRange","亮度范围",0.,1.,0.4)]);
                changed|=toggle(ui,point,"visualize","显示范围",false);
                if ui.small_button("移除色点").clicked(){remove=Some(index);}
            });});}
            if let Some(index)=remove{points.remove(index);changed=true;}
            if ui.button("添加点颜色").clicked(){points.push(json!({"hue":0,"saturation":0,"luminance":0,"hueShift":0,"saturationShift":0,"luminanceShift":0,"hueRange":30,"saturationRange":0.4,"luminanceRange":0.4,"visualize":false}));changed=true;}
        }
    });
    egui::CollapsingHeader::new("颜色分级")
        .id_salt("Color Grading")
        .show(ui, |ui| {
            for key in ["shadows", "midtones", "highlights", "global"] {
                ui.push_id(key, |ui| {
                    egui::CollapsingHeader::new(raw_section_title(key))
                        .id_salt(key)
                        .show(ui, |ui| {
                            changed |= controls(
                                ui,
                                &mut s["grading"][key],
                                &[
                                    ("hue", "色相", 0., 360., 0.),
                                    ("saturation", "饱和度", 0., 100., 0.),
                                    ("luminance", "亮度", -100., 100., 0.),
                                ],
                            );
                        });
                });
            }
            changed |= controls(
                ui,
                &mut s["grading"],
                &[
                    ("blending", "混合", 0., 100., 50.),
                    ("balance", "平衡", -100., 100., 0.),
                ],
            );
        });
    egui::CollapsingHeader::new("细节")
        .id_salt("Detail")
        .show(ui, |ui| {
            let detail = &mut s["detail"];
            changed |= raw_controls(
                ui,
                detail,
                &[
                    ("sharpenAmount", "锐化", 0., 150., 0.),
                    ("sharpenRadius", "半径", 0., 100., 10.),
                    ("sharpenDetail", "细节", 0., 100., 25.),
                    ("sharpenMasking", "蒙版", 0., 100., 0.),
                    ("noiseLuminance", "明亮度降噪", 0., 100., 0.),
                    ("noiseLuminanceDetail", "明亮度细节", 0., 100., 50.),
                    ("noiseLuminanceContrast", "明亮度对比度", 0., 100., 0.),
                    ("noiseColor", "颜色降噪", 0., 100., 0.),
                    ("noiseColorDetail", "颜色细节", 0., 100., 50.),
                    ("noiseColorSmoothness", "颜色平滑度", 0., 100., 50.),
                ],
                &mut diagnostic,
            );
            changed |= toggle(ui, detail, "showSharpenMask", "显示锐化蒙版", false);
        });
    egui::CollapsingHeader::new("光学")
        .id_salt("Optics")
        .show(ui, |ui| {
            let o = &mut s["optics"];
            changed |= toggle(ui, o, "removeChromaticAberration", "移除色差", false);
            changed |= toggle(ui, o, "enableLensProfile", "启用镜头配置文件", false);
            changed |= controls(
                ui,
                o,
                &[
                    ("profileDistortion", "配置文件畸变", 0., 100., 100.),
                    ("profileVignetting", "配置文件暗角", 0., 100., 100.),
                    ("distortion", "畸变", -100., 100., 0.),
                    ("purpleAmount", "移除紫边", 0., 100., 0.),
                    ("purpleHueLow", "紫色色相下限", 0., 360., 270.),
                    ("purpleHueHigh", "紫色色相上限", 0., 360., 310.),
                    ("greenAmount", "移除绿边", 0., 100., 0.),
                    ("greenHueLow", "绿色色相下限", 0., 360., 60.),
                    ("greenHueHigh", "绿色色相上限", 0., 360., 120.),
                    ("vignetteAmount", "暗角", -100., 100., 0.),
                    ("vignetteMidpoint", "中点", 0., 100., 50.),
                ],
            );
        });
    egui::CollapsingHeader::new("几何")
        .id_salt("Geometry")
        .show(ui, |ui| {
            let g = &mut s["geometry"];
            changed |= choice(ui, g, "upright", "透视校正", &["Off", "Guided"]);
            changed |= choice(ui, g, "projection", "投影", &["Perspective", "Rectilinear"]);
            changed |= controls(
                ui,
                g,
                &[
                    ("vertical", "垂直", -100., 100., 0.),
                    ("horizontal", "水平", -100., 100., 0.),
                    ("rotate", "旋转", -45., 45., 0.),
                    ("aspect", "长宽比", -100., 100., 0.),
                    ("scale", "缩放", -50., 100., 0.),
                    ("offsetX", "水平偏移", -100., 100., 0.),
                    ("offsetY", "垂直偏移", -100., 100., 0.),
                ],
            );
            changed |= toggle(ui, g, "constrainCrop", "约束裁剪", false);
            if filters::string(g, "upright", "Off") == "Guided"
                && let Some(guides) = g["guides"].as_array_mut()
            {
                for (i, guide) in guides.iter_mut().enumerate() {
                    ui.push_id(i, |ui| {
                        ui.label(format!("参考线 {}", i + 1));
                        changed |= controls(
                            ui,
                            guide,
                            &[
                                ("startX", "起点 X", 0., 1., 0.),
                                ("startY", "起点 Y", 0., 1., 0.),
                                ("endX", "终点 X", 0., 1., 1.),
                                ("endY", "终点 Y", 0., 1., 1.),
                            ],
                        );
                    });
                }
            }
        });
    egui::CollapsingHeader::new("校准")
        .id_salt("Calibration")
        .show(ui, |ui| {
            let c = &mut s["calibration"];
            changed |= choice(
                ui,
                c,
                "process",
                "处理版本",
                &[
                    "Version 6",
                    "Version 5",
                    "Version 4",
                    "Version 3",
                    "Version 2",
                    "Version 1",
                ],
            );
            changed |= controls(
                ui,
                c,
                &[
                    ("shadowTint", "阴影色调", -100., 100., 0.),
                    ("redHue", "红原色色相", -100., 100., 0.),
                    ("redSaturation", "红原色饱和度", -100., 100., 0.),
                    ("greenHue", "绿原色色相", -100., 100., 0.),
                    ("greenSaturation", "绿原色饱和度", -100., 100., 0.),
                    ("blueHue", "蓝原色色相", -100., 100., 0.),
                    ("blueSaturation", "蓝原色饱和度", -100., 100., 0.),
                ],
            );
        });
    changed | set_raw_diagnostic(s, diagnostic)
}

/// Average in linear light, then neutralize using Camera Raw's measured white-balance tables.
pub fn auto_white_balance(sample: &Raster) -> Option<[f64; 2]> {
    let mut sum = [0.; 3];
    let mut count = 0.;
    let step = sample.width.max(sample.height).div_ceil(31).max(1) as usize;
    for y in (0..sample.height).step_by(step) {
        for x in (0..sample.width).step_by(step) {
            let p = sample.get(x, y);
            if p[3] == 0 {
                continue;
            }
            for c in 0..3 {
                sum[c] += filters::srgb_to_linear(p[c] as f64 / 255.);
            }
            count += 1.;
        }
    }
    if count == 0. {
        return None;
    }
    filters::neutralize_camera_raw_color(sum.map(|v| filters::linear_to_srgb(v / count)))
}
fn histogram_bins(sample: &Raster) -> [[u64; 256]; 4] {
    let mut bins = [[0; 256]; 4];
    for y in 0..sample.height {
        for x in 0..sample.width {
            let p = sample.get(x, y);
            let weight = p[3] as u64;
            if weight == 0 {
                continue;
            }
            for c in 0..3 {
                // Three integer units retain the RGB average distribution without truncation.
                bins[c][p[c] as usize] += weight * 3;
                bins[3][p[c] as usize] += weight;
            }
        }
    }
    bins
}

fn same_scope_sample(a: &Raster, b: &Raster) -> bool {
    a.width == b.width
        && a.height == b.height
        && std::sync::Arc::ptr_eq(&a.pixels, &b.pixels)
        && a.tiles.len() == b.tiles.len()
        && a.tiles
            .iter()
            .zip(&b.tiles)
            .all(|((ak, av), (bk, bv))| ak == bk && std::sync::Arc::ptr_eq(av, bv))
}

#[derive(Clone)]
struct ScopeSample {
    width: u32,
    height: u32,
    pixels: std::sync::Weak<Vec<u8>>,
    tiles: ScopeTiles,
}
type ScopeTiles = Vec<((u32, u32), std::sync::Weak<Vec<u8>>)>;

impl ScopeSample {
    fn new(sample: &Raster) -> Self {
        Self {
            width: sample.width,
            height: sample.height,
            pixels: std::sync::Arc::downgrade(&sample.pixels),
            tiles: sample
                .tiles
                .iter()
                .map(|(key, pixels)| (*key, std::sync::Arc::downgrade(pixels)))
                .collect(),
        }
    }

    fn matches(&self, sample: &Raster) -> bool {
        self.width == sample.width
            && self.height == sample.height
            && std::ptr::eq(self.pixels.as_ptr(), std::sync::Arc::as_ptr(&sample.pixels))
            && self.tiles.len() == sample.tiles.len()
            && self.tiles.iter().zip(&sample.tiles).all(
                |((key, pixels), (sample_key, sample_pixels))| {
                    key == sample_key
                        && std::ptr::eq(pixels.as_ptr(), std::sync::Arc::as_ptr(sample_pixels))
                },
            )
    }
}

pub struct CameraRawView {
    source: std::sync::Arc<Raster>,
    region: [u32; 4],
    texture: egui::TextureHandle,
}

/// Upload only the visible source pixels, with one image pixel per physical screen pixel.
pub fn camera_raw_native_preview(
    ui: &mut egui::Ui,
    source: &std::sync::Arc<Raster>,
    cached: &mut Option<CameraRawView>,
) -> egui::Response {
    let pixels_per_point = ui.ctx().pixels_per_point().max(0.01);
    let size = Vec2::new(source.width as f32, source.height as f32) / pixels_per_point;
    egui::ScrollArea::both()
        .id_salt("camera-raw-native-scroll")
        .max_height(400.)
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, viewport| {
            let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
            let visible = egui::Rect::from_min_max(
                rect.min + viewport.min.to_vec2(),
                rect.min + viewport.max.to_vec2(),
            )
            .intersect(rect);
            if !visible.is_positive() {
                return response;
            }
            let x0 = ((visible.left() - rect.left()) * pixels_per_point)
                .floor()
                .clamp(0., source.width as f32) as u32;
            let y0 = ((visible.top() - rect.top()) * pixels_per_point)
                .floor()
                .clamp(0., source.height as f32) as u32;
            let x1 = ((visible.right() - rect.left()) * pixels_per_point)
                .ceil()
                .clamp(x0 as f32, source.width as f32) as u32;
            let y1 = ((visible.bottom() - rect.top()) * pixels_per_point)
                .ceil()
                .clamp(y0 as f32, source.height as f32) as u32;
            let region = [x0, y0, x1, y1];
            if x1 == x0 || y1 == y0 {
                return response;
            }
            if cached.as_ref().is_none_or(|cached| {
                cached.region != region || !same_scope_sample(&cached.source, source)
            }) {
                let mut pixels = Vec::with_capacity((x1 - x0) as usize * (y1 - y0) as usize * 4);
                for y in y0..y1 {
                    for x in x0..x1 {
                        pixels.extend(source.get(x, y));
                    }
                }
                let texture = ui.ctx().load_texture(
                    "camera-raw-native-region",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [(x1 - x0) as usize, (y1 - y0) as usize],
                        &pixels,
                    ),
                    egui::TextureOptions::NEAREST,
                );
                *cached = Some(CameraRawView {
                    source: source.clone(),
                    region,
                    texture,
                });
            }
            let texture = &cached.as_ref().unwrap().texture;
            let shown = egui::Rect::from_min_max(
                rect.min + Vec2::new(x0 as f32, y0 as f32) / pixels_per_point,
                rect.min + Vec2::new(x1 as f32, y1 as f32) / pixels_per_point,
            );
            ui.painter().image(
                texture.id(),
                shown,
                egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)),
                Color32::WHITE,
            );
            response
        })
        .inner
}

fn cached_histogram_bins(ui: &egui::Ui, sample: &Raster) -> std::sync::Arc<[[u64; 256]; 4]> {
    // Levels and its Auto action use every pixel, independently of display scopes.
    let id = egui::Id::new("filter-levels-histogram-cache");
    if let Some((previous, bins)) =
        ui.data(|data| data.get_temp::<(ScopeSample, std::sync::Arc<[[u64; 256]; 4]>)>(id))
        && previous.matches(sample)
    {
        return bins;
    }
    let bins = std::sync::Arc::new(histogram_bins(sample));
    ui.data_mut(|data| data.insert_temp(id, (ScopeSample::new(sample), bins.clone())));
    bins
}

pub fn prepare_camera_raw_scopes(ctx: &egui::Context, sample: &Raster) {
    cached_camera_raw_scopes(ctx, sample);
}

pub fn histogram(ui: &mut egui::Ui, sample: &Raster) {
    let bins = cached_camera_raw_scopes(ui.ctx(), sample).histogram;
    let (response, painter) =
        ui.allocate_painter(Vec2::new(ui.available_width(), 110.), Sense::hover());
    let rect = response.rect.shrink(3.);
    painter.rect_filled(
        rect,
        2.,
        crate::chrome::theme::palette(ui.ctx(), ui.visuals()).field,
    );
    let scales = cached_histogram_scales(ui, &bins);
    let max = scales[..3].iter().copied().fold(1., f64::max);
    for c in 0..3 {
        let color = [Color32::RED, Color32::GREEN, Color32::BLUE][c].gamma_multiply(0.55);
        let mut mesh = egui::epaint::Mesh::default();
        for i in 0..256 {
            let x = rect.left() + rect.width() * i as f32 / 255.;
            let top =
                rect.bottom() - rect.height() * (bins[c][i] as f64 / max).clamp(0., 1.) as f32;
            mesh.colored_vertex(Pos2::new(x, rect.bottom()), color);
            mesh.colored_vertex(Pos2::new(x, top), color);
            if i > 0 {
                let bottom_right = (i * 2) as u32;
                mesh.add_triangle(bottom_right - 2, bottom_right, bottom_right - 1);
                mesh.add_triangle(bottom_right - 1, bottom_right, bottom_right + 1);
            }
        }
        painter.add(egui::Shape::mesh(mesh));
    }
}

#[derive(Clone)]
struct CameraRawScopes {
    histogram: std::sync::Arc<[[u64; 256]; 4]>,
    vectorscope: std::sync::Arc<[u64; 64 * 64]>,
}

fn camera_raw_scope_step(width: u32, height: u32) -> usize {
    if u64::from(width) * u64::from(height) <= 4_000_000 {
        4
    } else {
        8
    }
}

/// Display statistics only: the graded image and native preview stay full resolution.
/// Match the original's sixteen bands, sampling every fourth/eighth pixel each way.
fn camera_raw_scope_bins(sample: &Raster, step: usize) -> ([[u64; 256]; 4], [u64; 64 * 64]) {
    let rows = sample.height.div_ceil(16);
    // Keep band storage on the heap; large array-valued Rayon reductions can
    // overflow the smaller worker stacks provided by Windows.
    let mut parts = vec![0_u64; 16 * (4 * 256 + 64 * 64)];
    parts
        .par_chunks_mut(4 * 256 + 64 * 64)
        .enumerate()
        .for_each(|(band, counts)| {
            let band = band as u32;
            let (histogram, bins) = counts.split_at_mut(4 * 256);
            let end = ((band + 1) * rows).min(sample.height);
            for y in (band * rows..end).step_by(step) {
                for x in (0..sample.width).step_by(step) {
                    let p = sample.get(x, y);
                    if p[3] == 0 {
                        continue;
                    }
                    let weight = u64::from(p[3]);
                    for c in 0..3 {
                        histogram[c * 256 + p[c] as usize] += weight * 3;
                        histogram[3 * 256 + p[c] as usize] += weight;
                    }
                    let [r, g, b] = std::array::from_fn::<_, 3, _>(|c| p[c] as f64 / 255.);
                    let max = r.max(g).max(b);
                    let min = r.min(g).min(b);
                    let chroma = max - min;
                    if chroma <= 1e-4 || max <= 1e-4 {
                        continue;
                    }
                    let hue = if max == r {
                        (g - b) / chroma
                    } else if max == g {
                        2. + (b - r) / chroma
                    } else {
                        4. + (r - g) / chroma
                    };
                    let angle = (hue / 6.).rem_euclid(1.) * std::f64::consts::TAU;
                    let saturation = chroma / max;
                    let column =
                        ((0.5 + angle.cos() * saturation * 0.48) * 64.).clamp(0., 63.) as usize;
                    let row =
                        ((0.5 + angle.sin() * saturation * 0.48) * 64.).clamp(0., 63.) as usize;
                    bins[row * 64 + column] += weight;
                }
            }
        });
    let mut histogram = [[0; 256]; 4];
    let mut bins = [0; 64 * 64];
    for part in parts.as_chunks::<{ 4 * 256 + 64 * 64 }>().0 {
        for (channel, next) in histogram
            .iter_mut()
            .zip(part[..4 * 256].as_chunks::<256>().0)
        {
            for (bin, count) in channel.iter_mut().zip(next) {
                *bin += count;
            }
        }
        for (bin, count) in bins.iter_mut().zip(&part[4 * 256..]) {
            *bin += count;
        }
    }
    (histogram, bins)
}

fn cached_camera_raw_scopes(ctx: &egui::Context, sample: &Raster) -> CameraRawScopes {
    let id = egui::Id::new("camera-raw-scopes-cache");
    if let Some((previous, bins)) =
        ctx.data(|data| data.get_temp::<(ScopeSample, CameraRawScopes)>(id))
        && previous.matches(sample)
    {
        return bins;
    }
    let (histogram, vectorscope) =
        camera_raw_scope_bins(sample, camera_raw_scope_step(sample.width, sample.height));
    let bins = CameraRawScopes {
        histogram: std::sync::Arc::new(histogram),
        vectorscope: std::sync::Arc::new(vectorscope),
    };
    ctx.data_mut(|data| data.insert_temp(id, (ScopeSample::new(sample), bins.clone())));
    bins
}

fn cached_vectorscope_bins(ui: &egui::Ui, sample: &Raster) -> std::sync::Arc<[u64; 64 * 64]> {
    cached_camera_raw_scopes(ui.ctx(), sample).vectorscope
}

fn vectorscope(ui: &mut egui::Ui, sample: &Raster) {
    let bins = cached_vectorscope_bins(ui, sample);
    let (response, painter) =
        ui.allocate_painter(Vec2::splat(ui.available_width().min(220.)), Sense::hover());
    let rect = response.rect;
    let colors = crate::chrome::theme::palette(ui.ctx(), ui.visuals());
    painter.rect_filled(rect, 2., colors.field);
    let peak = bins.iter().copied().max().unwrap_or(0);
    if peak == 0 {
        return;
    }
    let cell = rect.width() / 64.;
    for (index, &density) in bins.iter().enumerate().filter(|(_, density)| **density > 0) {
        let column = index % 64;
        let row = index / 64;
        let pos = Pos2::new(
            rect.left() + column as f32 * cell,
            rect.bottom() - (row + 1) as f32 * cell,
        );
        let opacity = 0.15 + 0.85 * density as f64 / peak as f64;
        painter.rect_filled(
            egui::Rect::from_min_size(pos, Vec2::splat(cell + 0.2)),
            0.,
            colors.text.gamma_multiply(opacity as f32),
        );
    }
}

#[cfg(test)]
mod restored_ui_tests {
    use super::*;

    #[test]
    fn scope_caches_release_full_source_pixels_and_tiles() {
        let ctx = egui::Context::default();
        let mut sample = Raster::solid(40, 30, [255, 0, 0, 255]);
        sample.set(2, 2, [0, 0, 255, 128]);
        let pixels = std::sync::Arc::downgrade(&sample.pixels);
        let tile = std::sync::Arc::downgrade(sample.tiles.values().next().unwrap());
        prepare_camera_raw_scopes(&ctx, &sample);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                cached_histogram_bins(ui, &sample);
                cached_vectorscope_bins(ui, &sample);
            });
        });
        drop(sample);
        assert!(pixels.upgrade().is_none());
        assert!(tile.upgrade().is_none());
    }

    #[test]
    fn levels_gamma_drag_is_inverse_of_source_midpoint_and_stops_at_input_bounds() {
        let mut values = [23., 2.4, 217., 0., 255.];
        let middle = values[0] + (values[2] - values[0]) * 0.5_f64.powf(values[1]);
        level_handle(&mut values, false, 1, middle);
        assert!((values[1] - 2.4).abs() < 1e-12);
        level_handle(&mut values, false, 0, 255.);
        assert_eq!(values[0], 216.);
        level_handle(&mut values, false, 2, 0.);
        assert_eq!(values[2], 217.);
        level_handle(&mut values, true, 0, 241.);
        level_handle(&mut values, true, 1, 13.);
        assert_eq!(
            values[3..],
            [241., 13.],
            "Output reversal is supported by the filter"
        );
    }

    #[test]
    fn histogram_spike_scale_keeps_clipped_extremes_and_empty_images() {
        let mut bins = [0; 256];
        assert_eq!(histogram_scale(&bins), 0.);
        bins[0] = 900;
        assert_eq!(
            histogram_scale(&bins),
            900.,
            "A clipped-only image remains visible"
        );
        bins[255] = 1200;
        for value in &mut bins[1..255] {
            *value = 10;
        }
        bins[128] = 1000;
        assert_eq!(
            histogram_scale(&bins),
            40.,
            "An isolated spike does not hide interior tones"
        );
        assert_eq!(bins[0], 900);
        assert_eq!(bins[255], 1200);
    }

    #[test]
    fn hue_wrapped_handles_keep_order_and_reject_crossing_or_full_circle() {
        let mut band =
            json!({"falloffStart":315.,"rangeStart":345.,"rangeEnd":15.,"falloffEnd":45.});
        assert!(set_hue_handle(&mut band, 1, 360.));
        assert_eq!(band["rangeStart"], 0.);
        let before = band.clone();
        assert!(!set_hue_handle(&mut band, 1, 30.));
        assert!(!set_hue_handle(&mut band, 3, 315.));
        assert!(!set_hue_handle(&mut band, 0, f64::NAN));
        assert_eq!(band, before);
        assert!(set_hue_handle(&mut band, 2, 20.));
    }

    #[test]
    fn spectrum_legend_combines_overlapping_ranges_and_respects_inversion() {
        let mut settings = hue_defaults(false);
        settings["adjustments"]["Master"]["hue"] = json!(10.);
        settings["adjustments"]["Reds"]["hue"] = json!(30.);
        assert_eq!(shifted_spectrum_hue(0., &settings), 40.);
        assert_eq!(shifted_spectrum_hue(60., &settings), 70.);
        let mut missing = settings.clone();
        missing["bands"] = json!({});
        assert_eq!(shifted_spectrum_hue(0., &missing), 40.);
        assert_eq!(shifted_spectrum_hue(60., &missing), 70.);
        settings["range"] = json!("Reds");
        settings["invertRange"] = json!(true);
        assert_eq!(shifted_spectrum_hue(0., &settings), 10.);
        assert_eq!(shifted_spectrum_hue(60., &settings), 100.);
        let colored = hue_defaults(true);
        assert_eq!(colored["adjustments"]["Master"]["saturation"], 25.);
        assert_eq!(colored["range"], "Master");
    }

    #[test]
    fn restored_dialog_controls_do_not_mutate_settings_without_input_or_persist_state() {
        let context = egui::Context::default();
        for kind in ["Levels", "Hue/Saturation"] {
            let mut settings = filters::default_settings(kind);
            let before = settings.clone();
            let mut state = FilterUiState {
                preview: false,
                ..Default::default()
            };
            let _ = context.run(Default::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    assert!(!show_dialog_controls(
                        ui,
                        kind,
                        &mut settings,
                        None,
                        None,
                        &mut state
                    ));
                });
            });
            assert_eq!(settings, before);
            assert!(!state.preview);
            assert!(state.pointer.is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_raw_resets_each_complete_section_and_preserves_all_others() {
        fn edited(value: &Value) -> Value {
            match value {
                Value::Number(value) => json!(value.as_f64().unwrap() + 7.),
                Value::String(value) => json!(format!("edited {value}")),
                Value::Bool(value) => json!(!value),
                Value::Array(values) => Value::Array(values.iter().map(edited).collect()),
                Value::Object(values) => Value::Object(
                    values
                        .iter()
                        .map(|(key, value)| (key.clone(), edited(value)))
                        .collect(),
                ),
                _ => value.clone(),
            }
        }
        let defaults = filters::camera_raw_defaults();
        let mut original = edited(&defaults);
        original["mixer"]["points"] = json!([{"hue": 30, "hueShift": 17}]);
        original["geometry"]["guides"] = json!([{"startX": 0.1, "endX": 0.9}]);
        original["customMetadata"] = json!("retained");
        for (section, _) in CAMERA_RAW_RESET_FIELDS {
            original["_enabledSections"][*section] = json!(false);
        }
        for (section, _) in CAMERA_RAW_RESET_FIELDS {
            let mut wrapped =
                json!({"cameraRaw": original, "kind": "Camera Raw Filter", "recordMetadata": 17});
            assert!(reset_camera_raw_section(&mut wrapped, section));
            let reset = &wrapped["cameraRaw"];
            assert!(camera_raw_section_is_default(reset, section, &defaults));
            assert_eq!(reset["_enabledSections"][*section], true);
            for (other, fields) in CAMERA_RAW_RESET_FIELDS {
                let source = if other == section {
                    &defaults
                } else {
                    &original
                };
                for field in *fields {
                    assert_eq!(
                        reset[*field], source[*field],
                        "resetting {section} changed {other}.{field}"
                    );
                }
                if other != section {
                    assert_eq!(reset["_enabledSections"][*other], false);
                }
            }
            assert_eq!(reset["customMetadata"], "retained");
            assert_eq!(reset["showShadowClipping"], original["showShadowClipping"]);
            assert_eq!(wrapped["recordMetadata"], 17);
            assert!(!reset_camera_raw_section(&mut wrapped, section));
        }
    }

    #[test]
    fn camera_raw_reset_restores_effect_choices_and_accepts_neutral_float_numbers() {
        let defaults = filters::camera_raw_defaults();
        let mut raw = defaults.clone();
        raw["glowStyle"] = json!("Bloom");
        raw["vignetteStyle"] = json!("Paint Overlay");
        raw["vignetteMidpoint"] = json!(1.);
        raw["vignetteFeather"] = json!(3.);
        raw["grainSize"] = json!(99.);
        assert!(reset_camera_raw_section(&mut raw, "effects"));
        assert_eq!(raw, defaults);
        raw["exposure"] = json!(0.0);
        raw["curve"]["rgb"][0]["x"] = json!(0.0);
        assert!(camera_raw_section_is_default(&raw, "light", &defaults));
        assert!(camera_raw_section_is_default(&raw, "curve", &defaults));
        let before = raw.clone();
        assert!(!reset_camera_raw_section(&mut raw, "unknown"));
        assert_eq!(raw, before);
    }

    #[test]
    fn camera_raw_panel_reset_event_is_consumed_once_without_persisted_fields() {
        let ctx = egui::Context::default();
        assert!(take_camera_raw_section_reset(&ctx).is_none());
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("camera-raw-section-reset"),
                "geometry".to_owned(),
            )
        });
        assert_eq!(
            take_camera_raw_section_reset(&ctx).as_deref(),
            Some("geometry")
        );
        assert!(take_camera_raw_section_reset(&ctx).is_none());
    }

    #[test]
    fn camera_raw_section_eyes_preserve_slider_values_and_neutralize_only_the_group() {
        let mut raw = filters::camera_raw_defaults();
        raw["exposure"] = json!(2.);
        raw["temperature"] = json!(31.);
        raw["glow"] = json!(75.);
        raw["glowRange"] = json!(45.);
        raw["curve"]["shadows"] = json!(65.);
        raw["mixer"]["hue"][0] = json!(40.);
        raw["grading"]["global"]["saturation"] = json!(55.);
        raw["detail"]["sharpenAmount"] = json!(85.);
        raw["optics"]["distortion"] = json!(24.);
        raw["geometry"]["rotate"] = json!(17.);
        raw["calibration"]["redHue"] = json!(41.);
        let base = raw.clone();
        for section in [
            "light",
            "color",
            "effects",
            "curve",
            "mixer",
            "grading",
            "detail",
            "optics",
            "geometry",
            "calibration",
        ] {
            let mut edited = base.clone();
            edited["_enabledSections"] = json!({section:false});
            let original = edited.clone();
            let effective = effective_camera_raw_settings(&edited);
            assert_eq!(edited, original, "eye changes effective copy only");
            assert!(effective.get("_enabledSections").is_none());
            match section {
                "light" => {
                    assert_eq!(effective["exposure"], 0.);
                    assert_eq!(effective["temperature"], 31.);
                }
                "color" => {
                    assert_eq!(effective["temperature"], 0.);
                    assert_eq!(effective["exposure"], 2.);
                }
                "effects" => {
                    assert_eq!(effective["glow"], 0.);
                    assert_eq!(effective["glowRange"], 45.);
                }
                _ => assert_eq!(effective[section], filters::camera_raw_defaults()[section]),
            }
            edited["_enabledSections"][section] = json!(true);
            assert_eq!(effective_camera_raw_settings(&edited), base);
        }
    }

    #[test]
    fn disabled_camera_raw_groups_render_neutral_and_keep_wrapper_fields() {
        let mut raw = filters::camera_raw_defaults();
        raw["exposure"] = json!(1.7);
        raw["temperature"] = json!(-31.);
        raw["clarity"] = json!(80.);
        raw["curve"]["shadows"] = json!(65.);
        raw["mixer"]["hue"][0] = json!(40.);
        raw["grading"]["global"]["saturation"] = json!(55.);
        raw["detail"]["sharpenAmount"] = json!(85.);
        raw["optics"]["distortion"] = json!(24.);
        raw["geometry"]["rotate"] = json!(17.);
        raw["calibration"]["redHue"] = json!(41.);
        raw["_enabledSections"] = json!({"light":false,"color":false,"effects":false,"curve":false,"mixer":false,"grading":false,"detail":false,"optics":false,"geometry":false,"calibration":false});
        let wrapped = json!({"cameraRaw":raw,"kind":"Camera Raw Filter","applicationMetadata":17});
        let effective = effective_camera_raw_settings(&wrapped);
        assert_eq!(effective["applicationMetadata"], 17);
        let source = Raster::from_rgba(
            7,
            5,
            (0..35)
                .flat_map(|i| {
                    [
                        (i * 11 % 256) as u8,
                        (i * 19 % 256) as u8,
                        (i * 29 % 256) as u8,
                        255,
                    ]
                })
                .collect(),
        );
        assert_eq!(
            filters::apply_filter(&source, "Camera Raw Filter", &effective).unwrap(),
            source
        );
    }
    #[test]
    fn transient_raw_diagnostics_restore_normal_grade_without_changing_controls() {
        let source = Raster::from_rgba(2, 1, vec![255, 60, 40, 255, 90, 100, 130, 255]);
        let mut settings = filters::camera_raw_defaults();
        settings["exposure"] = json!(0.5);
        settings["curve"]["lights"] = json!(65.);
        let original = settings.clone();
        let normal = filters::apply_filter(&source, "Camera Raw Filter", &settings).unwrap();
        assert!(set_raw_diagnostic(&mut settings, 1));
        let effective = effective_camera_raw_settings(&settings);
        assert_eq!(effective["clipping"], 1);
        assert!(effective.get("_altDiagnostic").is_none());
        assert_eq!(effective["curve"], original["curve"]);
        let diagnostic = filters::apply_filter(&source, "Camera Raw Filter", &effective).unwrap();
        assert_ne!(diagnostic, normal);
        assert!(set_raw_diagnostic(&mut settings, 0));
        assert_eq!(settings, original);
        assert_eq!(
            filters::apply_filter(
                &source,
                "Camera Raw Filter",
                &effective_camera_raw_settings(&settings)
            )
            .unwrap(),
            normal
        );
        assert!(set_raw_diagnostic(&mut settings, 3));
        assert!(
            effective_camera_raw_settings(&settings)["detail"]["showSharpenMask"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(settings["detail"]["showSharpenMask"], false);
        assert!(set_raw_diagnostic(&mut settings, 0));
        assert_eq!(settings, original);
    }
    #[test]
    fn controls_preserve_all_default_schemas_without_input() {
        let ctx = egui::Context::default();
        for &kind in filters::FILTERS {
            let mut settings = filters::default_settings(kind);
            let original = settings.clone();
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    assert!(!show(
                        ui,
                        kind,
                        &mut settings,
                        Some(&Raster::solid(4, 4, [80, 100, 120, 255]))
                    ));
                });
            });
            assert_eq!(settings, original, "kind={kind}");
        }
    }
    #[test]
    fn gray_world_balance_neutralizes_the_measured_camera_raw_renderer() {
        for color in [
            [150, 120, 100, 255],
            [100, 120, 150, 255],
            [100, 150, 100, 255],
        ] {
            let input = Raster::solid(16, 16, color);
            let [temperature, tint] = auto_white_balance(&input).unwrap();
            let mut settings = filters::camera_raw_defaults();
            settings["temperature"] = json!(temperature);
            settings["tint"] = json!(tint);
            let corrected = filters::apply_filter(&input, "Camera Raw Filter", &settings)
                .unwrap()
                .get(0, 0);
            assert!(
                corrected[..3].iter().max().unwrap() - corrected[..3].iter().min().unwrap() <= 1,
                "{color:?} -> {corrected:?} at {temperature}/{tint}"
            );
            assert_eq!(corrected[3], color[3]);
        }
        assert!(auto_white_balance(&Raster::blank(2, 2)).is_none());
    }
    #[test]
    fn histogram_alpha_weights_and_rgb_average_match_original() {
        let image = Raster::from_rgba(3, 1, vec![255, 0, 0, 255, 255, 0, 0, 85, 10, 20, 30, 0]);
        let bins = histogram_bins(&image);
        assert_eq!(bins[0][255], (255 + 85) * 3);
        assert_eq!(bins[1][0], (255 + 85) * 3);
        assert_eq!(bins[3][0], (255 + 85) * 2);
        assert_eq!(bins[3][255], 255 + 85);
        assert!(
            bins.iter()
                .all(|channel| channel.iter().sum::<u64>() == (255 + 85) * 3)
        );
        // The center luma bin would be populated by the former luminance histogram.
        assert_eq!(bins[3][54], 0);
    }

    #[test]
    fn hsv_scope_matches_primary_directions_density_and_alpha() {
        let image = Raster::from_rgba(
            6,
            1,
            vec![
                255, 0, 0, 255, 255, 0, 0, 85, 0, 255, 0, 255, 0, 0, 255, 128, 128, 128, 128, 255,
                255, 0, 0, 0,
            ],
        );
        let bins = camera_raw_scope_bins(&image, 1).1;
        assert_eq!(bins[32 * 64 + 62], 340); // red: right
        assert_eq!(bins[58 * 64 + 16], 255); // green: upper left when rendered
        assert_eq!(bins[5 * 64 + 16], 128); // blue: lower left when rendered
        assert_eq!(bins.iter().sum::<u64>(), 723);
        assert_eq!(bins.iter().filter(|&&count| count > 0).count(), 3);
        let pale_red = camera_raw_scope_bins(&Raster::solid(1, 1, [255, 128, 128, 255]), 1).1;
        assert_eq!(pale_red[32 * 64 + 47], 255);
    }

    #[test]
    fn scope_caches_reuse_immutable_pixels_and_invalidate_tile_edits() {
        let ctx = egui::Context::default();
        let mut sample = Raster::solid(3, 2, [255, 0, 0, 255]);
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let histogram = cached_histogram_bins(ui, &sample);
                let scope = cached_vectorscope_bins(ui, &sample);
                assert!(std::sync::Arc::ptr_eq(
                    &histogram,
                    &cached_histogram_bins(ui, &sample)
                ));
                assert!(std::sync::Arc::ptr_eq(
                    &scope,
                    &cached_vectorscope_bins(ui, &sample)
                ));
                sample.set(0, 0, [0, 0, 255, 128]);
                let edited_histogram = cached_histogram_bins(ui, &sample);
                let edited_scope = cached_vectorscope_bins(ui, &sample);
                assert!(!std::sync::Arc::ptr_eq(&histogram, &edited_histogram));
                assert!(!std::sync::Arc::ptr_eq(&scope, &edited_scope));
                assert_eq!(edited_histogram[2][255], 128 * 3);
                assert_eq!(edited_scope[5 * 64 + 16], 128);
                assert_eq!(scope[32 * 64 + 62], 2 * 255);
            });
        });
    }

    #[test]
    fn camera_raw_scope_sampling_matches_original_band_alignment_and_keeps_source() {
        assert_eq!(camera_raw_scope_step(2_000, 2_000), 4);
        assert_eq!(camera_raw_scope_step(2_001, 2_000), 8);
        assert_eq!(camera_raw_scope_step(u32::MAX, u32::MAX), 8);
        let mut image = Raster::solid(9, 33, [64, 64, 64, 255]);
        image.set(0, 3, [255, 0, 0, 128]); // The second band's first sampled row.
        image.set(1, 0, [0, 0, 255, 255]); // Outside the display sampling grid.
        let before = image.clone();
        let (histogram, scope) = camera_raw_scope_bins(&image, 4);
        assert_eq!(histogram[0][255], 128 * 3);
        assert_eq!(histogram[2][255], 0);
        assert_eq!(histogram[0].iter().sum::<u64>(), (32 * 255 + 128) * 3);
        assert_eq!(scope[32 * 64 + 62], 128);
        assert_eq!(scope.iter().sum::<u64>(), 128);
        assert_eq!(histogram_bins(&image)[2][255], 255 * 3);
        assert!(same_scope_sample(&before, &image));
        assert_eq!(before, image);
        let (histogram, scope) = camera_raw_scope_bins(&Raster::blank(0, 0), 4);
        assert!(histogram.iter().flatten().all(|&count| count == 0));
        assert!(scope.iter().all(|&count| count == 0));
    }

    #[test]
    fn camera_raw_scope_cache_shares_both_graphs_without_changing_levels_statistics() {
        let ctx = egui::Context::default();
        let image = Raster::solid(9, 33, [255, 0, 0, 255]);
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                prepare_camera_raw_scopes(ctx, &image);
                let cached = cached_camera_raw_scopes(ctx, &image);
                let reused = cached_camera_raw_scopes(ctx, &image);
                assert!(std::sync::Arc::ptr_eq(&cached.histogram, &reused.histogram));
                assert!(std::sync::Arc::ptr_eq(
                    &cached.vectorscope,
                    &reused.vectorscope
                ));
                assert!(std::sync::Arc::ptr_eq(
                    &cached.vectorscope,
                    &cached_vectorscope_bins(ui, &image)
                ));
                assert_eq!(cached.histogram[0][255], 33 * 255 * 3);
                assert_eq!(cached_histogram_bins(ui, &image)[0][255], 297 * 255 * 3);
            });
        });
    }

    #[test]
    fn camera_raw_scope_bands_fit_small_windows_worker_stacks() {
        let image = Raster::solid(129, 33, [255, 0, 0, 255]);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .stack_size(512 * 1024)
            .build()
            .unwrap();
        pool.install(|| {
            for _ in 0..8 {
                let (histogram, scope) = camera_raw_scope_bins(&image, 4);
                assert_eq!(histogram[0][255], 33 * 11 * 255 * 3);
                assert_eq!(scope[32 * 64 + 62], 33 * 11 * 255);
            }
        });
    }
}
