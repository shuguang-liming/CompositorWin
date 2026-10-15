use crate::{
    locale::text as localized_text,
    text::{self, TextStyle},
};
use anyhow::{Result, ensure};
use eframe::egui;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::Arc,
};

/// Real rich text controls: character selection is converted from egui's scalar offsets
/// to the UTF-16 offsets used by .comp character style runs.
pub fn show(ui: &mut egui::Ui, value: &mut Value) -> Result<()> {
    let editor_id = ui.id().with("text-content");
    show_controls(ui, value, editor_id)?;
    let (_, rect) = ui.allocate_space(egui::vec2(ui.available_width(), 200.));
    show_inline(ui, value, editor_id, rect, 1.)
}

/// Controls and an inline editor share `editor_id` even when they occupy different UIs.
pub fn show_controls(ui: &mut egui::Ui, value: &mut Value, editor_id: egui::Id) -> Result<()> {
    ui.push_id(editor_id.with("controls"), |ui| {
        controls(ui, value, editor_id)
    })
    .inner
}

/// Horizontal controls for the 42-point tool header. The caller owns its title and actions.
/// The canvas editor keeps ownership of text input and the UTF-16 character selection.
pub fn show_header_controls(
    ui: &mut egui::Ui,
    value: &mut Value,
    editor_id: egui::Id,
) -> Result<()> {
    let mut style = load_style(value)?;
    let range = ui
        .data(|data| data.get_temp::<Range<usize>>(editor_id.with("utf16-selection")))
        .unwrap_or(0..0);
    validate_font_range(&style, &range)?;
    let mut font_chosen = Ok(None);
    ui.push_id(editor_id.with("header-controls"), |ui| {
        // The outer header owns its scroll area and height. One centered row suffices.
        let height = ui.available_height().clamp(26., 42.);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.));
                ui.spacing_mut().interact_size.y = 26.;
                ui.spacing_mut().item_spacing.x = 10.;
                font_chosen = header_font_menu(ui, &style, &range, editor_id);
                ui.add_sized(
                    [52., 26.],
                    egui::DragValue::new(&mut style.font_size)
                        .range(1. ..=2000.)
                        .speed(1.),
                )
                .on_hover_text("字号（像素）");
                ui.label("px");
                crate::text_color::color_button(ui, &style, &range, editor_id);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.;
                    for (alignment, label) in [
                        ("Left", "左对齐"),
                        ("Center", "居中对齐"),
                        ("Right", "右对齐"),
                    ] {
                        if header_alignment_button(
                            ui,
                            alignment,
                            label,
                            style.alignment == alignment,
                        )
                        .clicked()
                        {
                            style.alignment = alignment.into();
                        }
                    }
                });
                ui.label("字距");
                ui.add_sized(
                    [45., 26.],
                    egui::DragValue::new(&mut style.tracking)
                        .range(-100. ..=1000.)
                        .speed(1.),
                );
                ui.label("行距");
                ui.add_sized(
                    [52., 26.],
                    egui::DragValue::new(&mut style.leading)
                        .range(0. ..=5000.)
                        .speed(1.)
                        .custom_formatter(|value, _| {
                            if value == 0. {
                                "自动".into()
                            } else {
                                format!("{value:.0}")
                            }
                        })
                        .custom_parser(|text| {
                            let text = text.trim();
                            if text.is_empty() || text == "自动" {
                                Some(0.)
                            } else {
                                text.parse().ok()
                            }
                        }),
                )
                .on_hover_text("基线到基线的行高；自动为字号的120%");
                ui.menu_button("更多", |ui| {
                    let mut fixed = style.box_size.is_some();
                    if ui.checkbox(&mut fixed, "段落文本框").changed() {
                        style.box_size = fixed.then_some([400., 200.]);
                    }
                    if let Some(size) = &mut style.box_size {
                        ui.horizontal(|ui| {
                            ui.label("宽度");
                            ui.add(
                                egui::DragValue::new(&mut size[0])
                                    .range(16. ..=30_000.)
                                    .speed(1.),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("高度");
                            ui.add(
                                egui::DragValue::new(&mut size[1])
                                    .range(16. ..=30_000.)
                                    .speed(1.),
                            );
                        });
                    }
                    ui.separator();
                    ui.weak(if range.is_empty() {
                        "在画布中选中字符以设置其字体和颜色。"
                    } else {
                        "字体和颜色应用到当前选中的字符。"
                    });
                });
            },
        );
    });
    if let Some(font) = font_chosen? {
        style.set_font(&font, range.clone())?;
    }
    style.validate()?;
    *value = style.to_value();
    Ok(())
}

fn validate_font_range(style: &TextStyle, range: &Range<usize>) -> Result<()> {
    ensure!(range.start <= range.end, "文字字体选区方向无效");
    ensure!(
        range.end <= style.content.encode_utf16().count(),
        "文字字体选区超出内容"
    );
    ensure!(
        text::utf16_to_byte(&style.content, range.start).is_some()
            && text::utf16_to_byte(&style.content, range.end).is_some(),
        "文字字体选区拆分了 UTF-16 字符"
    );
    Ok(())
}

/// Inspect intersecting runs and intervening base-font gaps without expanding every unit.
fn selection_font_name<'a>(style: &'a TextStyle, range: &Range<usize>) -> Result<Option<&'a str>> {
    validate_font_range(style, range)?;
    if range.is_empty() {
        return Ok(Some(style.font_at(range.start.saturating_sub(1))));
    }
    let font = style.font_at(range.start);
    let mut cursor = range.start;
    for run in style.font_runs.iter().flatten() {
        let end = run.location + run.length;
        if run.location >= range.end {
            break;
        }
        if end <= cursor {
            continue;
        }
        if (run.location > cursor && style.font_name != font) || run.font_name != font {
            return Ok(None);
        }
        cursor = end.min(range.end);
    }
    if cursor < range.end && style.font_name != font {
        return Ok(None);
    }
    Ok(Some(font))
}

#[derive(Clone)]
struct FontMenuPreview {
    original: Arc<TextStyle>,
    range: Range<usize>,
    projected: Option<Arc<TextStyle>>,
    face: Option<String>,
    frame: u64,
}

impl FontMenuPreview {
    fn begin(style: &TextStyle, range: Range<usize>) -> Self {
        Self {
            original: Arc::new(style.clone()),
            range,
            projected: None,
            face: None,
            frame: 0,
        }
    }

    fn show(&mut self, font: &str, frame: u64) -> Result<()> {
        if self.face.as_deref() != Some(font) {
            let projected = self.keep(font)?;
            self.projected = Some(Arc::new(projected));
            self.face = Some(font.into());
        }
        self.frame = frame;
        Ok(())
    }

    fn revert(&mut self) {
        self.projected = None;
        self.face = None;
    }

    fn keep(&self, font: &str) -> Result<TextStyle> {
        validate_font_range(&self.original, &self.range)?;
        let mut chosen = (*self.original).clone();
        chosen.set_font(font, self.range.clone())?;
        chosen.validate()?;
        Ok(chosen)
    }

    fn render_style<'a>(&'a self, current: &'a TextStyle, frame: u64) -> &'a TextStyle {
        if self.frame == frame && *self.original == *current {
            self.projected.as_deref().unwrap_or(current)
        } else {
            current
        }
    }
}

fn header_font_menu(
    ui: &mut egui::Ui,
    style: &TextStyle,
    range: &Range<usize>,
    editor_id: egui::Id,
) -> Result<Option<String>> {
    let selected = selection_font_name(style, range)?;
    let id = editor_id.with("font-menu-preview");
    let mut preview = ui
        .data(|data| data.get_temp::<FontMenuPreview>(id))
        .filter(|preview| *preview.original == *style && preview.range == *range);
    let mut hovered = None;
    let mut chosen = None;
    let menu = egui::ComboBox::from_id_salt("text-font")
        .selected_text(selected.unwrap_or("（多种字体）"))
        .width(210.)
        .truncate()
        .show_ui(ui, |ui| {
            preview.get_or_insert_with(|| FontMenuPreview::begin(style, range.clone()));
            if selected.is_none() {
                ui.add_enabled(false, egui::Label::new("（多种字体）"));
                ui.separator();
            }
            let names = header_font_names(ui);
            for name in names.iter() {
                let response = ui.selectable_label(selected == Some(name.as_str()), name);
                if response.hovered() {
                    hovered = Some(name.clone());
                }
                // Clicking the first letter's face still unifies a mixed selection.
                if response.clicked() {
                    chosen = Some(name.clone());
                    ui.close();
                }
            }
        });
    let open = egui::ComboBox::is_open(ui.ctx(), menu.response.id);
    if chosen.is_none()
        && open
        && let Some(mut preview) = preview
    {
        if let Some(font) = hovered {
            preview.show(&font, ui.ctx().cumulative_frame_nr())?;
        } else {
            preview.revert();
        }
        ui.data_mut(|data| data.insert_temp(id, preview));
    } else {
        ui.data_mut(|data| data.remove::<FontMenuPreview>(id));
    }
    Ok(chosen)
}

fn header_font_names(ui: &egui::Ui) -> Arc<Vec<String>> {
    let id = egui::Id::new("compositor-text-header-font-names");
    if let Some(names) = ui.data(|data| data.get_temp::<Arc<Vec<String>>>(id)) {
        return names;
    }
    // Called only when the face menu is opened, never once per header frame.
    let names = Arc::new(text::font_names());
    ui.data_mut(|data| data.insert_temp(id, names.clone()));
    names
}

fn header_alignment_button(
    ui: &mut egui::Ui,
    alignment: &str,
    label: &str,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(30., 26.), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            label,
        )
    });
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        if selected || response.hovered() || response.has_focus() {
            ui.painter().rect_filled(rect, 4., visuals.bg_fill);
        }
        let stroke = egui::Stroke::new(1.2_f32, visuals.fg_stroke.color);
        for (index, width) in [16., 11., 16., 11.].into_iter().enumerate() {
            let x = match alignment {
                "Center" => rect.center().x - width / 2.,
                "Right" => rect.right() - 7. - width,
                _ => rect.left() + 7.,
            };
            let y = rect.center().y - 6. + index as f32 * 4.;
            ui.painter()
                .line_segment([egui::pos2(x, y), egui::pos2(x + width, y)], stroke);
        }
    }
    response.on_hover_text(label)
}

fn load_style(value: &Value) -> Result<TextStyle> {
    if value.is_null() {
        Ok(TextStyle::default())
    } else {
        TextStyle::from_value(value)
    }
}

fn controls(ui: &mut egui::Ui, value: &mut Value, editor_id: egui::Id) -> Result<()> {
    let mut style = load_style(value)?;
    let cursor_id = editor_id.with("utf16-selection");
    let range = ui
        .data(|d| d.get_temp::<Range<usize>>(cursor_id))
        .unwrap_or(0..0);
    let mut font_chosen = Ok(None);
    ui.horizontal(|ui| {
        ui.label("字体");
        font_chosen = header_font_menu(ui, &style, &range, editor_id);
        ui.label("大小");
        ui.add(
            egui::DragValue::new(&mut style.font_size)
                .range(1. ..=2000.)
                .speed(0.25)
                .suffix(" 像素"),
        );
    });
    if let Some(font) = font_chosen? {
        style.set_font(&font, range.clone())?;
    }
    ui.horizontal(|ui| {
        ui.label("颜色");
        crate::text_color::color_button(ui, &style, &range, editor_id);
        egui::ComboBox::from_id_salt("text-alignment")
            .selected_text(localized_text(&style.alignment))
            .show_ui(ui, |ui| {
                for choice in ["Left", "Center", "Right"] {
                    ui.selectable_value(
                        &mut style.alignment,
                        choice.into(),
                        localized_text(choice),
                    );
                }
            });
        ui.label("字距");
        ui.add(
            egui::DragValue::new(&mut style.tracking)
                .range(-100. ..=1000.)
                .speed(0.1),
        );
    });
    ui.horizontal(|ui| {
        ui.label("行距");
        ui.add(
            egui::DragValue::new(&mut style.leading)
                .range(0. ..=5000.)
                .speed(0.25),
        );
        if style.leading == 0. {
            ui.weak(format!("自动：{:.1} 像素", style.font_size * 1.2));
        }
        let mut fixed = style.box_size.is_some();
        if ui.checkbox(&mut fixed, "段落文本框").changed() {
            style.box_size = if fixed { Some([400., 200.]) } else { None };
        }
    });
    if let Some(size) = &mut style.box_size {
        ui.horizontal(|ui| {
            ui.label("宽度");
            ui.add(
                egui::DragValue::new(&mut size[0])
                    .range(16. ..=30_000.)
                    .speed(1.),
            );
            ui.label("高度");
            ui.add(
                egui::DragValue::new(&mut size[1])
                    .range(16. ..=30_000.)
                    .speed(1.),
            );
        });
    }
    ui.weak(if range.is_empty() {
        "在文本框中选中字符，再设置这些字符的字体或颜色。"
    } else {
        "字体和颜色将应用到选中的字符。"
    });
    style.validate()?;
    *value = style.to_value();
    Ok(())
}

/// Draws an editable text field over a canvas rectangle without taking focus each frame.
pub fn show_inline(
    ui: &mut egui::Ui,
    value: &mut Value,
    editor_id: egui::Id,
    rect: egui::Rect,
    scale: f32,
) -> Result<()> {
    show_inline_with_interaction(ui, value, editor_id, rect, scale, true)
}

pub fn show_inline_with_interaction(
    ui: &mut egui::Ui,
    value: &mut Value,
    editor_id: egui::Id,
    rect: egui::Rect,
    scale: f32,
    interactive: bool,
) -> Result<()> {
    ensure!(scale.is_finite() && scale > 0., "文字编辑缩放比例无效");
    ensure!(rect.is_finite() && rect.is_positive(), "文字编辑区域无效");
    let mut style = load_style(value)?;
    ensure!(
        style.font_size * scale as f64 <= 2048.,
        "请缩小画布视图后编辑当前字号的文字"
    );
    // Only the layout sees a hovered face. Content edits and the stored draft retain
    // their exact original runs until a deliberate menu choice commits the face.
    let preview =
        ui.data(|data| data.get_temp::<FontMenuPreview>(editor_id.with("font-menu-preview")));
    let render_style = preview.as_ref().map_or(&style, |preview| {
        preview.render_style(&style, ui.ctx().cumulative_frame_nr())
    });
    let color_preview = crate::text_color::preview_style(ui.ctx(), editor_id, &style);
    let render_style = color_preview.as_deref().unwrap_or(render_style);
    let families = inline_fonts(ui, render_style);
    let previous_cursor = egui::TextEdit::load_state(ui.ctx(), editor_id)
        .and_then(|state| state.cursor.char_range())
        .map(|cursor| cursor.as_sorted_char_range());
    let original = render_style.clone();
    let mut edited = style.content.clone();
    let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, width: f32| {
        let mut rendering = original.clone();
        let _ = replace_content_from_cursor(
            &mut rendering,
            buffer.as_str(),
            previous_cursor.as_ref(),
            None,
        );
        let job = layout_job(&rendering, &families, scale, width);
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    ui.put(
        rect,
        egui::TextEdit::multiline(&mut edited)
            .interactive(interactive)
            .id(editor_id)
            .font(egui::FontId::new(
                style.font_size as f32 * scale,
                egui::FontFamily::Proportional,
            ))
            .frame(false)
            .margin(egui::Margin::ZERO)
            .min_size(rect.size())
            .desired_width(rect.width())
            .desired_rows(1)
            .layouter(&mut layouter),
    );
    let current_cursor = egui::TextEdit::load_state(ui.ctx(), editor_id)
        .and_then(|state| state.cursor.char_range())
        .map(|cursor| cursor.as_sorted_char_range());
    replace_content_from_cursor(
        &mut style,
        &edited,
        previous_cursor.as_ref(),
        current_cursor.as_ref(),
    )?;
    if let Some(range) = current_cursor {
        let utf16 = |index| {
            style
                .content
                .chars()
                .take(index)
                .map(char::len_utf16)
                .sum::<usize>()
        };
        ui.data_mut(|data| {
            data.insert_temp(
                editor_id.with("utf16-selection"),
                utf16(range.start)..utf16(range.end),
            )
        });
    }
    style.validate()?;
    *value = style.to_value();
    Ok(())
}

/// Repeated text makes a prefix/suffix diff ambiguous; the edit cursor identifies its location.
fn replace_content_from_cursor(
    style: &mut TextStyle,
    edited: &str,
    previous: Option<&Range<usize>>,
    current: Option<&Range<usize>>,
) -> Result<()> {
    if style.content == edited {
        return Ok(());
    }
    let old = style.content.chars().collect::<Vec<_>>();
    let new = edited.chars().collect::<Vec<_>>();
    let mut candidates = Vec::new();
    if let Some(previous) = previous {
        if !previous.is_empty() {
            candidates.push(previous.clone());
        } else if let Some(current) = current.filter(|range| range.is_empty()) {
            let start = previous.start.min(current.start);
            let inserted = current.start.saturating_sub(start);
            if let Some(end) = old
                .len()
                .checked_add(inserted)
                .and_then(|length| length.checked_sub(new.len()))
                .and_then(|removed| start.checked_add(removed))
            {
                candidates.push(start..end);
            }
        }
        candidates.push(previous.clone());
    }
    for range in candidates {
        if range.start > range.end || range.end > old.len() || range.start > new.len() {
            continue;
        }
        let Some(new_end) = new.len().checked_sub(old.len() - range.end) else {
            continue;
        };
        if new_end < range.start
            || old[..range.start] != new[..range.start]
            || old[range.end..] != new[new_end..]
        {
            continue;
        }
        let start = old[..range.start].iter().map(|c| c.len_utf16()).sum();
        let end = old[..range.end].iter().map(|c| c.len_utf16()).sum();
        let replacement = new[range.start..new_end].iter().collect::<String>();
        return style.replace_utf16(start..end, &replacement);
    }
    replace_content(style, edited)
}

/// Preserve surviving character runs and inherit formatting at the insertion boundary.
fn replace_content(style: &mut TextStyle, edited: &str) -> Result<()> {
    if edited != style.content {
        let old = style.content.chars().collect::<Vec<_>>();
        let new = edited.chars().collect::<Vec<_>>();
        let mut prefix = 0;
        while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < old.len() - prefix
            && suffix < new.len() - prefix
            && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
        {
            suffix += 1;
        }
        let start = old[..prefix].iter().map(|c| c.len_utf16()).sum();
        let end = old[..old.len() - suffix]
            .iter()
            .map(|c| c.len_utf16())
            .sum();
        let replacement = new[prefix..new.len() - suffix].iter().collect::<String>();
        style.replace_utf16(start..end, &replacement)?;
    }
    Ok(())
}

#[derive(Clone)]
struct InlineFonts {
    definitions: egui::FontDefinitions,
    attempted: HashSet<String>,
    fallbacks: Vec<String>,
}

fn inline_fonts(ui: &egui::Ui, style: &TextStyle) -> HashMap<String, egui::FontFamily> {
    let id = egui::Id::new("compositor-inline-fonts");
    let mut cache = ui
        .data_mut(|data| data.get_temp::<InlineFonts>(id))
        .unwrap_or_else(|| InlineFonts {
            definitions: ui.fonts(|fonts| fonts.definitions().clone()),
            attempted: HashSet::new(),
            fallbacks: {
                let installed = text::font_names();
                let cjk = ["Microsoft YaHei", "Microsoft JhengHei", "SimSun"]
                    .into_iter()
                    .find(|name| installed.iter().any(|font| font == name));
                cjk.into_iter()
                    .chain(["Segoe UI Emoji", "Segoe UI Symbol"])
                    .map(str::to_owned)
                    .collect()
            },
        });
    let names = std::iter::once(style.font_name.clone())
        .chain(
            style
                .font_runs
                .iter()
                .flatten()
                .map(|run| run.font_name.clone()),
        )
        .collect::<HashSet<_>>();
    let fallback_names = cache.fallbacks.clone();
    let mut changed = false;
    for name in names
        .iter()
        .map(String::as_str)
        .chain(fallback_names.iter().map(String::as_str))
    {
        if cache.attempted.insert(name.to_owned())
            && let Some(data) = crate::locale::shared_ui_font(name).or_else(|| {
                let (bytes, index) = text::font_data(name)?;
                let mut data = egui::FontData::from_owned(bytes);
                data.index = index;
                Some(Arc::new(data))
            })
        {
            cache
                .definitions
                .font_data
                .insert(format!("compositor:{name}"), data);
            changed = true;
        }
    }
    for name in &names {
        let family = egui::FontFamily::Name(format!("compositor:{name}").into());
        if cache.definitions.families.contains_key(&family) {
            continue;
        }
        let mut fallbacks = Vec::new();
        for font in std::iter::once(name.as_str()).chain(fallback_names.iter().map(String::as_str))
        {
            let key = format!("compositor:{font}");
            if cache.definitions.font_data.contains_key(&key) && !fallbacks.contains(&key) {
                fallbacks.push(key);
            }
        }
        fallbacks.extend(
            cache
                .definitions
                .families
                .get(&egui::FontFamily::Proportional)
                .into_iter()
                .flatten()
                .cloned(),
        );
        cache.definitions.families.insert(family, fallbacks);
        changed = true;
    }
    if changed {
        ui.ctx().set_fonts(cache.definitions.clone());
        ui.ctx().request_repaint();
    }
    ui.data_mut(|data| data.insert_temp(id, cache));
    let active = ui.fonts(|fonts| fonts.families());
    names
        .into_iter()
        .map(|name| {
            let family = egui::FontFamily::Name(format!("compositor:{name}").into());
            let family = if active.contains(&family) {
                family
            } else {
                egui::FontFamily::Proportional
            };
            (name, family)
        })
        .collect()
}

fn layout_job(
    style: &TextStyle,
    families: &HashMap<String, egui::FontFamily>,
    scale: f32,
    width: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob {
        text: style.content.clone(),
        halign: match style.alignment.as_str() {
            "Center" => egui::Align::Center,
            "Right" => egui::Align::RIGHT,
            _ => egui::Align::LEFT,
        },
        ..Default::default()
    };
    job.wrap.max_width = if style.box_size.is_some() {
        width
    } else {
        f32::INFINITY
    };
    let line_height = if style.leading > 0. {
        style.leading
    } else {
        style.font_size * 1.2
    } as f32
        * scale;
    let mut units = 0;
    for (byte, character) in style.content.char_indices() {
        let color = style
            .color_at(units)
            .map(|value| (value * 255.).round() as u8);
        let format = egui::TextFormat {
            font_id: egui::FontId::new(
                style.font_size as f32 * scale,
                families
                    .get(style.font_at(units))
                    .cloned()
                    .unwrap_or(egui::FontFamily::Proportional),
            ),
            color: egui::Color32::from_rgb(color[0], color[1], color[2]),
            extra_letter_spacing: style.tracking as f32 * scale,
            line_height: Some(line_height),
            ..Default::default()
        };
        if let Some(last) = job.sections.last_mut()
            && last.format == format
        {
            last.byte_range.end = byte + character.len_utf8();
        } else {
            job.sections.push(egui::text::LayoutSection {
                leading_space: 0.,
                byte_range: byte..byte + character.len_utf8(),
                format,
            });
        }
        units += character.len_utf16();
    }
    if job.sections.is_empty() {
        job.append(
            "",
            0.,
            egui::TextFormat {
                font_id: egui::FontId::new(
                    style.font_size as f32 * scale,
                    egui::FontFamily::Proportional,
                ),
                line_height: Some(line_height),
                ..Default::default()
            },
        );
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_menu_titles_cover_base_gaps_mixed_runs_and_caret_inheritance() {
        let mut style = TextStyle {
            content: "A😀BC".into(),
            ..Default::default()
        };
        style.set_font("Consolas", 1..3).unwrap();
        style.set_font("Consolas", 4..5).unwrap();
        assert_eq!(
            selection_font_name(&style, &(1..3)).unwrap(),
            Some("Consolas")
        );
        assert_eq!(selection_font_name(&style, &(0..3)).unwrap(), None);
        assert_eq!(selection_font_name(&style, &(1..5)).unwrap(), None);
        assert_eq!(selection_font_name(&style, &(3..4)).unwrap(), Some("Arial"));
        assert_eq!(selection_font_name(&style, &(0..0)).unwrap(), Some("Arial"));
        assert_eq!(
            selection_font_name(&style, &(3..3)).unwrap(),
            Some("Consolas")
        );
        assert_eq!(
            selection_font_name(&style, &(5..5)).unwrap(),
            Some("Consolas")
        );
        // Adjacent explicit and base runs with the same face remain uniform.
        style.font_runs.as_mut().unwrap()[0].font_name = "Arial".into();
        assert_eq!(selection_font_name(&style, &(0..4)).unwrap(), Some("Arial"));
    }

    #[test]
    fn font_menu_rejects_surrogate_splits_reversed_and_stale_utf16_ranges() {
        let style = TextStyle {
            content: "A😀B".into(),
            ..Default::default()
        };
        let reversed = std::ops::Range { start: 3, end: 1 };
        for range in [1..2, 2..3, 2..2, 4..5, reversed, usize::MAX..usize::MAX] {
            assert!(selection_font_name(&style, &range).is_err(), "{range:?}");
            assert!(
                FontMenuPreview::begin(&style, range)
                    .keep("Consolas")
                    .is_err()
            );
        }
        let empty = TextStyle {
            content: String::new(),
            ..Default::default()
        };
        assert_eq!(selection_font_name(&empty, &(0..0)).unwrap(), Some("Arial"));
    }

    #[test]
    fn font_hover_preview_reverts_exact_style_and_never_accumulates_faces() {
        let mut style = TextStyle {
            content: "A😀BC".into(),
            font_size: 27.,
            leading: 33.,
            tracking: 1.5,
            alignment: "Right".into(),
            box_size: Some([300., 140.]),
            ..Default::default()
        };
        style.set_font("Consolas", 0..1).unwrap();
        style.set_font("Times New Roman", 3..4).unwrap();
        style.set_color([1., 0.25, 0.5], 1..3).unwrap();
        let original = style.to_value();
        let mut preview = FontMenuPreview::begin(&style, 1..4);
        preview.show("Segoe UI", 9).unwrap();
        let rendered = preview.render_style(&style, 9);
        assert_eq!(rendered.font_at(1), "Segoe UI");
        assert_eq!(rendered.font_at(3), "Segoe UI");
        assert_eq!(rendered.font_at(0), style.font_at(0));
        assert_eq!(rendered.font_at(4), style.font_at(4));
        assert_eq!(rendered.color_runs, style.color_runs);
        assert_eq!(style.to_value(), original);
        preview.show("Arial", 10).unwrap();
        let mut expected = style.clone();
        expected.set_font("Arial", 1..4).unwrap();
        assert_eq!(preview.render_style(&style, 10), &expected);
        // A menu not visited this frame and a changed draft cannot retain old hover.
        assert_eq!(preview.render_style(&style, 11), &style);
        let mut changed = style.clone();
        changed.font_size += 1.;
        assert_eq!(preview.render_style(&changed, 10), &changed);
        preview.revert();
        assert_eq!(preview.render_style(&style, 10).to_value(), original);
        assert!(preview.keep("").is_err());
        assert!(preview.keep("bad\nfont").is_err());
        assert_eq!(style.to_value(), original);
    }

    #[test]
    fn choosing_first_letter_face_unifies_mixed_selection_and_preserves_outside_runs() {
        let mut style = TextStyle {
            content: "XA😀BY".into(),
            ..Default::default()
        };
        style.set_font("Consolas", 1..2).unwrap();
        style.set_font("Times New Roman", 2..4).unwrap();
        style.set_color([0., 0., 1.], 2..4).unwrap();
        assert_eq!(selection_font_name(&style, &(1..5)).unwrap(), None);
        let chosen = FontMenuPreview::begin(&style, 1..5)
            .keep(style.font_at(1))
            .unwrap();
        assert_eq!(
            selection_font_name(&chosen, &(1..5)).unwrap(),
            Some("Consolas")
        );
        assert_eq!(chosen.font_at(0), "Arial");
        assert_eq!(chosen.font_at(5), "Arial");
        assert_eq!(chosen.color_runs, style.color_runs);
        assert_eq!(chosen.content, style.content);
    }

    fn inline_frame(
        context: &egui::Context,
        value: &mut Value,
        id: egui::Id,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800., 600.),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show_inline(
                        ui,
                        value,
                        id,
                        egui::Rect::from_min_size(egui::pos2(80., 100.), egui::vec2(400., 160.)),
                        1.,
                    )
                    .unwrap();
                });
            },
        )
    }

    #[test]
    fn panel_preserves_existing_styles_without_interaction() {
        let context = egui::Context::default();
        let mut style = TextStyle {
            content: "A😀B".into(),
            font_size: 24.,
            ..Default::default()
        };
        style.set_color([1., 0., 0.], 1..3).unwrap();
        let mut value = style.to_value();
        let _ = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(ui, &mut value).unwrap();
            });
        });
        assert_eq!(TextStyle::from_value(&value).unwrap(), style);
    }

    #[test]
    fn inline_text_input_replaces_emoji_and_preserves_shifted_character_styles() {
        let context = egui::Context::default();
        let id = egui::Id::new("inline-unicode-input");
        let mut style = TextStyle {
            content: "A😀BC".into(),
            font_size: 24.,
            ..Default::default()
        };
        style.set_color([1., 0., 0.], 0..1).unwrap();
        style.set_font("Consolas", 0..1).unwrap();
        style.set_color([0., 0., 1.], 3..4).unwrap();
        style.set_font("Times New Roman", 3..4).unwrap();
        let mut value = style.to_value();
        inline_frame(&context, &mut value, id, vec![]);
        inline_frame(&context, &mut value, id, vec![]);
        let mut state = egui::TextEdit::load_state(&context, id).unwrap();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(1),
                egui::text::CCursor::new(2),
            )));
        state.store(&context, id);
        context.memory_mut(|memory| memory.request_focus(id));
        inline_frame(
            &context,
            &mut value,
            id,
            vec![egui::Event::Text("猫".into())],
        );
        let edited = TextStyle::from_value(&value).unwrap();
        assert_eq!(edited.content, "A猫BC");
        assert_eq!(edited.color_at(1), [1., 0., 0.]);
        assert_eq!(edited.font_at(1), "Consolas");
        assert_eq!(edited.color_at(2), [0., 0., 1.]);
        assert_eq!(edited.font_at(2), "Times New Roman");
        assert_eq!(
            context.data(|data| data.get_temp::<Range<usize>>(id.with("utf16-selection"))),
            Some(2..2)
        );
    }

    #[test]
    fn inline_selection_is_shared_as_utf16_with_controls_in_another_ui() {
        let context = egui::Context::default();
        let id = egui::Id::new("shared-inline-selection");
        let style = TextStyle {
            content: "A😀B".into(),
            font_size: 24.,
            ..Default::default()
        };
        let mut value = style.to_value();
        inline_frame(&context, &mut value, id, vec![]);
        let mut state = egui::TextEdit::load_state(&context, id).unwrap();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(1),
                egui::text::CCursor::new(2),
            )));
        state.store(&context, id);
        inline_frame(&context, &mut value, id, vec![]);
        assert_eq!(
            context.data(|data| data.get_temp::<Range<usize>>(id.with("utf16-selection"))),
            Some(1..3)
        );
        let _ = context.run(egui::RawInput::default(), |ctx| {
            egui::Window::new("Text controls").show(ctx, |ui| {
                show_controls(ui, &mut value, id).unwrap();
            });
        });
        assert_eq!(TextStyle::from_value(&value).unwrap(), style);
        assert_eq!(
            context.data(|data| data.get_temp::<Range<usize>>(id.with("utf16-selection"))),
            Some(1..3)
        );
    }

    #[test]
    fn deleting_repeated_text_preserves_the_actual_surviving_character_styles() {
        for (cursor, key, expected) in [
            (0, egui::Key::Delete, [[0., 0., 1.], [0., 1., 0.]]),
            (1, egui::Key::Backspace, [[0., 0., 1.], [0., 1., 0.]]),
            (2, egui::Key::Backspace, [[1., 0., 0.], [0., 1., 0.]]),
        ] {
            let context = egui::Context::default();
            let id = egui::Id::new(("repeated-delete", cursor, key));
            let mut style = TextStyle {
                content: "AAA".into(),
                font_size: 24.,
                ..Default::default()
            };
            for (index, color) in [[1., 0., 0.], [0., 0., 1.], [0., 1., 0.]]
                .into_iter()
                .enumerate()
            {
                style.set_color(color, index..index + 1).unwrap();
            }
            let mut value = style.to_value();
            inline_frame(&context, &mut value, id, vec![]);
            let mut state = egui::TextEdit::load_state(&context, id).unwrap();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(cursor),
                )));
            state.store(&context, id);
            context.memory_mut(|memory| memory.request_focus(id));
            inline_frame(
                &context,
                &mut value,
                id,
                vec![egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
            );
            let edited = TextStyle::from_value(&value).unwrap();
            assert_eq!(edited.content, "AA");
            assert_eq!(edited.color_at(0), expected[0], "{key:?} at {cursor}");
            assert_eq!(edited.color_at(1), expected[1], "{key:?} at {cursor}");
        }
    }

    #[test]
    fn replacing_selected_repeated_text_preserves_the_suffix_style() {
        let context = egui::Context::default();
        let id = egui::Id::new("repeated-selection-replace");
        let mut style = TextStyle {
            content: "AAA".into(),
            font_size: 24.,
            ..Default::default()
        };
        style.set_color([1., 0., 0.], 0..1).unwrap();
        style.set_color([0., 0., 1.], 1..2).unwrap();
        style.set_color([0., 1., 0.], 2..3).unwrap();
        let mut value = style.to_value();
        inline_frame(&context, &mut value, id, vec![]);
        let mut state = egui::TextEdit::load_state(&context, id).unwrap();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(1),
            )));
        state.store(&context, id);
        context.memory_mut(|memory| memory.request_focus(id));
        inline_frame(
            &context,
            &mut value,
            id,
            vec![egui::Event::Text("AA".into())],
        );
        let edited = TextStyle::from_value(&value).unwrap();
        assert_eq!(edited.content, "AAAA");
        assert_eq!(edited.color_at(2), [0., 0., 1.]);
        assert_eq!(edited.color_at(3), [0., 1., 0.]);
    }

    #[test]
    fn inline_editor_leaves_existing_focus_untouched() {
        let context = egui::Context::default();
        let id = egui::Id::new("unfocused-inline-editor");
        let other = egui::Id::new("another-control");
        let mut value = TextStyle {
            font_size: 24.,
            ..Default::default()
        }
        .to_value();
        context.memory_mut(|memory| memory.request_focus(other));
        inline_frame(&context, &mut value, id, vec![]);
        assert_ne!(context.memory(|memory| memory.focused()), Some(id));
    }

    #[test]
    fn layout_uses_utf16_run_colors_and_scaled_spacing() {
        let mut style = TextStyle {
            content: "A😀B".into(),
            font_size: 24.,
            leading: 30.,
            tracking: 2.,
            alignment: "Center".into(),
            box_size: Some([300., 100.]),
            ..Default::default()
        };
        style.set_color([1., 0., 0.], 1..3).unwrap();
        let job = layout_job(&style, &HashMap::new(), 2., 600.);
        assert_eq!(job.halign, egui::Align::Center);
        assert_eq!(job.wrap.max_width, 600.);
        assert_eq!(job.sections.len(), 3);
        assert_eq!(job.sections[1].byte_range, 1..5);
        assert_eq!(job.sections[1].format.color, egui::Color32::RED);
        assert_eq!(job.sections[1].format.font_id.size, 48.);
        assert_eq!(job.sections[1].format.line_height, Some(60.));
        assert_eq!(job.sections[1].format.extra_letter_spacing, 4.);
    }

    #[cfg(windows)]
    #[test]
    fn chinese_font_menu_click_commits_face_while_hover_and_escape_preserve_style() {
        fn frame(
            context: &egui::Context,
            value: &mut Value,
            id: egui::Id,
            events: Vec<egui::Event>,
        ) -> egui::FullOutput {
            context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000., 600.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        show_header_controls(ui, value, id).unwrap();
                    });
                },
            )
        }

        fn label_center(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
            fn find(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
                match shape {
                    egui::Shape::Text(text) if text.galley.job.text == label => {
                        Some(text.pos + text.galley.rect.center().to_vec2())
                    }
                    egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, label)),
                    _ => None,
                }
            }
            output
                .shapes
                .iter()
                .find_map(|shape| find(&shape.shape, label))
                .unwrap_or_else(|| panic!("The visible font menu is missing {label}"))
        }

        fn click(context: &egui::Context, value: &mut Value, id: egui::Id, pos: egui::Pos2) {
            for pressed in [true, false] {
                frame(
                    context,
                    value,
                    id,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Default::default(),
                        },
                    ],
                );
            }
        }

        let selected = "微软雅黑";
        assert!(text::font_names().iter().any(|name| name == selected));
        let expected_face = text::font_data(selected).unwrap();
        let context = egui::Context::default();
        context.style_mut(|style| style.animation_time = 0.);
        let id = egui::Id::new("chinese-font-menu-selection");
        let style = TextStyle {
            content: "中文 ABC".into(),
            font_size: 24.,
            ..Default::default()
        };
        let mut value = style.to_value();
        context.data_mut(|data| {
            data.insert_temp(
                id.with("utf16-selection"),
                0..style.content.encode_utf16().count(),
            );
            // Keep the real installed choice visible without scrolling a system-dependent list.
            data.insert_temp(
                egui::Id::new("compositor-text-header-font-names"),
                Arc::new(vec!["Arial".to_owned(), selected.to_owned()]),
            );
        });

        let output = frame(&context, &mut value, id, vec![]);
        click(&context, &mut value, id, label_center(&output, "Arial"));
        let output = frame(&context, &mut value, id, vec![]);
        frame(
            &context,
            &mut value,
            id,
            vec![egui::Event::PointerMoved(label_center(&output, selected))],
        );
        let preview_id = id.with("font-menu-preview");
        let preview = context
            .data(|data| data.get_temp::<FontMenuPreview>(preview_id))
            .unwrap();
        assert_eq!(preview.face.as_deref(), Some(selected));
        assert_eq!(preview.projected.unwrap().font_at(0), selected);
        assert_eq!(TextStyle::from_value(&value).unwrap(), style);

        for pressed in [true, false] {
            frame(
                &context,
                &mut value,
                id,
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: Some(egui::Key::Escape),
                    pressed,
                    repeat: false,
                    modifiers: Default::default(),
                }],
            );
        }
        assert!(
            context
                .data(|data| data.get_temp::<FontMenuPreview>(preview_id))
                .is_none()
        );
        assert_eq!(TextStyle::from_value(&value).unwrap(), style);

        let output = frame(&context, &mut value, id, vec![]);
        click(&context, &mut value, id, label_center(&output, "Arial"));
        let output = frame(&context, &mut value, id, vec![]);
        click(&context, &mut value, id, label_center(&output, selected));
        let chosen = TextStyle::from_value(&value).unwrap();
        assert_eq!(
            selection_font_name(&chosen, &(0..6)).unwrap(),
            Some(selected)
        );
        assert!(
            context
                .data(|data| data.get_temp::<FontMenuPreview>(preview_id))
                .is_none()
        );

        inline_frame(&context, &mut value, id, vec![]);
        inline_frame(&context, &mut value, id, vec![]);
        let _ = context.run(egui::RawInput::default(), |ctx| {
            ctx.fonts(|fonts| {
                let definitions = fonts.definitions();
                let key = format!("compositor:{selected}");
                let data = definitions.font_data.get(&key).unwrap();
                assert_eq!(data.index, expected_face.1);
                assert_eq!(data.font.as_ref(), expected_face.0.as_slice());
                let family = definitions
                    .families
                    .get(&egui::FontFamily::Name(key.clone().into()))
                    .unwrap();
                assert_eq!(family[0], key);
                let face = cosmic_text::ttf_parser::Face::parse(&data.font, data.index).unwrap();
                assert!(face.glyph_index('中').is_some());
            });
        });
    }

    #[cfg(windows)]
    #[test]
    fn inline_font_family_uses_installed_face_bytes_and_real_cjk_fallback() {
        let context = egui::Context::default();
        let id = egui::Id::new("installed-inline-font");
        let mut value = TextStyle {
            content: "English 中文".into(),
            font_size: 24.,
            ..Default::default()
        }
        .to_value();
        inline_frame(&context, &mut value, id, vec![]);
        inline_frame(&context, &mut value, id, vec![]);
        let _ = context.run(egui::RawInput::default(), |ctx| {
            ctx.fonts(|fonts| {
                let definitions = fonts.definitions();
                let data = definitions.font_data.get("compositor:Arial").unwrap();
                let face = cosmic_text::ttf_parser::Face::parse(&data.font, data.index).unwrap();
                assert!(face.glyph_index('A').is_some());
                let family = definitions
                    .families
                    .get(&egui::FontFamily::Name("compositor:Arial".into()))
                    .unwrap();
                assert_eq!(family[0], "compositor:Arial");
                let cjk = [
                    "compositor:Microsoft YaHei",
                    "compositor:Microsoft JhengHei",
                    "compositor:SimSun",
                ]
                .into_iter()
                .find_map(|name| definitions.font_data.get(name));
                if let Some(data) = cjk {
                    let face =
                        cosmic_text::ttf_parser::Face::parse(&data.font, data.index).unwrap();
                    assert!(face.glyph_index('中').is_some());
                }
            });
        });
    }
}
