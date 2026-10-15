//! Searchable online license links with byte-exact originals decoded on demand.
use eframe::egui;
use flate2::read::ZlibDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{io::Read, sync::OnceLock};

const RESOURCE: &[u8] = include_bytes!("../assets/open-source-licenses.bin");
const MAGIC: &[u8] = b"COMPOSITOR-OPEN-SOURCE-LICENSES\0v1\0";
const MAX_PART_BYTES: usize = 16 * 1024 * 1024;
const TITLE: &str = "开源软件许可证";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LicenseIndex {
    format_version: u32,
    bundle_bytes: usize,
    bundle_sha256: String,
    components: Vec<Component>,
    originals: Vec<Original>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Component {
    name: String,
    version: String,
    license: String,
    repository: String,
    url: String,
    license_links: Vec<LicenseLink>,
    originals: Vec<usize>,
}

#[derive(Deserialize)]
struct LicenseLink {
    name: String,
    url: String,
}

#[derive(Deserialize)]
struct Original {
    path: String,
    offset: usize,
    bytes: usize,
    sha256: String,
}

fn resource_part(index: bool) -> Result<(&'static [u8], usize), String> {
    if !RESOURCE.starts_with(MAGIC) {
        return Err("随附许可证资源标记错误".into());
    }
    let header = RESOURCE
        .get(MAGIC.len()..MAGIC.len() + 16)
        .ok_or("随附许可证资源不完整")?;
    let lengths = header
        .as_chunks::<4>()
        .0
        .iter()
        .map(|value| u32::from_le_bytes(*value) as usize)
        .collect::<Vec<_>>();
    let begin = MAGIC.len() + 16;
    let middle = begin + lengths[0];
    let end = middle + lengths[2];
    if end != RESOURCE.len() || lengths[1] > MAX_PART_BYTES || lengths[3] > MAX_PART_BYTES {
        return Err("随附许可证资源长度错误".into());
    }
    if index {
        Ok((&RESOURCE[begin..middle], lengths[1]))
    } else {
        Ok((&RESOURCE[middle..end], lengths[3]))
    }
}

fn decode(index: bool) -> Result<Vec<u8>, String> {
    let (compressed, expected) = resource_part(index)?;
    let mut bytes = Vec::new();
    ZlibDecoder::new(compressed)
        .take(expected as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取随附许可证失败：{error}"))?;
    if bytes.len() != expected {
        return Err("随附许可证解压长度错误".into());
    }
    Ok(bytes)
}

fn license_index() -> Result<&'static LicenseIndex, &'static str> {
    static INDEX: OnceLock<Result<LicenseIndex, String>> = OnceLock::new();
    INDEX
        .get_or_init(|| {
            let index: LicenseIndex =
                serde_json::from_slice(&decode(true)?).map_err(|error| error.to_string())?;
            if index.format_version != 1
                || index.originals.is_empty()
                || index.bundle_bytes > MAX_PART_BYTES
                || index
                    .originals
                    .iter()
                    .any(|source| source.sha256.len() != 64)
                || index.components.iter().any(|component| {
                    !component.url.starts_with("https://")
                        || component.originals.is_empty()
                        || component
                            .originals
                            .iter()
                            .any(|source| *source >= index.originals.len())
                })
            {
                return Err("随附许可证索引错误".into());
            }
            Ok(index)
        })
        .as_ref()
        .map_err(String::as_str)
}

fn original_text(source: &Original) -> Result<&'static str, &'static str> {
    static BUNDLE: OnceLock<Result<String, String>> = OnceLock::new();
    let bundle = BUNDLE
        .get_or_init(|| {
            let index = license_index()?;
            let bytes = decode(false)?;
            if bytes.len() != index.bundle_bytes
                || format!("{:x}", Sha256::digest(&bytes)) != index.bundle_sha256
            {
                return Err("随附许可证原文字节校验失败".into());
            }
            String::from_utf8(bytes).map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(String::as_str)?;
    let end = source
        .offset
        .checked_add(source.bytes)
        .ok_or("随附许可证原文范围错误")?;
    bundle
        .get(source.offset..end)
        .ok_or("随附许可证原文范围错误")
}

#[derive(Default)]
pub struct LicenseWindow {
    pub open: bool,
    query: String,
    selected_source: Option<usize>,
}

impl LicenseWindow {
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new(TITLE)
            .id(egui::Id::new("open-source-licenses"))
            .open(&mut open)
            .default_width(720.)
            .default_height(480.)
            .min_width(420.)
            .collapsible(false)
            .show(ctx, |ui| self.contents(ui));
        self.open = open;
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        let index = match license_index() {
            Ok(index) => index,
            Err(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
                return;
            }
        };
        ui.label("点击组件名称在线查看许可证；“随附原文”可离线查看版权和许可。");
        ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("搜索组件或许可证"));
        let query = self.query.trim().to_lowercase();
        let matching = index
            .components
            .iter()
            .enumerate()
            .filter(|(_, component)| {
                query.is_empty()
                    || component.name.to_lowercase().contains(&query)
                    || component.license.to_lowercase().contains(&query)
            })
            .collect::<Vec<_>>();
        ui.weak(format!("{} 个组件", matching.len()));
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.;
        egui::ScrollArea::vertical()
            .id_salt("license-components")
            .max_height(if self.selected_source.is_some() {
                180.
            } else {
                360.
            })
            .show_rows(ui, row_height, matching.len(), |ui, range| {
                for row in range {
                    let (identity, component) = matching[row];
                    ui.push_id(identity, |ui| {
                        ui.horizontal(|ui| {
                            if ui.small_button("随附原文").clicked() {
                                self.selected_source = Some(component.originals[0]);
                            }
                            ui.hyperlink_to(
                                format!("{} {}", component.name, component.version),
                                &component.url,
                            )
                            .on_hover_text(&component.license);
                            ui.add(
                                egui::Label::new(egui::RichText::new(&component.license).weak())
                                    .truncate(),
                            );
                        });
                    });
                }
            });
        if let Some(selected) = self.selected_source {
            ui.separator();
            ui.horizontal(|ui| {
                let mut selected = selected;
                egui::ComboBox::from_id_salt("license-original-file")
                    .width(400.)
                    .selected_text(&index.originals[selected].path)
                    .show_ui(ui, |ui| {
                        for (identity, original) in index.originals.iter().enumerate() {
                            ui.selectable_value(&mut selected, identity, &original.path);
                        }
                    });
                self.selected_source = Some(selected);
                if ui.small_button("收起").clicked() {
                    self.selected_source = None;
                }
            });
            let Some(selected) = self.selected_source else {
                return;
            };
            let source = &index.originals[selected];
            if let Some(component) = index
                .components
                .iter()
                .find(|component| component.originals.contains(&selected))
            {
                ui.horizontal_wrapped(|ui| {
                    for link in &component.license_links {
                        ui.hyperlink_to(&link.name, &link.url);
                    }
                    if component.repository.starts_with("https://") {
                        ui.hyperlink_to("组件来源", &component.repository);
                    }
                });
            }
            match original_text(source) {
                Ok(text) => {
                    if ui.small_button("复制原文").clicked() {
                        ui.ctx().copy_text(text.to_owned());
                    }
                    let lines = text.lines().collect::<Vec<_>>();
                    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
                    egui::ScrollArea::both()
                        .id_salt(("license-original-text", source.offset))
                        .max_height(260.)
                        .show_rows(ui, row_height, lines.len(), |ui, range| {
                            for line in &lines[range] {
                                ui.monospace(*line);
                            }
                        });
                }
                Err(error) => {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_license_originals_match_every_preserved_hash() {
        let index = license_index().unwrap();
        assert!(
            index
                .components
                .iter()
                .any(|component| component.name == "NSIS")
        );
        for source in &index.originals {
            let text = original_text(source).unwrap();
            assert_eq!(text.len(), source.bytes, "{}", source.path);
            assert_eq!(
                format!("{:x}", Sha256::digest(text.as_bytes())),
                source.sha256
            );
        }
        let application = original_text(&index.originals[0]).unwrap();
        assert!(application.contains("Copyright (c) 2026 Wonder Assembly LLC"));
        assert!(application.contains("MIT License"));
    }

    #[test]
    fn license_links_and_offline_originals_render_in_both_themes() {
        for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
            let ctx = egui::Context::default();
            ctx.set_visuals(visuals);
            let mut window = LicenseWindow {
                open: true,
                query: "Compositor".into(),
                selected_source: None,
            };
            let output = ctx.run(egui::RawInput::default(), |ctx| window.show(ctx));
            assert!(!output.shapes.is_empty());
            window.selected_source = Some(0);
            let output = ctx.run(egui::RawInput::default(), |ctx| window.show(ctx));
            assert!(!output.shapes.is_empty());
        }
    }

    #[test]
    fn clicking_a_component_opens_its_https_license_and_originals_work_offline() {
        fn frame(
            ctx: &egui::Context,
            window: &mut LicenseWindow,
            events: Vec<egui::Event>,
        ) -> egui::FullOutput {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000., 800.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| window.contents(ui));
                },
            )
        }
        fn center(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
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
                .unwrap_or_else(|| panic!("Missing license UI label: {label}"))
        }
        fn click(
            ctx: &egui::Context,
            window: &mut LicenseWindow,
            position: egui::Pos2,
        ) -> egui::FullOutput {
            frame(
                ctx,
                window,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            );
            frame(
                ctx,
                window,
                vec![egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            )
        }
        let ctx = egui::Context::default();
        let mut window = LicenseWindow {
            open: true,
            query: "Compositor".into(),
            selected_source: None,
        };
        frame(&ctx, &mut window, vec![]);
        let output = frame(&ctx, &mut window, vec![]);
        let component = &license_index().unwrap().components[0];
        let position = center(
            &output,
            &format!("{} {}", component.name, component.version),
        );
        let output = click(&ctx, &mut window, position);
        assert!(output.platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::OpenUrl(url) if url.url == "https://spdx.org/licenses/MIT.html")));
        assert!(window.selected_source.is_none());
        let output = frame(&ctx, &mut window, vec![]);
        let position = center(&output, "随附原文");
        let output = click(&ctx, &mut window, position);
        assert_eq!(window.selected_source, Some(0));
        assert!(
            !output
                .platform_output
                .commands
                .iter()
                .any(|command| matches!(command, egui::OutputCommand::OpenUrl(_)))
        );
        let text = original_text(&license_index().unwrap().originals[0]).unwrap();
        assert!(text.contains("Permission is hereby granted"));
    }
}
