use super::*;
use crate::chrome::capsule;

impl EditorApp {
    pub(super) fn original_welcome_ui(&mut self, ui: &mut egui::Ui) {
        if !self.welcome_clipboard_checked {
            self.welcome_clipboard_checked = true;
            if let Some(size) = clipboard_dimensions() {
                self.new_size = size;
            }
        }
        let mut create = false;
        let width = ui.available_width().min(500.);
        let bounds = Rect::from_center_size(ui.max_rect().center(), Vec2::new(width, 350.));
        ui.scope_builder(egui::UiBuilder::new().max_rect(bounds), |ui| {
            egui::Frame::NONE.inner_margin(28.).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 24.;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("新建画布").size(22.).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("⋮", |ui| {
                            ui.weak("预设尺寸");
                            for (label, size) in [
                                ("4K", [3840, 2160]),
                                ("1440p", [2560, 1440]),
                                ("1080p", [1920, 1080]),
                                ("iPhone 18 Pro", [1206, 2622]),
                                ("iPhone 18 Pro Max", [1320, 2868]),
                                ("MacBook Pro 14\"", [3024, 1964]),
                                ("MacBook Pro 16\"", [3456, 2234]),
                                ("Studio Display", [5120, 2880]),
                                ("Instagram 方形", [1080, 1080]),
                                ("Instagram 竖图", [1080, 1350]),
                                ("Instagram Story", [1080, 1920]),
                                ("YouTube 缩略图", [1080, 608]),
                                ("A4", [2480, 3508]),
                            ] {
                                if ui.selectable_label(self.new_size == size, label).clicked() {
                                    self.new_size = size;
                                    ui.close();
                                }
                            }
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.label("分辨率");
                                ui.add(
                                    egui::DragValue::new(&mut self.new_resolution)
                                        .range(1. ..=12000.)
                                        .suffix(" ppi"),
                                );
                            });
                        })
                        .response
                        .on_hover_text("屏幕与常见格式的预设尺寸");
                    });
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 16.;
                    let field_width = (ui.available_width() - 47.) / 2.;
                    dimension(ui, "宽度", &mut self.new_size[0], field_width);
                    ui.label(egui::RichText::new("×").color(ui.visuals().weak_text_color()));
                    dimension(ui, "高度", &mut self.new_size[1], field_width);
                });
                let valid = Document::valid_size(self.new_size[0], self.new_size[1]);
                if valid {
                    ui.weak("透明画布 · sRGB");
                } else {
                    ui.colored_label(
                        Color32::from_rgb(232, 161, 70),
                        "请输入有效的整数像素尺寸。",
                    );
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.;
                    if capsule(ui, "打开项目", false).clicked() {
                        self.command(Command::Open);
                    }
                    if capsule(ui, "导入图像", false).clicked() {
                        self.command(Command::Import);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        create |= ui
                            .add_enabled_ui(valid, |ui| capsule(ui, "创建画布", true))
                            .inner
                            .clicked();
                    });
                });
            });
        });
        let ready = self.is_welcome()
            && self.pending_io == 0
            && self.dialog.is_none()
            && self.color_picker.is_none()
            && !self.pdf_ui.is_open()
            && !egui::Popup::is_any_open(ui.ctx());
        if ready
            && Document::valid_size(self.new_size[0], self.new_size[1])
            && (create
                || ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter)))
        {
            let mut document = Document::new(self.new_size[0], self.new_size[1]);
            document.resolution = self.new_resolution;
            self.install_canvas_session(Session::new(document, None));
        }
    }
}

#[cfg(windows)]
fn clipboard_dimensions() -> Option<[u32; 2]> {
    use std::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(owner: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn GetClipboardData(format: u32) -> *mut c_void;
        fn RegisterClipboardFormatW(name: *const u16) -> u32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalSize(memory: *mut c_void) -> usize;
        fn GlobalLock(memory: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(memory: *mut c_void) -> i32;
    }
    struct Clipboard;
    impl Drop for Clipboard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    // Read only the image headers; suggesting a canvas size never decodes a large image.
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let _clipboard = Clipboard;
        let png_name: Vec<u16> = "PNG\0".encode_utf16().collect();
        let png = RegisterClipboardFormatW(png_name.as_ptr());
        for format in [png, 17, 8] {
            if format == 0 {
                continue;
            }
            let handle = GetClipboardData(format);
            if handle.is_null() {
                continue;
            }
            let length = GlobalSize(handle).min(32);
            if length < 12 {
                continue;
            }
            let pointer = GlobalLock(handle);
            if pointer.is_null() {
                continue;
            }
            let bytes = std::slice::from_raw_parts(pointer.cast::<u8>(), length);
            let size = dimensions_from_header(bytes, format == png);
            GlobalUnlock(handle);
            if size.is_some() {
                return size;
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn clipboard_dimensions() -> Option<[u32; 2]> {
    None
}

fn dimensions_from_header(bytes: &[u8], png: bool) -> Option<[u32; 2]> {
    let size = if png {
        if bytes.len() < 24
            || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
            || &bytes[8..12] != 13_u32.to_be_bytes().as_slice()
            || &bytes[12..16] != b"IHDR"
        {
            return None;
        }
        [
            u32::from_be_bytes(bytes[16..20].try_into().ok()?),
            u32::from_be_bytes(bytes[20..24].try_into().ok()?),
        ]
    } else {
        if bytes.len() < 12 {
            return None;
        }
        let header = u32::from_le_bytes(bytes[..4].try_into().ok()?);
        if header == 12 {
            [
                u16::from_le_bytes(bytes[4..6].try_into().ok()?) as u32,
                u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32,
            ]
        } else if header >= 40 {
            let width = i32::from_le_bytes(bytes[4..8].try_into().ok()?);
            let height = i32::from_le_bytes(bytes[8..12].try_into().ok()?);
            [u32::try_from(width).ok()?, height.unsigned_abs()]
        } else {
            return None;
        }
    };
    Document::valid_size(size[0], size[1]).then_some(size)
}

fn dimension(ui: &mut egui::Ui, title: &str, value: &mut u32, width: f32) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, 72.),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.spacing_mut().item_spacing.y = 8.;
            ui.label(egui::RichText::new(title).size(12.));
            egui::Frame::NONE
                .fill(crate::chrome::theme::palette(ui.ctx(), ui.visuals()).field)
                .corner_radius(7.)
                .inner_margin(12.)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [width - 48., 24.],
                            egui::DragValue::new(value).range(1..=30000).speed(1.),
                        );
                        ui.weak("px");
                    });
                });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::dimensions_from_header;

    #[test]
    fn top_down_dib_keeps_image_dimensions() {
        let mut header = [0_u8; 40];
        header[..4].copy_from_slice(&40_u32.to_le_bytes());
        header[4..8].copy_from_slice(&1920_i32.to_le_bytes());
        header[8..12].copy_from_slice(&(-1080_i32).to_le_bytes());
        assert_eq!(dimensions_from_header(&header, false), Some([1920, 1080]));
        header[4..8].copy_from_slice(&(-1_i32).to_le_bytes());
        assert_eq!(dimensions_from_header(&header, false), None);
    }

    #[test]
    fn png_header_rejects_empty_and_oversized_canvas() {
        let mut header = *b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\0\0\0\0\0";
        header[16..20].copy_from_slice(&640_u32.to_be_bytes());
        header[20..24].copy_from_slice(&400_u32.to_be_bytes());
        assert_eq!(dimensions_from_header(&header, true), Some([640, 400]));
        header[20..24].copy_from_slice(&0_u32.to_be_bytes());
        assert_eq!(dimensions_from_header(&header, true), None);
        header[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(dimensions_from_header(&header, true), None);
        assert_eq!(dimensions_from_header(&header[..15], true), None);
    }
}
