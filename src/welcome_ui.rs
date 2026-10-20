use super::*;
use crate::canvas_units::{self, CanvasBackground, CanvasUnit};
use crate::chrome::capsule;

#[derive(Clone)]
struct NewCanvasSettings {
    unit: CanvasUnit,
    background: CanvasBackground,
    dimensions: [String; 2],
    pixel_size: [u32; 2],
}

impl NewCanvasSettings {
    fn new(size: [u32; 2], resolution: f64) -> Self {
        let mut settings = Self {
            unit: CanvasUnit::Pixels,
            background: CanvasBackground::Transparent,
            dimensions: [String::new(), String::new()],
            pixel_size: size,
        };
        settings.set_pixels(size, resolution);
        settings
    }

    fn size(&self, resolution: f64) -> Option<[u32; 2]> {
        if !canvas_units::valid_resolution(resolution) {
            return None;
        }
        let size = [
            self.unit.pixels(&self.dimensions[0], resolution)?,
            self.unit.pixels(&self.dimensions[1], resolution)?,
        ];
        Document::valid_size(size[0], size[1]).then_some(size)
    }

    fn set_pixels(&mut self, size: [u32; 2], resolution: f64) {
        self.pixel_size = size;
        self.dimensions = size.map(|pixels| self.unit.text(pixels, resolution));
    }

    fn switch_unit(&mut self, resolution: f64) {
        let size = self.size(resolution);
        self.unit = self.unit.next();
        if let Some(size) = size {
            self.set_pixels(size, resolution);
        }
    }
}

impl EditorApp {
    pub(super) fn original_welcome_ui(&mut self, ui: &mut egui::Ui) {
        if !self.welcome_clipboard_checked {
            self.welcome_clipboard_checked = true;
            if let Some(size) = clipboard_dimensions() {
                self.new_size = size;
            }
        }
        // A new placeholder starts with pixels and transparency even when the
        // previous document used print units. No print preferences leak into it.
        let settings_key = egui::Id::new(("new-canvas-settings", self.session().id));
        let mut settings = ui
            .ctx()
            .data(|data| data.get_temp::<NewCanvasSettings>(settings_key))
            .unwrap_or_else(|| NewCanvasSettings::new(self.new_size, self.new_resolution));
        if settings.pixel_size != self.new_size {
            settings.set_pixels(self.new_size, self.new_resolution);
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
                                if ui
                                    .selectable_label(
                                        settings.size(self.new_resolution) == Some(size),
                                        label,
                                    )
                                    .clicked()
                                {
                                    self.new_size = size;
                                    settings.set_pixels(size, self.new_resolution);
                                    ui.close();
                                }
                            }
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.label("分辨率");
                                ui.add(
                                    egui::DragValue::new(&mut self.new_resolution)
                                        .range(1. ..=9600.)
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
                    dimension(
                        ui,
                        "宽度",
                        &mut settings.dimensions[0],
                        settings.unit,
                        field_width,
                    );
                    ui.label(egui::RichText::new("×").color(ui.visuals().weak_text_color()));
                    dimension(
                        ui,
                        "高度",
                        &mut settings.dimensions[1],
                        settings.unit,
                        field_width,
                    );
                });
                let size = settings.size(self.new_resolution);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.;
                    if capsule(ui, settings.background.label(), false)
                        .on_hover_text("点击切换透明、白色或黑色背景")
                        .clicked()
                    {
                        settings.background = settings.background.next();
                    }
                    ui.weak("·");
                    if capsule(ui, settings.unit.label(), false)
                        .on_hover_text("点击切换 px、in、cm、mm；画布像素尺寸保持不变")
                        .clicked()
                    {
                        settings.switch_unit(self.new_resolution);
                    }
                    ui.weak("·");
                    let size_help = size.map_or_else(String::new, |[width, height]| {
                        format!("；当前画布为 {width} × {height} 像素")
                    });
                    if capsule(ui, &format!("{:.0} ppi", self.new_resolution), false)
                        .on_hover_text(format!(
                            "点击切换 72／300 ppi；更多菜单可自定分辨率{size_help}"
                        ))
                        .clicked()
                    {
                        self.new_resolution = if self.new_resolution == 300. {
                            72.
                        } else {
                            300.
                        };
                    }
                });
                let valid = settings.size(self.new_resolution).is_some();
                if !valid {
                    ui.colored_label(
                        Color32::from_rgb(232, 161, 70),
                        if settings.unit == CanvasUnit::Pixels {
                            "请输入有效的整数像素尺寸，分辨率为 1–9600 ppi。"
                        } else {
                            "当前尺寸或分辨率超出画布上限，请减小尺寸或 ppi。"
                        },
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
                    if capsule(ui, "打开 PDF", false)
                        .on_hover_text("选择 PDF 页码和清晰度，再使用绘图工具编辑")
                        .clicked()
                    {
                        self.command(Command::OpenPdf);
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
        let size = settings.size(self.new_resolution);
        if let Some(size) = size {
            settings.pixel_size = size;
            self.new_size = size;
        }
        let background = settings.background;
        ui.ctx()
            .data_mut(|data| data.insert_temp(settings_key, settings));
        let ready = self.is_welcome()
            && self.pending_io == 0
            && self.dialog.is_none()
            && self.color_picker.is_none()
            && !self.pdf_ui.is_open()
            && !egui::Popup::is_any_open(ui.ctx());
        if ready
            && let Some([width, height]) = size
            && (create
                || ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter)))
        {
            match canvas_units::new_document(width, height, self.new_resolution, background) {
                Ok(document) => self.install_canvas_session(Session::new(document, None)),
                Err(error) => self.message = Some(error.to_string()),
            }
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

fn dimension(ui: &mut egui::Ui, title: &str, value: &mut String, unit: CanvasUnit, width: f32) {
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
                            egui::TextEdit::singleline(value)
                                .char_limit(24)
                                .desired_width(width - 48.),
                        );
                        ui.weak(unit.symbol());
                    });
                });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_canvas_print_fields_follow_dpi_and_switch_units_without_pixel_drift() {
        let mut settings = NewCanvasSettings::new([2550, 3300], 300.);
        assert_eq!(settings.unit, CanvasUnit::Pixels);
        assert_eq!(settings.background, CanvasBackground::Transparent);
        settings.switch_unit(300.);
        assert_eq!(settings.unit, CanvasUnit::Inches);
        assert_eq!(settings.dimensions, ["8.5", "11"]);
        assert_eq!(settings.size(300.), Some([2550, 3300]));
        assert_eq!(settings.size(72.), Some([612, 792]));
        for expected in [
            CanvasUnit::Centimeters,
            CanvasUnit::Millimeters,
            CanvasUnit::Pixels,
        ] {
            settings.switch_unit(300.);
            assert_eq!(settings.unit, expected);
            assert_eq!(settings.size(300.), Some([2550, 3300]));
        }
        assert_eq!(settings.dimensions, ["2550", "3300"]);
        settings.switch_unit(300.);
        settings.set_pixels([2480, 3508], 300.);
        assert_eq!(settings.size(300.), Some([2480, 3508]));
    }

    #[test]
    fn welcome_rejects_invalid_fields_and_does_not_reuse_print_background_defaults() {
        let mut settings = NewCanvasSettings::new([1920, 1080], 72.);
        settings.dimensions[0] = "19.5".into();
        assert_eq!(settings.size(72.), None);
        settings.dimensions = ["30000".into(), "30000".into()];
        assert_eq!(settings.size(72.), None);
        settings.set_pixels([1920, 1080], 72.);
        assert_eq!(settings.size(9601.), None);
        settings.unit = CanvasUnit::Inches;
        settings.background = CanvasBackground::Black;
        let fresh = NewCanvasSettings::new([1920, 1080], 72.);
        assert_eq!(fresh.unit, CanvasUnit::Pixels);
        assert_eq!(fresh.background, CanvasBackground::Transparent);
        assert_eq!(fresh.size(72.), Some([1920, 1080]));
    }

    #[test]
    fn welcome_enter_creates_selected_print_resolution_and_background() {
        let mut app = EditorApp::create(None, None);
        app.tabs = vec![Session::new(Document::new(8, 8), None)];
        app.tabs[0].placeholder = true;
        app.current = 0;
        app.new_size = [40, 30];
        app.new_resolution = 300.;
        app.welcome_clipboard_checked = true;
        let context = egui::Context::default();
        let mut settings = NewCanvasSettings::new(app.new_size, app.new_resolution);
        settings.switch_unit(300.);
        settings.background = CanvasBackground::White;
        let settings_key = egui::Id::new(("new-canvas-settings", app.session().id));
        context.data_mut(|data| data.insert_temp(settings_key, settings));
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
                events: vec![egui::Event::Key {
                    key: Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    app.original_welcome_ui(ui);
                });
            },
        );
        assert!(!app.is_welcome());
        let document = &app.session().doc;
        assert_eq!([document.width, document.height], [40, 30]);
        assert_eq!(document.resolution, 300.);
        assert_eq!(
            document
                .active()
                .unwrap()
                .image
                .as_ref()
                .unwrap()
                .get(39, 29),
            [255; 4]
        );
    }

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
