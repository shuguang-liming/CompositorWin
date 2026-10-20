//! Headless automation and local GUI clients. Standard output is a JSON protocol.
use crate::{
    automation::{Engine, Operation},
    history::History,
    image_io,
    model::{Document, Layer, Raster},
    pdf_io, project, render,
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{self, BufRead, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const SKILL: &str = include_str!("../skills/compositor-editor/SKILL.md");

#[derive(Debug)]
struct CliError {
    code: String,
    message: String,
    operation_index: Option<usize>,
    upstream: Option<Value>,
}
impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for CliError {}
fn error(code: impl Into<String>, message: impl Into<String>) -> anyhow::Error {
    CliError {
        code: code.into(),
        message: message.into(),
        operation_index: None,
        upstream: None,
    }
    .into()
}
pub fn success(id: Value, result: Value) -> Value {
    json!({"protocolVersion":1,"id":id,"ok":true,"result":result})
}
pub fn failure(id: Value, err: &anyhow::Error) -> Value {
    let code = err
        .downcast_ref::<CliError>()
        .map(|e| e.code.as_str())
        .unwrap_or_else(|| {
            if let Some(error) = err.downcast_ref::<project::ProjectAccessError>() {
                error.code
            } else if err.downcast_ref::<serde_json::Error>().is_some() {
                "invalid_request"
            } else if err.downcast_ref::<io::Error>().is_some() {
                "io_error"
            } else {
                "operation_failed"
            }
        });
    let mut value = json!({"protocolVersion":1,"id":id,"ok":false,"error":{"code":code,"message":format!("{err:#}")}});
    if let Some(upstream) = err
        .downcast_ref::<CliError>()
        .and_then(|e| e.upstream.as_ref())
    {
        value["error"] = upstream.clone();
    }
    if let Some(index) = err
        .downcast_ref::<CliError>()
        .and_then(|e| e.operation_index)
    {
        value["error"]["operationIndex"] = json!(index);
    }
    value
}

pub fn document_info(doc: &Document) -> Value {
    json!({"documentId":doc.id,"width":doc.width,"height":doc.height,"resolution":doc.resolution,
        "activeLayerId":doc.active_layer_id,"layerCount":doc.layers.len(),"guides":doc.guides,
        "selection":doc.selection.as_ref().map(|s| json!({"bounds":s.bounds(),"empty":s.is_empty()})),
        "layers":doc.layers.iter().map(|l| json!({"id":l.id,"name":l.name,"visible":l.visible,
            "parentId":l.parent_id,"isGroup":l.is_group,"opacity":l.opacity,"blendMode":l.blend_mode,
            "transform":l.transform,"clippingSourceId":l.mask_source_id,
            "image":l.image.as_ref().map(|r| json!({"width":r.width,"height":r.height,"depth":r.deep.as_ref().map_or("8-bit",|d|if d.depth==crate::deep_pixels::Depth::Hdr {"HDR"}else{"16-bit"})})),
            "mask":l.mask.as_ref().map(|m| json!({"enabled":m.enabled,"linked":m.linked,"placement":m.placement,"width":m.raster.width,"height":m.raster.height})),
            "text":l.text,"shape":l.shape,"adjustment":l.adjustment,"effects":l.effects})).collect::<Vec<_>>()})
}

pub fn apply_transaction(
    doc: &mut Document,
    history: &mut History,
    engine: &mut Engine,
    operations: Vec<Operation>,
) -> Result<Value> {
    ensure!(operations.len() <= 1000, "每批最多 1000 项操作");
    let before = doc.clone();
    let mut candidate = before.clone();
    let mut results = Vec::with_capacity(operations.len());
    let retained_history_bytes = history.retained_bytes(&before) as u64;
    for (index, operation) in operations.iter().enumerate() {
        let result = engine
            .apply_with_workspace(
                &mut candidate,
                operation,
                &[&before],
                retained_history_bytes,
            )
            .map_err(|err| CliError {
                code: "operation_failed".into(),
                message: format!("操作 {index} 失败：{err:#}"),
                operation_index: Some(index),
                upstream: None,
            })?;
        results.push(result);
    }
    project::validate_document(&candidate)?;
    let changed = before != candidate;
    history.record("Agent batch", before, &candidate);
    *doc = candidate;
    Ok(
        json!({"changed":changed,"revision":history.revision(),"results":results,"document":document_info(doc)}),
    )
}

fn fingerprint(path: &Path) -> Result<String> {
    let digest = if path.is_dir()
        || path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("comp"))
    {
        project::fingerprint(path)?
    } else {
        let mut file = fs::File::open(path)?;
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; 65536];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        hash.finalize().into()
    };
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}
fn absolute(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other.as_os_str()),
        }
    }
    Ok(clean)
}

struct OpenDocument {
    document: Document,
    history: History,
    source: Option<PathBuf>,
    fingerprint: Option<String>,
    warnings: Vec<String>,
    pdf_size: Option<(u32, u32, f64, pdf_io::PdfPageSize)>,
}
#[derive(Default)]
struct Session {
    opened: Option<OpenDocument>,
    engine: Engine,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenParams {
    path: PathBuf,
    page: Option<u32>,
    dpi: Option<u32>,
    #[serde(default)]
    discard: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewParams {
    width: u32,
    height: u32,
    #[serde(default = "resolution")]
    resolution: f64,
    #[serde(default)]
    discard: bool,
}
fn resolution() -> f64 {
    72.
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ApplyParams {
    operations: Vec<Operation>,
    #[serde(default)]
    expected_revision: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SaveParams {
    path: PathBuf,
    #[serde(default)]
    overwrite: bool,
    #[serde(default)]
    expected_fingerprint: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExportParams {
    path: PathBuf,
    #[serde(default)]
    overwrite: bool,
    #[serde(default)]
    max_side: Option<u32>,
    #[serde(default = "quality")]
    quality: u8,
    #[serde(default = "matte")]
    matte: [u8; 3],
    #[serde(default)]
    bit_depth: Option<u8>,
    #[serde(default)]
    cmyk_profile: Option<PathBuf>,
}
fn quality() -> u8 {
    90
}
fn matte() -> [u8; 3] {
    [255; 3]
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseParams {
    #[serde(default)]
    discard: bool,
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

fn pdf_info(path: &Path) -> Result<Value> {
    let path = absolute(path)?;
    let information = pdf_io::info(&path)?;
    Ok(json!({"path":path,"pageCount":information.pages.len(),
        "pages":information.pages.iter().enumerate().map(|(index, size)| json!({
            "page":index + 1,"widthPoints":size.width_points,"heightPoints":size.height_points
        })).collect::<Vec<_>>(),
        "editing":"rasterized_pages","pageNumbering":"one_based",
        "message":"PDF 按页转为图片编辑；打开时须明确 page 与 dpi，不保留原文字、矢量、表单或签名结构。"}))
}

fn import_for_session(
    p: &OpenParams,
) -> Result<(image_io::ImportResult, Option<pdf_io::PdfPageSize>)> {
    if !is_pdf(&p.path) {
        ensure!(
            p.page.is_none() && p.dpi.is_none(),
            "page 和 dpi 仅用于 PDF 输入"
        );
        return image_io::import(&p.path).map(|imported| (imported, None));
    }
    let information = pdf_io::info(&p.path)?;
    let count = information.pages.len();
    let page = p.page.ok_or_else(|| error("pdf_page_required", format!(
        "此 PDF 共 {count} 页；请明确 page（从 1 起）与 dpi（72–600）。使用 pdf-info 查看页面；多页请逐页导入，或使用界面导入对话框。"
    )))?;
    ensure!(
        page > 0 && page as usize <= count,
        "PDF 页码须在 1–{count} 之间"
    );
    let dpi = p
        .dpi
        .ok_or_else(|| error("pdf_dpi_required", "PDF 需要明确 dpi（72–600，例如 150）"))?;
    ensure!((72..=600).contains(&dpi), "PDF dpi 须在 72–600 之间");
    let rendered = pdf_io::render_pages(&p.path, &[page - 1], dpi)?
        .into_iter()
        .next()
        .context("PDF 页面未返回图像")?;
    let mut document = Document::new(rendered.raster.width, rendered.raster.height);
    document.resolution = rendered.effective_dpi;
    let resolved = if (rendered.effective_dpi - f64::from(dpi)).abs() < 0.5 {
        format!("{dpi} DPI")
    } else {
        format!(
            "{:.0} DPI（请求 {dpi} DPI，页面过大受系统渲染上限限制）",
            rendered.effective_dpi
        )
    };
    let name = format!(
        "{} · 第 {page} 页",
        p.path.file_stem().unwrap_or_default().to_string_lossy()
    );
    let layer = Layer::raster(name, rendered.raster);
    document.active_layer_id = Some(layer.id);
    document.layers = vec![layer];
    Ok((
        image_io::ImportResult {
            document,
            conversions: vec![format!(
                "PDF 共 {count} 页；仅将明确选择的第 {page} 页按 {resolved} 转为图片。其余页面未导入；原文字、矢量、表单与签名结构不保留，请逐页处理并另存 .comp 工程。"
            )],
        },
        Some(rendered.size),
    ))
}

impl Session {
    fn check_replace(&self, discard: bool) -> Result<()> {
        if !discard
            && self
                .opened
                .as_ref()
                .is_some_and(|s| s.history.is_modified())
        {
            return Err(error(
                "unsaved_changes",
                "当前会话有未保存编辑；保存后再打开，或明确 discard:true",
            ));
        }
        Ok(())
    }
    fn opened(&self) -> Result<&OpenDocument> {
        self.opened
            .as_ref()
            .ok_or_else(|| error("no_document", "请先 open 或 new 文档"))
    }
    fn opened_mut(&mut self) -> Result<&mut OpenDocument> {
        self.opened
            .as_mut()
            .ok_or_else(|| error("no_document", "请先 open 或 new 文档"))
    }
    fn inspect(&self) -> Result<Value> {
        let state = self.opened()?;
        Ok(
            json!({"document":document_info(&state.document),"source":state.source,"fingerprint":state.fingerprint,
            "revision":state.history.revision(),"dirty":state.history.is_modified(),"warnings":state.warnings,
            "undo":state.history.undo_name(),"redo":state.history.redo_name()}),
        )
    }
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        match method {
            "fonts" => {
                empty_params(params)?;
                Ok(json!({"fonts":crate::text::font_names()}))
            }
            "capabilities" | "schema" => {
                empty_params(params)?;
                Ok(capabilities())
            }
            "pdf-info" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {
                    path: PathBuf,
                }
                let p: Params = serde_json::from_value(params)?;
                pdf_info(&p.path)
            }
            "open" | "pdf-open" => {
                let p: OpenParams = serde_json::from_value(params)?;
                ensure!(
                    method != "pdf-open" || is_pdf(&p.path),
                    "pdf-open 需要 PDF 输入路径"
                );
                self.check_replace(p.discard)?;
                let path = absolute(&p.path)?;
                let before = fingerprint(&path)?;
                let (imported, page_size) = import_for_session(&OpenParams {
                    path: path.clone(),
                    ..p
                })?;
                ensure!(
                    before == fingerprint(&path)?,
                    "读取期间源文件发生变化，请重试"
                );
                let pdf_size = page_size.map(|size| {
                    (
                        imported.document.width,
                        imported.document.height,
                        imported.document.resolution,
                        size,
                    )
                });
                self.opened = Some(OpenDocument {
                    document: imported.document,
                    history: History::default(),
                    source: Some(path),
                    fingerprint: Some(before),
                    warnings: imported.conversions,
                    pdf_size,
                });
                self.inspect()
            }
            "new" => {
                let p: NewParams = serde_json::from_value(params)?;
                self.check_replace(p.discard)?;
                ensure!(
                    Document::valid_size(p.width, p.height),
                    "画布尺寸超出支持范围"
                );
                ensure!(
                    p.resolution.is_finite() && (1. ..=9600.).contains(&p.resolution),
                    "分辨率无效"
                );
                let mut document = Document::new(p.width, p.height);
                document.resolution = p.resolution;
                self.opened = Some(OpenDocument {
                    document,
                    history: History::default(),
                    source: None,
                    fingerprint: None,
                    warnings: vec![],
                    pdf_size: None,
                });
                self.inspect()
            }
            "inspect" => {
                empty_params(params)?;
                self.inspect()
            }
            "apply" => {
                let p: ApplyParams = serde_json::from_value(params)?;
                let state = self
                    .opened
                    .as_mut()
                    .ok_or_else(|| error("no_document", "请先 open 或 new 文档"))?;
                if p.expected_revision
                    .is_some_and(|id| id != state.history.revision())
                {
                    return Err(error("conflict", "会话 revision 已改变，请重新 inspect"));
                }
                apply_transaction(
                    &mut state.document,
                    &mut state.history,
                    &mut self.engine,
                    p.operations,
                )
            }
            "undo" | "redo" => {
                empty_params(params)?;
                let state = self.opened_mut()?;
                let changed = if method == "undo" {
                    state.history.undo(&mut state.document)
                } else {
                    state.history.redo(&mut state.document)
                };
                Ok(json!({"changed":changed,"state":self.inspect()?}))
            }
            "save" => {
                let p: SaveParams = serde_json::from_value(params)?;
                ensure!(
                    p.path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("comp")),
                    "项目输出必须使用 .comp 文件"
                );
                let target = absolute(&p.path)?;
                let state = self.opened_mut()?;
                let same_source = state
                    .source
                    .as_ref()
                    .map(|path| project::same_project_path(path, &target))
                    .transpose()?
                    .unwrap_or(false);
                let expected = if same_source {
                    let opened = state
                        .fingerprint
                        .as_deref()
                        .context("当前源项目缺少读取指纹")?;
                    if p.expected_fingerprint
                        .as_ref()
                        .is_some_and(|expected| !expected.eq_ignore_ascii_case(opened))
                    {
                        return Err(error("conflict", "指定 fingerprint 与已打开的源项目不一致"));
                    }
                    Some(opened)
                } else {
                    p.expected_fingerprint.as_deref()
                };
                let saved = project::save_checked(
                    &state.document,
                    &target,
                    expected,
                    same_source || p.overwrite,
                )?;
                state.fingerprint = Some(saved.iter().map(|byte| format!("{byte:02x}")).collect());
                state.source = Some(target.clone());
                state.history.mark_saved(state.history.revision());
                Ok(
                    json!({"path":target,"fingerprint":state.fingerprint,"revision":state.history.revision(),"document":document_info(&state.document)}),
                )
            }
            "export" => {
                let p: ExportParams = serde_json::from_value(params)?;
                let state = self.opened()?;
                let physical_size = state
                    .pdf_size
                    .filter(|(width, height, resolution, _)| {
                        (*width, *height) == (state.document.width, state.document.height)
                            && *resolution == state.document.resolution
                    })
                    .map(|(_, _, _, size)| size);
                export(&state.document, p, physical_size)
            }
            "close" => {
                let p: CloseParams = serde_json::from_value(params)?;
                self.check_replace(p.discard)?;
                self.opened = None;
                Ok(json!({"closed":true}))
            }
            _ => Err(error(
                "unknown_method",
                format!("未知方法：{method}；使用 capabilities 查看支持的方法"),
            )),
        }
    }
}

fn empty_params(params: Value) -> Result<()> {
    ensure!(
        params.as_object().is_some_and(|map| map.is_empty()),
        "此方法 params 必须为空对象"
    );
    Ok(())
}
fn export(
    doc: &Document,
    p: ExportParams,
    physical_size: Option<pdf_io::PdfPageSize>,
) -> Result<Value> {
    if p.path.exists() && !p.overwrite {
        return Err(error(
            "already_exists",
            "导出目标已存在；使用新路径，或明确 overwrite:true",
        ));
    }
    ensure!((1..=100).contains(&p.quality), "JPEG quality 范围是 1–100");
    ensure!(
        p.bit_depth.is_none_or(|depth| [8, 16].contains(&depth)),
        "bitDepth 必须为 8 或 16"
    );
    if let Some(max_side) = p.max_side {
        ensure!(
            max_side > 0 && max_side <= crate::model::MAX_SIDE,
            "预览 maxSide 无效"
        );
    }
    let format = p
        .path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    ensure!(
        [
            "png", "jpg", "jpeg", "tif", "tiff", "bmp", "gif", "webp", "pdf", "exr"
        ]
        .contains(&format.as_str()),
        "导出格式支持 PNG、JPEG、TIFF、BMP、GIF、WebP、PDF、EXR"
    );
    ensure!(
        p.bit_depth != Some(16) || ["png", "tif", "tiff"].contains(&format.as_str()),
        "16 位整数导出只支持 PNG 或 TIFF；HDR 请使用 EXR"
    );
    ensure!(
        p.cmyk_profile.is_none()
            || ["jpg", "jpeg", "tif", "tiff", "pdf"].contains(&format.as_str()),
        "CMYK 导出只支持 JPEG、TIFF 或 PDF"
    );
    ensure!(
        p.cmyk_profile.is_none() || p.bit_depth != Some(16),
        "CMYK 输出使用 8 位墨色通道"
    );
    let mut raster = p.max_side.map_or_else(
        || render::render_document(doc),
        |side| render::render_preview(doc, side),
    );
    if let Some(depth) = p.bit_depth {
        if depth == 8 {
            raster = Raster::from_rgba(raster.width, raster.height, raster.to_rgba());
        } else {
            let mut doc = Document::new(raster.width, raster.height);
            let layer = Layer::raster("Export", raster);
            let id = layer.id;
            doc.layers = vec![layer];
            crate::edit::convert_layer_depth(&mut doc, id, Some(crate::deep_pixels::Depth::U16))?;
            raster = doc.layers[0].image.as_ref().unwrap().as_ref().clone();
        }
    }
    if let Some(parent) = p.path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    if let Some(profile) = &p.cmyk_profile {
        let profile = crate::color_management::CmykProfile::load(profile)?;
        crate::print_export::export(
            &raster,
            &p.path,
            &format,
            &profile,
            doc.resolution,
            p.quality,
        )?;
    } else {
        export_document_raster(
            doc,
            &raster,
            &p.path,
            &format,
            p.quality,
            p.matte,
            physical_size,
        )?;
    }
    Ok(
        json!({"path":absolute(&p.path)?,"format":format,"width":raster.width,"height":raster.height,"bytes":fs::metadata(&p.path)?.len(),"sha256":fingerprint(&p.path)?}),
    )
}

pub(crate) fn export_document_raster(
    document: &Document,
    raster: &Raster,
    path: &Path,
    format: &str,
    quality: u8,
    matte: [u8; 3],
    physical_size: Option<pdf_io::PdfPageSize>,
) -> Result<()> {
    if format == "pdf" {
        // Keep the document's physical page size even when a preview has fewer
        // pixels. Passing an explicit size also avoids aspect-ratio rounding
        // changing one dimension when a single DPI value cannot represent both.
        ensure!(
            document.resolution.is_finite() && document.resolution > 0.,
            "PDF 文档分辨率无效"
        );
        let size = physical_size.unwrap_or(pdf_io::PdfPageSize {
            width_points: f64::from(document.width) * 72. / document.resolution,
            height_points: f64::from(document.height) * 72. / document.resolution,
        });
        pdf_io::export_pages(path, std::iter::once(Ok((raster.clone(), size))))
    } else {
        image_io::export_raster(raster, path, format, document.resolution, quality, matte)
    }
}

pub fn capabilities() -> Value {
    json!({"name":"Compositor","protocolVersion":1,"applicationVersion":env!("CARGO_PKG_VERSION"),
        "headless":true,"stdio":true,"liveControl":true,"externalVisionApi":false,
        "methods":["capabilities","fonts","pdf-info","pdf-open","open","new","inspect","apply","undo","redo","save","export","close"],
        "methodExamples":{
            "open":{"path":"input.comp"},"pdf-info":{"path":"input.pdf"},"pdf-open":{"path":"input.pdf","page":1,"dpi":150},
            "new":{"width":1024,"height":768},"inspect":{},
            "apply":{"operations":[{"op":"add_layer","name":"背景"}]},"undo":{},"redo":{},
            "save":{"path":"edited.comp","overwrite":false},
            "export":{"path":"preview.png","maxSide":1024,"quality":90,"matte":[255,255,255],"overwrite":false,"bitDepth":16,"cmykProfile":null},"close":{"discard":false}},
        "operations":crate::automation::capabilities(),"filters":crate::filters::FILTERS.iter().map(|name| json!({"name":name,"settings":crate::filters::default_settings(name)})).collect::<Vec<_>>(),
        "project":{"readVersions":[1,14],"writeVersion":11,"writeVersions":{"8-bit":11,"16-bit":12,"HDR":13,"smoothCorners":14},"container":"single-file","containerVersion":1,"legacyDirectoryRead":true,"selectionPersisted":false,"historyPersisted":false},
        "exportFormats":["png","jpeg","tiff","bmp","gif","webp","pdf","exr"],
        "color":{"workingSpace":"extended-sRGB","layerDepths":["8-bit","16-bit","HDR"],"exportOptions":{"bitDepth":[8,16],"cmykProfile":"path to a validated CMYK printer ICC profile; JPEG, TIFF or PDF only"}},
        "pdf":{"editing":"rasterized_pages","pageNumbering":"one_based","openRequires":["page","dpi"],"dpiRange":[72,600],"export":"one flattened raster page per document; maxSide preserves physical page size","liveOpen":"Use the GUI PDF import dialog; live controls existing documents only","limitations":"Original PDF text, vectors, forms and signatures are not preserved"},
        "limits":{"requestBytes":MAX_REQUEST_BYTES,"operationsPerBatch":1000},
        "concurrency":"apply is one atomic in-memory transaction; save checks the opened source fingerprint; shared GUI changes require expectedRevision",
        "help":"cli capabilities | fonts | pdf-info INPUT.pdf | pdf-open INPUT.pdf --page N --dpi N --output PATH.comp | inspect INPUT [--page N --dpi N] | new --width N --height N --output PATH.comp | apply INPUT --operations @FILE --output PATH.comp [--page N --dpi N] | export INPUT --output PATH.png|PATH.pdf [--page N --dpi N] | serve | live METHOD --params @FILE [--endpoint FILE] | skill show | skill install [--path FOLDER]"})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default = "object")]
    params: Value,
}
fn object() -> Value {
    json!({})
}

// Consume overlong lines without retaining their bytes, then continue at the next request.
fn read_line(reader: &mut impl BufRead) -> io::Result<Option<Result<Vec<u8>>>> {
    let mut line = Vec::new();
    let mut too_long = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if line.is_empty() && !too_long {
                None
            } else if too_long {
                Some(Err(error("request_too_large", "请求超过 16 MiB")))
            } else {
                Some(Ok(line))
            });
        }
        let end = available
            .iter()
            .position(|b| *b == b'\n')
            .map(|index| index + 1);
        let count = end.unwrap_or(available.len());
        if !too_long {
            if line.len() + count > MAX_REQUEST_BYTES {
                too_long = true;
                line.clear();
            } else {
                line.extend_from_slice(&available[..count]);
            }
        }
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(if too_long {
                Err(error("request_too_large", "请求超过 16 MiB"))
            } else {
                Ok(line)
            }));
        }
    }
}
fn serve(reader: &mut impl BufRead, writer: &mut impl Write) -> Result<()> {
    let mut session = Session::default();
    while let Some(line) = read_line(reader)? {
        let response = match line {
            Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) => {
                    let id = value.get("id").cloned().unwrap_or(Value::Null);
                    match serde_json::from_value::<Request>(value) {
                        Ok(request) => match session.request(&request.method, request.params) {
                            Ok(result) => success(request.id, result),
                            Err(err) => failure(request.id, &err),
                        },
                        Err(err) => failure(id, &err.into()),
                    }
                }
                Err(err) => failure(Value::Null, &err.into()),
            },
            Err(err) => failure(Value::Null, &err),
        };
        serde_json::to_writer(&mut *writer, &response)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}

#[derive(Default)]
struct Arguments {
    positional: Vec<OsString>,
    flags: BTreeMap<String, OsString>,
}
impl Arguments {
    fn parse(args: Vec<OsString>) -> Result<Self> {
        let mut parsed = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            if let Some(flag) = arg.to_str().and_then(|s| s.strip_prefix("--")) {
                let value = if flag == "overwrite" {
                    OsString::from("true")
                } else {
                    args.next()
                        .ok_or_else(|| error("invalid_arguments", format!("--{flag} 缺少参数")))?
                };
                ensure!(!parsed.flags.contains_key(flag), "重复参数：--{flag}");
                parsed.flags.insert(flag.into(), value);
            } else {
                parsed.positional.push(arg);
            }
        }
        Ok(parsed)
    }
    fn take(&mut self, name: &str) -> Option<OsString> {
        self.flags.remove(name)
    }
    fn required(&mut self, name: &str) -> Result<OsString> {
        self.take(name)
            .ok_or_else(|| error("invalid_arguments", format!("缺少 --{name}")))
    }
    fn input(&mut self) -> Result<PathBuf> {
        ensure!(self.positional.len() == 1, "此命令需要一个输入路径");
        Ok(PathBuf::from(self.positional.remove(0)))
    }
    fn finish(self) -> Result<()> {
        ensure!(
            self.flags.is_empty() && self.positional.is_empty(),
            "存在未知或多余参数：{:?} {:?}",
            self.flags.keys(),
            self.positional
        );
        Ok(())
    }
    fn number(&mut self, name: &str) -> Result<Option<u32>> {
        self.take(name)
            .map(|s| {
                s.to_str()
                    .context("数值不是 UTF-8")?
                    .parse()
                    .context("整数参数无效")
            })
            .transpose()
    }
    fn open_params(&mut self, path: &Path) -> Result<Value> {
        Ok(json!({"path":path,"page":self.number("page")?,"dpi":self.number("dpi")?}))
    }
}
fn json_input(value: OsString) -> Result<Value> {
    let text = value.to_str().context("JSON 参数不是 UTF-8")?;
    let bytes = if text == "-" {
        let mut bytes = Vec::new();
        io::stdin()
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        bytes
    } else if let Some(path) = text.strip_prefix('@') {
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        bytes
    } else {
        text.as_bytes().to_vec()
    };
    ensure!(bytes.len() <= MAX_REQUEST_BYTES, "JSON 文件超过 16 MiB");
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    Ok(serde_json::from_slice(bytes)?)
}
fn one_shot(command: &str, args: Vec<OsString>) -> Result<Value> {
    let mut args = Arguments::parse(args)?;
    match command {
        "fonts" => {
            args.finish()?;
            Ok(json!({"fonts":crate::text::font_names()}))
        }
        "capabilities" | "schema" | "help" | "--help" => {
            args.finish()?;
            Ok(capabilities())
        }
        "pdf-info" => {
            let input = args.input()?;
            args.finish()?;
            pdf_info(&input)
        }
        "pdf-open" => {
            let input = args.input()?;
            let output = PathBuf::from(args.required("output")?);
            let params = args.open_params(&input)?;
            let overwrite = args.take("overwrite").is_some();
            args.finish()?;
            let mut s = Session::default();
            let opened = s.request("pdf-open", params)?;
            let warnings = opened["warnings"].clone();
            let saved = s.request("save", json!({"path":output,"overwrite":overwrite}))?;
            Ok(json!({"opened":opened,"saved":saved,"warnings":warnings}))
        }
        "inspect" => {
            let input = args.input()?;
            let params = args.open_params(&input)?;
            args.finish()?;
            let mut s = Session::default();
            s.request("open", params)
        }
        "new" => {
            let width = args.number("width")?.context("缺少 --width")?;
            let height = args.number("height")?.context("缺少 --height")?;
            let output = PathBuf::from(args.required("output")?);
            let overwrite = args.take("overwrite").is_some();
            args.finish()?;
            let mut s = Session::default();
            s.request("new", json!({"width":width,"height":height}))?;
            s.request("save", json!({"path":output,"overwrite":overwrite}))
        }
        "apply" => {
            let input = args.input()?;
            let output = PathBuf::from(args.required("output")?);
            let operations = json_input(args.required("operations")?)?;
            let overwrite = args.take("overwrite").is_some();
            let expected = args
                .take("expected-fingerprint")
                .map(|s| s.to_string_lossy().into_owned());
            let params = args.open_params(&input)?;
            args.finish()?;
            let mut s = Session::default();
            let opened = s.request("open", params)?;
            if expected
                .as_ref()
                .is_some_and(|hash| opened["fingerprint"].as_str() != Some(hash))
            {
                return Err(error("conflict", "输入 fingerprint 不匹配"));
            }
            let applied = s.request("apply", json!({"operations":operations}))?;
            let saved = s.request("save", json!({"path":output,"overwrite":overwrite}))?;
            Ok(json!({"applied":applied,"saved":saved,"warnings":opened["warnings"]}))
        }
        "export" => {
            let input = args.input()?;
            let output = PathBuf::from(args.required("output")?);
            let max_side = args.number("max-side")?;
            let quality = args.number("quality")?.unwrap_or(90);
            let bit_depth = args.number("bit-depth")?;
            let cmyk_profile = args.take("cmyk-profile").map(PathBuf::from);
            ensure!(quality <= 100 && quality > 0, "quality 范围是 1–100");
            let overwrite = args.take("overwrite").is_some();
            let params = args.open_params(&input)?;
            args.finish()?;
            let mut s = Session::default();
            s.request("open", params)?;
            s.request(
                "export",
                json!({"path":output,"maxSide":max_side,"quality":quality,"overwrite":overwrite,"bitDepth":bit_depth,"cmykProfile":cmyk_profile}),
            )
        }
        "live" => {
            ensure!(args.positional.len() == 1, "live 需要一个 method");
            let method = args
                .positional
                .remove(0)
                .into_string()
                .map_err(|_| error("invalid_arguments", "method 必须为 UTF-8"))?;
            let params = args
                .take("params")
                .map(json_input)
                .transpose()?
                .unwrap_or_else(object);
            let endpoint = args.take("endpoint").map(PathBuf::from);
            args.finish()?;
            live(&method, params, endpoint.as_deref())
        }
        "skill" => {
            ensure!(args.positional.len() == 1, "skill 需要 show 或 install");
            let action = args.positional.remove(0);
            ensure!(
                action == "install",
                "使用 cli skill show 或 cli skill install"
            );
            let path = args.take("path").map(PathBuf::from);
            args.finish()?;
            install_skill(path)
        }
        _ => Err(error(
            "unknown_command",
            format!("未知命令 {command}；使用 cli --help"),
        )),
    }
}

fn live_exchange(
    endpoint: &Value,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    let address: std::net::SocketAddr = endpoint["address"]
        .as_str()
        .context("endpoint 缺少 address")?
        .parse()?;
    ensure!(
        address.ip().is_loopback(),
        "Agent endpoint 必须是本机回环地址"
    );
    ensure!(
        endpoint["token"]
            .as_str()
            .is_some_and(|token| !token.is_empty()),
        "endpoint 缺少 token"
    );
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let id = json!(Uuid::new_v4());
    let bytes = serde_json::to_vec(
        &json!({"token":endpoint["token"],"id":id,"method":method,"params":params}),
    )?;
    if bytes.len() >= 4 * 1024 * 1024 {
        return Err(error("request_too_large", "GUI Agent 请求超过 4 MiB"));
    }
    stream.write_all(&bytes)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = io::BufReader::new(stream);
    let response = read_line(&mut reader)?.context("GUI 未返回响应")??;
    let response: Value = serde_json::from_slice(&response)?;
    ensure!(
        response["protocolVersion"] == 1
            && (response["id"] == id || (response["ok"] == false && response["id"].is_null())),
        "GUI 响应协议或 id 不匹配"
    );
    Ok(response)
}

fn live(method: &str, params: Value, selected: Option<&Path>) -> Result<Value> {
    let endpoints = if let Some(path) = selected {
        vec![path.to_path_buf()]
    } else {
        let root = crate::app::preferences_directory();
        fs::read_dir(root)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("agent-") && s.ends_with(".json"))
            })
            .collect::<Vec<_>>()
    };
    let mut available = Vec::new();
    for path in endpoints {
        let probe = (|| -> Result<Value> {
            let mut bytes = Vec::new();
            fs::File::open(&path)?.take(65537).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 65536, "endpoint 文件过大");
            let value: Value = serde_json::from_slice(&bytes)?;
            let response = live_exchange(&value, "capabilities", object(), Duration::from_secs(3))?;
            ensure!(
                response["ok"] == true && response["result"]["liveControl"] == true,
                "endpoint 未通过 Compositor 身份验证"
            );
            Ok(value)
        })();
        match probe {
            Ok(value) => available.push((path, value)),
            Err(err) if selected.is_some() => {
                return Err(err.context("指定的 Agent endpoint 不可用"));
            }
            Err(_) => continue,
        }
    }
    if available.is_empty() {
        return Err(error("gui_not_running", "没有发现可连接的 Compositor 窗口"));
    }
    if available.len() != 1 {
        return Err(error(
            "multiple_windows",
            format!(
                "存在多个窗口，请用 --endpoint 选择：{:?}",
                available.iter().map(|e| &e.0).collect::<Vec<_>>()
            ),
        ));
    }
    let (_, endpoint) = available.remove(0);
    let response = live_exchange(&endpoint, method, params, Duration::from_secs(190))?;
    if response["ok"] == true {
        Ok(response["result"].clone())
    } else {
        Err(CliError {
            code: response["error"]["code"]
                .as_str()
                .unwrap_or("gui_error")
                .into(),
            message: response["error"]["message"]
                .as_str()
                .unwrap_or("GUI 请求失败")
                .into(),
            operation_index: response["error"]["operationIndex"]
                .as_u64()
                .map(|index| index as usize),
            upstream: Some(response["error"].clone()),
        }
        .into())
    }
}

fn install_skill(selected: Option<PathBuf>) -> Result<Value> {
    let folder = selected.unwrap_or_else(|| {
        std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(
                    std::env::var_os("USERPROFILE")
                        .or_else(|| std::env::var_os("HOME"))
                        .unwrap_or_else(|| OsString::from(".")),
                )
                .join(".codex")
            })
            .join("skills/compositor-editor")
    });
    let path = folder.join("SKILL.md");
    let executable = std::env::current_exe()?;
    let content = format!(
        "{SKILL}\n<!-- COMPOSITOR-INSTALL -->\n本机安装的 Compositor EXE：\n```json\n{}\n```\n",
        serde_json::to_string(&executable)?
    );
    fs::create_dir_all(&folder)?;
    let backup = if path.exists() {
        let existing = fs::read(&path)?;
        if existing == content.as_bytes() {
            None
        } else {
            let backup = folder.join(format!("SKILL.md.{}.bak", Uuid::new_v4()));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&backup)?;
            file.write_all(&existing)?;
            file.sync_all()?;
            Some(backup)
        }
    } else {
        None
    };
    image_io::write_atomic(&path, content.as_bytes())?;
    Ok(
        json!({"path":absolute(&path)?,"executable":executable,"name":"compositor-editor","backup":backup}),
    )
}

pub fn run(mut args: Vec<OsString>) -> i32 {
    if args.is_empty() {
        args.push("help".into());
    }
    let command = args.remove(0).to_string_lossy().into_owned();
    let result = if command == "serve" {
        if !args.is_empty() {
            Err(error("invalid_arguments", "serve 不接受其他参数"))
        } else {
            return match serve(&mut io::stdin().lock(), &mut io::stdout().lock()) {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("{err:#}");
                    1
                }
            };
        }
    } else if command == "skill" && args == [OsString::from("show")] {
        return if io::stdout().write_all(SKILL.as_bytes()).is_ok() {
            0
        } else {
            1
        };
    } else {
        one_shot(&command, args)
    };
    let (value, code) = match result {
        Ok(value) => (success(Value::Null, value), 0),
        Err(err) => (failure(Value::Null, &err), 1),
    };
    match serde_json::to_writer(io::stdout().lock(), &value)
        .and_then(|_| io::stdout().write_all(b"\n").map_err(serde_json::Error::io))
    {
        Ok(()) => code,
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

#[cfg(windows)]
pub fn attach_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process: u32) -> i32;
    }
    // Inherited redirected handles remain in use; an interactive shell owns its console.
    unsafe {
        AttachConsole(u32::MAX);
    }
}
#[cfg(not(windows))]
pub fn attach_console() {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_batch_preserves_document_and_history() {
        let mut s = Session::default();
        s.request("new", json!({"width":16,"height":16})).unwrap();
        let before = s.opened().unwrap().document.clone();
        let revision = s.opened().unwrap().history.revision();
        let err=s.request("apply",json!({"operations":[{"op":"add_layer","name":"temporary"},{"op":"delete_layers","layers":[Uuid::nil()]}]})).unwrap_err();
        assert_eq!(failure(Value::Null, &err)["error"]["operationIndex"], 1);
        assert_eq!(s.opened().unwrap().document, before);
        assert_eq!(s.opened().unwrap().history.revision(), revision);
    }
    #[test]
    fn jsonl_recovers_after_bad_requests_and_keeps_ids() {
        let input=b"not json\n{\"id\":3,\"method\":\"new\",\"params\":{\"width\":8,\"height\":9}}\n{\"id\":\"hello\",\"method\":\"inspect\"}\n";
        let mut output = vec![];
        serve(&mut &input[..], &mut output).unwrap();
        let values = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 3);
        assert_eq!(values[0]["ok"], false);
        assert_eq!(values[1]["id"], 3);
        assert_eq!(values[2]["id"], "hello");
        assert_eq!(values[2]["result"]["document"]["height"], 9);
    }
    #[test]
    fn opened_file_conflicts_and_export_are_independent_of_saved_state() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("测试.comp");
        let mut s = Session::default();
        s.request("new", json!({"width":8,"height":8})).unwrap();
        s.request("save", json!({"path":path})).unwrap();
        s.request(
            "apply",
            json!({"operations":[{"op":"add_layer","name":"edit"}]}),
        )
        .unwrap();
        s.request("export", json!({"path":folder.path().join("结果.png")}))
            .unwrap();
        assert!(s.opened().unwrap().history.is_modified());
        let mut external = project::load(&path).unwrap();
        external.layers[0].name = "external".into();
        project::save(&external, &path).unwrap();
        let err = s.request("save", json!({"path":path})).unwrap_err();
        assert_eq!(failure(Value::Null, &err)["error"]["code"], "conflict");
        assert_eq!(project::load(&path).unwrap().layers[0].name, "external");
    }

    #[test]
    fn cli_high_precision_export_preserves_source_and_validates_options() {
        use crate::deep_pixels::{DeepRaster, Depth};
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("低位颜色.comp");
        let mut document = Document::new(2, 1);
        let bits = vec![30001, 40002, 50003, 65535, 30117, 40118, 50119, 32768];
        document.layers[0].image = Some(std::sync::Arc::new(Raster::from_deep(
            DeepRaster::from_u16(2, 1, bits.clone()).unwrap(),
        )));
        project::save(&document, &source).unwrap();
        let output = folder.path().join("导出16位.png");
        one_shot(
            "export",
            vec![
                source.as_os_str().into(),
                "--output".into(),
                output.as_os_str().into(),
                "--bit-depth".into(),
                "16".into(),
            ],
        )
        .unwrap();
        let decoded = image_io::decode_image(&output).unwrap();
        assert_eq!(decoded.deep.unwrap().to_bits(), bits);
        assert_eq!(project::load(&source).unwrap(), document);
        let mut session = Session::default();
        session.request("open", json!({"path":source})).unwrap();
        let revision = session.opened().unwrap().history.revision();
        for params in [
            json!({"path":folder.path().join("bad.jpg"),"bitDepth":16}),
            json!({"path":folder.path().join("bad.png"),"cmykProfile":"missing.icc"}),
            json!({"path":folder.path().join("bad.tiff"),"bitDepth":16,"cmykProfile":"missing.icc"}),
        ] {
            let target = PathBuf::from(params["path"].as_str().unwrap());
            assert!(session.request("export", params).is_err());
            assert!(!target.exists());
        }
        session
            .request(
                "apply",
                json!({"operations":[{"op":"convert_depth","depth":"HDR"}]}),
            )
            .unwrap();
        assert_ne!(session.opened().unwrap().history.revision(), revision);
        assert_eq!(
            session.opened().unwrap().document.layers[0]
                .image
                .as_ref()
                .unwrap()
                .deep
                .as_ref()
                .unwrap()
                .depth,
            Depth::Hdr
        );
        let exr = folder.path().join("CLI浮点.exr");
        session.request("export", json!({"path":exr})).unwrap();
        assert_eq!(
            image_io::decode_image(&exr).unwrap().deep.unwrap().depth,
            Depth::Hdr
        );
    }

    #[test]
    #[cfg(windows)]
    fn pdf_requires_explicit_page_and_dpi_without_replacing_the_session() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("两页.pdf");
        pdf_io::export_pages(
            &path,
            [
                Ok((
                    Raster::solid(36, 18, [10, 180, 30, 255]),
                    pdf_io::PdfPageSize {
                        width_points: 36.,
                        height_points: 18.,
                    },
                )),
                Ok((
                    Raster::solid(36, 18, [220, 20, 40, 255]),
                    pdf_io::PdfPageSize {
                        width_points: 36.25,
                        height_points: 18.5,
                    },
                )),
            ],
        )
        .unwrap();
        let information = pdf_info(&path).unwrap();
        assert_eq!(information["pageCount"], 2);
        assert_eq!(information["pages"][1]["page"], 2);
        let mut s = Session::default();
        s.request("new", json!({"width":8,"height":9})).unwrap();
        let before = s.opened().unwrap().document.clone();
        let missing_page = s.request("open", json!({"path":path})).unwrap_err();
        assert_eq!(
            failure(Value::Null, &missing_page)["error"]["code"],
            "pdf_page_required"
        );
        assert!(missing_page.to_string().contains("2 页"));
        assert_eq!(s.opened().unwrap().document, before);
        let missing_dpi = s
            .request("open", json!({"path":path,"page":2}))
            .unwrap_err();
        assert_eq!(
            failure(Value::Null, &missing_dpi)["error"]["code"],
            "pdf_dpi_required"
        );
        for params in [
            json!({"path":path,"page":0,"dpi":150}),
            json!({"path":path,"page":3,"dpi":150}),
            json!({"path":path,"page":1,"dpi":71}),
            json!({"path":path,"page":1,"dpi":601}),
        ] {
            assert!(s.request("open", params).is_err());
            assert_eq!(s.opened().unwrap().document, before);
        }
        let opened = s
            .request("pdf-open", json!({"path":path,"page":2,"dpi":72}))
            .unwrap();
        assert_eq!(opened["document"]["resolution"], 72.);
        let selected = s
            .opened()
            .unwrap()
            .document
            .active()
            .unwrap()
            .image
            .as_ref()
            .unwrap()
            .get(4, 4);
        assert!(
            selected[0] > 180 && selected[1] < 60,
            "Selected page 2 must contain the red raster: {selected:?}"
        );
        assert!(opened["warnings"][0].as_str().unwrap().contains("第 2 页"));
        let preview = folder.path().join("第二页预览.pdf");
        s.request("export", json!({"path":preview,"maxSide":20}))
            .unwrap();
        let preview_information = pdf_io::info(&preview).unwrap();
        assert!((preview_information.pages[0].width_points - 36.25).abs() < 0.001);
        assert!((preview_information.pages[0].height_points - 18.5).abs() < 0.001);
        let output = folder.path().join("第二页.comp");
        let converted = one_shot(
            "pdf-open",
            vec![
                path.into_os_string(),
                "--page".into(),
                "2".into(),
                "--dpi".into(),
                "72".into(),
                "--output".into(),
                output.clone().into_os_string(),
            ],
        )
        .unwrap();
        assert_eq!(
            converted["saved"]["document"]["width"],
            opened["document"]["width"]
        );
        assert_eq!(project::load(&output).unwrap().resolution, 72.);
    }

    #[test]
    #[cfg(windows)]
    fn pdf_preview_keeps_physical_page_size_and_dirty_state() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("缩略预览.pdf");
        let mut s = Session::default();
        s.request("new", json!({"width":103,"height":77,"resolution":150}))
            .unwrap();
        s.request(
            "apply",
            json!({"operations":[{"op":"add_layer","name":"edit"}]}),
        )
        .unwrap();
        let revision = s.opened().unwrap().history.revision();
        let result = s
            .request("export", json!({"path":path,"maxSide":40}))
            .unwrap();
        assert_eq!(result["format"], "pdf");
        assert_eq!(result["width"], 40);
        assert!(result["height"].as_u64().unwrap() < 77);
        let information = pdf_io::info(&path).unwrap();
        assert_eq!(information.pages.len(), 1);
        assert!((information.pages[0].width_points - 103. * 72. / 150.).abs() < 0.001);
        assert!((information.pages[0].height_points - 77. * 72. / 150.).abs() < 0.001);
        assert!(s.opened().unwrap().history.is_modified());
        assert_eq!(s.opened().unwrap().history.revision(), revision);
        let prior_bytes = fs::read(&path).unwrap();
        let exists = s
            .request("export", json!({"path":path,"maxSide":20}))
            .unwrap_err();
        assert_eq!(
            failure(Value::Null, &exists)["error"]["code"],
            "already_exists"
        );
        assert_eq!(fs::read(path).unwrap(), prior_bytes);
    }

    #[test]
    fn pdf_options_are_rejected_for_non_pdf_inputs() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("图像.png");
        image_io::export_raster(
            &Raster::solid(4, 4, [20, 30, 40, 255]),
            &path,
            "png",
            72.,
            90,
            [255; 3],
        )
        .unwrap();
        let mut s = Session::default();
        let err = s
            .request("open", json!({"path":path,"page":1,"dpi":150}))
            .unwrap_err();
        assert!(err.to_string().contains("仅用于 PDF"));
        assert!(s.opened.is_none());
    }
}
