//! Export options operate on a snapshot and never resize the editable document.
use super::*;

pub(super) struct ExportDialog {
    document: Document,
    name: String,
    format: String,
    size: [u32; 2],
    scale: f64,
    sixteen: bool,
    cmyk: bool,
}

impl ExportDialog {
    pub(super) fn new(document: Document, name: String) -> Self {
        Self {
            size: [document.width, document.height],
            sixteen: render::has_deep_layers(&document),
            document,
            name,
            format: "png".into(),
            scale: 100.,
            cmyk: false,
        }
    }
    fn dimensions(&mut self, scale: f64) {
        self.scale = scale.clamp(1., 200.);
        self.size = [self.document.width, self.document.height]
            .map(|v| (v as f64 * self.scale / 100.).round().clamp(1., 30000.) as u32);
    }
}

impl EditorApp {
    pub(super) fn open_export_dialog(&mut self) {
        if self.export_dialog.is_some() {
            return;
        }
        self.settle_gestures();
        self.cancel_dialog();
        self.command_palette = Default::default();
        self.pdf_ui.cancel();
        self.export_dialog = Some(ExportDialog::new(
            self.session().doc.clone(),
            self.session().title().trim_end_matches(" *").to_owned(),
        ));
    }

    pub(super) fn choose_print_profile(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("选择 CMYK 印刷色彩配置")
            .add_filter("ICC 色彩配置", &["icc", "icm"])
            .pick_file()
        else {
            return;
        };
        match crate::color_management::CmykProfile::load(&path) {
            Ok(profile) => {
                self.print_profile = Some(Arc::new(profile));
                self.refresh_proof();
            }
            Err(error) => self.message = Some(format!("{error:#}")),
        }
    }

    pub(super) fn refresh_proof(&mut self) {
        for session in &mut self.tabs {
            session.changed();
        }
    }

    pub(super) fn show_export_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut options) = self.export_dialog.take() else {
            return;
        };
        let mut export = false;
        let mut cancel = false;
        let mut profile = false;
        let response = egui::Modal::new(egui::Id::new("export-image-options")).show(ctx, |ui| {
            ui.set_min_width(340.);
            ui.heading("导出图像");
            egui::ComboBox::from_id_salt("export-format")
                .selected_text(options.format.to_uppercase())
                .show_ui(ui, |ui| {
                    for (format, label) in [
                        ("png", "PNG"),
                        ("jpg", "JPEG"),
                        ("tif", "TIFF"),
                        ("webp", "WebP"),
                        ("bmp", "BMP"),
                        ("gif", "GIF"),
                        ("pdf", "PDF"),
                        ("exr", "OpenEXR / HDR"),
                    ] {
                        ui.selectable_value(&mut options.format, format.into(), label);
                    }
                });
            let mut scale = options.scale;
            if ui
                .add(egui::Slider::new(&mut scale, 1. ..=200.).text("缩放 %"))
                .changed()
            {
                options.dimensions(scale);
            }
            ui.horizontal(|ui| {
                ui.label("尺寸");
                for axis in 0..2 {
                    let before = options.size[axis];
                    if ui
                        .add(
                            egui::DragValue::new(&mut options.size[axis])
                                .range(1..=30000)
                                .suffix(if axis == 0 { " ×" } else { " px" }),
                        )
                        .changed()
                        && options.size[axis] != before
                    {
                        let original = [options.document.width, options.document.height];
                        options
                            .dimensions(options.size[axis] as f64 / original[axis] as f64 * 100.);
                    }
                }
            });
            if ["png", "tif"].contains(&options.format.as_str()) {
                ui.checkbox(&mut options.sixteen, "16 位／通道（保留细腻渐变）");
            } else if options.format == "exr" {
                ui.label("保留 HDR 浮点颜色和超过白色的亮部");
            }
            let print_format = ["jpg", "tif", "pdf"].contains(&options.format.as_str());
            if !print_format {
                options.cmyk = false;
            }
            ui.add_enabled(
                print_format,
                egui::Checkbox::new(&mut options.cmyk, "CMYK 印刷输出"),
            );
            if options.cmyk {
                if let Some(profile) = &self.print_profile {
                    ui.label(profile.name());
                } else {
                    ui.label("请先选择印刷厂提供的 CMYK ICC 配置");
                }
                profile = ui.button("选择 ICC 配置…").clicked();
                ui.small("透明区域铺白，导出文件嵌入所选配置。");
            }
            if options.format == "jpg" {
                ui.add(egui::Slider::new(&mut self.jpeg_quality, 1..=100).text("JPEG 质量"));
            }
            ui.label(format!(
                "{} × {} 像素 · {} PPI",
                options.size[0], options.size[1], options.document.resolution
            ));
            ui.horizontal(|ui| {
                export = ui
                    .add_enabled(
                        Document::valid_size(options.size[0], options.size[1])
                            && (!options.cmyk || self.print_profile.is_some()),
                        egui::Button::new("选择位置并导出…"),
                    )
                    .clicked();
                cancel = ui.button("取消").clicked();
            });
        });
        if profile {
            self.choose_print_profile();
        }
        if cancel || response.should_close() {
            return;
        }
        if !export {
            self.export_dialog = Some(options);
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("导出图像")
            .add_filter(options.format.to_uppercase(), &[&options.format])
            .set_file_name(format!("{}.{}", options.name, options.format))
            .save_file()
        else {
            self.export_dialog = Some(options);
            return;
        };
        let (path, format) = match Self::dialog_output_path(path, &[&options.format]) {
            Ok(value) => value,
            Err(error) => {
                self.message = Some(error);
                self.export_dialog = Some(options);
                return;
            }
        };
        self.start_configured_export(options, path, format);
    }

    fn start_configured_export(&mut self, options: ExportDialog, path: PathBuf, format: String) {
        let sender = self.sender.clone();
        let profile = options.cmyk.then(|| self.print_profile.clone()).flatten();
        let quality = self.jpeg_quality;
        let matte = self.jpeg_matte;
        self.message = Some("正在生成导出图像…".into());
        match gui_threads::spawn("导出图像", move || {
            let result = gui_threads::run("导出图像", || {
                let doc = &options.document;
                let full = render::render_document(doc);
                let mut raster = if options.size == [full.width, full.height] {
                    full
                } else {
                    crate::preview::area_resize(&full, options.size[0], options.size[1])
                };
                if let Some(profile) = profile {
                    crate::print_export::export(
                        &raster,
                        &path,
                        &format,
                        &profile,
                        doc.resolution,
                        quality,
                    )
                    .map_err(|error| format!("{error:#}"))?;
                } else {
                    if !options.sixteen && format != "exr" {
                        raster = Raster::from_rgba(raster.width, raster.height, raster.to_rgba());
                    } else if options.sixteen
                        && ["png", "tif"].contains(&format.as_str())
                        && raster.deep.is_none()
                    {
                        let values: Vec<f32> = raster
                            .to_rgba()
                            .into_iter()
                            .map(|v| v as f32 / 255.)
                            .collect();
                        raster = Raster::from_deep(crate::deep_pixels::DeepRaster::from_f32(
                            raster.width,
                            raster.height,
                            crate::deep_pixels::Depth::U16,
                            &values,
                        )?);
                    }
                    image_io::export_raster(
                        &raster,
                        &path,
                        &format,
                        doc.resolution,
                        quality,
                        [matte[0], matte[1], matte[2]],
                    )
                    .map_err(|error| format!("{error:#}"))?;
                }
                Ok(path)
            });
            let _ = sender.send(Worker::Export(result));
        }) {
            Ok(_) => self.pending_io += 1,
            Err(error) => self.message = Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn editor() -> EditorApp {
        let mut app = EditorApp::create(None, None);
        app.tabs = vec![Session::new(Document::new(16, 12), None)];
        app.current = 0;
        app
    }

    #[test]
    fn export_replaces_other_modals_and_reopening_preserves_options_and_snapshot() {
        let mut app = editor();
        app.dialog = Some(Dialog::CameraRaw);
        app.begin_pdf_export();
        app.open_command_palette();
        assert!(app.pdf_ui.is_open());
        assert!(app.command_palette.is_open());
        app.open_export_dialog();
        assert!(app.dialog.is_none());
        assert!(!app.pdf_ui.is_open());
        assert!(!app.command_palette.is_open());
        let options = app.export_dialog.as_mut().unwrap();
        options.format = "tif".into();
        options.dimensions(50.);
        let before = options.document.clone();
        app.session_mut().doc.width = 100;
        app.open_export_dialog();
        let options = app.export_dialog.as_ref().unwrap();
        assert_eq!(options.format, "tif");
        assert_eq!(options.size, [8, 6]);
        assert_eq!(options.document, before);
        assert_eq!(app.session().doc.width, 100);
    }

    #[test]
    fn escape_closes_only_export_options_without_changing_the_document() {
        let mut app = editor();
        let before = app.session().doc.clone();
        let ctx = egui::Context::default();
        app.open_export_dialog();
        let frame = |app: &mut EditorApp, events| {
            let _ = ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    app.show_export_dialog(ctx);
                },
            );
        };
        frame(&mut app, vec![]);
        assert!(app.export_dialog.is_some());
        frame(
            &mut app,
            vec![egui::Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(app.export_dialog.is_none());
        assert_eq!(app.session().doc, before);
        assert_eq!(app.pending_io, 0);
    }

    #[test]
    fn export_resize_changes_only_snapshot_output_size() {
        let doc = Document::new(801, 603);
        let before = doc.clone();
        let mut options = ExportDialog::new(doc.clone(), "Image".into());
        options.dimensions(50.);
        assert_eq!(options.size, [401, 302]);
        assert_eq!(options.document, before);
        assert_eq!(doc, before);
        options.dimensions(200.);
        assert_eq!(options.size, [1602, 1206]);
    }
}
