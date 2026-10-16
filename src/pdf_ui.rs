use super::*;
use crate::pdf_io::{PdfInfo, PdfPage, PdfPageSize};
use anyhow::{Context, Result, ensure};

struct PdfImportDialog {
    path: PathBuf,
    target: Option<Uuid>,
    info: PdfInfo,
    pages: String,
    dpi: u32,
    error: Option<String>,
}

struct PdfExportDialog {
    pages: Vec<Uuid>,
}

#[derive(Default)]
pub(super) struct PdfUi {
    waiting: VecDeque<PdfImportDialog>,
    import: Option<PdfImportDialog>,
    export: Option<PdfExportDialog>,
}

impl PdfUi {
    pub(super) fn enqueue(&mut self, path: PathBuf, target: Option<Uuid>, info: PdfInfo) {
        let pages = if target.is_some() || info.pages.len() == 1 {
            "1".into()
        } else {
            format!("1-{}", info.pages.len())
        };
        self.waiting.push_back(PdfImportDialog {
            path,
            target,
            info,
            pages,
            dpi: 300,
            error: None,
        });
    }

    pub(super) fn is_open(&self) -> bool {
        self.import.is_some() || self.export.is_some()
    }

    pub(super) fn cancel(&mut self) {
        self.import = None;
        self.export = None;
        self.waiting.clear();
    }
}

fn page_indices(text: &str, count: usize) -> Result<Vec<u32>> {
    ensure!(count > 0, "PDF 没有页面");
    let mut pages = std::collections::BTreeSet::new();
    for part in text.split([',', '，']) {
        let part = part.trim();
        ensure!(!part.is_empty(), "请输入页码，例如 1,3-5");
        let (first, last) = if let Some((a, b)) = part.split_once('-') {
            (a.trim().parse::<usize>()?, b.trim().parse::<usize>()?)
        } else {
            let n = part.parse::<usize>().context("页码应为数字，例如 1,3-5")?;
            (n, n)
        };
        ensure!(
            first > 0 && first <= last && last <= count,
            "页码范围是 1–{count}，请按从小到大填写范围"
        );
        for page in first..=last {
            pages.insert((page - 1) as u32);
        }
    }
    Ok(pages.into_iter().collect())
}

pub(super) fn page_size(session: &Session) -> PdfPageSize {
    if let Some((width, height, resolution, size)) = session.pdf_size
        && (width, height) == (session.doc.width, session.doc.height)
        && resolution == session.doc.resolution
    {
        return size;
    }
    PdfPageSize {
        width_points: f64::from(session.doc.width) * 72. / session.doc.resolution,
        height_points: f64::from(session.doc.height) * 72. / session.doc.resolution,
    }
}

impl EditorApp {
    pub(super) fn open_pdf(&mut self, path: PathBuf, target: Option<Uuid>) {
        self.message = Some(format!("正在读取 PDF：{}…", path.display()));
        match self.imports.submit(
            import_worker::ImportTask::PdfInfo { path, target },
            &self.sender,
        ) {
            Ok(()) => self.pending_io += 1,
            Err(error) => self.message = Some(error),
        }
    }

    pub(super) fn install_pdf_pages(
        &mut self,
        path: PathBuf,
        target: Option<Uuid>,
        dpi: u32,
        pages: Vec<PdfPage>,
    ) {
        let result = (|| -> Result<()> {
            ensure!(!pages.is_empty(), "没有导入任何 PDF 页面");
            ensure!(
                target.is_none() || pages.len() == 1,
                "导入为图层时一次请选择一页；多页请使用“打开”"
            );
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let mut sessions = Vec::new();
            for page in pages {
                let name = format!("{stem} - 第 {} 页", page.index + 1);
                let mut doc = Document::new(page.raster.width, page.raster.height);
                doc.resolution = f64::from(dpi);
                let layer = Layer::raster(&name, page.raster);
                doc.active_layer_id = Some(layer.id);
                doc.layers = vec![layer];
                let mut session = Session::new(doc, None);
                session.display_name = Some(name);
                session.pdf_size = Some((
                    session.doc.width,
                    session.doc.height,
                    f64::from(dpi),
                    page.size,
                ));
                sessions.push(session);
            }
            // Check the entire batch before adding any tabs; an oversized PDF
            // must leave the existing workspace intact.
            self.check_workspace_documents(
                sessions.iter().map(|s| &s.doc),
                self.retained_sampler_bytes(),
            )?;
            if let Some(target) = target {
                let session = sessions.pop().context("PDF 页面为空")?;
                self.install_import(
                    path.clone(),
                    image_io::ImportResult {
                        document: session.doc,
                        conversions: vec![],
                    },
                    Some(target),
                );
            } else {
                for session in sessions {
                    self.install_canvas_session(session);
                }
                self.remember(&path);
                self.message = Some(
                    "PDF 页面已打开，可使用绘图工具编辑；Ctrl+S 保存可编辑的 .comp 项目".into(),
                );
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.message = Some(format!("{error:#}"));
        }
    }

    pub(super) fn begin_pdf_export(&mut self) {
        self.settle_gestures();
        self.pdf_ui.export = Some(PdfExportDialog {
            pages: vec![self.session().id],
        });
    }

    fn export_pdf_selection(&mut self, ids: &[Uuid]) -> Result<()> {
        ensure!(!ids.is_empty(), "请至少选择一页");
        let pages = ids
            .iter()
            .map(|id| {
                let session = self
                    .tabs
                    .iter()
                    .find(|s| s.id == *id && !s.placeholder)
                    .context("选中的页面已经关闭")?;
                ensure!(
                    !session.edit_busy && !session.reload_busy,
                    "请等待页面编辑完成再导出"
                );
                Ok((session.doc.clone(), page_size(session)))
            })
            .collect::<Result<Vec<_>>>()?;
        let Some(path) = rfd::FileDialog::new()
            .set_title("导出 PDF")
            .add_filter("PDF 文档", &["pdf"])
            .set_file_name(format!(
                "{}.pdf",
                self.session().title().trim_end_matches(" *")
            ))
            .save_file()
        else {
            return Ok(());
        };
        ensure!(
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")),
            "请使用 .pdf 扩展名，并重新选择保存位置"
        );
        self.start_pdf_export(pages, path);
        Ok(())
    }

    pub(super) fn start_pdf_export(&mut self, pages: Vec<(Document, PdfPageSize)>, path: PathBuf) {
        let sender = self.sender.clone();
        let worker = std::thread::Builder::new()
            .name("pdf-export".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::pdf_io::export_pages(
                        &path,
                        pages
                            .into_iter()
                            .map(|(doc, size)| Ok((render::render_document(&doc), size))),
                    )
                    .map(|_| path)
                    .map_err(|e| format!("{e:#}"))
                }))
                .unwrap_or_else(|_| {
                    Err("PDF 导出异常，请减少页面数量后重试；现有项目已保留".into())
                });
                let _ = sender.send(Worker::Export(result));
            });
        match worker {
            Ok(_) => {
                self.pending_io += 1;
                self.message = Some("正在导出 PDF…".into());
            }
            Err(error) => self.message = Some(format!("启动 PDF 导出线程失败：{error}")),
        }
    }

    pub(super) fn pdf_dialogs(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() || self.pending_import.is_some() || !self.report.is_empty() {
            return;
        }
        if self.pdf_ui.import.is_none() && self.pdf_ui.export.is_none() {
            self.pdf_ui.import = self.pdf_ui.waiting.pop_front();
        }
        if let Some(mut dialog) = self.pdf_ui.import.take() {
            let mut import = false;
            let mut cancel = false;
            let response = egui::Modal::new(egui::Id::new("pdf-import")).show(ctx, |ui| {
                ui.set_width(430.);
                ui.heading("打开 PDF 页面");
                ui.label(dialog.path.file_name().unwrap_or_default().to_string_lossy());
                ui.label(format!("共 {} 页 · 每页打开为独立画布", dialog.info.pages.len()));
                ui.add_space(8.);
                ui.label("页面将转为图片，可使用现有绘图工具编辑。原文字、链接和表单不保留为可编辑的 PDF 对象。");
                ui.horizontal(|ui| {
                    ui.label("页码");
                    ui.text_edit_singleline(&mut dialog.pages);
                });
                ui.weak("填写 1,3-5；导入为图层时一次选择一页。");
                ui.horizontal(|ui| {
                    ui.label("清晰度");
                    egui::ComboBox::from_id_salt("pdf-dpi").selected_text(format!("{} DPI", dialog.dpi)).show_ui(ui, |ui| {
                        for dpi in [72, 150, 300, 600] {
                            ui.selectable_value(&mut dialog.dpi, dpi, format!("{dpi} DPI"));
                        }
                    });
                });
                let selection = page_indices(&dialog.pages, dialog.info.pages.len());
                let validation = selection.and_then(|indices| {
                    ensure!(dialog.target.is_none() || indices.len() == 1, "导入为图层请只选一页，多页请使用“打开”");
                    let pixels = indices.iter().map(|i| {
                        let size = dialog.info.pages[*i as usize];
                        let width = (size.width_points * f64::from(dialog.dpi) / 72.).ceil() as u64;
                        let height = (size.height_points * f64::from(dialog.dpi) / 72.).ceil() as u64;
                        width.saturating_mul(height)
                    }).fold(0_u64, u64::saturating_add);
                    ensure!(pixels <= crate::model::import_pixel_budget(), "选中的页面过大，请降低 DPI 或分批导入");
                    Ok((indices, pixels))
                });
                match &validation {
                    Ok((indices, pixels)) => { ui.weak(format!("选中 {} 页 · 原始像素约 {:.0} MiB（编辑和预览还需额外内存）", indices.len(), *pixels as f64 * 4. / 1048576.)); }
                    Err(error) => { ui.colored_label(Color32::LIGHT_RED, error.to_string()); }
                }
                if let Some(error) = &dialog.error { ui.colored_label(Color32::LIGHT_RED, error); }
                ui.add_space(8.);
                ui.horizontal(|ui| {
                    import = ui.add_enabled(validation.is_ok(), egui::Button::new("打开页面")).clicked();
                    cancel = ui.button("取消").clicked();
                });
            });
            if import {
                match page_indices(&dialog.pages, dialog.info.pages.len()).and_then(|indices| {
                    self.imports
                        .submit(
                            import_worker::ImportTask::PdfPages {
                                path: dialog.path.clone(),
                                target: dialog.target,
                                indices,
                                dpi: dialog.dpi,
                            },
                            &self.sender,
                        )
                        .map_err(anyhow::Error::msg)
                }) {
                    Ok(()) => {
                        self.pending_io += 1;
                        self.message = Some("正在渲染 PDF 页面…".into());
                    }
                    Err(error) => {
                        dialog.error = Some(format!("{error:#}"));
                        self.pdf_ui.import = Some(dialog);
                    }
                }
            } else if !cancel && !response.should_close() {
                self.pdf_ui.import = Some(dialog);
            }
        }
        if let Some(mut dialog) = self.pdf_ui.export.take() {
            dialog
                .pages
                .retain(|id| self.tabs.iter().any(|s| s.id == *id && !s.placeholder));
            let mut export = false;
            let mut cancel = false;
            let response = egui::Modal::new(egui::Id::new("pdf-export")).show(ctx, |ui| {
                ui.set_width(430.);
                ui.heading("导出 PDF");
                ui.label("选择打开的画布作为页面。按下方顺序合并，透明区域使用白色背景。");
                egui::ScrollArea::vertical()
                    .max_height(180.)
                    .show(ui, |ui| {
                        for session in self.tabs.iter().filter(|s| !s.placeholder) {
                            let mut selected = dialog.pages.contains(&session.id);
                            if ui.checkbox(&mut selected, session.title()).changed() {
                                if selected {
                                    dialog.pages.push(session.id);
                                } else {
                                    dialog.pages.retain(|id| *id != session.id);
                                }
                            }
                        }
                    });
                ui.horizontal(|ui| {
                    if ui.button("选择全部画布").clicked() {
                        dialog.pages = self
                            .tabs
                            .iter()
                            .filter(|s| !s.placeholder)
                            .map(|s| s.id)
                            .collect();
                    }
                    if ui.button("只选当前画布").clicked() {
                        dialog.pages = vec![self.session().id];
                    }
                });
                ui.separator();
                ui.label(format!("导出顺序（{} 页）", dialog.pages.len()));
                let mut swap = None;
                egui::ScrollArea::vertical()
                    .id_salt("pdf-order")
                    .max_height(140.)
                    .show(ui, |ui| {
                        for (i, id) in dialog.pages.iter().enumerate() {
                            let session = self
                                .tabs
                                .iter()
                                .find(|s| s.id == *id)
                                .expect("retained PDF page");
                            ui.horizontal(|ui| {
                                if ui.add_enabled(i > 0, egui::Button::new("上移")).clicked() {
                                    swap = Some((i, i - 1));
                                }
                                if ui
                                    .add_enabled(
                                        i + 1 < dialog.pages.len(),
                                        egui::Button::new("下移"),
                                    )
                                    .clicked()
                                {
                                    swap = Some((i, i + 1));
                                }
                                ui.label(format!("{}. {}", i + 1, session.title()));
                            });
                        }
                    });
                if let Some((a, b)) = swap {
                    dialog.pages.swap(a, b);
                }
                ui.weak("导出为图片型 PDF。需要继续编辑图层时，请另外保存 .comp 项目。");
                ui.horizontal(|ui| {
                    export = ui
                        .add_enabled(!dialog.pages.is_empty(), egui::Button::new("选择保存位置…"))
                        .clicked();
                    cancel = ui.button("取消").clicked();
                });
            });
            if export {
                if let Err(error) = self.export_pdf_selection(&dialog.pages) {
                    self.message = Some(format!("{error:#}"));
                }
            } else if !cancel && !response.should_close() {
                self.pdf_ui.export = Some(dialog);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_selection_has_no_duplicates_and_rejects_out_of_range_input() {
        assert_eq!(page_indices("1,3-5,3，2", 5).unwrap(), [0, 1, 2, 3, 4]);
        for input in ["", "0", "6", "3-2", "1-999999999", "1,", "one"] {
            assert!(page_indices(input, 5).is_err(), "{input}");
        }
    }

    #[test]
    fn pdf_size_keeps_original_page_bounds_until_canvas_dimensions_change() {
        let mut session = Session::new(Document::new(2480, 3508), None);
        session.doc.resolution = 300.;
        session.pdf_size = Some((
            2480,
            3508,
            300.,
            PdfPageSize {
                width_points: 595.,
                height_points: 842.,
            },
        ));
        assert_eq!(page_size(&session).height_points, 842.);
        session.doc.resolution = 150.;
        assert_eq!(page_size(&session).width_points, 2480. * 72. / 150.);
        session.doc.resolution = 300.;
        session.doc.width = 1200;
        assert_eq!(page_size(&session).width_points, 288.);
    }
}
