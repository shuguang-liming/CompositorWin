//! Camera Raw preview gestures and grading wheels, using the original v11 JSON fields.
use crate::{filters, model::Raster};
use eframe::egui::{self, Color32, Pos2, Rect, Response, Sense, Stroke, Vec2};
use serde_json::{Value, json};

pub const MIXER_CENTERS: [f64; 8] = [0., 30., 60., 120., 180., 240., 270., 300.];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixerChannel {
    Hue,
    Saturation,
    Luminance,
}
impl MixerChannel {
    pub fn key(self) -> &'static str {
        match self {
            Self::Hue => "hue",
            Self::Saturation => "saturation",
            Self::Luminance => "luminance",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradeRegion {
    Shadows,
    Midtones,
    Highlights,
    Global,
}
impl GradeRegion {
    pub fn key(self) -> &'static str {
        match self {
            Self::Shadows => "shadows",
            Self::Midtones => "midtones",
            Self::Highlights => "highlights",
            Self::Global => "global",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Shadows => "阴影",
            Self::Midtones => "中间调",
            Self::Highlights => "高光",
            Self::Global => "全局",
        }
    }
}

/// Swift samples HSV saturation and HSL luminance; the point-color schema retains that behavior.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorSample {
    pub rgb: [u8; 3],
    pub tone: f64,
    pub hue: f64,
    pub saturation: f64,
    pub luminance: f64,
}
impl ColorSample {
    /// Input is straight-alpha sRGB, matching Raster. Fully transparent pixels have no sample.
    pub fn from_rgba(color: [u8; 4]) -> Option<Self> {
        if color[3] == 0 {
            return None;
        }
        let [r, g, b] = color[..3].try_into().ok()?;
        let [red, green, blue] = [r, g, b].map(|v| v as f64 / 255.);
        let high = red.max(green).max(blue);
        let low = red.min(green).min(blue);
        let chroma = high - low;
        let hue = if chroma < 1e-6 {
            0.
        } else if high == red {
            ((green - blue) / chroma * 60.).rem_euclid(360.)
        } else if high == green {
            ((blue - red) / chroma * 60. + 120.).rem_euclid(360.)
        } else {
            ((red - green) / chroma * 60. + 240.).rem_euclid(360.)
        };
        Some(Self {
            rgb: [r, g, b],
            tone: 0.2126 * red + 0.7152 * green + 0.0722 * blue,
            hue,
            saturation: if high == 0. { 0. } else { chroma / high },
            luminance: (high + low) / 2.,
        })
    }
}

pub fn sample_pixel(raster: &Raster, point: [f64; 2]) -> Option<ColorSample> {
    if !point.iter().all(|v| v.is_finite())
        || point[0] < 0.
        || point[1] < 0.
        || point[0] >= raster.width as f64
        || point[1] >= raster.height as f64
    {
        return None;
    }
    ColorSample::from_rgba(raster.get(point[0].floor() as u32, point[1].floor() as u32))
}

fn raw_mut(settings: &mut Value) -> &mut Value {
    if settings.get("cameraRaw").is_some() {
        &mut settings["cameraRaw"]
    } else {
        settings
    }
}

/// Neutralize the original pixel by inverting the renderer's linear-light temperature/tint gains.
/// Missing channels have no solution and leave the settings untouched.
pub fn pick_white_balance(settings: &mut Value, sample: ColorSample) -> bool {
    let [red, green, blue] = sample.rgb.map(|v| filters::srgb_to_linear(v as f64 / 255.));
    if red <= 1e-4 || green <= 1e-4 || blue <= 1e-4 {
        return false;
    }
    let (a1, b1, c1) = (0.35 * red, 0.15 * red + 0.30 * green, green - red);
    let (a2, b2, c2) = (-0.35 * blue, 0.15 * blue + 0.30 * green, green - blue);
    let determinant = a1 * b2 - a2 * b1;
    if determinant.abs() <= 1e-8 {
        return false;
    }
    let temperature = (c1 * b2 - c2 * b1) / determinant * 100.;
    let tint = (a1 * c2 - a2 * c1) / determinant * 100.;
    if !temperature.is_finite() || !tint.is_finite() {
        return false;
    }
    let raw = raw_mut(settings);
    let temperature = temperature.clamp(-100., 100.);
    let tint = tint.clamp(-100., 100.);
    let changed = filters::number(raw, "temperature", 0.) != temperature
        || filters::number(raw, "tint", 0.) != tint
        || filters::string(raw, "whiteBalance", "Custom") != "Custom";
    raw["temperature"] = json!(temperature);
    raw["tint"] = json!(tint);
    raw["whiteBalance"] = json!("Custom");
    changed
}

/// Match Swift's closest purple/green center, 25-degree half-width and first-pick amount 50.
pub fn pick_defringe(settings: &mut Value, sample: ColorSample) -> bool {
    if !sample.hue.is_finite() {
        return false;
    }
    let hue = sample.hue.rem_euclid(360.);
    let prefix = if (hue - 290.).abs() < (hue - 90.).abs() {
        "purple"
    } else {
        "green"
    };
    let optics = &mut raw_mut(settings)["optics"];
    let low = format!("{prefix}HueLow");
    let high = format!("{prefix}HueHigh");
    let amount = format!("{prefix}Amount");
    let lower = (hue - 25.).clamp(0., 360.);
    let upper = (hue + 25.).clamp(lower, 360.);
    let changed = filters::number(optics, &low, 0.) != lower
        || filters::number(optics, &high, 0.) != upper
        || filters::number(optics, &amount, 0.) == 0.;
    optics[&low] = json!(lower);
    optics[&high] = json!(upper);
    if filters::number(optics, &amount, 0.) == 0. {
        optics[&amount] = json!(50.);
    }
    changed
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveTarget {
    Parametric,
    Rgb,
    Red,
    Green,
    Blue,
}
impl CurveTarget {
    pub fn key(self) -> Option<&'static str> {
        match self {
            Self::Parametric => None,
            Self::Rgb => Some("rgb"),
            Self::Red => Some("red"),
            Self::Green => Some("green"),
            Self::Blue => Some("blue"),
        }
    }
}

/// Target a parametric tone region or the point whose input is closest to the sampled tone.
pub fn adjust_curve(settings: &mut Value, tone: f64, target: CurveTarget, delta: f64) -> bool {
    if !tone.is_finite() || !delta.is_finite() || delta == 0. {
        return false;
    }
    let tone = tone.clamp(0., 1.);
    let curve = &mut raw_mut(settings)["curve"];
    if let Some(key) = target.key() {
        let Some(points) = curve[key].as_array_mut() else {
            return false;
        };
        let nearest = points
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                p["x"]
                    .as_f64()
                    .filter(|x| x.is_finite())
                    .map(|x| (i, (x - tone).abs()))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        let Some(index) = nearest else {
            return false;
        };
        let old = filters::number(&points[index], "y", 0.);
        let value = (old + delta / 100.).clamp(0., 1.);
        points[index]["y"] = json!(value);
        value != old
    } else {
        let key = if tone < filters::number(curve, "shadowSplit", 25.) / 100. {
            "shadows"
        } else if tone < filters::number(curve, "darkSplit", 50.) / 100. {
            "darks"
        } else if tone < filters::number(curve, "lightSplit", 75.) / 100. {
            "lights"
        } else {
            "highlights"
        };
        let old = filters::number(curve, key, 0.);
        let value = (old + delta).clamp(-100., 100.);
        curve[key] = json!(value);
        value != old
    }
}

#[derive(Clone, Debug)]
pub struct CurveDrag {
    pub baseline: Value,
    pub tone: f64,
    pub start_y: f64,
    pub target: CurveTarget,
}
impl CurveDrag {
    pub fn settings_at(&self, current_y: f64) -> Value {
        let mut value = self.baseline.clone();
        adjust_curve(
            &mut value,
            self.tone,
            self.target,
            (self.start_y - current_y) * 0.35,
        );
        value
    }
}

/// Neighbors overlap across a 40-degree radius, including red at the 0/360 seam.
pub fn mixer_weights(hue: f64) -> [f64; 8] {
    if !hue.is_finite() {
        return [0.; 8];
    }
    let hue = hue.rem_euclid(360.);
    MIXER_CENTERS.map(|center| {
        let distance = (hue - center).abs();
        (1. - distance.min(360. - distance) / 40.).max(0.)
    })
}

/// Add a slider delta to the sampled color families, retaining unrelated settings.
pub fn adjust_mixer(settings: &mut Value, hue: f64, channel: MixerChannel, delta: f64) -> bool {
    if !delta.is_finite() || !hue.is_finite() || delta == 0. {
        return false;
    }
    let raw = raw_mut(settings);
    let key = channel.key();
    let values = &mut raw["mixer"][key];
    let original = values.clone();
    let weights = mixer_weights(hue);
    let updated: [f64; 8] = std::array::from_fn(|i| {
        let old = values
            .get(i)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .unwrap_or(0.);
        (old + weights[i] * delta).clamp(-100., 100.)
    });
    *values = json!(updated);
    *values != original
}

/// Keep the drag's baseline once; pass absolute screen Y positions to avoid compounding deltas.
#[derive(Clone, Debug)]
pub struct MixerDrag {
    pub baseline: Value,
    pub hue: f64,
    pub start_y: f64,
    pub channel: MixerChannel,
}
impl MixerDrag {
    pub fn settings_at(&self, current_y: f64) -> Value {
        let mut value = self.baseline.clone();
        adjust_mixer(
            &mut value,
            self.hue,
            self.channel,
            (self.start_y - current_y) * 0.35,
        );
        value
    }
}

/// Resample the selected point, or append a new one when no existing index is selected.
/// Resampling retains its three shifts and resets ranges to Swift's picked-color defaults.
pub fn pick_point_color(
    settings: &mut Value,
    sample: ColorSample,
    selected: Option<usize>,
) -> Option<usize> {
    if ![sample.hue, sample.saturation, sample.luminance]
        .iter()
        .all(|v| v.is_finite())
    {
        return None;
    }
    let mixer = &mut raw_mut(settings)["mixer"];
    if !mixer["points"].is_array() {
        mixer["points"] = json!([]);
    }
    let points = mixer["points"].as_array_mut()?;
    let previous = selected.and_then(|index| points.get(index));
    let picked = json!({
        "hue": sample.hue.rem_euclid(360.),
        "saturation": sample.saturation.clamp(0., 1.),
        "luminance": sample.luminance.clamp(0., 1.),
        "hueShift": previous.map(|p| filters::number(p, "hueShift", 0.)).unwrap_or(0.).clamp(-100., 100.),
        "saturationShift": previous.map(|p| filters::number(p, "saturationShift", 0.)).unwrap_or(0.).clamp(-100., 100.),
        "luminanceShift": previous.map(|p| filters::number(p, "luminanceShift", 0.)).unwrap_or(0.).clamp(-100., 100.),
        "hueRange": 30., "saturationRange": 0.4, "luminanceRange": 0.4, "visualize": false
    });
    if let Some(index) = selected.filter(|index| *index < points.len()) {
        points[index] = picked;
        Some(index)
    } else if points.len() < 8 {
        points.push(picked);
        Some(points.len() - 1)
    } else {
        None
    }
}

/// Rect is [left, top, width, height] in screen space. Saved guides use lower-left origin.
pub fn normalized_guide_point(point: [f64; 2], rect: [f64; 4]) -> Option<[f64; 2]> {
    if !point.iter().chain(rect.iter()).all(|v| v.is_finite()) || rect[2] <= 0. || rect[3] <= 0. {
        return None;
    }
    let x = (point[0] - rect[0]) / rect[2];
    let y = (point[1] - rect[1]) / rect[3];
    if !(0. ..=1.).contains(&x) || !(0. ..=1.).contains(&y) {
        return None;
    }
    Some([x, 1. - y])
}

/// Endpoints are normalized lower-left coordinates. Short/invalid gestures do not change settings.
pub fn append_guide(settings: &mut Value, start: [f64; 2], end: [f64; 2]) -> bool {
    if !start
        .iter()
        .chain(end.iter())
        .all(|v| v.is_finite() && (0. ..=1.).contains(v))
        || (end[0] - start[0]).hypot(end[1] - start[1]) <= 0.01
    {
        return false;
    }
    let geometry = &mut raw_mut(settings)["geometry"];
    if !geometry["guides"].is_array() {
        geometry["guides"] = json!([]);
    }
    geometry["guides"].as_array_mut().unwrap().push(json!({
        "startX": start[0], "startY": start[1], "endX": end[0], "endY": end[1]
    }));
    geometry["upright"] = json!("Guided");
    true
}

pub fn guide_screen_points(guide: &Value, rect: Rect) -> Option<[Pos2; 2]> {
    let read = |x, y| {
        let x = guide[x].as_f64()?;
        let y = guide[y].as_f64()?;
        (x.is_finite() && y.is_finite()).then(|| {
            Pos2::new(
                rect.left() + x as f32 * rect.width(),
                rect.bottom() - y as f32 * rect.height(),
            )
        })
    };
    Some([read("startX", "startY")?, read("endX", "endY")?])
}

/// Right is red at 0 degrees; hue increases counterclockwise, with saturation radial.
pub fn wheel_value(offset: [f64; 2], radius: f64) -> Option<[f64; 2]> {
    if !offset.iter().all(|v| v.is_finite()) || !radius.is_finite() || radius <= 0. {
        return None;
    }
    Some([
        (-offset[1]).atan2(offset[0]).to_degrees().rem_euclid(360.),
        (offset[0].hypot(offset[1]) / radius * 100.).clamp(0., 100.),
    ])
}

/// Paint a full HSV disk. Pointer events update the same JSON consumed by the pixel renderer.
pub fn grading_wheel(ui: &mut egui::Ui, wheel: &mut Value, side: f32) -> Response {
    let side = side.max(24.);
    let (rect, mut response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click_and_drag());
    let radius = side * 0.5 - 6.;
    let center = rect.center();
    if response.double_clicked() {
        if filters::number(wheel, "hue", 0.) != 0. || filters::number(wheel, "saturation", 0.) != 0.
        {
            wheel["hue"] = json!(0.);
            wheel["saturation"] = json!(0.);
            response.mark_changed();
        }
    } else if (response.dragged() || response.is_pointer_button_down_on())
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some([hue, saturation]) = wheel_value(
            [(pointer.x - center.x) as f64, (pointer.y - center.y) as f64],
            radius as f64,
        )
        && ((filters::number(wheel, "hue", 0.) - hue).abs() > 1e-6
            || (filters::number(wheel, "saturation", 0.) - saturation).abs() > 1e-6)
    {
        wheel["hue"] = json!(hue);
        wheel["saturation"] = json!(saturation);
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        // Concentric rings interpolate saturation as well as angular hue; the center stays white.
        let mut mesh = egui::epaint::Mesh::default();
        const SECTORS: usize = 96;
        const RINGS: usize = 8;
        for ring in 0..=RINGS {
            let saturation = ring as f32 / RINGS as f32;
            for sector in 0..=SECTORS {
                let hue = sector as f32 / SECTORS as f32;
                let angle = hue * std::f32::consts::TAU;
                let pos = center + Vec2::new(angle.cos(), -angle.sin()) * radius * saturation;
                let color = Color32::from(egui::ecolor::Hsva::new(hue, saturation, 1., 1.));
                mesh.colored_vertex(pos, color);
            }
        }
        for ring in 0..RINGS {
            for sector in 0..SECTORS {
                let a = (ring * (SECTORS + 1) + sector) as u32;
                let b = a + (SECTORS + 1) as u32;
                mesh.add_triangle(a, b, a + 1);
                mesh.add_triangle(a + 1, b, b + 1);
            }
        }
        ui.painter().add(egui::Shape::mesh(mesh));
        ui.painter()
            .circle_stroke(center, radius, Stroke::new(1.0f32, Color32::GRAY));
        let hue = filters::number(wheel, "hue", 0.).to_radians();
        let distance =
            (filters::number(wheel, "saturation", 0.) / 100.).clamp(0., 1.) as f32 * radius;
        let dot = center + Vec2::new(hue.cos() as f32, -hue.sin() as f32) * distance;
        ui.painter().circle_filled(dot, 5., Color32::WHITE);
        ui.painter()
            .circle_stroke(dot, 5., Stroke::new(1.0f32, Color32::BLACK));
    }
    response.on_hover_text("拖动以设置色相和饱和度；双击重置色相和饱和度。")
}

/// Four independent wheels in two rows, plus each wheel's signed luminance control.
pub fn show_grading_wheels(ui: &mut egui::Ui, settings: &mut Value) -> bool {
    let mut changed = false;
    let grading = &mut raw_mut(settings)["grading"];
    for pair in [
        [GradeRegion::Shadows, GradeRegion::Midtones],
        [GradeRegion::Highlights, GradeRegion::Global],
    ] {
        ui.horizontal(|ui| {
            for region in pair {
                ui.push_id(region.key(), |ui| {
                    ui.vertical(|ui| {
                        ui.label(region.label());
                        let wheel = &mut grading[region.key()];
                        changed |= grading_wheel(ui, wheel, 96.).changed();
                        ui.label(format!(
                            "{:.0}° / {:.0}%",
                            filters::number(wheel, "hue", 0.),
                            filters::number(wheel, "saturation", 0.)
                        ));
                        let mut luminance =
                            filters::number(wheel, "luminance", 0.).clamp(-100., 100.);
                        if ui
                            .add(
                                egui::DragValue::new(&mut luminance)
                                    .range(-100. ..=100.)
                                    .prefix("明度 "),
                            )
                            .changed()
                        {
                            wheel["luminance"] = json!(luminance);
                            changed = true;
                        }
                    });
                });
            }
        });
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_balance_pick_neutralizes_real_rendered_channels_and_rejects_missing_channels() {
        let mut settings =
            json!({"cameraRaw":filters::camera_raw_defaults(),"kind":"Camera Raw Filter"});
        settings["cameraRaw"]["whiteBalance"] = json!("Auto");
        let source = Raster::solid(1, 1, [150, 140, 130, 255]);
        let sample = ColorSample::from_rgba(source.get(0, 0)).unwrap();
        assert!(pick_white_balance(&mut settings, sample));
        assert_eq!(settings["cameraRaw"]["whiteBalance"], "Custom");
        let p = filters::apply_filter(&source, "Camera Raw Filter", &settings)
            .unwrap()
            .get(0, 0);
        assert!(
            p[..3].iter().max().unwrap() - p[..3].iter().min().unwrap() <= 1,
            "{p:?}"
        );
        assert_eq!(p[3], 255);
        assert!(!pick_white_balance(&mut settings, sample));
        let before = settings.clone();
        assert!(!pick_white_balance(
            &mut settings,
            ColorSample::from_rgba([0, 100, 100, 255]).unwrap()
        ));
        assert_eq!(settings, before);
        assert!(pick_white_balance(
            &mut settings,
            ColorSample::from_rgba([255, 10, 5, 255]).unwrap()
        ));
        assert!((-100. ..=100.).contains(&settings["cameraRaw"]["temperature"].as_f64().unwrap()));
        assert!((-100. ..=100.).contains(&settings["cameraRaw"]["tint"].as_f64().unwrap()));
    }

    #[test]
    fn defringe_pick_selects_original_hue_family_retains_strength_and_changes_pixels() {
        let mut settings = filters::camera_raw_defaults();
        let sample = ColorSample::from_rgba([200, 30, 200, 255]).unwrap();
        assert!(pick_defringe(&mut settings, sample));
        assert_eq!(settings["optics"]["purpleHueLow"], 275.);
        assert_eq!(settings["optics"]["purpleHueHigh"], 325.);
        assert_eq!(settings["optics"]["purpleAmount"], 50.);
        settings["optics"]["purpleAmount"] = json!(72.);
        assert!(!pick_defringe(&mut settings, sample));
        assert_eq!(settings["optics"]["purpleAmount"], 72.);
        let source = Raster::solid(1, 1, [200, 30, 200, 255]);
        let p = filters::apply_filter(&source, "Camera Raw Filter", &settings)
            .unwrap()
            .get(0, 0);
        assert_ne!(p, source.get(0, 0));
        assert_eq!(p[3], 255);
        let sample = ColorSample::from_rgba([100, 200, 0, 255]).unwrap();
        assert!(pick_defringe(&mut settings, sample));
        assert_eq!(settings["optics"]["greenHueLow"], 65.);
        assert_eq!(settings["optics"]["greenHueHigh"], 115.);
        assert_eq!(settings["optics"]["greenAmount"], 50.);
        let before = settings.clone();
        assert!(!pick_defringe(
            &mut settings,
            ColorSample {
                hue: f64::NAN,
                ..sample
            }
        ));
        assert_eq!(settings, before);
    }

    #[test]
    fn curve_targeted_drag_uses_tone_regions_nearest_points_and_absolute_baseline() {
        let mut settings = filters::camera_raw_defaults();
        settings["curve"]["shadowSplit"] = json!(30.);
        settings["curve"]["darkSplit"] = json!(60.);
        settings["curve"]["lightSplit"] = json!(80.);
        for (tone, key) in [
            (0.1, "shadows"),
            (0.3, "darks"),
            (0.6, "lights"),
            (0.8, "highlights"),
        ] {
            let drag = CurveDrag {
                baseline: settings.clone(),
                tone,
                start_y: 100.,
                target: CurveTarget::Parametric,
            };
            let updated = drag.settings_at(0.);
            assert_eq!(updated["curve"][key], 35.);
            assert_eq!(drag.settings_at(0.), updated);
            assert_eq!(drag.settings_at(100.), drag.baseline);
            assert_eq!(drag.settings_at(-1000.)["curve"][key], 100.);
        }
        settings["curve"]["red"] = json!([{"x":0.,"y":0.},{"x":0.4,"y":0.4},{"x":1.,"y":1.}]);
        let drag = CurveDrag {
            baseline: settings.clone(),
            tone: 0.43,
            start_y: 100.,
            target: CurveTarget::Red,
        };
        let updated = drag.settings_at(0.);
        assert_eq!(updated["curve"]["red"][1], json!({"x":0.4,"y":0.75}));
        assert_eq!(updated["curve"]["rgb"], settings["curve"]["rgb"]);
        let source = Raster::solid(1, 1, [120, 120, 120, 255]);
        let p = filters::apply_filter(&source, "Camera Raw Filter", &updated)
            .unwrap()
            .get(0, 0);
        assert!(p[0] > p[1] && p[1] == p[2], "{p:?}");
        let mut endpoint = settings.clone();
        assert!(adjust_curve(&mut endpoint, 0., CurveTarget::Blue, 35.));
        assert_eq!(
            endpoint["curve"]["blue"][0]["x"],
            settings["curve"]["blue"][0]["x"]
        );
        assert_eq!(endpoint["curve"]["blue"][0]["y"].as_f64(), Some(0.35));
        let before = endpoint.clone();
        assert!(!adjust_curve(
            &mut endpoint,
            f64::NAN,
            CurveTarget::Rgb,
            10.
        ));
        assert_eq!(endpoint, before);
    }

    #[test]
    fn sampling_matches_original_hsv_saturation_and_hsl_luminance() {
        assert!(ColorSample::from_rgba([255, 0, 0, 0]).is_none());
        let sample = ColorSample::from_rgba([255, 128, 128, 17]).unwrap();
        assert_eq!(sample.rgb, [255, 128, 128]);
        assert_eq!(sample.hue, 0.);
        assert!((sample.saturation - 127. / 255.).abs() < 1e-10);
        assert!((sample.luminance - 383. / 510.).abs() < 1e-10);
        let mut raster = Raster::solid(200, 200, [0, 255, 0, 255]);
        raster.set(129, 130, [0, 0, 255, 255]);
        assert_eq!(sample_pixel(&raster, [129.9, 130.5]).unwrap().hue, 240.);
        assert!(sample_pixel(&raster, [-0.1, 0.]).is_none());
        assert!(sample_pixel(&raster, [200., 0.]).is_none());
        assert!(sample_pixel(&raster, [f64::NAN, 0.]).is_none());
    }

    #[test]
    fn targeted_drag_overlaps_color_families_and_uses_baseline() {
        let mut settings = filters::camera_raw_defaults();
        settings["exposure"] = json!(2.);
        let drag = MixerDrag {
            baseline: settings,
            hue: 30.,
            start_y: 100.,
            channel: MixerChannel::Saturation,
        };
        let first = drag.settings_at(0.);
        assert_eq!(
            first["mixer"]["saturation"],
            json!([8.75, 35., 8.75, 0., 0., 0., 0., 0.])
        );
        assert_eq!(first["mixer"]["hue"], json!(vec![0; 8]));
        assert_eq!(first["exposure"], 2.);
        assert_eq!(drag.settings_at(0.), first);
        assert_eq!(drag.settings_at(100.), drag.baseline);
        assert_eq!(drag.settings_at(-1000.)["mixer"]["saturation"][1], 100.);
        assert!((mixer_weights(359.)[0] - 0.975).abs() < 1e-10);
    }

    #[test]
    fn point_pick_resamples_existing_shifts_and_limits_to_eight() {
        let mut settings =
            json!({"cameraRaw":filters::camera_raw_defaults(),"kind":"Camera Raw Filter"});
        let sample = ColorSample::from_rgba([20, 70, 250, 255]).unwrap();
        assert_eq!(pick_point_color(&mut settings, sample, None), Some(0));
        settings["cameraRaw"]["mixer"]["points"][0]["hueShift"] = json!(23.);
        settings["cameraRaw"]["mixer"]["points"][0]["hueRange"] = json!(150.);
        let red = ColorSample::from_rgba([255, 0, 0, 255]).unwrap();
        assert_eq!(pick_point_color(&mut settings, red, Some(0)), Some(0));
        let point = &settings["cameraRaw"]["mixer"]["points"][0];
        assert_eq!(point["hueShift"], 23.);
        assert_eq!(point["hueRange"], 30.);
        assert_eq!(point["hue"], 0.);
        for index in 1..8 {
            assert_eq!(pick_point_color(&mut settings, sample, None), Some(index));
        }
        let full = settings.clone();
        assert!(pick_point_color(&mut settings, sample, None).is_none());
        assert_eq!(settings, full);
        assert_eq!(pick_point_color(&mut settings, sample, Some(3)), Some(3));
    }

    #[test]
    fn guide_points_round_trip_lower_left_schema_and_reject_short_drags() {
        let screen = [100., 200., 400., 300.];
        assert_eq!(
            normalized_guide_point([200., 260.], screen),
            Some([0.25, 0.8])
        );
        assert!(normalized_guide_point([99., 260.], screen).is_none());
        let mut settings = filters::camera_raw_defaults();
        let before = settings.clone();
        assert!(!append_guide(&mut settings, [0., 0.], [0.005, 0.]));
        assert_eq!(settings, before);
        assert!(append_guide(&mut settings, [0.25, 0.8], [0.8, 0.7]));
        assert_eq!(settings["geometry"]["upright"], "Guided");
        assert_eq!(settings["geometry"]["guides"].as_array().unwrap().len(), 1);
        let points = guide_screen_points(
            &settings["geometry"]["guides"][0],
            Rect::from_min_size(Pos2::new(100., 200.), Vec2::new(400., 300.)),
        )
        .unwrap();
        assert!((points[0].x - 200.).abs() < 1e-5 && (points[0].y - 260.).abs() < 1e-5);
    }

    #[test]
    fn wheel_directions_and_radial_clamping_match_original() {
        assert_eq!(wheel_value([10., 0.], 10.), Some([0., 100.]));
        assert_eq!(wheel_value([0., -5.], 10.), Some([90., 50.]));
        assert_eq!(wheel_value([-10., 0.], 10.), Some([180., 100.]));
        assert_eq!(wheel_value([0., 30.], 10.), Some([270., 100.]));
        assert!(wheel_value([1., 1.], 0.).is_none());
    }

    #[test]
    fn actual_egui_pointer_updates_renderer_settings() {
        let context = egui::Context::default();
        let mut wheel = json!({"hue":0., "saturation":0., "luminance":17.});
        let mut center = Pos2::ZERO;
        let _ = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                center = grading_wheel(ui, &mut wheel, 96.).rect.center();
            });
        });
        let pointer = center + Vec2::new(0., -21.);
        let input = egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let output = context.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert!(grading_wheel(ui, &mut wheel, 96.).changed());
            });
        });
        assert_eq!(wheel["hue"], 90.);
        assert_eq!(wheel["saturation"], 50.);
        assert_eq!(wheel["luminance"], 17.);
        assert!(
            output
                .shapes
                .iter()
                .any(|shape| matches!(shape.shape, egui::Shape::Mesh(_)))
        );
    }
}
