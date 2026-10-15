//! Bound text-color requests and presentation-only picker previews.
use crate::text::{self, TextStyle};
use anyhow::{Result, ensure};
use eframe::egui::{self, Color32, Sense, Stroke, Vec2};
use std::{ops::Range, sync::Arc};

#[derive(Clone)]
pub struct TextColorRequest {
    pub editor_id: egui::Id,
    pub range: Range<usize>,
    pub original: Arc<TextStyle>,
}

impl TextColorRequest {
    pub fn new(editor_id: egui::Id, style: &TextStyle, range: Range<usize>) -> Result<Self> {
        validate_range(style, &range)?;
        Ok(Self {
            editor_id,
            range,
            original: Arc::new(style.clone()),
        })
    }

    pub fn opening_color(&self) -> [u8; 4] {
        let index = if self.range.is_empty() {
            self.range.start.saturating_sub(1)
        } else {
            self.range.start
        };
        let color = self
            .original
            .color_at(index)
            .map(|v| (v * 255.).round() as u8);
        [color[0], color[1], color[2], 255]
    }

    pub fn project(&self, current: &TextStyle, color: [u8; 4]) -> Result<TextStyle> {
        ensure!(
            current.content == self.original.content,
            "文字内容已改变，旧颜色选区已失效"
        );
        validate_range(current, &self.range)?;
        let mut projected = current.clone();
        projected.set_color(
            [color[0], color[1], color[2]].map(|v| v as f64 / 255.),
            self.range.clone(),
        )?;
        projected.validate()?;
        Ok(projected)
    }
}

fn validate_range(style: &TextStyle, range: &Range<usize>) -> Result<()> {
    style.validate()?;
    ensure!(
        range.start <= range.end && range.end <= style.content.encode_utf16().count(),
        "文字颜色选区无效"
    );
    ensure!(
        text::utf16_to_byte(&style.content, range.start).is_some()
            && text::utf16_to_byte(&style.content, range.end).is_some(),
        "文字颜色选区拆分了 UTF-16 字符"
    );
    Ok(())
}

fn request_id() -> egui::Id {
    egui::Id::new("bound-text-color-request")
}

pub fn take_request(context: &egui::Context) -> Option<TextColorRequest> {
    context.data_mut(|data| {
        let request = data.get_temp::<TextColorRequest>(request_id());
        data.remove::<TextColorRequest>(request_id());
        request
    })
}

pub fn color_button(
    ui: &mut egui::Ui,
    style: &TextStyle,
    range: &Range<usize>,
    editor_id: egui::Id,
) {
    let index = if range.is_empty() {
        range.start.saturating_sub(1)
    } else {
        range.start
    };
    let preview = preview_style(ui.ctx(), editor_id, style);
    let color = preview
        .as_deref()
        .unwrap_or(style)
        .color_at(index)
        .map(|v| (v * 255.).round() as u8);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(36., 18.), Sense::click());
    ui.painter()
        .rect_filled(rect, 4, Color32::from_rgb(color[0], color[1], color[2]));
    ui.painter().rect_stroke(
        rect,
        4,
        Stroke::new(1.0_f32, ui.visuals().widgets.inactive.fg_stroke.color),
        egui::StrokeKind::Inside,
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "文字颜色")
    });
    if response.on_hover_text("文字颜色").clicked()
        && let Ok(request) = TextColorRequest::new(editor_id, style, range.clone())
    {
        ui.data_mut(|data| data.insert_temp(request_id(), request));
    }
}

#[derive(Clone)]
struct ColorPreview {
    current: Arc<TextStyle>,
    projected: Arc<TextStyle>,
    color: [u8; 4],
}

pub fn set_preview(
    context: &egui::Context,
    request: &TextColorRequest,
    current: &TextStyle,
    color: [u8; 4],
) -> Result<()> {
    if context
        .data(|data| {
            data.get_temp::<ColorPreview>(request.editor_id.with("bound-text-color-preview"))
        })
        .is_some_and(|preview| *preview.current == *current && preview.color == color)
    {
        return Ok(());
    }
    let projected = request.project(current, color)?;
    context.data_mut(|data| {
        data.insert_temp(
            request.editor_id.with("bound-text-color-preview"),
            ColorPreview {
                current: Arc::new(current.clone()),
                projected: Arc::new(projected),
                color,
            },
        )
    });
    Ok(())
}

pub fn clear_preview(context: &egui::Context, editor_id: egui::Id) {
    context
        .data_mut(|data| data.remove::<ColorPreview>(editor_id.with("bound-text-color-preview")));
}

pub fn preview_style(
    context: &egui::Context,
    editor_id: egui::Id,
    current: &TextStyle,
) -> Option<Arc<TextStyle>> {
    context
        .data(|data| data.get_temp::<ColorPreview>(editor_id.with("bound-text-color-preview")))
        .filter(|preview| *preview.current == *current)
        .map(|preview| preview.projected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{ColorRun, FontRun};

    fn mixed() -> TextStyle {
        TextStyle {
            content: "A😀中文B".into(),
            red: 0.2,
            green: 0.3,
            blue: 0.4,
            color_runs: Some(vec![
                ColorRun {
                    location: 1,
                    length: 2,
                    red: 1.,
                    green: 0.,
                    blue: 0.,
                },
                ColorRun {
                    location: 3,
                    length: 2,
                    red: 0.,
                    green: 1.,
                    blue: 0.,
                },
            ]),
            font_runs: Some(vec![FontRun {
                location: 3,
                length: 2,
                font_name: "Calibri".into(),
            }]),
            ..Default::default()
        }
    }

    #[test]
    fn capture_uses_first_selected_character_or_previous_caret_color() {
        let style = mixed();
        let id = egui::Id::new("draft");
        assert_eq!(
            TextColorRequest::new(id, &style, 3..5)
                .unwrap()
                .opening_color(),
            [0, 255, 0, 255]
        );
        assert_eq!(
            TextColorRequest::new(id, &style, 3..3)
                .unwrap()
                .opening_color(),
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn projection_changes_only_captured_characters_and_preserves_font_and_outside_color() {
        let style = mixed();
        let request = TextColorRequest::new(egui::Id::new("draft"), &style, 3..5).unwrap();
        let mut current = style.clone();
        current.font_size = 123.;
        let projected = request.project(&current, [19, 41, 67, 1]).unwrap();
        assert_eq!(projected.font_runs, current.font_runs);
        assert_eq!(projected.font_size, 123.);
        assert_eq!(projected.color_at(1), style.color_at(1));
        assert_eq!(projected.color_at(5), style.color_at(5));
        assert_eq!(projected.color_at(3), [19., 41., 67.].map(|v| v / 255.));
        assert_eq!(
            current.color_runs, style.color_runs,
            "Preview does not change the stored draft"
        );
    }

    #[test]
    fn cancel_preview_restores_exact_original_and_invalid_target_content_is_rejected() {
        let context = egui::Context::default();
        let id = egui::Id::new("draft");
        let style = mixed();
        let request = TextColorRequest::new(id, &style, 1..5).unwrap();
        set_preview(&context, &request, &style, [17, 21, 49, 255]).unwrap();
        assert_ne!(*preview_style(&context, id, &style).unwrap(), style);
        assert!(preview_style(&context, egui::Id::new("other"), &style).is_none());
        clear_preview(&context, id);
        assert!(preview_style(&context, id, &style).is_none());
        let mut changed = style.clone();
        changed.content.push('X');
        assert!(request.project(&changed, [0, 0, 0, 255]).is_err());
        assert_eq!(style, mixed());
    }

    #[test]
    fn empty_selection_colors_whole_text_and_malformed_utf16_ranges_are_rejected() {
        let style = mixed();
        let id = egui::Id::new("draft");
        let projected = TextColorRequest::new(id, &style, 3..3)
            .unwrap()
            .project(&style, [20, 40, 60, 255])
            .unwrap();
        assert!(projected.color_runs.is_none());
        assert_eq!(projected.font_runs, style.font_runs);
        for range in [2..3, Range { start: 4, end: 3 }, 0..99] {
            assert!(TextColorRequest::new(id, &style, range).is_err());
        }
    }
}
