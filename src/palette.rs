//! Movable color controls with live palette updates and canvas sampling.
use eframe::egui::{self, Color32, Sense, Vec2};

#[path = "picker_ui.rs"]
mod picker_ui;

pub struct ColorPicker {
    pub background: bool,
    pub original: [u8; 4],
    pub color: [u8; 4],
    hsva: egui::ecolor::Hsva,
    hex: String,
    hex_focused: bool,
    hex_dirty: bool,
}
impl ColorPicker {
    pub fn new(background: bool, color: [u8; 4]) -> Self {
        Self {
            background,
            original: color,
            color,
            hsva: picker_hsva(color),
            hex: format_hex(color),
            hex_focused: false,
            hex_dirty: false,
        }
    }
    pub fn sample(&mut self, color: [u8; 4]) {
        self.color = color;
        let mut hsva = picker_hsva(color);
        if hsva.s == 0. || hsva.v == 0. {
            hsva.h = self.hsva.h;
        }
        if hsva.v == 0. {
            hsva.s = self.hsva.s;
        }
        self.hsva = hsva;
        if !self.hex_focused {
            self.hex = format_hex(color);
        }
    }
    /// None keeps editing; true accepts, false restores the opening color.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<bool> {
        self.show_original_ui(
            ctx,
            if self.background {
                "背景色"
            } else {
                "前景色"
            },
        )
    }
    pub fn show_with_title(&mut self, ctx: &egui::Context, title: &str) -> Option<bool> {
        self.show_original_ui(ctx, title)
    }
}
fn picker_hsva(color: [u8; 4]) -> egui::ecolor::Hsva {
    egui::ecolor::Hsva::from_rgba_unmultiplied(
        color[0] as f32 / 255.,
        color[1] as f32 / 255.,
        color[2] as f32 / 255.,
        color[3] as f32 / 255.,
    )
}
fn format_hex(color: [u8; 4]) -> String {
    format!("#{:02X}{:02X}{:02X}", color[0], color[1], color[2])
}
fn parse_hex(text: &str, alpha: u8) -> Option<[u8; 4]> {
    let text = text.trim().trim_start_matches('#');
    if !text.is_ascii() || !matches!(text.len(), 3 | 6 | 8) {
        return None;
    }
    let expanded;
    let text = if text.len() == 3 {
        expanded = text.chars().flat_map(|c| [c, c]).collect::<String>();
        expanded.as_str()
    } else {
        text
    };
    Some([
        u8::from_str_radix(&text[0..2], 16).ok()?,
        u8::from_str_radix(&text[2..4], 16).ok()?,
        u8::from_str_radix(&text[4..6], 16).ok()?,
        if text.len() == 8 {
            u8::from_str_radix(&text[6..8], 16).ok()?
        } else {
            alpha
        },
    ])
}
pub fn sample_ring(
    painter: &egui::Painter,
    center: egui::Pos2,
    original: [u8; 4],
    sampled: [u8; 4],
) {
    let radius = 46.;
    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(24_f32, Color32::from_gray(115)),
    );
    for (color, top) in [(sampled, true), (original, false)] {
        let mut clip = painter.clip_rect();
        if top {
            clip.max.y = clip.max.y.min(center.y);
        } else {
            clip.min.y = clip.min.y.max(center.y);
        }
        painter.with_clip_rect(clip).circle_stroke(
            center,
            radius,
            egui::Stroke::new(16_f32, Color32::from_rgb(color[0], color[1], color[2])),
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_short_rgb_explicit_alpha_and_invalid_unicode() {
        assert_eq!(parse_hex(" #aB3 ", 90), Some([170, 187, 51, 90]));
        assert_eq!(parse_hex("abcdef12", 255), Some([171, 205, 239, 18]));
        assert_eq!(parse_hex("zzffdd", 255), None);
        assert_eq!(parse_hex("你ffffff", 255), None);
    }
    #[test]
    fn sampling_preserves_cancel_color() {
        let mut picker = ColorPicker::new(true, [11, 22, 33, 44]);
        picker.sample([99, 88, 77, 66]);
        assert_eq!(picker.original, [11, 22, 33, 44]);
        assert_eq!(picker.color, [99, 88, 77, 66]);
        assert_eq!(picker.hex, "#63584D");
    }

    #[test]
    fn picker_hsb_uses_original_srgb_channels_and_round_trips_exact_rgb() {
        for color in [
            [0, 0, 0, 255],
            [255, 255, 255, 0],
            [255, 128, 0, 17],
            [127, 63, 162, 90],
            [18, 52, 86, 255],
            [11, 22, 33, 0],
        ] {
            let mut picker = ColorPicker::new(false, color);
            let expected_brightness = color[..3].iter().copied().max().unwrap() as f32 / 255.;
            assert!((picker.hsva.v - expected_brightness).abs() < 1e-6);
            picker.update_from_hsva();
            assert_eq!(picker.color, color);
            assert_eq!(picker.original, color);
        }
    }

    #[test]
    fn transparent_sampling_preserves_rgb_and_black_keeps_the_hue_and_saturation() {
        let mut picker = ColorPicker::new(false, [255, 128, 0, 0]);
        let hue = picker.hsva.h;
        picker.sample([128, 128, 128, 0]);
        assert_eq!(picker.hsva.h, hue);
        assert_eq!(picker.hsva.s, 0.);
        picker.hsva.s = 0.5;
        picker.sample([0, 0, 0, 0]);
        assert_eq!(picker.hsva.h, hue);
        assert_eq!(picker.hsva.s, 0.5);
        picker.sample([11, 22, 33, 0]);
        assert_eq!(picker.color, [11, 22, 33, 0]);
        picker.update_from_hsva();
        assert_eq!(picker.color, [11, 22, 33, 0]);
        assert_eq!(picker.original, [255, 128, 0, 0]);
    }

    #[test]
    fn canvas_sampling_preserves_a_focused_hex_draft() {
        let mut picker = ColorPicker::new(false, [11, 22, 33, 44]);
        picker.hex = "#FF8000".into();
        picker.hex_focused = true;
        picker.hex_dirty = true;
        picker.sample([77, 88, 99, 44]);
        assert_eq!(picker.color, [77, 88, 99, 44]);
        assert_eq!(picker.hex, "#FF8000");
        picker.hex_focused = false;
        picker.sample([77, 88, 99, 44]);
        assert_eq!(picker.hex, "#4D5863");
        assert_eq!(picker.original, [11, 22, 33, 44]);
    }
}
