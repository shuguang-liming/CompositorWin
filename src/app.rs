use crate::locale::text as tr;
use crate::{
    gpu::GpuCanvas,
    history::History,
    image_io,
    model::{BlendMode, Document, Layer, Mask, Raster, Sampling, Selection},
    project, render,
    selection::{self, SelectionMode},
};
use eframe::egui::{self, Color32, Key, Pos2, Rect, Sense, Stroke, Vec2};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, mpsc},
    time::{Duration, Instant, SystemTime},
};
use uuid::Uuid;

#[path = "layer_panel.rs"]
mod layer_panel;

#[path = "selection_panel.rs"]
mod selection_panel;

#[cfg(test)]
#[path = "app_state_tests.rs"]
mod state_tests;

#[cfg(test)]
#[path = "renderer_recovery_tests.rs"]
mod renderer_recovery_tests;

#[path = "renderer_recovery.rs"]
mod renderer_recovery;

#[path = "app_chrome.rs"]
mod chrome_ui;
#[path = "app_text_color.rs"]
mod text_color_ui;

#[path = "app_tools.rs"]
mod tool_header_ui;

#[path = "welcome_ui.rs"]
mod welcome_ui;

#[path = "agent_bridge.rs"]
mod agent_bridge;
#[path = "gui_threads.rs"]
mod gui_threads;
#[path = "import_worker.rs"]
mod import_worker;
#[path = "pdf_ui.rs"]
mod pdf_ui;
#[path = "theme_settings.rs"]
mod theme_settings;

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
enum Tool {
    Move,
    Rectangle,
    Ellipse,
    Lasso,
    Polygon,
    Wand,
    Object,
    Crop,
    Brush,
    Eraser,
    Heal,
    Clone,
    Blur,
    Smudge,
    Liquify,
    Gradient,
    Shape,
    Text,
    Eyedropper,
    Hand,
    Zoom,
}
impl Tool {
    const ALL: [Self; 21] = [
        Self::Move,
        Self::Rectangle,
        Self::Ellipse,
        Self::Lasso,
        Self::Polygon,
        Self::Wand,
        Self::Object,
        Self::Crop,
        Self::Brush,
        Self::Eraser,
        Self::Heal,
        Self::Clone,
        Self::Blur,
        Self::Smudge,
        Self::Liquify,
        Self::Gradient,
        Self::Shape,
        Self::Text,
        Self::Eyedropper,
        Self::Hand,
        Self::Zoom,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Move => "移动 (V)",
            Self::Rectangle => "矩形选框 (M)",
            Self::Ellipse => "椭圆",
            Self::Lasso => "套索 (L)",
            Self::Polygon => "多边形套索",
            Self::Wand => "魔棒 (W)",
            Self::Object => "对象选择",
            Self::Crop => "裁剪 (C)",
            Self::Brush => "画笔 (B)",
            Self::Eraser => "橡皮擦 (E)",
            Self::Heal => "修复画笔 (J)",
            Self::Clone => "仿制图章 (S)",
            Self::Blur => "模糊",
            Self::Smudge => "涂抹",
            Self::Liquify => "液化",
            Self::Gradient => "渐变 (G)",
            Self::Shape => "形状 (U)",
            Self::Text => "文字 (T)",
            Self::Eyedropper => "吸管 (I)",
            Self::Hand => "抓手 (H)",
            Self::Zoom => "缩放 (Z)",
        }
    }
    fn paints(self) -> bool {
        matches!(
            self,
            Self::Brush
                | Self::Eraser
                | Self::Heal
                | Self::Clone
                | Self::Blur
                | Self::Smudge
                | Self::Liquify
        )
    }
}
impl Tool {
    fn paint_index(self) -> Option<usize> {
        match self {
            Self::Brush => Some(0),
            Self::Eraser => Some(1),
            Self::Heal => Some(2),
            Self::Clone => Some(3),
            Self::Blur => Some(4),
            Self::Smudge => Some(5),
            Self::Liquify => Some(6),
            _ => None,
        }
    }
}
struct Session {
    placeholder: bool,
    id: Uuid,
    doc: Document,
    history: History,
    path: Option<PathBuf>,
    display_name: Option<String>,
    pdf_size: Option<(u32, u32, f64, crate::pdf_io::PdfPageSize)>,
    selected: HashSet<Uuid>,
    /// Physical screen pixels per document pixel (1.0 is actual size at any DPI).
    zoom: f32,
    pixels_per_point: f32,
    pan: Vec2,
    selection_amounts: [f64; 3],
    serial: u64,
    rendered: u64,
    texture: Option<egui::TextureHandle>,
    gpu_id: Option<egui::TextureId>,
    preview_busy: bool,
    preview_failed: Option<(u64, crate::preview::ViewRequest)>,
    preview_view: Option<crate::preview::ViewRequest>,
    rendered_view: Option<crate::preview::ViewRequest>,
    stamp: u64,
    save_busy: bool,
    edit_busy: bool,
    gradient_task: Option<(Uuid, bool)>,
    mask_paint_white: bool,
    reload_busy: bool,
    preview_cache: Arc<Mutex<crate::preview::PreviewCache>>,
    outline: crate::canvas_overlay::SelectionOutlineCache,
    thumbnails: crate::canvas_overlay::ThumbnailCache,
    mask_thumbnails: crate::canvas_overlay::ThumbnailCache,
}
impl Session {
    fn new(doc: Document, path: Option<PathBuf>) -> Self {
        let selected = doc.active_layer_id.into_iter().collect();
        let stamp = path.as_ref().map(|p| package_stamp(p)).unwrap_or(0);
        Self {
            placeholder: false,
            id: Uuid::new_v4(),
            doc,
            history: History::default(),
            path,
            display_name: None,
            pdf_size: None,
            selected,
            zoom: 0.,
            pixels_per_point: 1.,
            pan: Vec2::ZERO,
            selection_amounts: [1., 1., 2.],
            serial: 1,
            rendered: 0,
            texture: None,
            gpu_id: None,
            preview_busy: false,
            preview_failed: None,
            preview_view: None,
            rendered_view: None,
            stamp,
            save_busy: false,
            edit_busy: false,
            gradient_task: None,
            mask_paint_white: false,
            reload_busy: false,
            preview_cache: Arc::new(Mutex::new(crate::preview::PreviewCache::default())),
            outline: crate::canvas_overlay::SelectionOutlineCache::default(),
            thumbnails: crate::canvas_overlay::ThumbnailCache::default(),
            mask_thumbnails: crate::canvas_overlay::ThumbnailCache::default(),
        }
    }
    fn title(&self) -> String {
        format!(
            "{}{}",
            self.path
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|p| p.to_string_lossy().to_string())
                .or_else(|| self.display_name.clone())
                .unwrap_or_else(|| "未命名".into()),
            if self.history.is_modified() { " *" } else { "" }
        )
    }
    fn changed(&mut self) {
        self.serial = self.serial.wrapping_add(1);
        self.preview_failed = None;
        self.thumbnails.retain_document(&self.doc);
        let mask_ids = self
            .doc
            .layers
            .iter()
            .filter(|layer| layer.mask.is_some())
            .map(|layer| layer.id)
            .collect();
        self.mask_thumbnails.retain(&mask_ids);
    }
    fn edit(&mut self, name: &str, f: impl FnOnce(&mut Document)) {
        let before = self.doc.clone();
        f(&mut self.doc);
        if self.history.record(name, before, &self.doc) {
            self.changed();
        }
    }
}
enum Worker {
    PdfInfo {
        path: PathBuf,
        target: Option<Uuid>,
        result: Result<crate::pdf_io::PdfInfo, String>,
    },
    PdfOpen {
        path: PathBuf,
        target: Option<Uuid>,
        dpi: u32,
        result: Result<Vec<crate::pdf_io::PdfPage>, String>,
    },
    DiskChanged {
        target: WatchTarget,
        stamp: u64,
    },
    ColorSample {
        session: Uuid,
        serial: u64,
        sampler: Arc<render::CompositeSampler>,
    },
    ColorSampleFailure {
        session: Uuid,
        serial: u64,
        error: String,
    },
    WarpPreview {
        session: Uuid,
        generation: u64,
        result: Result<Layer, String>,
    },
    Open {
        path: PathBuf,
        result: Result<image_io::ImportResult, String>,
        target: Option<Uuid>,
    },
    Save {
        id: Uuid,
        revision: Uuid,
        path: PathBuf,
        result: Result<(), String>,
    },
    Preview {
        id: Uuid,
        serial: u64,
        view: crate::preview::ViewRequest,
        raster: Raster,
    },
    PreviewFailure {
        id: Uuid,
        serial: u64,
        view: crate::preview::ViewRequest,
        error: String,
    },
    Export(Result<PathBuf, String>),
    RawPreview {
        path: PathBuf,
        key: [u32; 4],
        result: Result<Raster, String>,
    },
    DialogPreview {
        generation: u64,
        sample: Option<Arc<Raster>>,
        graded_sample: Option<Arc<Raster>>,
        display_sample: Option<Arc<Raster>>,
        result: Result<(Raster, usize), String>,
    },
    Gradient {
        revision: Uuid,
        serial: u64,
        state: Box<GradientState>,
        result: Result<Document, String>,
    },
    Edit {
        id: Uuid,
        revision: Uuid,
        serial: u64,
        name: String,
        before: Document,
        result: Result<Document, String>,
    },
    Reload {
        id: Uuid,
        stamp: u64,
        revision: Uuid,
        serial: u64,
        force: bool,
        result: Result<Document, String>,
    },
}
#[derive(Clone)]
struct LayerPayload {
    document: Uuid,
    ids: Vec<Uuid>,
}
#[derive(Clone)]
struct TabPayload(Uuid);
enum SegmentOperation {
    Selection {
        point: Option<[f64; 2]>,
        mode: SelectionMode,
    },
    Remove {
        layer: Uuid,
        settings: Value,
    },
    Preview {
        layer: Uuid,
        settings: Value,
        generation: u64,
    },
}
struct SegmentRequest {
    session: Uuid,
    before: Document,
    revision: Uuid,
    serial: u64,
    operation: SegmentOperation,
}
struct SampleDrag {
    original: [u8; 4],
    session: Uuid,
}
struct GuideDrag {
    before: Document,
    id: Uuid,
    horizontal: bool,
}
#[derive(Clone)]
struct GradientState {
    session: Uuid,
    request: Uuid,
    base_revision: Uuid,
    before: Document,
    start: [f64; 2],
    end: [f64; 2],
    layer: Uuid,
    mask: bool,
    revision: Option<Uuid>,
    parameters: (String, [u8; 4], [u8; 4], f64),
}
#[derive(Clone)]
enum Dialog {
    CanvasSize,
    ImageSize,
    Grid,
    Guides,
    ColorRange,
    Jpeg,
    Trim,
    Text {
        layer: Option<Uuid>,
        origin: [f64; 2],
    },
    Adjustment {
        kind: String,
    },
    Filter {
        kind: String,
    },
    Effect {
        kind: String,
    },
    CameraRaw,
    Raw {
        path: PathBuf,
        target: Option<Uuid>,
    },
}
#[derive(Clone, Copy)]
enum Command {
    New,
    Open,
    Import,
    Save,
    SaveAs,
    ExportPng,
    ExportJpeg,
    ExportTiff,
    ExportOther,
    ExportPdf,
    Undo,
    Redo,
    Close,
    Copy,
    CopyMerged,
    Paste,
    Cut,
    AddLayer,
    Group,
    Ungroup,
    Duplicate,
    Delete,
    Merge,
    MergeDown,
    Mask,
    InvertMask,
    ApplyMask,
    DeleteMask,
    CopyMask,
    PasteMask,
    LoadMask,
    BlurMask,
    InvertPixels,
    Clip,
    SelectAll,
    Deselect,
    InvertSelection,
    Subject,
    Expand,
    Contract,
    Feather,
    LoadAlpha,
    CropSelection,
    CanvasSize,
    ImageSize,
    Trim,
    FlipCanvasX,
    FlipCanvasY,
    Fit,
    Actual,
    Grid,
    Rulers,
    GridSettings,
    Guides,
    ColorRange,
    CropCommit,
    CropCancel,
    Shortcuts,
    FillForeground,
    FillBackground,
    ClearPixels,
    ContentFill,
}
type TextBoxDrag = ([f64; 2], [f64; 2], [f64; 2], bool);
struct Drag {
    target: Option<(Uuid, Uuid)>,
    before: Document,
    work_before: Document,
    start: [f64; 2],
    last: [f64; 2],
    points: Vec<[f64; 2]>,
    mode: Tool,
    selection_mode: SelectionMode,
    selection_antialias: bool,
    constrain_armed: bool,
    handle: Option<usize>,
    pan: Vec2,
    distort: bool,
    rotate: bool,
    centered: bool,
    pixel_move: bool,
    outline_move: bool,
    duplicate_pixels: bool,
}

#[derive(Clone)]
struct WorkerSender {
    sender: mpsc::Sender<Worker>,
    context: Arc<Mutex<Option<egui::Context>>>,
}
impl WorkerSender {
    fn bind_context(&self, context: egui::Context) {
        *self.context.lock().unwrap_or_else(|e| e.into_inner()) = Some(context);
    }
    fn send(&self, worker: Worker) -> Result<(), mpsc::SendError<()>> {
        self.sender.send(worker).map_err(|_| mpsc::SendError(()))?;
        wake_renderer(&self.context);
        Ok(())
    }
}

fn wake_renderer(context: &Mutex<Option<egui::Context>>) {
    // A repaint callback may reenter application code; never call it while locked.
    let context = context.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(context) = context {
        context.request_repaint();
    }
}

#[derive(Clone, PartialEq, Eq)]
struct WatchTarget {
    id: Uuid,
    path: PathBuf,
    stamp: u64,
}
#[derive(Default)]
struct WatchState {
    targets: Vec<WatchTarget>,
    generation: u64,
    stopped: bool,
}
struct PackageWatcher {
    shared: Arc<(Mutex<WatchState>, Condvar)>,
    sender: WorkerSender,
    started: bool,
}
impl PackageWatcher {
    fn new(sender: WorkerSender) -> Self {
        Self {
            shared: Arc::new((Mutex::new(WatchState::default()), Condvar::new())),
            sender,
            started: false,
        }
    }
    fn sync(&mut self, targets: Vec<WatchTarget>) -> Result<(), String> {
        let (mutex, wake) = &*self.shared;
        let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
        if state.targets == targets && (self.started || targets.is_empty()) {
            return Ok(());
        }
        state.targets = targets;
        state.generation = state.generation.wrapping_add(1);
        let start = !self.started && !state.targets.is_empty();
        drop(state);
        if start {
            let shared = self.shared.clone();
            let sender = self.sender.clone();
            gui_threads::spawn("磁盘监听", move || Self::run(shared, sender))?;
            self.started = true;
        }
        wake.notify_one();
        Ok(())
    }
    fn run(shared: Arc<(Mutex<WatchState>, Condvar)>, sender: WorkerSender) {
        let (mutex, wake) = &*shared;
        let mut reported = HashMap::new();
        let mut generation = 0;
        let mut deadline = Instant::now() + Duration::from_millis(500);
        loop {
            let (targets, current_generation) = {
                let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
                while !state.stopped && (state.targets.is_empty() || Instant::now() < deadline) {
                    state = if state.targets.is_empty() {
                        wake.wait(state).unwrap_or_else(|e| e.into_inner())
                    } else {
                        wake.wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
                            .unwrap_or_else(|e| e.into_inner())
                            .0
                    };
                }
                if state.stopped {
                    return;
                }
                (state.targets.clone(), state.generation)
            };
            deadline = Instant::now() + Duration::from_millis(500);
            if generation != current_generation {
                reported.clear();
                generation = current_generation;
            }
            reported.retain(|id, (target, _)| targets.iter().any(|t| t.id == *id && t == target));
            for target in targets {
                let stamp = package_stamp(&target.path);
                if stamp == target.stamp {
                    reported.remove(&target.id);
                    continue;
                }
                if reported
                    .get(&target.id)
                    .is_some_and(|(previous, previous_stamp)| {
                        previous == &target && *previous_stamp == stamp
                    })
                {
                    continue;
                }
                let state = mutex.lock().unwrap_or_else(|e| e.into_inner());
                if state.stopped {
                    return;
                }
                if !state.targets.contains(&target) {
                    continue;
                }
                reported.insert(target.id, (target.clone(), stamp));
                drop(state);
                if sender.send(Worker::DiskChanged { target, stamp }).is_err() {
                    return;
                }
            }
        }
    }
}
impl Drop for PackageWatcher {
    fn drop(&mut self) {
        let (mutex, wake) = &*self.shared;
        let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        state.targets.clear();
        wake.notify_one();
    }
}

pub struct EditorApp {
    agent_bridge: Option<agent_bridge::AgentBridge>,
    theme_preference: egui::ThemePreference,
    custom_theme: Option<crate::custom_theme::ThemeFile>,
    custom_theme_enabled: bool,
    theme_settings_open: bool,
    theme_settings_message: Option<String>,
    theme_settings_dirty: bool,
    open_source_licenses: crate::open_source_licenses::LicenseWindow,
    tool_modes: [Tool; 5],
    tabs: Vec<Session>,
    current: usize,
    gpu: Option<GpuCanvas>,
    sender: WorkerSender,
    receiver: mpsc::Receiver<Worker>,
    pending_io: usize,
    imports: import_worker::ImportQueue,
    pdf_ui: pdf_ui::PdfUi,
    busy_since: Option<Instant>,
    segment_sender: mpsc::Sender<SegmentRequest>,
    tool: Tool,
    foreground: [u8; 4],
    background: [u8; 4],
    color_picker: Option<crate::palette::ColorPicker>,
    text_color_target: Option<text_color_ui::TextColorTarget>,
    sample_drag: Option<SampleDrag>,
    sample_cache: Option<(Uuid, Arc<render::CompositeSampler>)>,
    sample_busy: Option<(Uuid, u64)>,
    sample_point: Option<[f64; 2]>,
    show_sample_ring: bool,
    diameter: f64,
    hardness: f64,
    blur_radius: f64,
    opacity: f64,
    smoothing: f64,
    brush_presets: [Option<[f64; 4]>; 7],
    mask_target: bool,
    mask_alone: bool,
    mask_clipboard: Option<Mask>,
    selection_mode: SelectionMode,
    antialias: bool,
    tolerance: u8,
    contiguous: bool,
    wand_all: bool,
    wand_radius: u8,
    clone_source: Option<[f64; 2]>,
    clone_aligned: bool,
    clone_all: bool,
    clone_offset: Option<[f64; 2]>,
    last_brush: Option<(Uuid, Uuid, bool, [f64; 2])>,
    healing_mode: usize,
    shape_kind: String,
    corner_radius: f64,
    line_width: f64,
    transform_percent: bool,
    show_transform: bool,
    transform_auto_select: bool,
    locks_transform_ratio: bool,
    warp_preview: Option<(Uuid, Layer)>,
    warp_preview_busy: bool,
    warp_generation: u64,
    warp_key: Option<(Uuid, [[u64; 2]; 4], bool)>,
    gradient_kind: String,
    gradient_transparent: bool,
    gradient_reverse: bool,
    gradient: Option<GradientState>,
    gradient_handle: Option<usize>,
    crop_ratio: [u32; 2],
    crop_rect: Option<[f64; 4]>,
    crop_drag: Option<crate::crop_interaction::CropDrag>,
    crop_session: Option<Uuid>,
    guide_drag: Option<GuideDrag>,
    guides_locked: bool,
    new_guide_horizontal: bool,
    new_guide_position: f64,
    recent: Vec<PathBuf>,
    drag: Option<Drag>,
    brush_stroke: Option<crate::brush::BrushStroke>,
    polygon: Vec<[f64; 2]>,
    polygon_options: Option<(SelectionMode, bool)>,
    selection_press: Option<egui::Modifiers>,
    layer_clipboard: Option<(Document, Vec<Uuid>)>,
    pixel_clipboard: Option<(Raster, [f64; 2])>,
    clipboard_sequence: u32,
    paste_key_handled: bool,
    visibility_sweep: Option<(bool, Document, HashSet<Uuid>)>,
    property_edit: Option<(Uuid, Document)>,
    text_box_drag: Option<TextBoxDrag>,
    text_apply_requested: bool,
    dialog: Option<Dialog>,
    dialog_value: Value,
    preview_key: Option<(Uuid, u64, String, Value)>,
    filter_ui: crate::filter_panel::FilterUiState,
    dialog_ready_source: Option<(Uuid, u64)>,
    dialog_texture: Option<egui::TextureHandle>,
    dialog_sample: Option<Arc<Raster>>,
    dialog_graded_sample: Option<Arc<Raster>>,
    dialog_display_sample: Option<Arc<Raster>>,
    raw_native_view: bool,
    raw_view_texture: Option<crate::filter_panel::CameraRawView>,
    dialog_source_cache: Arc<Mutex<crate::preview::PreviewCache>>,
    dialog_preview_busy: bool,
    dialog_generation: u64,
    dialog_preview_bytes: usize,
    dialog_preview_error: Option<String>,
    jpeg_quality: u8,
    jpeg_matte: [u8; 4],
    jpeg_zoom: f32,
    new_size: [u32; 2],
    welcome_clipboard_checked: bool,
    new_resolution: f64,
    size_units: String,
    size_relative: bool,
    size_delta: [i32; 2],
    size_original: [u32; 2],
    size_proportional: bool,
    resize_sampling: Sampling,
    canvas_anchor: [f64; 2],
    message: Option<String>,
    report: Vec<String>,
    pending_import: Option<(PathBuf, image_io::ImportResult, Option<Uuid>)>,
    import_queue: VecDeque<(PathBuf, image_io::ImportResult, Option<Uuid>)>,
    raw_queue: VecDeque<(PathBuf, Option<Uuid>)>,
    dialog_target: Option<(Uuid, Option<Uuid>)>,
    dialog_mask: bool,
    raw_settings: image_io::RawDevelopSettings,
    raw_texture: Option<egui::TextureHandle>,
    raw_busy: bool,
    raw_key: Option<(PathBuf, [u32; 4])>,
    raw_pointer_mode: String,
    raw_mixer_drag: Option<crate::raw_interaction::MixerDrag>,
    raw_curve_drag: Option<crate::raw_interaction::CurveDrag>,
    raw_guide_start: Option<[f64; 2]>,
    raw_point_selection: Option<usize>,
    close_pending: Option<Uuid>,
    exit_pending: bool,
    allow_exit: bool,
    window_motion: crate::window_motion::WindowMotion,
    external_pending: Option<(Uuid, u64)>,
    grid: bool,
    rulers: bool,
    pixel_grid: bool,
    snap: bool,
    snap_targets: [bool; 4],
    grid_spacing: f64,
    grid_color: [u8; 4],
    grid_subdivisions: u32,
    selection_amount_dialog: Option<selection_panel::SelectionAmountDialog>,
    floating_layers: bool,
    floating_tools: bool,
    shortcuts_open: bool,
    shortcuts: BTreeMap<String, String>,
    watcher: PackageWatcher,
    disk_changes: HashMap<Uuid, (WatchTarget, u64)>,
    qa_ready: Option<(PathBuf, Instant)>,
    restore_maximized_after_frame: Option<u64>,
    started: Instant,
    smoke: bool,
    smoke_done: bool,
    save_count: u32,
    watch_count: u32,
}
impl EditorApp {
    fn brush_line_start(&self, point: [f64; 2]) -> [f64; 2] {
        self.last_brush
            .filter(|(session, layer, mask, _)| {
                *session == self.session().id
                    && Some(*layer) == self.session().doc.active_layer_id
                    && *mask == self.mask_target
            })
            .map_or(point, |(_, _, _, last)| last)
    }
    fn pick_move_target(&mut self, point: [f64; 2], ctrl: bool, shift: bool) {
        if self.mask_target || self.transform_auto_select == ctrl {
            return;
        }
        if transform_frame(&self.session().doc, &self.session().selected, false).is_some_and(
            |frame| {
                let uv = geometric_transform(frame).from_document(point);
                (0. ..=1.).contains(&uv[0]) && (0. ..=1.).contains(&uv[1])
            },
        ) {
            return;
        }
        if let Some(id) = hit_layer(&self.session().doc, point) {
            let session = self.session_mut();
            session.doc.active_layer_id = Some(id);
            if shift {
                session.selected.insert(id);
            } else {
                session.selected = HashSet::from([id]);
            }
        }
    }
    fn drag_is_current(&self, drag: &Drag) -> bool {
        drag.target.is_none_or(|(session, revision)| {
            session == self.session().id && revision == self.session().history.revision()
        })
    }
    fn cancel_drag(&mut self, drag: Drag) {
        if (drag.mode == Tool::Move || drag.mode.paints() || drag.outline_move)
            && self.drag_is_current(&drag)
        {
            self.session_mut().doc = drag.before;
            self.session_mut().changed();
        }
    }
    fn settle_gestures(&mut self) {
        self.polygon.clear();
        self.polygon_options = None;
        self.selection_press = None;
        self.last_brush = None;
        self.clone_source = None;
        self.clone_offset = None;
        if let Some(drag) = self.drag.take() {
            self.cancel_drag(drag);
        }
        if let Some(drag) = self.guide_drag.take() {
            self.session_mut().doc = drag.before;
            self.session_mut().changed();
        }
        self.brush_stroke = None;
        if let Some((_, before, _)) = self.visibility_sweep.take() {
            let session = self.session_mut();
            if session
                .history
                .record("Layer visibility", before, &session.doc)
            {
                session.changed();
            }
        }
        if let Some((id, before)) = self.property_edit.take()
            && let Some(session) = self.tabs.iter_mut().find(|s| s.id == id)
            && session
                .history
                .record("Layer properties", before, &session.doc)
        {
            session.changed();
        }
        let session = self.session_mut();
        session
            .selected
            .retain(|id| session.doc.layer(*id).is_some());
        self.gradient = None;
        self.gradient_handle = None;
        self.crop_drag = None;
        self.text_box_drag = None;
        self.sample_drag = None;
        self.sample_cache = None;
        self.sample_busy = None;
        self.sample_point = None;
        self.warp_generation = self.warp_generation.wrapping_add(1);
        self.warp_preview = None;
        self.warp_preview_busy = false;
        self.warp_key = None;
    }
    fn activate_tab(&mut self, index: usize) {
        if index == self.current {
            return;
        }
        self.settle_gestures();
        self.current = index;
        self.tabs[index].rendered = 0;
        self.tabs[index].gpu_id = None;
        self.tabs[index].preview_failed = None;
    }
    fn finish_color_picker(&mut self, accept: bool) {
        if self.text_color_target.is_some() {
            self.finish_text_color_picker(accept);
            return;
        }
        if let Some(picker) = self.color_picker.take() {
            let color = if accept {
                picker.color
            } else {
                picker.original
            };
            if picker.background {
                self.background = color;
            } else {
                self.foreground = color;
            }
        }
        self.sample_drag = None;
        self.sample_point = None;
    }

    fn apply_sampled_color(&mut self, mut color: [u8; 4]) {
        if color[3] == 0 {
            return;
        }
        color[3] = self
            .color_picker
            .as_ref()
            .map_or(self.foreground[3], |picker| picker.color[3]);
        if self.text_color_target.is_some() {
            if let Some(picker) = &mut self.color_picker {
                picker.sample(color);
            }
            self.preview_text_color_picker();
            return;
        }
        if let Some(picker) = &mut self.color_picker {
            picker.sample(color);
            if picker.background {
                self.background = color;
            } else {
                self.foreground = color;
            }
        } else {
            self.foreground = color;
        }
    }

    fn show_color_picker(&mut self, ctx: &egui::Context) {
        self.open_text_color_request(ctx);
        if self.text_color_target.is_some() {
            self.preview_text_color_picker();
        }
        let text_target = self.text_color_target.is_some();
        if let Some(picker) = &mut self.color_picker {
            let finish = if text_target {
                picker.show_with_title(ctx, "文字颜色")
            } else {
                picker.show(ctx)
            };
            if !text_target {
                if picker.background {
                    self.background = picker.color;
                } else {
                    self.foreground = picker.color;
                }
            }
            if let Some(accept) = finish {
                self.finish_color_picker(accept);
            } else if text_target {
                self.preview_text_color_picker();
            }
        }
    }

    fn update_warp_preview(&mut self) {
        let Some(drag) = self.drag.as_ref().filter(|drag| drag.distort) else {
            if self.warp_preview.is_some() || self.warp_preview_busy || self.warp_key.is_some() {
                self.warp_generation = self.warp_generation.wrapping_add(1);
                self.warp_preview = None;
                self.warp_preview_busy = false;
                self.warp_key = None;
                self.session_mut().changed();
            }
            return;
        };
        let mask = self.mask_target;
        let Some((id, corners)) = distort_corners(drag, mask) else {
            return;
        };
        let key = (
            self.session().id,
            corners.map(|p| p.map(f64::to_bits)),
            mask,
        );
        if self.warp_preview_busy || self.warp_key.as_ref() == Some(&key) {
            return;
        }
        let mut doc = drag.work_before.clone();
        let scale = (1000. / doc.width.max(doc.height) as f64).min(1.);
        self.warp_key = Some(key);
        self.warp_preview_busy = true;
        self.warp_generation = self.warp_generation.wrapping_add(1);
        let generation = self.warp_generation;
        let session = self.session().id;
        let sender = self.sender.clone();
        if let Err(error) = gui_threads::spawn("变形预览", move || {
            let result = gui_threads::run("变形预览", || {
                (|| -> anyhow::Result<Layer> {
                    doc = crate::preview::prepare(&doc, 1000);
                    doc.width = (doc.width as f64 * scale).round().max(1.) as u32;
                    doc.height = (doc.height as f64 * scale).round().max(1.) as u32;
                    doc.selection = None;
                    for layer in &mut doc.layers {
                        layer.transform.origin = layer.transform.origin.map(|v| v * scale);
                        layer.transform.size = layer.transform.size.map(|v| v * scale);
                        if let Some(mask) = &mut layer.mask
                            && let Some(transform) = &mut mask.placement
                        {
                            transform.origin = transform.origin.map(|v| v * scale);
                            transform.size = transform.size.map(|v| v * scale);
                        }
                    }
                    crate::edit::free_distort(
                        &mut doc,
                        id,
                        corners.map(|p| p.map(|v| v * scale)),
                        mask,
                    )?;
                    let mut layer = doc.layer(id).unwrap().clone();
                    layer.transform.origin = layer.transform.origin.map(|v| v / scale);
                    layer.transform.size = layer.transform.size.map(|v| v / scale);
                    if let Some(mask) = &mut layer.mask
                        && let Some(transform) = &mut mask.placement
                    {
                        transform.origin = transform.origin.map(|v| v / scale);
                        transform.size = transform.size.map(|v| v / scale);
                    }
                    Ok(layer)
                })()
                .map_err(|e| format!("{e:#}"))
            });
            let _ = sender.send(Worker::WarpPreview {
                session,
                generation,
                result,
            });
        }) {
            self.warp_preview_busy = false;
            self.message = Some(error);
        }
    }
    fn check_workspace(&self, candidate: &Document) -> anyhow::Result<()> {
        self.check_workspace_documents(std::iter::once(candidate), self.retained_sampler_bytes())
    }
    fn retained_sampler_bytes(&self) -> u64 {
        self.sample_cache
            .as_ref()
            .map_or(0, |(_, sampler)| sampler.allocated_bytes() as u64)
    }
    fn check_workspace_documents<'a>(
        &'a self,
        candidates: impl IntoIterator<Item = &'a Document>,
        sampler_bytes: u64,
    ) -> anyhow::Result<()> {
        let histories: u64 = self
            .tabs
            .iter()
            .map(|s| s.history.retained_bytes(&s.doc) as u64)
            .fold(0, u64::saturating_add);
        crate::workspace_limits::ensure_workspace_budget(
            self.tabs
                .iter()
                .map(|s| &s.doc)
                .chain(
                    self.pending_import
                        .iter()
                        .map(|(_, import, _)| &import.document),
                )
                .chain(
                    self.import_queue
                        .iter()
                        .map(|(_, import, _)| &import.document),
                )
                .chain(candidates),
            histories.saturating_add(sampler_bytes),
        )
        .map(|_| ())
    }
    fn operation_busy(&self) -> bool {
        self.pending_io > 0
            || self.raw_busy
            || self.dialog_preview_busy
            || self.sample_busy.is_some()
            || self
                .tabs
                .iter()
                .any(|s| s.save_busy || s.edit_busy || s.reload_busy || s.preview_busy)
    }
    fn cancel_dialog(&mut self) {
        if self.text_color_target.is_some() {
            self.finish_color_picker(false);
        }
        self.filter_ui = crate::filter_panel::FilterUiState::default();
        self.dialog_ready_source = None;
        self.selection_amount_dialog = None;
        self.dialog = None;
        self.dialog_target = None;
        self.dialog_sample = None;
        self.dialog_graded_sample = None;
        self.dialog_display_sample = None;
        self.raw_native_view = false;
        self.raw_view_texture = None;
        self.dialog_source_cache = Arc::new(Mutex::new(crate::preview::PreviewCache::default()));
        self.preview_key = None;
        self.dialog_generation = self.dialog_generation.wrapping_add(1);
        self.dialog_preview_busy = false;
        self.dialog_texture = None;
        self.dialog_preview_error = None;
        self.raw_mixer_drag = None;
        self.raw_curve_drag = None;
        self.raw_guide_start = None;
        self.raw_point_selection = None;
        self.text_box_drag = None;
        self.session_mut().rendered = 0;
    }
    fn dialog_destination(&self, dialog: &Dialog) -> (Uuid, Option<Uuid>) {
        let layer = match dialog {
            Dialog::Text { layer, .. } => *layer,
            Dialog::Filter { .. }
            | Dialog::Effect { .. }
            | Dialog::Adjustment { .. }
            | Dialog::CameraRaw => self.session().doc.active_layer_id,
            _ => None,
        };
        (self.session().id, layer)
    }
    fn begin_dialog(&mut self, dialog: Dialog) {
        if self.is_welcome() && !matches!(dialog, Dialog::Raw { target: None, .. }) {
            return;
        }
        self.cancel_dialog();
        self.dialog_target = Some(self.dialog_destination(&dialog));
        self.dialog_mask = self.mask_target;
        self.dialog = Some(dialog);
    }
    fn set_tool(&mut self, tool: Tool) {
        if self.tool == tool {
            return;
        }
        if let Some(index) = self.tool.paint_index() {
            self.brush_presets[index] =
                Some([self.diameter, self.hardness, self.opacity, self.smoothing]);
        }
        if let Some(index) = tool.paint_index() {
            let preset = self.brush_presets[index].unwrap_or([40.0, 0.8, 1.0, 0.0]);
            [self.diameter, self.hardness, self.opacity, self.smoothing] = preset;
        }
        if tool.rail_group() < 5 {
            self.tool_modes[tool.rail_group()] = tool;
        }
        self.tool = tool;
        self.crop_drag = None;
        self.crop_session = None;
        self.crop_rect = None;
        if tool == Tool::Crop {
            self.crop_ratio = [0, 0];
            self.initialize_crop();
        }
        self.polygon.clear();
        self.polygon_options = None;
        self.selection_press = None;
    }
    fn initialize_crop(&mut self) {
        let doc = &self.session().doc;
        self.crop_rect = Some(
            doc.selection
                .as_ref()
                .and_then(|s| s.bounds())
                .map(|[x0, y0, x1, y1]| [x0 as f64, y0 as f64, (x1 - x0) as f64, (y1 - y0) as f64])
                .unwrap_or([0., 0., doc.width as f64, doc.height as f64]),
        );
        self.crop_drag = None;
        self.crop_session = Some(self.session().id);
        self.change_crop_ratio();
    }
    fn crop_aspect_ratio(&self) -> Option<f64> {
        match self.crop_ratio {
            [1, 0] => {
                let doc = &self.session().doc;
                Some(doc.width as f64 / doc.height as f64)
            }
            [width, height] if width > 0 && height > 0 => Some(width as f64 / height as f64),
            _ => None,
        }
    }
    fn change_crop_ratio(&mut self) {
        self.crop_drag = None;
        let Some(ratio) = self.crop_aspect_ratio() else {
            return;
        };
        let doc = &self.session().doc;
        let frame = self
            .crop_rect
            .unwrap_or([0., 0., doc.width as f64, doc.height as f64]);
        let height = frame[2] / ratio;
        let next = crate::crop_interaction::snapped([
            frame[0],
            frame[1] + (frame[3] - height) / 2.,
            frame[2],
            height,
        ]);
        if crate::crop_interaction::valid(next) {
            self.crop_rect = Some(next);
        }
    }
    fn cycle_tool_mode(&mut self) {
        if self.drag.is_some() || self.session().edit_busy {
            return;
        }
        let next = match self.tool {
            Tool::Rectangle => Tool::Ellipse,
            Tool::Ellipse => Tool::Rectangle,
            Tool::Lasso => Tool::Polygon,
            Tool::Polygon => Tool::Lasso,
            Tool::Wand => Tool::Object,
            Tool::Object => Tool::Wand,
            Tool::Brush => Tool::Eraser,
            Tool::Eraser => Tool::Brush,
            Tool::Blur => Tool::Smudge,
            Tool::Smudge => Tool::Liquify,
            Tool::Liquify => Tool::Blur,
            Tool::Heal => {
                self.healing_mode = (self.healing_mode + 1) % 3;
                return;
            }
            Tool::Clone => {
                self.clone_all = !self.clone_all;
                return;
            }
            Tool::Shape => {
                self.shape_kind = match self.shape_kind.as_str() {
                    "Rectangle" => "Ellipse",
                    "Ellipse" => "Line",
                    _ => "Rectangle",
                }
                .into();
                return;
            }
            Tool::Gradient => {
                self.gradient_kind = match self.gradient_kind.as_str() {
                    "Linear" => "Radial",
                    "Radial" => "Angle",
                    "Angle" => "Reflected",
                    "Reflected" => "Diamond",
                    _ => "Linear",
                }
                .into();
                return;
            }
            _ => return,
        };
        self.set_tool(next);
    }
    fn gradient_parameters(&self) -> (String, [u8; 4], [u8; 4], f64) {
        let mut from = self.palette_color(false);
        let mut to = if self.gradient_transparent {
            [from[0], from[1], from[2], 0]
        } else {
            self.palette_color(true)
        };
        if self.gradient_reverse {
            std::mem::swap(&mut from, &mut to);
        }
        (self.gradient_kind.clone(), from, to, self.opacity)
    }
    fn display_document(&self) -> Document {
        let mut doc = self.session().doc.clone();
        if let Some((session, layer)) = &self.warp_preview
            && *session == self.session().id
            && let Some(target) = doc.layer_mut(layer.id)
        {
            *target = layer.clone();
        }
        if let Some(Dialog::Text {
            layer: Some(id), ..
        }) = &self.dialog
            && self
                .dialog_target
                .is_some_and(|(target, _)| target == self.session().id)
            && let Some(layer) = doc.layer_mut(*id)
        {
            layer.visible = false;
        }
        if self.mask_alone
            && let Some(layer) = doc.active().cloned()
            && let Some(mask) = layer.mask
        {
            let mut display = Layer::raster("Mask", mask.raster.as_ref().clone());
            display.id = layer.id;
            display.transform = mask.placement.unwrap_or(layer.transform);
            doc.layers = vec![display];
        }
        doc
    }
    fn nudge(&mut self, delta: [f64; 2], outline: bool) {
        if outline {
            self.session_mut().edit("Move selection outline", |doc| {
                if let Some(selection) = &doc.selection {
                    doc.selection = Some(selection::translated(
                        selection,
                        delta[0] as i32,
                        delta[1] as i32,
                        doc.width,
                        doc.height,
                    ));
                }
            });
        } else if self.mask_target {
            self.apply("Move mask", |doc| {
                if let Some(id) = doc.active_layer_id
                    && let Some(layer) = doc.layer(id)
                {
                    let mut transform = layer
                        .mask
                        .as_ref()
                        .and_then(|m| m.placement)
                        .unwrap_or(layer.transform);
                    transform.origin[0] += delta[0];
                    transform.origin[1] += delta[1];
                    crate::edit::set_transform(doc, id, transform, true)?;
                }
                Ok(())
            });
        } else if self
            .session()
            .doc
            .selection
            .as_ref()
            .is_some_and(|s| !s.is_empty())
        {
            if let Some(id) = self.session().doc.active_layer_id {
                self.apply("Move selected pixels", |doc| {
                    crate::edit::move_selected_pixels(doc, id, false, delta, false)
                });
            }
        } else {
            let ids = self.session().selected.iter().copied().collect::<Vec<_>>();
            self.apply("Nudge", |doc| crate::edit::move_layers(doc, &ids, delta));
        }
    }
    fn update_gradient(&mut self, mut state: GradientState) {
        if state.session != self.session().id || self.session().edit_busy {
            return;
        }
        let revision = self.session().history.revision();
        if state.revision.unwrap_or(state.base_revision) != revision {
            return;
        }
        let serial = self.session().serial;
        let sender = self.sender.clone();
        state.parameters = self.gradient_parameters();
        state.request = Uuid::new_v4();
        self.session_mut().edit_busy = true;
        self.session_mut().gradient_task = Some((state.request, false));
        self.gradient = Some(state.clone());
        if let Err(error) = gui_threads::spawn("渐变编辑", move || {
            let result = gui_threads::run("渐变编辑", || {
                let mut doc = state.before.clone();
                let (kind, from, to, opacity) = &state.parameters;
                let shape = match kind.as_str() {
                    "Radial" => crate::brush::GradientShape::Radial,
                    "Angle" => crate::brush::GradientShape::Angle,
                    "Reflected" => crate::brush::GradientShape::Reflected,
                    "Diamond" => crate::brush::GradientShape::Diamond,
                    _ => crate::brush::GradientShape::Linear,
                };
                crate::brush::gradient(
                    &mut doc,
                    state.layer,
                    state.mask,
                    state.start,
                    state.end,
                    *from,
                    *to,
                    shape,
                    *opacity,
                )
                .map(|_| doc)
                .map_err(|e| format!("{e:#}"))
            });
            let _ = sender.send(Worker::Gradient {
                revision,
                serial,
                state: Box::new(state),
                result,
            });
        }) {
            self.session_mut().edit_busy = false;
            self.session_mut().gradient_task = None;
            self.apply_gradient();
            self.message = Some(error);
        }
    }
    fn specialized_filter_dialog(&self) -> bool {
        matches!(&self.dialog, Some(Dialog::Filter { kind } | Dialog::Adjustment { kind })
            if matches!(kind.as_str(), "Levels" | "Hue/Saturation"))
    }

    /// Existing native sampler preparation runs once for the current document and retains
    /// correct spatial/global-effect fallback. No full image is built on every click.
    fn prepare_filter_sampler(&mut self) {
        if !self.specialized_filter_dialog()
            || !matches!(&self.dialog,Some(Dialog::Filter {kind}|Dialog::Adjustment {kind}) if kind=="Hue/Saturation")
            || self
                .dialog_target
                .is_none_or(|(target, _)| target != self.session().id)
        {
            return;
        }
        if self.sample_cache.as_ref().is_some_and(|(id, sampler)| {
            *id == self.session().id && sampler.matches(&self.session().doc)
        }) {
            return;
        }
        let key = (self.session().id, self.session().serial);
        if self.sample_busy == Some(key) {
            return;
        }
        self.sample_cache = None;
        self.sample_point = None;
        self.sample_busy = Some(key);
        self.start_color_sampler(key);
    }

    fn start_color_sampler(&mut self, key: (Uuid, u64)) {
        let doc = self.session().doc.clone();
        let sender = self.sender.clone();
        match gui_threads::spawn("颜色取样", move || {
            let result = gui_threads::run("颜色取样", || {
                Ok(Arc::new(render::CompositeSampler::new(&doc)))
            });
            let event = match result {
                Ok(sampler) => Worker::ColorSample {
                    session: key.0,
                    serial: key.1,
                    sampler,
                },
                Err(error) => Worker::ColorSampleFailure {
                    session: key.0,
                    serial: key.1,
                    error,
                },
            };
            let _ = sender.send(event);
        }) {
            Ok(_) => self.sample_busy = Some(key),
            Err(error) => {
                self.sample_busy = None;
                self.message = Some(error);
            }
        }
    }

    fn filter_canvas_preview_texture(&self) -> Option<egui::TextureId> {
        if !self.specialized_filter_dialog()
            || !self.filter_ui.preview
            || self
                .dialog_target
                .is_none_or(|(target, _)| target != self.session().id)
        {
            return None;
        }
        let (id, serial) = self.dialog_ready_source?;
        if id != self.session().id || serial != self.session().serial {
            return None;
        }
        self.dialog_texture.as_ref().map(|texture| texture.id())
    }

    /// `rect` displays the whole document, either the canvas rect or its scaled preview.
    /// Returns true while this modal edit owns the canvas, suppressing brush/selection
    /// handlers even before an eyedropper is armed. Pan/zoom remains in the caller.
    fn filter_canvas_gesture(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        rect: Rect,
    ) -> bool {
        use crate::filter_panel::FilterPointer;
        if !self.specialized_filter_dialog()
            || self
                .dialog_target
                .is_none_or(|(target, _)| target != self.session().id)
        {
            return false;
        }
        if ui.input(|i| i.key_down(Key::Space)) || self.tool == Tool::Hand {
            if response.dragged() {
                self.session_mut().pan += response.drag_delta();
            }
            if ui.input(|i| !i.pointer.primary_down()) {
                self.filter_ui.pointer_end();
            }
            return true;
        }
        let Some(mode) = self.filter_ui.pointer else {
            return true;
        };
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        let Some(position) = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|i| i.pointer.latest_pos()))
        else {
            if ui.input(|i| !i.pointer.primary_down()) {
                self.filter_ui.pointer_end();
            }
            return true;
        };
        let start = response.drag_started() || response.clicked();
        if start {
            let position = if response.drag_started() {
                ui.input(|i| i.pointer.press_origin()).unwrap_or(position)
            } else {
                position
            };
            if rect.contains(position) {
                let docpoint = [
                    (position.x - rect.left()) as f64 / rect.width().max(1.) as f64
                        * self.session().doc.width as f64,
                    (position.y - rect.top()) as f64 / rect.height().max(1.) as f64
                        * self.session().doc.height as f64,
                ];
                if let FilterPointer::Levels(_) = mode {
                    if let Some((_, layer)) = self.dialog_target
                        && let Some(layer) = layer.and_then(|id| self.session().doc.layer(id))
                    {
                        let source = if self.dialog_mask {
                            layer.mask.as_ref().map(|mask| mask.raster.clone())
                        } else {
                            layer.image.clone()
                        };
                        let transform = if self.dialog_mask {
                            layer
                                .mask
                                .as_ref()
                                .and_then(|mask| mask.placement)
                                .unwrap_or(layer.transform)
                        } else {
                            layer.transform
                        };
                        if let Some(source) = source {
                            let unit = transform.from_document(docpoint);
                            let pixel = [
                                unit[0] * source.width as f64,
                                unit[1] * source.height as f64,
                            ];
                            self.filter_ui.pointer_start(
                                &mut self.dialog_value,
                                &source,
                                pixel,
                                position.x as f64,
                            );
                        }
                    }
                } else if let Some(color) = self
                    .sample_cache
                    .as_ref()
                    .filter(|(id, sampler)| {
                        *id == self.session().id && sampler.matches(&self.session().doc)
                    })
                    .and_then(|(_, sampler)| sampler.sample(docpoint))
                    .and_then(crate::raw_interaction::ColorSample::from_rgba)
                {
                    self.filter_ui.pointer_color_start(
                        &mut self.dialog_value,
                        color,
                        position.x as f64,
                    );
                }
            }
        }
        if response.dragged() || response.drag_stopped() {
            self.filter_ui.pointer_drag(
                &mut self.dialog_value,
                position.x as f64,
                ui.input(|i| i.modifiers.command),
            );
        }
        if response.drag_stopped() || ui.input(|i| !i.pointer.primary_down()) {
            self.filter_ui.pointer_end();
        }
        true
    }

    fn dialog_preview(&mut self, ctx: &egui::Context, dialog: &Dialog) {
        self.prepare_filter_sampler();
        if self.dialog_preview_busy || (self.specialized_filter_dialog() && !self.filter_ui.preview)
        {
            return;
        }
        let target = self.dialog_target.map_or(self.current, |(id, _)| {
            self.tabs
                .iter()
                .position(|s| s.id == id)
                .unwrap_or(self.current)
        });
        let session = &self.tabs[target];
        let mut value = self.dialog_value.clone();
        let kind = match dialog {
            Dialog::Jpeg => {
                value = json!({"quality":self.jpeg_quality,"matte":self.jpeg_matte});
                "jpeg".to_owned()
            }
            Dialog::Filter { kind } if kind == "Remove Background" => "background".into(),
            Dialog::Filter { kind } => format!("filter:{kind}"),
            Dialog::Adjustment { kind } => format!("adjustment:{kind}"),
            Dialog::Effect { kind } => format!("effect:{kind}"),
            Dialog::CameraRaw => "filter:Camera Raw Filter".to_owned(),
            Dialog::ColorRange => "color-range".to_owned(),
            _ => return,
        };
        // Camera Raw previews always use the original raster so the dialog
        // never replaces an image with a fixed-size thumbnail.
        let key = (session.id, session.serial, kind.clone(), value.clone());
        if self.preview_key.as_ref() == Some(&key) {
            return;
        }
        let mut preview_document = session.doc.clone();
        if let Some((_, layer)) = self.dialog_target {
            preview_document.active_layer_id = layer;
        }
        self.preview_key = Some(key);
        self.dialog_generation = self.dialog_generation.wrapping_add(1);
        self.dialog_preview_busy = true;
        let generation = self.dialog_generation;
        let doc = preview_document;
        if kind == "background" {
            let result = doc
                .active_layer_id
                .ok_or_else(|| "Select a pixel layer".to_owned())
                .and_then(|layer| {
                    self.segment_sender
                        .send(SegmentRequest {
                            session: self.tabs[target].id,
                            before: doc,
                            revision: self.tabs[target].history.revision(),
                            serial: self.tabs[target].serial,
                            operation: SegmentOperation::Preview {
                                layer,
                                settings: value,
                                generation,
                            },
                        })
                        .map_err(|e| e.to_string())
                });
            if let Err(e) = result {
                self.dialog_preview_busy = false;
                self.dialog_preview_error = Some(e);
            }
            return;
        }
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        let mask = self.dialog_mask;
        let source_cache = self.dialog_source_cache.clone();
        if let Err(error) = gui_threads::spawn("滤镜预览", move || {
            let mut sample = None;
            let mut graded_sample = None;
            let mut display_sample = None;
            let result = gui_threads::run("滤镜预览", || {
                let result = (|| -> anyhow::Result<(Raster, usize)> {
                    let mut cache = source_cache
                        .lock()
                        .map_err(|_| anyhow::anyhow!("预览源缓存不可用"))?;
                    let prepared = if kind == "filter:Camera Raw Filter" {
                        doc.clone()
                    } else {
                        cache.prepare(&doc, 1000)
                    };
                    sample = prepared
                        .active()
                        .and_then(|l| l.image.clone())
                        .or_else(|| Some(cache.render(&doc, 1000)));
                    if kind == "color-range" {
                        let source = cache.render(&doc, 1000);
                        sample = Some(source.clone());
                        let colors = color_range_colors(&value);
                        let fuzziness =
                            crate::filters::number(&value, "fuzziness", 40.).clamp(0., 255.) as u8;
                        let selected = selection::color_range(&source, &colors, fuzziness);
                        let mut rgba = source.to_rgba();
                        for (p, &amount) in rgba
                            .as_chunks_mut::<4>()
                            .0
                            .iter_mut()
                            .zip(selected.canvas_coverage().iter())
                        {
                            let a = amount as u32;
                            p[0] = ((p[0] as u32 * (255 - a) + 255 * a) / 255) as u8;
                            p[1] = (p[1] as u32 * (255 - a) / 255) as u8;
                            p[2] = ((p[2] as u32 * (255 - a) + 220 * a) / 255) as u8;
                        }
                        return Ok((Raster::from_rgba(source.width, source.height, rgba), 0));
                    }
                    if kind == "jpeg" {
                        let original = cache.render(&doc, 1200);
                        let quality = value["quality"].as_u64().unwrap_or(90) as u8;
                        let matte = std::array::from_fn(|i| {
                            value["matte"][i].as_u64().unwrap_or(255) as u8
                        });
                        return image_io::jpeg_preview(&original, quality, matte);
                    }
                    let mut preview = doc.clone();
                    if let Some(kind) = kind.strip_prefix("filter:") {
                        preview = prepared;
                        preview.selection = doc.selection.clone();
                        let layer = preview
                            .active()
                            .ok_or_else(|| anyhow::anyhow!("Select a pixel layer"))?;
                        let reduced = if mask {
                            layer.mask.as_ref().map(|m| m.raster.clone())
                        } else {
                            layer.image.clone()
                        };
                        sample = reduced.clone();
                        let full = doc.active().and_then(|l| {
                            if mask {
                                l.mask.as_ref().map(|m| m.raster.clone())
                            } else {
                                l.image.clone()
                            }
                        });
                        if let (Some(reduced), Some(full)) = (&reduced, &full) {
                            crate::preview::scale_filter(
                                &mut value,
                                kind,
                                reduced.width as f64 / full.width as f64,
                            );
                        } else if kind == "Vignette" && !mask {
                            value["previewScale"] = json!(
                                (1000.
                                    / (doc.width.max(doc.height) as f64)
                                        .max(layer.transform.size[0])
                                        .max(layer.transform.size[1]))
                                .min(1.)
                            );
                        }
                        let id = layer.id;
                        let mut reuse_grade = false;
                        if kind == "Camera Raw Filter" {
                            value = crate::filter_panel::effective_camera_raw_settings(&value);
                            if let Some(source) = &reduced {
                                let scale = full
                                    .as_ref()
                                    .map_or(1., |full| source.width as f64 / full.width as f64);
                                let mut grade = committed_filter_settings(kind, value.clone());
                                crate::preview::scale_filter(&mut grade, kind, scale);
                                reuse_grade = doc.selection.is_none()
                                    && camera_raw_preview_is_clean(&value, &grade);
                                if !reuse_grade {
                                    graded_sample = Some(Arc::new(
                                        crate::filters::apply_filter(source, kind, &grade)
                                            .map_err(anyhow::Error::msg)?,
                                    ));
                                }
                            }
                        }
                        crate::filter_edit::apply(&mut preview, id, mask, kind, &value)?;
                        if kind == "Camera Raw Filter" {
                            display_sample = preview.active().and_then(|layer| {
                                if mask {
                                    layer.mask.as_ref().map(|mask| mask.raster.clone())
                                } else {
                                    layer.image.clone()
                                }
                            });
                        }
                        if reuse_grade {
                            graded_sample = display_sample.clone();
                        }
                    } else if let Some(kind) = kind.strip_prefix("adjustment:") {
                        let mut layer = Layer::new(kind, doc.width, doc.height);
                        layer.adjustment = Some(value);
                        let at = preview
                            .active_layer_id
                            .and_then(|id| preview.layers.iter().position(|l| l.id == id))
                            .map_or(preview.layers.len(), |i| i + 1);
                        preview.layers.insert(at, layer);
                    } else if let Some(kind) = kind.strip_prefix("effect:")
                        && let Some(layer) = preview.active_mut()
                    {
                        let mut effects = layer.effects.take().unwrap_or_else(|| json!({}));
                        effects[kind] = value;
                        layer.effects = Some(effects);
                    }
                    Ok((
                        crate::preview::PreviewCache::default()
                            .render(&preview, 1000)
                            .as_ref()
                            .clone(),
                        0,
                    ))
                })()
                .map_err(|e| format!("{e:#}"));
                if let Some(sample) = &graded_sample {
                    crate::filter_panel::prepare_camera_raw_scopes(&ctx, sample);
                }
                result
            });
            let _ = sender.send(Worker::DialogPreview {
                generation,
                sample,
                graded_sample,
                display_sample,
                result,
            });
            ctx.request_repaint();
        }) {
            self.dialog_preview_busy = false;
            self.dialog_preview_error = Some(error.clone());
            self.message = Some(error);
        }
    }
    fn raw_pointer_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("raw-preview-tool")
                .selected_text(tr(&self.raw_pointer_mode))
                .show_ui(ui, |ui| {
                    for mode in [
                        "Sample",
                        "White Balance",
                        "Defringe",
                        "Curve Parametric",
                        "Curve RGB",
                        "Curve Red",
                        "Curve Green",
                        "Curve Blue",
                        "Mixer Hue",
                        "Mixer Saturation",
                        "Mixer Luminance",
                        "Point Color",
                        "Upright Guide",
                    ] {
                        ui.selectable_value(&mut self.raw_pointer_mode, mode.into(), tr(mode));
                    }
                });
            if self.raw_pointer_mode == "Point Color" {
                let raw = if self.dialog_value.get("cameraRaw").is_some() {
                    &self.dialog_value["cameraRaw"]
                } else {
                    &self.dialog_value
                };
                let count = raw["mixer"]["points"].as_array().map_or(0, Vec::len);
                egui::ComboBox::from_id_salt("raw-point-selection")
                    .selected_text(
                        self.raw_point_selection
                            .map_or_else(|| "新建取样".into(), |i| format!("重新取样 {}", i + 1)),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.raw_point_selection, None, "新建取样");
                        for index in 0..count {
                            ui.selectable_value(
                                &mut self.raw_point_selection,
                                Some(index),
                                format!("重新取样 {}", index + 1),
                            );
                        }
                    });
            }
            if self.raw_pointer_mode == "Upright Guide" && ui.button("清除参考线").clicked() {
                let raw = if self.dialog_value.get("cameraRaw").is_some() {
                    &mut self.dialog_value["cameraRaw"]
                } else {
                    &mut self.dialog_value
                };
                raw["geometry"]["guides"] = json!([]);
                raw["geometry"]["upright"] = json!("Off");
            }
        });
        ui.weak(match self.raw_pointer_mode.as_str() {
            "Point Color" => "单击预览取样颜色。",
            "White Balance" => "单击中性区域设置白平衡。",
            "Defringe" => "单击紫色或绿色边缘设置色相范围。",
            "Upright Guide" => "沿预览中的水平或垂直边缘拖动。",
            "Sample" => "单击预览查看原始颜色。",
            mode if mode.starts_with("Curve ") => "在色调上垂直拖动调整曲线。",
            _ => "在颜色上垂直拖动调整混色器通道。",
        });
    }
    fn raw_preview_gesture(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        source: &Raster,
    ) {
        use crate::raw_interaction::{self, CurveDrag, CurveTarget, MixerChannel, MixerDrag};
        let rect = response.rect;
        let Some((session, layer)) = self.dialog_target else {
            return;
        };
        let Some(doc) = self.tabs.iter().find(|s| s.id == session).map(|s| &s.doc) else {
            return;
        };
        let Some(layer) = layer.and_then(|id| doc.layer(id)) else {
            return;
        };
        let transform = if self.dialog_mask {
            layer
                .mask
                .as_ref()
                .and_then(|m| m.placement)
                .unwrap_or(layer.transform)
        } else {
            layer.transform
        };
        let canvas_size = [doc.width as f64, doc.height as f64];
        let native_view = self.raw_native_view;
        let sample_point = |position: Pos2| {
            if native_view {
                return [
                    (position.x - rect.left()) as f64 / rect.width() as f64 * source.width as f64,
                    (position.y - rect.top()) as f64 / rect.height() as f64 * source.height as f64,
                ];
            }
            let point = [
                (position.x - rect.left()) as f64 / rect.width() as f64 * canvas_size[0],
                (position.y - rect.top()) as f64 / rect.height() as f64 * canvas_size[1],
            ];
            let unit = transform.from_document(point);
            [
                unit[0] * source.width as f64,
                unit[1] * source.height as f64,
            ]
        };
        let graded = self.dialog_graded_sample.clone();
        let displayed = graded.as_deref().unwrap_or(source);
        let position = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|i| i.pointer.latest_pos()));
        if let Some(position) = position {
            let point = sample_point(position);
            if response.clicked()
                && let Some(sample) = raw_interaction::sample_pixel(
                    if matches!(self.raw_pointer_mode.as_str(), "White Balance" | "Defringe") {
                        source
                    } else {
                        displayed
                    },
                    point,
                )
            {
                if self.raw_pointer_mode == "Point Color" {
                    self.raw_point_selection = raw_interaction::pick_point_color(
                        &mut self.dialog_value,
                        sample,
                        self.raw_point_selection,
                    );
                } else if self.raw_pointer_mode == "Sample" {
                    self.message = Some(format!(
                        "RGB {}, {}, {} · 色相 {:.1}°",
                        sample.rgb[0], sample.rgb[1], sample.rgb[2], sample.hue
                    ));
                } else if self.raw_pointer_mode == "White Balance" {
                    raw_interaction::pick_white_balance(&mut self.dialog_value, sample);
                } else if self.raw_pointer_mode == "Defringe" {
                    raw_interaction::pick_defringe(&mut self.dialog_value, sample);
                }
            }
            if response.drag_started() {
                let start = ui.input(|i| i.pointer.press_origin()).unwrap_or(position);
                let sampled_point = sample_point(start);
                if self.raw_pointer_mode.starts_with("Mixer ")
                    && let Some(sample) = raw_interaction::sample_pixel(displayed, sampled_point)
                {
                    self.raw_mixer_drag = Some(MixerDrag {
                        baseline: self.dialog_value.clone(),
                        hue: sample.hue,
                        start_y: start.y as f64,
                        channel: match self.raw_pointer_mode.as_str() {
                            "Mixer Hue" => MixerChannel::Hue,
                            "Mixer Saturation" => MixerChannel::Saturation,
                            _ => MixerChannel::Luminance,
                        },
                    });
                }
                if self.raw_pointer_mode.starts_with("Curve ")
                    && let Some(sample) = raw_interaction::sample_pixel(displayed, sampled_point)
                {
                    self.raw_curve_drag = Some(CurveDrag {
                        baseline: self.dialog_value.clone(),
                        tone: sample.tone,
                        start_y: start.y as f64,
                        target: match self.raw_pointer_mode.as_str() {
                            "Curve RGB" => CurveTarget::Rgb,
                            "Curve Red" => CurveTarget::Red,
                            "Curve Green" => CurveTarget::Green,
                            "Curve Blue" => CurveTarget::Blue,
                            _ => CurveTarget::Parametric,
                        },
                    });
                }
                if self.raw_pointer_mode == "Upright Guide" {
                    self.raw_guide_start = raw_interaction::normalized_guide_point(
                        [start.x as f64, start.y as f64],
                        [
                            rect.left() as f64,
                            rect.top() as f64,
                            rect.width() as f64,
                            rect.height() as f64,
                        ],
                    );
                }
            }
            if response.dragged()
                && let Some(drag) = &self.raw_mixer_drag
            {
                self.dialog_value = drag.settings_at(position.y as f64);
            }
            if response.dragged()
                && let Some(drag) = &self.raw_curve_drag
            {
                self.dialog_value = drag.settings_at(position.y as f64);
            }
            if response.drag_stopped() {
                self.raw_mixer_drag = None;
                self.raw_curve_drag = None;
                if let Some(start) = self.raw_guide_start.take()
                    && let Some(end) = raw_interaction::normalized_guide_point(
                        [position.x as f64, position.y as f64],
                        [
                            rect.left() as f64,
                            rect.top() as f64,
                            rect.width() as f64,
                            rect.height() as f64,
                        ],
                    )
                {
                    raw_interaction::append_guide(&mut self.dialog_value, start, end);
                }
            }
        }
        let raw = if self.dialog_value.get("cameraRaw").is_some() {
            &self.dialog_value["cameraRaw"]
        } else {
            &self.dialog_value
        };
        if let Some(guides) = raw["geometry"]["guides"].as_array() {
            for guide in guides {
                if let Some(points) = raw_interaction::guide_screen_points(guide, rect) {
                    ui.painter()
                        .line_segment(points, Stroke::new(1.5_f32, Color32::LIGHT_BLUE));
                }
            }
        }
        if let Some(start) = self.raw_guide_start
            && let Some(end) = position
        {
            let start = Pos2::new(
                rect.left() + start[0] as f32 * rect.width(),
                rect.bottom() - start[1] as f32 * rect.height(),
            );
            ui.painter()
                .line_segment([start, end], Stroke::new(1.5_f32, Color32::WHITE));
        }
    }
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let started = Instant::now();
        if let Some(state) = &cc.wgpu_render_state {
            log::info!(
                "graphics adapter={:?} limits={:?}",
                state.adapter.get_info(),
                state.device.limits()
            );
        }
        crate::locale::install_fonts(&cc.egui_ctx);
        // Adapter probing happens before eframe creates the window, but older
        // Windows 10 drivers can still reject a WGSL compute pipeline or a
        // storage-texture layout during construction.  Treat that as a
        // graphics capability mismatch instead of letting a validation panic
        // terminate the whole editor: the existing CPU compositor remains a
        // fully usable fallback.
        let gpu = cc.wgpu_render_state.clone().and_then(|state| {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| GpuCanvas::new(state))) {
                Ok(canvas) => Some(canvas),
                Err(_) => {
                    log::error!(
                        "GPU canvas initialization failed; continuing with the CPU compositor"
                    );
                    None
                }
            }
        });
        let mut app = Self::create(gpu, cc.storage);
        crate::chrome::theme::apply_custom(
            &cc.egui_ctx,
            app.custom_theme
                .as_ref()
                .filter(|_| app.custom_theme_enabled),
        );
        cc.egui_ctx.set_theme(app.theme_preference);
        if let Some((_, qa_started)) = &mut app.qa_ready {
            *qa_started = started;
        }
        app.sender.bind_context(cc.egui_ctx.clone());
        match agent_bridge::AgentBridge::start(cc.egui_ctx.clone()) {
            Ok(bridge) => app.agent_bridge = Some(bridge),
            Err(error) => app.message = Some(format!("Agent 接口启动失败：{error:#}")),
        }
        app
    }
    /// Reattach this editing session to a replacement native renderer.
    pub fn recover_renderer(&mut self, cc: &eframe::CreationContext<'_>) {
        self.finish_renderer_gestures();
        // Texture handles belong to the previous Context, including CPU-preview uploads.
        // Keep documents, history, CPU caches, receivers and background jobs in place.
        self.gpu = None;
        for session in &mut self.tabs {
            session.texture = None;
            session.gpu_id = None;
            session.rendered = 0;
            session.rendered_view = None;
            session.preview_failed = None;
            session.thumbnails.clear();
            session.mask_thumbnails.clear();
        }
        self.raw_texture = None;
        self.raw_view_texture = None;
        self.dialog_texture = None;
        // In-flight previews still arrive through the original receiver. Only request
        // a replacement when there is no result already being produced for this key.
        if !self.raw_busy {
            self.raw_key = None;
        }
        if !self.dialog_preview_busy {
            self.preview_key = None;
        }
        self.raw_mixer_drag = None;
        self.raw_curve_drag = None;
        self.raw_guide_start = None;
        self.filter_ui.pointer_end();
        self.window_motion = crate::window_motion::WindowMotion::new();
        self.restore_maximized_after_frame = None;
        self.sender.bind_context(cc.egui_ctx.clone());
        if let Some(bridge) = &mut self.agent_bridge {
            bridge.bind_context(cc.egui_ctx.clone());
        }
        crate::locale::install_fonts(&cc.egui_ctx);
        crate::chrome::theme::apply_custom(
            &cc.egui_ctx,
            self.custom_theme
                .as_ref()
                .filter(|_| self.custom_theme_enabled),
        );
        cc.egui_ctx.set_theme(self.theme_preference);
        // Recovery currently targets Glow, which has no WGPU compute canvas.
        // Its CPU compositor reuses the retained preview caches on the next frame.
        cc.egui_ctx.request_repaint();
    }
    fn finish_renderer_gestures(&mut self) {
        if let Some(drag) = self.drag.take()
            && self.drag_is_current(&drag)
        {
            if drag.mode.paints() {
                self.finish_brush(drag.before, drag.last);
            } else if drag.mode == Tool::Move || drag.outline_move {
                if drag.outline_move {
                    let session = self.session_mut();
                    if session
                        .history
                        .record("Move selection outline", drag.before, &session.doc)
                    {
                        session.changed();
                    }
                } else if drag.distort
                    && let Some((id, corners)) = distort_corners(&drag, self.mask_target)
                {
                    let before = drag.before;
                    let mut after = before.clone();
                    let mask = self.mask_target;
                    // A failed replacement renderer may be followed immediately by
                    // emergency backups and process exit. Finish this gesture here
                    // so its final pixels cannot be stranded in a worker result.
                    match crate::edit::free_distort(&mut after, id, corners, mask)
                        .and_then(|()| self.check_workspace(&after))
                    {
                        Ok(()) => {
                            let session = self.session_mut();
                            session.doc = after;
                            if session.history.record("Free distort", before, &session.doc) {
                                session.changed();
                            }
                        }
                        Err(error) => self.message = Some(error.to_string()),
                    }
                } else {
                    if drag.pixel_move
                        && let Some(id) = drag.before.active_layer_id
                    {
                        self.session_mut().doc = drag.before.clone();
                        let offset = [drag.last[0] - drag.start[0], drag.last[1] - drag.start[1]];
                        let mask = self.mask_target;
                        if let Err(error) = crate::edit::move_selected_pixels(
                            &mut self.session_mut().doc,
                            id,
                            mask,
                            offset,
                            drag.duplicate_pixels,
                        ) {
                            self.message = Some(error.to_string());
                        }
                    }
                    let session = self.session_mut();
                    if session
                        .history
                        .record("Transform", drag.before, &session.doc)
                    {
                        session.changed();
                    }
                }
            }
        }
        if let Some(guide) = self.guide_drag.take() {
            let session = self.session_mut();
            if session.history.record("Guide", guide.before, &session.doc) {
                session.changed();
            }
        }
        // The document mutations above have now entered undo history. Ordinary
        // gesture cleanup must not restore their pre-drag document snapshots.
        self.settle_gestures();
    }
    pub fn defer_maximized_restore(&mut self, ctx: &egui::Context) {
        self.restore_maximized_after_frame = Some(ctx.cumulative_frame_nr());
    }
    fn restore_window_after_first_frame(&mut self, ctx: &egui::Context) {
        if let Some(first_frame) = self.restore_maximized_after_frame {
            if ctx.cumulative_frame_nr() > first_frame {
                self.restore_maximized_after_frame = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            } else {
                // eframe presents and shows the first frame before the next native redraw.
                // A discarded egui pass belongs to the same frame and must not maximize early.
                ctx.request_repaint();
            }
        }
    }
    fn create(gpu: Option<GpuCanvas>, storage: Option<&dyn eframe::Storage>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let sender = WorkerSender {
            sender,
            context: Arc::new(Mutex::new(None)),
        };
        let segment_sender = segmentation_worker(sender.clone());
        let shortcuts = load_shortcuts();
        let smoke = std::env::args_os().any(|a| a == "--smoke-test");
        let mut app = Self {
            agent_bridge: None,
            theme_preference: egui::ThemePreference::System,
            custom_theme: None,
            custom_theme_enabled: false,
            theme_settings_open: false,
            theme_settings_message: None,
            theme_settings_dirty: false,
            open_source_licenses: crate::open_source_licenses::LicenseWindow::default(),
            tabs: vec![Session::new(Document::new(1280, 800), None)],
            current: 0,
            gpu,
            watcher: PackageWatcher::new(sender.clone()),
            sender,
            receiver,
            pending_io: 0,
            imports: import_worker::ImportQueue::default(),
            pdf_ui: pdf_ui::PdfUi::default(),
            busy_since: None,
            segment_sender,
            tool: Tool::Move,
            tool_modes: [
                Tool::Rectangle,
                Tool::Lasso,
                Tool::Wand,
                Tool::Brush,
                Tool::Blur,
            ],
            foreground: [45, 159, 231, 255],
            background: [255; 4],
            color_picker: None,
            text_color_target: None,
            sample_drag: None,
            sample_cache: None,
            sample_busy: None,
            sample_point: None,
            show_sample_ring: true,
            diameter: 40.,
            hardness: 0.7,
            blur_radius: 4.,
            opacity: 1.,
            smoothing: 0.,
            brush_presets: [None; 7],
            mask_target: false,
            mask_alone: false,
            mask_clipboard: None,
            selection_mode: SelectionMode::Replace,
            antialias: true,
            tolerance: 24,
            contiguous: true,
            wand_all: true,
            wand_radius: 0,
            clone_source: None,
            clone_aligned: true,
            clone_all: false,
            clone_offset: None,
            last_brush: None,
            healing_mode: 0,
            shape_kind: "Rectangle".into(),
            corner_radius: 0.,
            line_width: 4.,
            transform_percent: false,
            show_transform: true,
            transform_auto_select: false,
            locks_transform_ratio: true,
            warp_preview: None,
            warp_preview_busy: false,
            warp_generation: 0,
            warp_key: None,
            gradient_kind: "Linear".into(),
            gradient_transparent: false,
            gradient_reverse: false,
            gradient: None,
            gradient_handle: None,
            crop_ratio: [0, 0],
            crop_rect: None,
            crop_drag: None,
            crop_session: None,
            guide_drag: None,
            guides_locked: false,
            new_guide_horizontal: true,
            new_guide_position: 0.,
            recent: load_recent(),
            drag: None,
            brush_stroke: None,
            polygon: vec![],
            polygon_options: None,
            selection_press: None,
            layer_clipboard: None,
            pixel_clipboard: None,
            clipboard_sequence: clipboard_sequence(),
            paste_key_handled: false,
            visibility_sweep: None,
            property_edit: None,
            text_box_drag: None,
            text_apply_requested: false,
            dialog: None,
            dialog_value: Value::Null,
            preview_key: None,
            filter_ui: crate::filter_panel::FilterUiState::default(),
            dialog_ready_source: None,
            dialog_texture: None,
            dialog_sample: None,
            dialog_graded_sample: None,
            dialog_display_sample: None,
            raw_native_view: false,
            raw_view_texture: None,
            dialog_source_cache: Arc::new(Mutex::new(crate::preview::PreviewCache::default())),
            dialog_preview_busy: false,
            dialog_generation: 0,
            dialog_preview_bytes: 0,
            dialog_preview_error: None,
            jpeg_quality: 90,
            jpeg_matte: [255; 4],
            jpeg_zoom: 0.,
            new_size: [1920, 1080],
            welcome_clipboard_checked: false,
            new_resolution: 72.,
            size_units: "Pixels".into(),
            size_relative: false,
            size_delta: [0, 0],
            size_original: [1280, 800],
            size_proportional: true,
            resize_sampling: Sampling::High,
            canvas_anchor: [0.5, 0.5],
            message: None,
            report: vec![],
            pending_import: None,
            import_queue: VecDeque::new(),
            raw_queue: VecDeque::new(),
            dialog_target: None,
            dialog_mask: false,
            raw_settings: Default::default(),
            raw_texture: None,
            raw_busy: false,
            raw_key: None,
            raw_pointer_mode: "Sample".into(),
            raw_mixer_drag: None,
            raw_curve_drag: None,
            raw_guide_start: None,
            raw_point_selection: None,
            close_pending: None,
            exit_pending: false,
            allow_exit: false,
            window_motion: crate::window_motion::WindowMotion::new(),
            external_pending: None,
            grid: false,
            rulers: false,
            pixel_grid: true,
            snap: true,
            snap_targets: [true; 4],
            grid_spacing: 64.,
            grid_color: [255, 255, 255, 35],
            grid_subdivisions: 8,
            selection_amount_dialog: None,
            floating_layers: false,
            floating_tools: false,
            shortcuts_open: false,
            shortcuts,
            disk_changes: HashMap::new(),
            qa_ready: std::env::var_os("COMPOSITOR_QA_READY_PATH")
                .map(|path| (PathBuf::from(path), Instant::now())),
            restore_maximized_after_frame: None,
            started: Instant::now(),
            smoke,
            smoke_done: false,
            save_count: 0,
            watch_count: 0,
        };
        if let Some(saved) = storage.and_then(|s| eframe::get_value::<Value>(s, "editor")) {
            app.theme_preference =
                serde_json::from_value(saved["themePreference"].clone()).unwrap_or_default();
            if !saved["customTheme"].is_null() {
                match serde_json::from_value::<crate::custom_theme::ThemeFile>(
                    saved["customTheme"].clone(),
                ) {
                    Ok(theme) => {
                        app.custom_theme = Some(theme);
                        app.custom_theme_enabled =
                            saved["customThemeEnabled"].as_bool().unwrap_or(false);
                    }
                    Err(error) => {
                        app.theme_settings_message =
                            Some(format!("保存的主题无效，已恢复原版主题：{error}"));
                    }
                }
            }
            app.floating_layers = saved["floatingLayers"].as_bool().unwrap_or(false);
            app.floating_tools = saved["floatingTools"].as_bool().unwrap_or(false);
            app.grid_spacing =
                crate::filters::number(&saved, "gridSpacing", 64.).clamp(1., 10_000.);
            app.grid_subdivisions =
                saved["gridSubdivisions"].as_u64().unwrap_or(8).clamp(1, 32) as u32;
            app.rulers = saved["rulers"].as_bool().unwrap_or(false);
            app.grid = saved["grid"].as_bool().unwrap_or(false);
            app.snap = saved["snap"].as_bool().unwrap_or(true);
            app.show_sample_ring = saved["sampleRing"].as_bool().unwrap_or(true);
            app.transform_auto_select = saved["transformAutoSelect"].as_bool().unwrap_or(false);
            app.locks_transform_ratio = saved["locksTransformRatio"].as_bool().unwrap_or(true);
            if let Ok(targets) = serde_json::from_value(saved["snapTargets"].clone()) {
                app.snap_targets = targets;
            }
            if let Ok(color) = serde_json::from_value(saved["gridColor"].clone()) {
                app.grid_color = color;
            }
            app.jpeg_quality = saved["jpegQuality"].as_u64().unwrap_or(90).clamp(1, 100) as u8;
        }
        if !smoke {
            app.session_mut().placeholder = true;
        }
        // Windows shell paths may contain unpaired UTF-16 code units. Keep
        // the native OsString until opening the file; env::args would panic
        // before the import worker can report an error for such a path.
        for arg in std::env::args_os()
            .skip(1)
            .filter(|arg| !arg.to_string_lossy().starts_with("--"))
        {
            let path = PathBuf::from(arg);
            if path.exists() {
                app.open(path, false);
            }
        }
        app
    }
    fn session(&self) -> &Session {
        &self.tabs[self.current]
    }
    fn session_mut(&mut self) -> &mut Session {
        &mut self.tabs[self.current]
    }
    fn open(&mut self, path: PathBuf, as_layer: bool) {
        let target = (as_layer && !self.is_welcome()).then_some(self.session().id);
        self.open_target(path, target);
    }
    fn open_target(&mut self, path: PathBuf, target: Option<Uuid>) {
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
        {
            self.open_pdf(path, target);
            return;
        }
        if target.is_none()
            && let Some(index) = self
                .tabs
                .iter()
                .position(|s| s.path.as_ref() == Some(&path))
        {
            self.activate_tab(index);
            return;
        }
        if image_io::is_raw(&path) {
            if self.dialog.is_some() {
                self.raw_queue.push_back((path, target));
                return;
            }
            self.raw_settings = Default::default();
            self.raw_texture = None;
            self.begin_dialog(Dialog::Raw {
                path: path.clone(),
                target,
            });
            self.raw_preview(path);
            return;
        }
        self.message = Some(format!("正在打开 {}…", path.display()));
        match self.imports.submit(
            import_worker::ImportTask::Open { path, target },
            &self.sender,
        ) {
            Ok(()) => self.pending_io += 1,
            Err(error) => self.message = Some(error),
        }
    }
    fn dialog_output_path(path: PathBuf, supported: &[&str]) -> Result<(PathBuf, String), String> {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !supported.contains(&extension.as_str()) {
            let formats = supported
                .iter()
                .map(|extension| format!(".{extension}"))
                .collect::<Vec<_>>()
                .join("、");
            return Err(format!("请使用 {formats} 扩展名，并重新选择保存位置"));
        }
        // Preserve the exact path whose replacement the native dialog confirmed.
        // Adding or replacing its extension here could overwrite a different file.
        Ok((path, extension))
    }
    fn save(&mut self, save_as: bool) {
        if self.session().save_busy {
            return;
        }
        let legacy_directory = self.session().path.as_ref().is_some_and(|p| p.is_dir());
        let path = if save_as || self.session().path.is_none() || legacy_directory {
            rfd::FileDialog::new()
                .set_title("保存 Compositor 项目")
                .add_filter("Compositor 项目", &["comp"])
                .set_file_name(format!(
                    "{}{}.comp",
                    self.session().title().trim_end_matches(" *"),
                    if legacy_directory { "-单文件" } else { "" }
                ))
                .save_file()
        } else {
            self.session().path.clone()
        };
        let Some(path) = path else {
            return;
        };
        let path = match Self::dialog_output_path(path, &["comp"]) {
            Ok((path, _)) => path,
            Err(error) => {
                self.message = Some(error);
                return;
            }
        };
        let d = self.session().doc.clone();
        let id = self.session().id;
        let revision = self.session().history.revision();
        let sender = self.sender.clone();
        match gui_threads::spawn("保存项目", move || {
            let result = gui_threads::run("保存项目", || {
                project::save(&d, &path).map_err(|e| format!("{e:#}"))
            });
            let _ = sender.send(Worker::Save {
                id,
                revision,
                path,
                result,
            });
        }) {
            Ok(_) => self.session_mut().save_busy = true,
            Err(error) => self.message = Some(error),
        }
    }
    fn remember(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(20);
        if let Ok(data) = serde_json::to_vec_pretty(&self.recent) {
            let target = preferences_path().join("recent.json");
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = image_io::write_atomic(&target, &data) {
                self.message = Some(format!("最近打开的项目：{e}"));
            }
        }
    }
    fn export(&mut self, extension: &str) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("导出图像")
            .add_filter(extension, &[extension])
            .set_file_name(format!(
                "{}.{}",
                self.session().title().trim_end_matches(" *"),
                extension
            ))
            .save_file()
        else {
            return;
        };
        self.export_to_path(extension.to_owned(), path);
    }
    fn export_other(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("导出图像")
            .add_filter("PNG 图像", &["png"])
            .add_filter("JPEG 图像", &["jpg", "jpeg"])
            .add_filter("TIFF 图像", &["tif", "tiff"])
            .add_filter("BMP 图像", &["bmp"])
            .add_filter("GIF 图像", &["gif"])
            .add_filter("WebP 图像", &["webp"])
            .add_filter("PDF 文档（当前页面）", &["pdf"])
            .set_file_name(format!(
                "{}.png",
                self.session().title().trim_end_matches(" *")
            ))
            .save_file()
        else {
            return;
        };
        let (path, extension) = match Self::dialog_output_path(
            path,
            &[
                "png", "jpg", "jpeg", "tif", "tiff", "bmp", "gif", "webp", "pdf",
            ],
        ) {
            Ok(output) => output,
            Err(error) => {
                self.message = Some(error);
                return;
            }
        };
        self.export_to_path(extension, path);
    }
    fn export_to_path(&mut self, extension: String, path: PathBuf) {
        let requested_extension = extension.as_str();
        let supported = match extension.as_str() {
            "jpg" | "jpeg" => &["jpg", "jpeg"][..],
            "tif" | "tiff" => &["tif", "tiff"][..],
            _ => std::slice::from_ref(&requested_extension),
        };
        let (path, extension) = match Self::dialog_output_path(path, supported) {
            Ok(output) => output,
            Err(error) => {
                self.message = Some(error);
                return;
            }
        };
        let d = self.session().doc.clone();
        let pdf_size = pdf_ui::page_size(self.session());
        if extension == "pdf" {
            self.start_pdf_export(vec![(d, pdf_size)], path);
            return;
        }
        let sender = self.sender.clone();
        let matte = [self.jpeg_matte[0], self.jpeg_matte[1], self.jpeg_matte[2]];
        let quality = self.jpeg_quality;
        self.message = Some("Rendering full-resolution export…".into());
        match gui_threads::spawn("导出图像", move || {
            let result = gui_threads::run("导出图像", || {
                let raster = render::render_document(&d);
                image_io::export_raster(&raster, &path, &extension, d.resolution, quality, matte)
                    .map(|_| path)
                    .map_err(|e| format!("{e:#}"))
            });
            let _ = sender.send(Worker::Export(result));
        }) {
            Ok(_) => self.pending_io += 1,
            Err(error) => self.message = Some(error),
        }
    }
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Worker::PdfInfo {
                    path,
                    target,
                    result,
                } => {
                    self.pending_io = self.pending_io.saturating_sub(1);
                    match result {
                        Ok(info) => self.pdf_ui.enqueue(path, target, info),
                        Err(error) => self.message = Some(error),
                    }
                    self.imports.complete();
                }
                Worker::PdfOpen {
                    path,
                    target,
                    dpi,
                    result,
                } => {
                    self.pending_io = self.pending_io.saturating_sub(1);
                    match result {
                        Ok(pages) => self.install_pdf_pages(path, target, dpi, pages),
                        Err(error) => self.message = Some(error),
                    }
                    self.imports.complete();
                }
                Worker::DiskChanged { target, stamp } => {
                    if self.tabs.iter().any(|s| {
                        s.id == target.id
                            && s.path.as_ref() == Some(&target.path)
                            && s.stamp == target.stamp
                    }) {
                        self.disk_changes.insert(target.id, (target, stamp));
                    }
                }
                Worker::ColorSample {
                    session,
                    serial,
                    sampler,
                } => {
                    if self.sample_busy == Some((session, serial)) {
                        self.sample_busy = None;
                        if session == self.session().id
                            && serial == self.session().serial
                            && sampler.matches(&self.session().doc)
                        {
                            if let Err(error) = self.check_workspace_documents(
                                std::iter::empty(),
                                sampler.allocated_bytes() as u64,
                            ) {
                                self.message = Some(error.to_string());
                                continue;
                            }
                            if let Some(color) =
                                self.sample_point.and_then(|point| sampler.sample(point))
                            {
                                self.apply_sampled_color(color);
                            }
                            self.sample_cache = Some((session, sampler));
                            ctx.request_repaint();
                        }
                    }
                }
                Worker::ColorSampleFailure {
                    session,
                    serial,
                    error,
                } => {
                    if self.sample_busy == Some((session, serial)) {
                        self.sample_busy = None;
                        self.message = Some(error);
                    }
                }
                Worker::WarpPreview {
                    session,
                    generation,
                    result,
                } => {
                    if generation == self.warp_generation {
                        self.warp_preview_busy = false;
                        if session == self.session().id
                            && self.drag.as_ref().is_some_and(|drag| drag.distort)
                        {
                            match result {
                                Ok(layer) => {
                                    self.warp_preview = Some((session, layer));
                                    self.session_mut().changed();
                                }
                                Err(e) => self.message = Some(e),
                            }
                        }
                    }
                }
                Worker::Open {
                    path,
                    result,
                    target,
                } => {
                    self.pending_io = self.pending_io.saturating_sub(1);
                    match result {
                        Ok(import) => {
                            if !import.conversions.is_empty() {
                                match self.check_workspace(&import.document) {
                                    Ok(()) if self.pending_import.is_some() => {
                                        self.import_queue.push_back((path, import, target));
                                    }
                                    Ok(()) => {
                                        self.report = import.conversions.clone();
                                        self.pending_import = Some((path, import, target));
                                    }
                                    Err(error) => self.message = Some(error.to_string()),
                                }
                            } else {
                                self.install_import(path, import, target);
                                ctx.request_repaint();
                            }
                        }
                        Err(e) => self.message = Some(e),
                    }
                    self.imports.complete();
                }
                Worker::RawPreview { path, key, result } => {
                    self.raw_busy = false;
                    if matches!(&self.dialog,Some(Dialog::Raw{path:active,..})if *active==path)
                        && key == raw_settings_key(self.raw_settings)
                    {
                        match result {
                            Ok(r) => {
                                self.raw_texture = Some(ctx.load_texture(
                                    "raw-preview",
                                    egui::ColorImage::from_rgba_unmultiplied(
                                        [r.width as usize, r.height as usize],
                                        &r.to_rgba(),
                                    ),
                                    egui::TextureOptions::LINEAR,
                                ))
                            }
                            Err(e) => self.message = Some(e),
                        }
                    }
                    self.imports.complete();
                }
                Worker::DialogPreview {
                    generation,
                    sample,
                    graded_sample,
                    display_sample,
                    result,
                } => {
                    if generation == self.dialog_generation {
                        self.dialog_ready_source =
                            self.preview_key.as_ref().map(|key| (key.0, key.1));
                        self.dialog_sample = sample;
                        self.dialog_graded_sample = graded_sample;
                        self.dialog_display_sample = display_sample;
                        self.dialog_preview_busy = false;
                        match result {
                            Ok((r, bytes)) => {
                                self.dialog_preview_bytes = bytes;
                                self.dialog_preview_error = None;
                                self.dialog_texture = Some(ctx.load_texture(
                                    "dialog-preview",
                                    egui::ColorImage::from_rgba_unmultiplied(
                                        [r.width as usize, r.height as usize],
                                        &r.to_rgba(),
                                    ),
                                    egui::TextureOptions::LINEAR,
                                ));
                            }
                            Err(e) => {
                                self.dialog_preview_error = Some(e);
                                self.dialog_texture = None;
                            }
                        }
                    }
                }
                Worker::Gradient {
                    revision,
                    serial,
                    mut state,
                    result,
                } => {
                    let result = result.and_then(|doc| {
                        self.check_workspace(&doc)
                            .map(|_| doc)
                            .map_err(|e| e.to_string())
                    });
                    let owns_ui = self
                        .gradient
                        .as_ref()
                        .is_some_and(|pending| pending.request == state.request);
                    if let Some(session) = self.tabs.iter_mut().find(|s| s.id == state.session) {
                        match tool_header_ui::finish_gradient_result(
                            session, &mut state, revision, serial, result,
                        ) {
                            Ok(true)
                                if owns_ui
                                    && self.tabs[self.current].id == state.session
                                    && self.tool == Tool::Gradient
                                    && self.mask_target == state.mask
                                    && self.session().doc.active_layer_id == Some(state.layer) =>
                            {
                                self.gradient = Some(*state);
                            }
                            Ok(_) => {
                                if owns_ui {
                                    self.apply_gradient();
                                }
                            }
                            Err(error) => {
                                if owns_ui {
                                    self.apply_gradient();
                                }
                                self.message = Some(error);
                            }
                        }
                    }
                }
                Worker::Edit {
                    id,
                    revision,
                    serial,
                    name,
                    before,
                    result,
                } => {
                    let result = result.and_then(|doc| {
                        self.check_workspace(&doc)
                            .map(|_| doc)
                            .map_err(|e| e.to_string())
                    });
                    if let Some(s) = self.tabs.iter_mut().find(|s| s.id == id) {
                        s.edit_busy = false;
                        match result{
                            Ok(d)if s.history.revision()==revision&&s.serial==serial=>{
                                if d.active_layer_id != before.active_layer_id {
                                    s.selected = d.active_layer_id.into_iter().collect();
                                } else {
                                    s.selected.retain(|id| d.layer(*id).is_some());
                                }
                                s.doc=d;if s.history.record(name,before,&s.doc){s.changed();}
                            }
                            Ok(_)=>self.message=Some("The document changed during processing. Reapply the operation to your current edits.".into()),
                            Err(e)=>self.message=Some(e),
                        }
                    }
                }
                Worker::Reload {
                    id,
                    stamp,
                    revision,
                    serial,
                    force,
                    result,
                } => {
                    let result = result.and_then(|doc| {
                        self.check_workspace(&doc)
                            .map(|_| doc)
                            .map_err(|e| e.to_string())
                    });
                    if let Some(s) = self.tabs.iter_mut().find(|s| s.id == id) {
                        s.reload_busy = false;
                        match result {
                            Ok(mut d) if s.history.revision() == revision && s.serial == serial => {
                                d.selection = s.doc.selection.as_ref().map(|selection| {
                                    selection::translated(selection, 0, 0, d.width, d.height)
                                });
                                if s.doc
                                    .active_layer_id
                                    .is_some_and(|id| d.layer(id).is_some())
                                {
                                    d.active_layer_id = s.doc.active_layer_id;
                                }
                                if d == s.doc {
                                    s.stamp = stamp;
                                    continue;
                                }
                                if !force && s.history.is_modified() {
                                    self.external_pending = Some((id, stamp));
                                    continue;
                                }
                                s.selected.retain(|id| d.layer(*id).is_some());
                                if s.selected.is_empty() {
                                    s.selected = d.active_layer_id.into_iter().collect();
                                }
                                s.doc = d;
                                s.stamp = stamp;
                                s.history = History::default();
                                s.changed();
                                self.watch_count += 1;
                            }
                            Ok(_) => self.external_pending = Some((id, stamp)),
                            Err(e) => self.message = Some(format!("外部更新：{e}")),
                        }
                    }
                }
                Worker::Save {
                    id,
                    revision,
                    path,
                    result,
                } => {
                    if let Some(s) = self.tabs.iter_mut().find(|s| s.id == id) {
                        s.save_busy = false;
                        match result {
                            Ok(()) => {
                                s.history.mark_saved(revision);
                                s.stamp = package_stamp(&path);
                                s.path = Some(path.clone());
                                self.save_count += 1;
                                self.message = Some("Project saved".into());
                            }
                            Err(e) => self.message = Some(e),
                        }
                    }
                    if self.tabs.iter().any(|s| s.path.as_ref() == Some(&path)) {
                        self.remember(&path);
                    }
                }
                Worker::Preview {
                    id,
                    serial,
                    view,
                    raster,
                } => {
                    if let Some(s) = self.tabs.iter_mut().find(|s| s.id == id) {
                        s.preview_busy = false;
                        if s.serial == serial && s.preview_view == Some(view) {
                            let image = egui::ColorImage::from_rgba_unmultiplied(
                                [raster.width as usize, raster.height as usize],
                                &raster.to_rgba(),
                            );
                            let options = if view.native() {
                                egui::TextureOptions::NEAREST
                            } else {
                                egui::TextureOptions::LINEAR
                            };
                            if let Some(t) = &mut s.texture {
                                t.set(image, options);
                            } else {
                                s.texture =
                                    Some(ctx.load_texture(format!("preview-{id}"), image, options));
                            }
                            s.rendered = serial;
                            s.rendered_view = Some(view);
                            s.gpu_id = None;
                        }
                    }
                }
                Worker::PreviewFailure {
                    id,
                    serial,
                    view,
                    error,
                } => {
                    if let Some(session) = self.tabs.iter_mut().find(|session| session.id == id) {
                        session.preview_busy = false;
                        if session.serial == serial && session.preview_view == Some(view) {
                            // Retry when the image/view changes, a tab is reactivated, or
                            // the renderer is restored; do not restart a failing job each frame.
                            session.preview_failed = Some((serial, view));
                            self.message = Some(error);
                        }
                    }
                }
                Worker::Export(result) => {
                    self.pending_io = self.pending_io.saturating_sub(1);
                    self.message = Some(match result {
                        Ok(p) => format!("已导出 {}", p.display()),
                        Err(e) => e,
                    })
                }
            }
        }
    }
    fn render_canvas(&mut self, ctx: &egui::Context, view: crate::preview::ViewRequest) {
        if (self.session().rendered == self.session().serial
            && self.session().rendered_view == Some(view))
            || self.session().preview_failed == Some((self.session().serial, view))
            || self.session().preview_busy
        {
            return;
        }
        let d = self.display_document();
        let gpu_id = self.gpu_frame(|gpu| gpu.and_then(|gpu| gpu.render_view(&d, view)));
        if let Some(id) = gpu_id {
            let s = self.session_mut();
            s.gpu_id = Some(id);
            s.rendered = s.serial;
            s.rendered_view = Some(view);
        } else {
            let cache = self.session().preview_cache.clone();
            let id = self.session().id;
            let serial = self.session().serial;
            let sender = self.sender.clone();
            let ctx = ctx.clone();
            match gui_threads::spawn("画布预览", move || {
                let result = gui_threads::run("画布预览", || {
                    Ok(cache
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .render_view(&d, view)
                        .as_ref()
                        .clone())
                });
                let event = match result {
                    Ok(raster) => Worker::Preview {
                        id,
                        serial,
                        view,
                        raster,
                    },
                    Err(error) => Worker::PreviewFailure {
                        id,
                        serial,
                        view,
                        error,
                    },
                };
                let _ = sender.send(event);
                ctx.request_repaint();
            }) {
                Ok(_) => self.session_mut().preview_busy = true,
                Err(error) => {
                    self.session_mut().preview_failed = Some((serial, view));
                    self.message = Some(error);
                }
            }
        }
    }
    fn gpu_frame(
        &mut self,
        render: impl FnOnce(Option<&mut GpuCanvas>) -> Option<egui::TextureId>,
    ) -> Option<egui::TextureId> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(self.gpu.as_mut()))) {
            Ok(id) => id,
            Err(_) => {
                // A synchronous driver/validation panic must not take down an
                // active editing session or repeat on every future redraw.
                log::error!("GPU canvas render failed; disabling GPU compositing for this session");
                self.disable_gpu_compositing();
                None
            }
        }
    }
    fn disable_gpu_compositing(&mut self) {
        // The native GPU texture is shared by all tabs. Its ID becomes invalid
        // when the canvas drops; invalidate every tab so switching documents
        // cannot hand eframe a freed texture or leave an old frame displayed.
        for session in &mut self.tabs {
            session.gpu_id = None;
            session.rendered_view = None;
            session.preview_failed = None;
        }
        self.gpu = None;
    }
    fn install_import(
        &mut self,
        path: PathBuf,
        import: image_io::ImportResult,
        target: Option<Uuid>,
    ) {
        if let Err(e) = self.check_workspace(&import.document) {
            self.message = Some(e.to_string());
            return;
        }
        self.message = None;
        self.remember(&path);
        if let Some(target) = target {
            let Some(index) = self.tabs.iter().position(|s| s.id == target) else {
                self.message =
                    Some("The destination project was closed before import completed".into());
                return;
            };
            if index == self.current {
                self.settle_gestures();
            }
            let source = import.document;
            let ids: Vec<_> = source
                .layers
                .iter()
                .filter(|l| l.parent_id.is_none())
                .map(|l| l.id)
                .collect();
            let before = self.tabs[index].doc.clone();
            let mut candidate = before.clone();
            let result = crate::edit::copy_layers_to(&source, &mut candidate, &ids, None)
                .and_then(|_| self.check_workspace(&candidate));
            match result {
                Ok(()) => {
                    let session = &mut self.tabs[index];
                    session.doc = candidate;
                    session.selected = session.doc.active_layer_id.into_iter().collect();
                    if session
                        .history
                        .record("Import layers", before, &session.doc)
                    {
                        session.changed();
                    }
                }
                Err(error) => self.message = Some(error.to_string()),
            }
        } else {
            if let Some(index) = self
                .tabs
                .iter()
                .position(|s| s.path.as_ref() == Some(&path))
            {
                self.activate_tab(index);
                return;
            }
            let saved = (path.is_dir()
                || path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("comp")))
            .then_some(path);
            self.install_canvas_session(Session::new(import.document, saved));
        }
    }
    fn raw_preview(&mut self, path: PathBuf) {
        if self.raw_busy {
            return;
        }
        self.raw_busy = true;
        let settings = self.raw_settings;
        let key = raw_settings_key(settings);
        self.raw_key = Some((path.clone(), key));
        if let Err(error) = self.imports.submit(
            import_worker::ImportTask::RawPreview {
                path,
                settings,
                key,
            },
            &self.sender,
        ) {
            self.raw_busy = false;
            self.message = Some(error);
        }
    }
    fn import_raw(&mut self, path: PathBuf, target: Option<Uuid>) {
        let settings = self.raw_settings;
        self.message = Some("Developing RAW image…".into());
        match self.imports.submit(
            import_worker::ImportTask::RawImport {
                path,
                settings,
                target,
            },
            &self.sender,
        ) {
            Ok(()) => self.pending_io += 1,
            Err(error) => self.message = Some(error),
        }
    }
    fn request_close(&mut self, index: usize) {
        if matches!(self.dialog, Some(Dialog::Text { .. }))
            && self
                .dialog_target
                .is_some_and(|(id, _)| id == self.tabs[index].id)
        {
            self.text_apply_requested = true;
            self.close_pending = Some(self.tabs[index].id);
            return;
        }
        if self.tabs[index].history.is_modified()
            || self.tabs[index].save_busy
            || self.tabs[index].edit_busy
            || self.tabs[index].reload_busy
            || (index == self.current
                && (self.drag.is_some()
                    || self.guide_drag.is_some()
                    || self.gradient_handle.is_some()))
        {
            self.close_pending = Some(self.tabs[index].id);
        } else {
            self.close_tab(index);
        }
    }
    fn close_tab(&mut self, index: usize) {
        let active = self.session().id;
        let closed = self.tabs[index].id;
        self.disk_changes.remove(&closed);
        if self.external_pending.is_some_and(|(id, _)| id == closed) {
            self.external_pending = None;
        }
        if self.close_pending == Some(self.tabs[index].id) {
            self.close_pending = None;
        }
        if index == self.current {
            self.settle_gestures();
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            let mut placeholder = Session::new(Document::new(1280, 800), None);
            placeholder.placeholder = true;
            self.tabs.push(placeholder);
        }
        self.current = self
            .tabs
            .iter()
            .position(|s| s.id == active)
            .unwrap_or(index.min(self.tabs.len() - 1));
        if self.session().id != active {
            let session = self.session_mut();
            session.rendered = 0;
            session.gpu_id = None;
            session.preview_failed = None;
        }
    }
    fn menu(&mut self, ctx: &egui::Context) -> Vec<Command> {
        let mut actions = vec![];
        egui::TopBottomPanel::top("menu")
            .exact_height(24.)
            .show(ctx, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    ui.menu_button("文件", |ui| {
                        for (label, c) in [
                            ("新建…     Ctrl+N", Command::New),
                            ("打开…     Ctrl+O", Command::Open),
                            ("导入为图层…", Command::Import),
                            ("保存       Ctrl+S", Command::Save),
                            ("另存为…   Ctrl+Shift+S", Command::SaveAs),
                            ("导出 PNG…", Command::ExportPng),
                            ("导出 JPEG…", Command::ExportJpeg),
                            ("导出 TIFF…", Command::ExportTiff),
                            ("导出其他格式…", Command::ExportOther),
                            ("导出 PDF（可合并多页）…", Command::ExportPdf),
                            ("关闭      Ctrl+W", Command::Close),
                        ] {
                            if ui
                                .add_enabled(self.command_available(c), egui::Button::new(label))
                                .clicked()
                            {
                                actions.push(c);
                                ui.close();
                            }
                        }
                        ui.menu_button("最近打开", |ui| {
                            if self.recent.is_empty() {
                                ui.label("没有最近打开的项目");
                            }
                            for path in self.recent.clone() {
                                if ui.button(path.display().to_string()).clicked() {
                                    self.open(path, false);
                                    ui.close();
                                }
                            }
                            if !self.recent.is_empty() && ui.button("清空记录").clicked() {
                                self.recent.clear();
                                let _ = image_io::write_atomic(
                                    &preferences_path().join("recent.json"),
                                    b"[]",
                                );
                                ui.close();
                            }
                        });
                    });
                    ui.menu_button("编辑", |ui| {
                        for (label, c) in [
                            ("撤销       Ctrl+Z", Command::Undo),
                            ("重做       Ctrl+Shift+Z", Command::Redo),
                            ("剪切       Ctrl+X", Command::Cut),
                            ("复制       Ctrl+C", Command::Copy),
                            ("合并复制", Command::CopyMerged),
                            ("粘贴       Ctrl+V", Command::Paste),
                            ("键盘快捷键…", Command::Shortcuts),
                        ] {
                            if ui
                                .add_enabled(self.command_available(c), egui::Button::new(label))
                                .clicked()
                            {
                                actions.push(c);
                                ui.close();
                            }
                        }
                        ui.separator();
                        for (label, c) in [
                            ("填充前景色", Command::FillForeground),
                            ("填充背景色", Command::FillBackground),
                            ("清除所选像素", Command::ClearPixels),
                            ("反相所选像素", Command::InvertPixels),
                            ("内容识别填充", Command::ContentFill),
                        ] {
                            if ui
                                .add_enabled(self.command_available(c), egui::Button::new(label))
                                .clicked()
                            {
                                actions.push(c);
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("图层", |ui| {
                        for (label, c) in [
                            ("新建图层", Command::AddLayer),
                            ("将所选图层编组", Command::Group),
                            ("取消编组", Command::Ungroup),
                            ("复制图层", Command::Duplicate),
                            ("删除", Command::Delete),
                            ("合并所选图层", Command::Merge),
                            ("向下合并", Command::MergeDown),
                            ("添加图层蒙版", Command::Mask),
                            ("反相蒙版", Command::InvertMask),
                            ("应用蒙版", Command::ApplyMask),
                            ("删除蒙版", Command::DeleteMask),
                            ("复制蒙版", Command::CopyMask),
                            ("粘贴蒙版", Command::PasteMask),
                            ("选择蒙版区域", Command::LoadMask),
                            ("模糊／羽化蒙版…", Command::BlurMask),
                            ("创建／释放剪贴蒙版", Command::Clip),
                        ] {
                            if ui
                                .add_enabled(self.command_available(c), egui::Button::new(label))
                                .clicked()
                            {
                                actions.push(c);
                                ui.close();
                            }
                        }
                        ui.menu_button("新建调整图层", |ui| {
                            for kind in ADJUSTMENTS {
                                if ui
                                    .add_enabled(!self.is_welcome(), egui::Button::new(tr(kind)))
                                    .clicked()
                                {
                                    self.begin_dialog(Dialog::Adjustment {
                                        kind: kind.to_string(),
                                    });
                                    self.dialog_value = adjustment_defaults(kind);
                                    ui.close();
                                }
                            }
                        });
                        ui.menu_button("图层效果", |ui| {
                            for kind in [
                                "stroke",
                                "shadow",
                                "colorOverlay",
                                "innerShadow",
                                "outerGlow",
                                "innerGlow",
                            ] {
                                if ui
                                    .add_enabled(
                                        !self.is_welcome(),
                                        egui::Button::new(humanize(kind)),
                                    )
                                    .clicked()
                                {
                                    self.begin_dialog(Dialog::Effect { kind: kind.into() });
                                    self.dialog_value = effect_defaults(kind);
                                    ui.close();
                                }
                            }
                        });
                    });
                    ui.menu_button("选择", |ui| {
                        for (label, c) in [
                            ("全选       Ctrl+A", Command::SelectAll),
                            ("取消选择   Ctrl+D", Command::Deselect),
                            ("反选       Ctrl+Shift+I", Command::InvertSelection),
                            ("选择主体", Command::Subject),
                            ("色彩范围…", Command::ColorRange),
                            ("载入图层透明度选区", Command::LoadAlpha),
                            ("扩展…", Command::Expand),
                            ("收缩…", Command::Contract),
                            ("羽化…", Command::Feather),
                        ] {
                            if ui
                                .add_enabled(self.command_available(c), egui::Button::new(label))
                                .clicked()
                            {
                                actions.push(c);
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("图像", |ui| {
                        for (label, c) in [
                            ("画布大小…", Command::CanvasSize),
                            ("图像大小…", Command::ImageSize),
                            ("裁剪到选区", Command::CropSelection),
                            ("修剪透明边缘", Command::Trim),
                            ("水平翻转画布", Command::FlipCanvasX),
                            ("垂直翻转画布", Command::FlipCanvasY),
                        ] {
                            if ui
                                .add_enabled(self.command_available(c), egui::Button::new(label))
                                .clicked()
                            {
                                actions.push(c);
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("滤镜", |ui| {
                        if ui
                            .add_enabled(!self.is_welcome(), egui::Button::new("Camera Raw 调色…"))
                            .clicked()
                        {
                            self.begin_dialog(Dialog::CameraRaw);
                            self.dialog_value = camera_raw_defaults();
                            ui.close();
                        }
                        for kind in ADJUSTMENTS.iter().chain(FINISHING) {
                            if ui
                                .add_enabled(!self.is_welcome(), egui::Button::new(tr(kind)))
                                .clicked()
                            {
                                self.begin_dialog(Dialog::Filter {
                                    kind: kind.to_string(),
                                });
                                self.dialog_value = adjustment_defaults(kind);
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("视图", |ui| {
                        for (label, c) in [
                            ("适合窗口", Command::Fit),
                            ("100%", Command::Actual),
                            ("网格", Command::Grid),
                            ("标尺", Command::Rulers),
                            ("网格设置…", Command::GridSettings),
                            ("参考线…", Command::Guides),
                        ] {
                            if ui
                                .add_enabled(self.command_available(c), egui::Button::new(label))
                                .clicked()
                            {
                                actions.push(c);
                                ui.close();
                            }
                        }
                        ui.checkbox(&mut self.snap, "吸附");
                        ui.menu_button("吸附到", |ui| {
                            for (index, label) in ["画布边界", "图层", "参考线", "网格"]
                                .into_iter()
                                .enumerate()
                            {
                                ui.checkbox(&mut self.snap_targets[index], label);
                            }
                        });
                        ui.checkbox(&mut self.pixel_grid, "像素网格");
                        ui.checkbox(&mut self.floating_layers, "浮动图层面板");
                        ui.checkbox(&mut self.floating_tools, "浮动工具面板");
                        ui.separator();
                        if ui.button("主题设置…").clicked() {
                            self.theme_settings_open = true;
                            ui.close();
                        }
                    });
                    ui.menu_button("帮助", |ui| {
                        if ui.button("开源软件许可证").clicked() {
                            self.open_source_licenses.open = true;
                            ui.close();
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.theme_controls_ui(ui);
                    });
                });
            });
        actions
    }
    fn theme_controls_ui(&mut self, ui: &mut egui::Ui) {
        let before = self.theme_preference;
        let dark = ui.visuals().dark_mode;
        ui.menu_button(
            if self.theme_preference == egui::ThemePreference::System {
                "主题：自动"
            } else {
                "主题：手动"
            },
            |ui| {
                for (preference, label) in [
                    (egui::ThemePreference::System, "跟随系统"),
                    (egui::ThemePreference::Light, "日间模式"),
                    (egui::ThemePreference::Dark, "夜间模式"),
                ] {
                    if ui
                        .selectable_value(&mut self.theme_preference, preference, label)
                        .clicked()
                    {
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("主题设置…").clicked() {
                    self.theme_settings_open = true;
                    ui.close();
                }
            },
        )
        .response
        .on_hover_text("自动跟随 Windows，或固定使用日间／夜间模式");
        if ui
            .button(if dark { "切换日间" } else { "切换夜间" })
            .on_hover_text("手动切换主题；在右侧主题菜单中恢复跟随系统")
            .clicked()
        {
            self.theme_preference = if dark {
                egui::ThemePreference::Light
            } else {
                egui::ThemePreference::Dark
            };
        }
        if before != self.theme_preference {
            ui.ctx().set_theme(self.theme_preference);
            ui.ctx().request_repaint();
        }
    }
    fn tools_ui(&mut self, ui: &mut egui::Ui) {
        self.tool_rail_ui(ui);
    }
    fn layers_ui(&mut self, ui: &mut egui::Ui) -> Vec<Command> {
        self.restored_layers_ui(ui)
    }

    fn shortcuts(&mut self, ctx: &egui::Context) -> Vec<Command> {
        if self.selection_amount_dialog.is_some()
            || self.theme_settings_open
            || self.open_source_licenses.open
        {
            return vec![];
        }
        if self.pdf_ui.is_open() {
            return vec![];
        }
        let accepts_clipboard = !ctx.wants_keyboard_input() && self.color_picker.is_none();
        ctx.input_mut(|input| {
            adapt_native_clipboard(input, &mut self.paste_key_handled, accepts_clipboard);
        });
        if self.color_picker.is_some() {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.finish_color_picker(false);
            } else if !ctx.wants_keyboard_input() && ctx.input(|i| i.key_pressed(Key::Enter)) {
                self.finish_color_picker(true);
            }
            return vec![];
        }
        if matches!(self.dialog, Some(Dialog::Text { .. })) {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.cancel_dialog();
                return vec![];
            }
            if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::Enter)) {
                self.text_apply_requested = true;
            }
        }
        if ctx.wants_keyboard_input() {
            return vec![];
        }
        let mut actions = vec![];
        ctx.input_mut(|i| {
            enum Action {
                Command(Command),
                Palette(u8),
                Tool(Tool),
            }
            let mut bindings = Vec::new();
            for (name, command) in [
                ("New", Command::New),
                ("Open", Command::Open),
                ("Save As", Command::SaveAs),
                ("Save", Command::Save),
                ("Redo", Command::Redo),
                ("Undo", Command::Undo),
                ("Copy", Command::Copy),
                ("Paste", Command::Paste),
                ("Cut", Command::Cut),
                ("Select All", Command::SelectAll),
                ("Deselect", Command::Deselect),
                ("Close", Command::Close),
            ] {
                if let Some(chord) = self.shortcuts.get(name).and_then(|s| parse_shortcut(s)) {
                    bindings.push((chord, Action::Command(command)));
                }
            }
            for (name, action) in [
                ("Swap colors", 0),
                ("Default colors", 1),
                ("Cycle tool mode", 2),
            ] {
                if let Some(chord) = self.shortcuts.get(name).and_then(|s| parse_shortcut(s)) {
                    bindings.push((chord, Action::Palette(action)));
                }
            }
            for (name, tool) in tool_bindings() {
                if let Some(chord) = self.shortcuts.get(name).and_then(|s| parse_shortcut(s)) {
                    bindings.push((chord, Action::Tool(tool)));
                }
            }
            // egui allows extra modifiers: more specific chords must consume events first.
            bindings.sort_by_key(|(chord, _)| {
                std::cmp::Reverse(
                    [
                        chord.modifiers.ctrl,
                        chord.modifiers.alt,
                        chord.modifiers.shift,
                        chord.modifiers.command,
                    ]
                    .into_iter()
                    .filter(|set| *set)
                    .count(),
                )
            });
            for (chord, action) in bindings {
                if !i.consume_shortcut(&chord) {
                    continue;
                }
                match action {
                    Action::Command(command) => actions.push(command),
                    Action::Palette(0) => self.swap_palette_colors(),
                    Action::Palette(1) => self.reset_palette_colors(),
                    Action::Palette(_) => self.cycle_tool_mode(),
                    Action::Tool(tool) => {
                        if i.modifiers.shift
                            && self.tool == tool
                            && matches!(
                                tool,
                                Tool::Rectangle | Tool::Lasso | Tool::Wand | Tool::Blur
                            )
                        {
                            self.cycle_tool_mode();
                        } else {
                            self.set_tool(tool);
                        }
                    }
                }
            }
            if i.key_pressed(Key::Delete) {
                actions.push(if self.session().doc.selection.is_some() {
                    Command::ClearPixels
                } else {
                    Command::Delete
                });
            }
            if self.tool == Tool::Polygon && i.key_pressed(Key::Backspace) {
                self.polygon.pop();
            }
            if self.tool.paints() {
                if i.key_pressed(Key::OpenBracket) {
                    self.diameter = (self.diameter * 0.9).max(1.);
                }
                if i.key_pressed(Key::CloseBracket) {
                    self.diameter = (self.diameter * 1.1).min(2500.);
                }
                for (key, opacity) in [
                    (Key::Num0, 1.),
                    (Key::Num1, 0.1),
                    (Key::Num2, 0.2),
                    (Key::Num3, 0.3),
                    (Key::Num4, 0.4),
                    (Key::Num5, 0.5),
                    (Key::Num6, 0.6),
                    (Key::Num7, 0.7),
                    (Key::Num8, 0.8),
                    (Key::Num9, 0.9),
                ] {
                    if i.key_pressed(key) {
                        self.opacity = opacity;
                    }
                }
            }
            if i.key_pressed(Key::Escape) {
                self.cancel_gradient();
                self.crop_rect = None;
                self.crop_drag = None;
                if let Some(drag) = self.guide_drag.take() {
                    self.session_mut().doc = drag.before;
                    self.session_mut().changed();
                }
                if let Some(drag) = self.drag.take() {
                    self.cancel_drag(drag);
                }
                self.brush_stroke = None;
                self.polygon.clear();
                self.polygon_options = None;
                self.selection_press = None;
                self.cancel_dialog();
            }
            if i.key_pressed(Key::Enter) && self.tool == Tool::Gradient {
                self.apply_gradient();
            }
            if i.key_pressed(Key::Enter) && self.tool == Tool::Crop {
                actions.push(Command::CropCommit);
            }
            if i.key_pressed(Key::Enter) && self.tool == Tool::Polygon && self.polygon.len() > 2 {
                let points = std::mem::take(&mut self.polygon);
                let (mode, antialias) = self
                    .polygon_options
                    .take()
                    .unwrap_or((self.selection_mode, self.antialias));
                let s = self.session_mut();
                s.edit("Polygon selection", |d| {
                    let incoming = selection::polygon(d.width, d.height, &points, antialias);
                    d.selection = Some(selection::combine(d.selection.as_ref(), &incoming, mode));
                });
            }
            if self.tool == Tool::Move
                || matches!(
                    self.tool,
                    Tool::Rectangle
                        | Tool::Ellipse
                        | Tool::Lasso
                        | Tool::Polygon
                        | Tool::Wand
                        | Tool::Object
                )
            {
                let mut delta = [0., 0.];
                let step = if i.modifiers.shift { 10. } else { 1. };
                if i.key_pressed(Key::ArrowLeft) {
                    delta[0] -= step;
                }
                if i.key_pressed(Key::ArrowRight) {
                    delta[0] += step;
                }
                if i.key_pressed(Key::ArrowUp) {
                    delta[1] -= step;
                }
                if i.key_pressed(Key::ArrowDown) {
                    delta[1] += step;
                }
                if delta != [0., 0.] {
                    self.nudge(delta, self.tool != Tool::Move);
                }
            }
        });
        actions
    }
    fn command(&mut self, c: Command) {
        if !matches!(c, Command::Expand | Command::Contract | Command::Feather) {
            self.selection_amount_dialog = None;
        }
        if !self.command_available(c) {
            return;
        }
        match c {
            Command::FillForeground | Command::FillBackground | Command::ClearPixels => {
                if let Some(id) = self.session().doc.active_layer_id {
                    let color = self.palette_color(matches!(c, Command::FillBackground));
                    let mask = self.mask_target;
                    self.apply("Edit selected pixels", |d| {
                        if matches!(c, Command::ClearPixels) {
                            crate::edit::clear(d, id, mask)
                        } else {
                            crate::edit::fill(d, id, mask, color)
                        }
                    });
                }
            }
            Command::ContentFill => {
                if let Some(id) = self.session().doc.active_layer_id {
                    self.background_edit("Content-aware fill", move |d| {
                        crate::edit::content_aware_fill_selected(d, id)
                    });
                }
            }
            Command::New => {
                self.welcome_clipboard_checked = true;
                if let Ok(mut clipboard) = arboard::Clipboard::new()
                    && let Ok(image) = clipboard.get_image()
                    && image.width <= 30_000
                    && image.height <= 30_000
                    && Document::valid_size(image.width as u32, image.height as u32)
                {
                    self.new_size = [image.width as u32, image.height as u32];
                }
                self.size_original = self.new_size;
                self.size_relative = false;
                self.size_delta = [0, 0];
                if !self.is_welcome() {
                    let mut placeholder = Session::new(Document::new(1280, 800), None);
                    placeholder.placeholder = true;
                    self.tabs.push(placeholder);
                    self.activate_tab(self.tabs.len() - 1);
                }
            }
            Command::Open | Command::Import => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("打开图像或 Compositor 项目")
                    .add_filter(
                        "图像／项目",
                        &[
                            "comp", "json", "png", "jpg", "jpeg", "tif", "tiff", "svg", "psd",
                            "psb", "dng", "nef", "cr2", "cr3", "arw", "raf", "rw2", "orf", "heic",
                            "heif", "bmp", "gif", "webp", "pdf",
                        ],
                    )
                    .pick_file()
                {
                    let path = if path.file_name().is_some_and(|p| p == "manifest.json") {
                        // `rfd` normally returns an absolute path, but a
                        // caller can still provide a relative/root-like path
                        // on older Win10 shell integrations. Never unwrap a
                        // parent supplied by the file dialog.
                        path.parent()
                            .filter(|parent| !parent.as_os_str().is_empty())
                            .map(Path::to_path_buf)
                            .unwrap_or(path)
                    } else {
                        path
                    };
                    self.open(path, matches!(c, Command::Import));
                }
            }
            Command::Save => self.save(false),
            Command::SaveAs => self.save(true),
            Command::ExportPng => self.export("png"),
            Command::ExportTiff => self.export("tif"),
            Command::ExportOther => self.export_other(),
            Command::ExportPdf => self.begin_pdf_export(),
            Command::ExportJpeg => {
                self.begin_dialog(Dialog::Jpeg);
                self.preview_key = None;
                self.jpeg_matte = self.background;
                self.jpeg_zoom = 0.;
            }
            Command::Undo => {
                let s = self.session_mut();
                if s.history.undo(&mut s.doc) {
                    s.changed();
                    s.selected = s.doc.active_layer_id.into_iter().collect();
                }
            }
            Command::Redo => {
                let s = self.session_mut();
                if s.history.redo(&mut s.doc) {
                    s.changed();
                    s.selected = s.doc.active_layer_id.into_iter().collect();
                }
            }
            Command::Close => self.request_close(self.current),
            Command::Copy | Command::CopyMerged | Command::Cut => {
                self.copy(matches!(c, Command::CopyMerged), matches!(c, Command::Cut))
            }
            Command::Paste => self.paste(),
            Command::AddLayer => self.session_mut().edit("New layer", |d| {
                let l = Layer::new("图层", d.width, d.height);
                d.active_layer_id = Some(l.id);
                d.layers.push(l);
            }),
            Command::Duplicate => {
                let ids = self.session().selected.clone();
                self.apply("Duplicate", |d| {
                    crate::edit::duplicate_layers(d, &ids.iter().copied().collect::<Vec<_>>())
                        .map(|_| ())
                });
            }
            Command::Delete => {
                let ids = self.session().selected.clone();
                self.apply("Delete", |d| {
                    crate::edit::delete_layers(d, &ids.iter().copied().collect::<Vec<_>>())
                });
            }
            Command::Group => {
                let ids = self.session().selected.clone();
                self.apply("Group", |d| {
                    crate::edit::add_group(d, "组", &ids.iter().copied().collect::<Vec<_>>())
                        .map(|_| ())
                });
            }
            Command::Ungroup => {
                if let Some(id) = self.session().doc.active_layer_id {
                    self.apply("Ungroup", |d| crate::edit::ungroup(d, id));
                }
            }
            Command::Merge | Command::MergeDown => self.merge(matches!(c, Command::MergeDown)),
            Command::Mask => self.session_mut().edit("Add mask", |d| {
                let selection = d.selection.clone();
                let transform =
                    crate::model::Transform::new(0., 0., d.width as f64, d.height as f64);
                if let Some(l) = d.active_mut() {
                    let mut mask = Mask::white();
                    if let Some(selection) = selection {
                        mask.raster = Arc::new(Raster::from_rgba(
                            selection.width,
                            selection.height,
                            selection
                                .canvas_coverage()
                                .iter()
                                .flat_map(|&c| [c, c, c, 255])
                                .collect(),
                        ));
                        mask.placement = Some(transform);
                    }
                    l.mask = Some(mask);
                }
            }),
            Command::DeleteMask => self.session_mut().edit("Delete mask", |d| {
                if let Some(l) = d.active_mut() {
                    l.mask = None;
                }
            }),
            Command::CopyMask => {
                self.mask_clipboard = self.session().doc.active().and_then(|l| l.mask.clone())
            }
            Command::PasteMask => {
                if let Some(mask) = self.mask_clipboard.clone() {
                    self.session_mut().edit("Paste mask", |d| {
                        if let Some(l) = d.active_mut() {
                            l.mask = Some(mask);
                        }
                    });
                }
            }
            Command::LoadMask => {
                if let Some(id) = self.session().doc.active_layer_id {
                    self.background_edit("Select masked area", move |d| {
                        d.selection = Some(crate::edit::layer_selection(d, id, true)?);
                        Ok(())
                    });
                }
            }
            Command::BlurMask => {
                self.finish_color_picker(false);
                self.mask_target = true;
                self.begin_dialog(Dialog::Filter {
                    kind: "Gaussian Blur".into(),
                });
                self.dialog_value = adjustment_defaults("Gaussian Blur");
            }
            Command::InvertPixels => {
                if let Some(id) = self.session().doc.active_layer_id {
                    let mask = self.mask_target;
                    self.apply("Invert pixels", |d| crate::edit::invert_pixels(d, id, mask));
                }
            }
            Command::InvertMask => self.session_mut().edit("Invert mask", |d| {
                if let Some(m) = d.active_mut().and_then(|l| l.mask.as_mut()) {
                    let r = Arc::make_mut(&mut m.raster);
                    let mut p = r.to_rgba();
                    for pixel in p.as_chunks_mut::<4>().0 {
                        let v = 255 - pixel[0];
                        pixel.copy_from_slice(&[v, v, v, 255]);
                    }
                    *r = Raster::from_rgba(r.width, r.height, p);
                }
            }),
            Command::ApplyMask => {
                if let Some(id) = self.session().doc.active_layer_id {
                    self.apply("Apply mask", |d| crate::edit::apply_mask(d, id));
                }
            }
            Command::Clip => self.session_mut().edit("Clipping mask", |d| {
                if let Some(i) = d
                    .active_layer_id
                    .and_then(|id| d.layers.iter().position(|l| l.id == id))
                {
                    let source = if d.layers[i].mask_source_id.is_some() {
                        None
                    } else {
                        d.layers[..i]
                            .iter()
                            .rev()
                            .find(|l| l.parent_id == d.layers[i].parent_id && !l.is_group)
                            .map(|l| l.id)
                    };
                    d.layers[i].mask_source_id = source;
                }
            }),
            Command::SelectAll => self.session_mut().edit("Select all", |d| {
                d.selection = Some(Selection::full(d.width, d.height))
            }),
            Command::Deselect => self.session_mut().edit("Deselect", |d| d.selection = None),
            Command::InvertSelection => self.session_mut().edit("Inverse selection", |d| {
                d.selection = d.selection.as_ref().map_or_else(
                    || Some(Selection::full(d.width, d.height)),
                    selection::inverse,
                )
            }),
            Command::Subject => {
                self.segment(SegmentOperation::Selection {
                    point: None,
                    mode: self.selection_mode,
                });
            }
            Command::LoadAlpha => {
                self.background_edit("Load alpha", |doc| {
                    let r = if let Some(layer) = doc.active() {
                        let mut copy = doc.clone();
                        copy.layers = vec![layer.clone()];
                        copy.layers[0].parent_id = None;
                        render::render_document(&copy)
                    } else {
                        render::render_document(doc)
                    };
                    doc.selection = Some(selection::alpha(&r, false));
                    Ok(())
                });
            }
            Command::Expand | Command::Contract | Command::Feather => {
                self.prompt_selection_amount(c);
            }
            Command::CropSelection => {
                let bounds = self
                    .session()
                    .doc
                    .selection
                    .as_ref()
                    .and_then(Selection::bounds);
                if let Some([x0, y0, x1, y1]) = bounds {
                    self.apply_crop([x0 as f64, y0 as f64, (x1 - x0) as f64, (y1 - y0) as f64]);
                }
            }
            Command::CanvasSize | Command::ImageSize => {
                self.new_size = [self.session().doc.width, self.session().doc.height];
                self.size_original = self.new_size;
                self.size_relative = false;
                self.size_delta = [0, 0];
                self.new_resolution = self.session().doc.resolution;
                self.begin_dialog(if matches!(c, Command::CanvasSize) {
                    Dialog::CanvasSize
                } else {
                    Dialog::ImageSize
                });
            }
            Command::Trim => {
                self.dialog_value = json!({"basis":"Transparent","tolerance":0,"top":true,"left":true,"bottom":true,"right":true});
                self.begin_dialog(Dialog::Trim);
            }
            Command::FlipCanvasX | Command::FlipCanvasY => {
                self.session_mut().edit("Flip canvas", |d| {
                    crate::edit::flip_canvas(d, matches!(c, Command::FlipCanvasX));
                });
            }
            Command::Fit => {
                self.session_mut().zoom = 0.;
                self.session_mut().pan = Vec2::ZERO;
            }
            Command::Actual => self.session_mut().zoom = 1.,
            Command::Grid => self.grid = !self.grid,
            Command::Rulers => self.rulers = !self.rulers,
            Command::GridSettings => self.begin_dialog(Dialog::Grid),
            Command::Guides => self.begin_dialog(Dialog::Guides),
            Command::ColorRange => {
                self.dialog_value = json!({"fuzziness":40,"colors":[self.foreground]});
                self.begin_dialog(Dialog::ColorRange);
            }
            Command::CropCommit => {
                self.crop_drag = None;
                if let Some(rect) = self.crop_rect.take() {
                    self.apply_crop(rect);
                }
            }
            Command::CropCancel => {
                self.crop_rect = None;
                self.crop_drag = None;
            }
            Command::Shortcuts => self.shortcuts_open = true,
        }
        let s = self.session_mut();
        s.selected.retain(|id| s.doc.layer(*id).is_some());
        if s.selected.is_empty() {
            s.selected = s.doc.active_layer_id.into_iter().collect();
        }
    }
}

const ADJUSTMENTS: &[&str] = &[
    "Hue/Saturation",
    "Levels",
    "Curves",
    "Exposure",
    "Gradient Map",
    "Grain",
    "Black & White",
    "Color Balance",
    "Invert",
    "Gaussian Blur",
    "Motion Blur",
    "Add Noise",
];
const FINISHING: &[&str] = &[
    "Vignette",
    "Bloom / Glow",
    "Tonal Contrast",
    "Lens Correction",
    "Dither",
    "Remove Background",
];
fn humanize_english(s: &str) -> String {
    let mut result = String::new();
    for c in s.chars() {
        if c.is_uppercase() && !result.is_empty() {
            result.push(' ');
        }
        result.push(c);
    }
    if let Some(c) = result.get_mut(..1) {
        c.make_ascii_uppercase();
    }
    result
}
fn humanize(s: &str) -> String {
    tr(&humanize_english(s)).into_owned()
}
fn adjustment_defaults(kind: &str) -> Value {
    if kind == "Remove Background" {
        return json!({"backgroundQuality":"Basic","refineEdges":12,"matteContrast":25,"shiftEdge":0});
    }
    crate::filters::default_settings(kind)
}
fn effect_defaults(kind: &str) -> Value {
    match kind {
        "stroke" => {
            json!({"enabled":true,"size":4,"red":0,"green":0,"blue":0,"opacity":1,"inside":false})
        }
        "shadow" | "innerShadow" => {
            json!({"enabled":true,"angle":120,"distance":12,"blur":8,"red":0,"green":0,"blue":0,"opacity":0.5})
        }
        "colorOverlay" => json!({"enabled":true,"red":1,"green":0,"blue":0,"opacity":0.5}),
        _ => json!({"enabled":true,"size":12,"red":1,"green":1,"blue":1,"opacity":0.7}),
    }
}
fn camera_raw_defaults() -> Value {
    crate::filters::camera_raw_defaults()
}
fn json_widgets(ui: &mut egui::Ui, value: &mut Value, id: &str) {
    ui.push_id(id, |ui| match value {
        Value::Object(map) => {
            for (k, v) in map {
                if id == "trim" && k == "basis" {
                    continue;
                }
                if v.is_object() || v.is_array() {
                    ui.collapsing(humanize(k), |ui| json_widgets(ui, v, k));
                } else {
                    ui.horizontal(|ui| {
                        ui.label(match (id, k.as_str()) {
                            ("trim", "left") => "左边".into(),
                            ("trim", "right") => "右边".into(),
                            _ => humanize(k),
                        });
                        json_widgets(ui, v, k);
                    });
                }
            }
        }
        Value::Array(values) => {
            for (i, v) in values.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("{}", i + 1));
                    json_widgets(ui, v, &i.to_string());
                });
            }
        }
        Value::Number(n) => {
            let mut v = n.as_f64().unwrap_or(0.);
            if ui.add(egui::DragValue::new(&mut v).speed(0.1)).changed() && v.is_finite() {
                *value = json!(v);
            }
        }
        Value::Bool(b) => {
            ui.checkbox(b, "");
        }
        Value::String(s) => {
            ui.text_edit_singleline(s);
        }
        _ => {
            ui.label("—");
        }
    });
}
fn size_unit_factor(unit: &str, resolution: f64, original: u32) -> f64 {
    let ppi = resolution.clamp(1.0, 9600.0);
    match unit {
        "Percent" => original.max(1) as f64 / 100.0,
        "Inches" => ppi,
        "Centimeters" => ppi / 2.54,
        "Millimeters" => ppi / 25.4,
        _ => 1.0,
    }
}
fn transform_frame(
    doc: &Document,
    selected: &HashSet<Uuid>,
    mask: bool,
) -> Option<crate::model::Transform> {
    let active = doc.active()?;
    if mask {
        return Some(active.mask.as_ref()?.placement.unwrap_or(active.transform));
    }
    if selected.len() <= 1 && !active.is_group {
        return Some(active.transform);
    }
    let ids = selected.iter().copied().collect::<Vec<_>>();
    let [x, y, w, h] = crate::edit::selected_bounds(doc, &ids);
    Some(crate::model::Transform::new(x, y, w.max(1.), h.max(1.)))
}
fn scale_selection(s: &Selection, w: u32, h: u32) -> Selection {
    let mut dst = Selection::empty(w, h);
    let out = Arc::make_mut(&mut dst.coverage);
    for y in 0..h {
        for x in 0..w {
            out[(y * w + x) as usize] = s.get(
                (x as u64 * s.width as u64 / w as u64) as u32,
                (y as u64 * s.height as u64 / h as u64) as u32,
            );
        }
    }
    dst
}
fn color_range_colors(value: &Value) -> Vec<[u8; 4]> {
    value["colors"]
        .as_array()
        .map(|colors| {
            colors
                .iter()
                .map(|c| {
                    std::array::from_fn(|i| {
                        c[i].as_f64()
                            .unwrap_or(if i == 3 { 255. } else { 0. })
                            .clamp(0., 255.) as u8
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
fn distort_corners(drag: &Drag, mask: bool) -> Option<(Uuid, [[f64; 2]; 4])> {
    let layer = drag.work_before.active()?;
    let handle = drag.handle?;
    if handle >= 8 {
        return None;
    }
    let transform = if mask {
        layer.mask.as_ref()?.placement.unwrap_or(layer.transform)
    } else {
        layer.transform
    };
    let mut corners = geometric_transform(transform).corners();
    let delta = [drag.last[0] - drag.start[0], drag.last[1] - drag.start[1]];
    for corner in if handle < 4 {
        vec![handle]
    } else {
        let edge = handle - 4;
        vec![edge, (edge + 1) % 4]
    } {
        corners[corner][0] += delta[0];
        corners[corner][1] += delta[1];
    }
    Some((layer.id, corners))
}
fn geometric_transform(mut transform: crate::model::Transform) -> crate::model::Transform {
    transform.flip_x = false;
    transform.flip_y = false;
    transform
}
fn transform_handles(transform: crate::model::Transform, zoom: f32) -> [[f64; 2]; 9] {
    let transform = geometric_transform(transform);
    let corners = transform.corners();
    [
        corners[0],
        corners[1],
        corners[2],
        corners[3],
        transform.to_document([0.5, 0.]),
        transform.to_document([1., 0.5]),
        transform.to_document([0.5, 1.]),
        transform.to_document([0., 0.5]),
        transform.to_document([0.5, -24. / zoom.max(0.005) as f64 / transform.size[1]]),
    ]
}
fn resize_transform(
    old: crate::model::Transform,
    handle: usize,
    start: [f64; 2],
    point: [f64; 2],
    centered: bool,
    proportional: bool,
) -> crate::model::Transform {
    let geometric = geometric_transform(old);
    let a = geometric.from_document(start);
    let b = geometric.from_document(point);
    let delta = [(b[0] - a[0]) * old.size[0], (b[1] - a[1]) * old.size[1]];
    let left = matches!(handle, 0 | 3 | 7);
    let right = matches!(handle, 1 | 2 | 5);
    let top = matches!(handle, 0 | 1 | 4);
    let bottom = matches!(handle, 2 | 3 | 6);
    let mut size = old.size;
    let factor = if centered { 2. } else { 1. };
    if left || right {
        size[0] += delta[0] * factor * if left { -1. } else { 1. };
    }
    if top || bottom {
        size[1] += delta[1] * factor * if top { -1. } else { 1. };
    }
    if proportional {
        if !left && !right {
            size[0] = size[1].abs() * old.size[0] / old.size[1];
        } else if !top && !bottom {
            size[1] = size[0].abs() * old.size[1] / old.size[0];
        } else if (size[0] / old.size[0]).abs() > (size[1] / old.size[1]).abs() {
            size[1] = size[0].abs() * old.size[1] / old.size[0] * size[1].signum();
        } else {
            size[0] = size[1].abs() * old.size[0] / old.size[1] * size[0].signum();
        }
    }
    let offset = [
        if centered {
            (old.size[0] - size[0]) / 2.
        } else if left {
            old.size[0] - size[0]
        } else {
            0.
        },
        if centered {
            (old.size[1] - size[1]) / 2.
        } else if top {
            old.size[1] - size[1]
        } else {
            0.
        },
    ];
    let center = geometric.to_document([
        (offset[0] + size[0] / 2.) / old.size[0],
        (offset[1] + size[1] / 2.) / old.size[1],
    ]);
    let mut new = old;
    new.size = size.map(|v| v.abs().max(1.));
    new.origin = [center[0] - new.size[0] / 2., center[1] - new.size[1] / 2.];
    new.flip_x ^= size[0] < 0.;
    new.flip_y ^= size[1] < 0.;
    new
}
fn hit_layer(doc: &Document, point: [f64; 2]) -> Option<Uuid> {
    doc.layers
        .iter()
        .rev()
        .find(|l| {
            if !l.visible || l.is_group || l.opacity == 0. {
                return false;
            }
            let mut parent = l.parent_id;
            for _ in 0..64 {
                let Some(id) = parent else {
                    break;
                };
                let Some(layer) = doc.layer(id) else {
                    return false;
                };
                if !layer.visible || layer.opacity == 0. {
                    return false;
                }
                parent = layer.parent_id;
            }
            let p = l.transform.from_document(point);
            if p.iter().any(|v| *v < 0. || *v >= 1.) {
                return false;
            }
            l.image.as_ref().is_some_and(|r| {
                r.get(
                    (p[0] * r.width as f64) as u32,
                    (p[1] * r.height as f64) as u32,
                )[3] > 0
            })
        })
        .map(|l| l.id)
}
fn package_stamp(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    fn add(path: &Path, hash: &mut impl Hasher) {
        if let Ok(m) = path.metadata() {
            path.hash(hash);
            m.len().hash(hash);
            m.modified().unwrap_or(SystemTime::UNIX_EPOCH).hash(hash);
        }
    }
    if !path.is_dir() {
        add(path, &mut hash);
        return hash.finish();
    }
    add(&path.join("manifest.json"), &mut hash);
    if let Ok(files) = std::fs::read_dir(path.join("images")) {
        let mut paths: Vec<_> = files.filter_map(Result::ok).map(|f| f.path()).collect();
        paths.sort();
        for p in paths {
            add(&p, &mut hash);
        }
    }
    hash.finish()
}
fn selection_repaint_delay(time: f64) -> Duration {
    Duration::from_secs_f64((0.25 - time.rem_euclid(0.25)).clamp(0.000_001, 0.25))
}
fn preferences_path() -> PathBuf {
    std::env::var_os("COMPOSITOR_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| PathBuf::from("."))
        })
        .join("preferences")
}
fn clipboard_sequence() -> u32 {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn GetClipboardSequenceNumber() -> u32;
        }
        // Reads a process-independent counter; Windows retains no pointer.
        unsafe { GetClipboardSequenceNumber() }
    }
    #[cfg(not(windows))]
    {
        0
    }
}
fn raw_settings_key(s: image_io::RawDevelopSettings) -> [u32; 4] {
    [s.exposure, s.temperature, s.tint, s.boost].map(f32::to_bits)
}
pub fn preferences_directory() -> PathBuf {
    preferences_path()
}
fn shortcuts_path() -> PathBuf {
    preferences_path().join("shortcuts.json")
}
fn load_recent() -> Vec<PathBuf> {
    std::fs::read(preferences_path().join("recent.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
fn segmentation_worker(sender: WorkerSender) -> mpsc::Sender<SegmentRequest> {
    let (tx, rx) = mpsc::channel::<SegmentRequest>();
    let worker = std::thread::Builder::new()
        .name("subject-segmentation".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let mut engine = None;
            let mut preview_cache: Option<(
                std::sync::Weak<Raster>,
                Arc<Raster>,
                crate::segmentation::Segmentation,
            )> = None;
            while let Ok(request) = rx.recv() {
                if let SegmentOperation::Preview {
                    layer,
                    settings,
                    generation,
                } = &request.operation
                {
                    let mut sample = None;
                    let result = gui_threads::run("抠图预览", || {
                        (|| -> anyhow::Result<(Raster, usize)> {
                            if engine.is_none() {
                                engine =
                                    Some(crate::segmentation::SegmentationEngine::load_default()?);
                            }
                            let source = request
                                .before
                                .layer(*layer)
                                .and_then(|l| l.image.clone())
                                .ok_or_else(|| anyhow::anyhow!("Select a pixel layer"))?;
                            if preview_cache.as_ref().is_none_or(|(original, _, _)| {
                                !original.ptr_eq(&Arc::downgrade(&source))
                            }) {
                                let mut preview_document =
                                    Document::new(source.width, source.height);
                                let mut pixel_layer =
                                    Layer::raster("Subject preview", source.as_ref().clone());
                                pixel_layer.id = *layer;
                                preview_document.active_layer_id = Some(*layer);
                                preview_document.layers = vec![pixel_layer];
                                let prepared = crate::preview::prepare(&preview_document, 1000);
                                let small =
                                    prepared.active().and_then(|l| l.image.clone()).unwrap();
                                let prediction = engine.as_ref().unwrap().predict(&small)?;
                                preview_cache = Some((Arc::downgrade(&source), small, prediction));
                            }
                            let (original, small, prediction) = preview_cache.as_ref().unwrap();
                            let original = original.upgrade().ok_or_else(|| {
                                anyhow::anyhow!("The subject source was released")
                            })?;
                            sample = Some(small.clone());
                            let settings = crate::matte::preview_settings(
                                settings,
                                small.width as f64 / original.width as f64,
                            );
                            let alpha = crate::matte::refine(&prediction.alpha, small, &settings)?;
                            let mut preview_document = request.before.clone();
                            preview_document.layer_mut(*layer).unwrap().image = Some(small.clone());
                            crate::matte::apply_subject_mask(
                                &mut preview_document,
                                *layer,
                                &alpha,
                            )?;
                            let layer = preview_document
                                .layer(*layer)
                                .ok_or_else(|| anyhow::anyhow!("The preview layer was removed"))?;
                            let mut rgba = small.to_rgba();
                            for (i, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                                let point = layer.transform.to_document([
                                    (i as u32 % small.width) as f64 / small.width as f64
                                        + 0.5 / small.width as f64,
                                    (i as u32 / small.width) as f64 / small.height as f64
                                        + 0.5 / small.height as f64,
                                ]);
                                pixel[3] = (pixel[3] as f64
                                    * render::raster_mask_coverage(layer, point))
                                .round()
                                .clamp(0., 255.) as u8;
                            }
                            Ok((Raster::from_rgba(small.width, small.height, rgba), 0))
                        })()
                        .map_err(|e| format!("{e:#}"))
                    });
                    let _ = sender.send(Worker::DialogPreview {
                        generation: *generation,
                        sample,
                        graded_sample: None,
                        display_sample: None,
                        result,
                    });
                    continue;
                }
                let name = match &request.operation {
                    SegmentOperation::Selection { point: Some(_), .. } => "Object selection",
                    SegmentOperation::Selection { .. } => "Select subject",
                    SegmentOperation::Remove { .. } => "Remove background",
                    SegmentOperation::Preview { .. } => unreachable!("Preview was handled above"),
                }
                .to_owned();
                let mut doc = request.before.clone();
                let result = gui_threads::run("主体处理", || {
                    (|| -> anyhow::Result<Document> {
                        if engine.is_none() {
                            engine = Some(crate::segmentation::SegmentationEngine::load_default()?);
                        }
                        let engine = engine.as_ref().unwrap();
                        match request.operation {
                            SegmentOperation::Selection { point, mode } => {
                                let raster =
                                    crate::preview::PreviewCache::default().render(&doc, 1600);
                                let predicted = engine.predict(&raster)?;
                                let selection = if let Some(p) = point {
                                    predicted.object_at([
                                        (p[0] * raster.width as f64 / doc.width as f64)
                                            .floor()
                                            .max(0.) as u32,
                                        (p[1] * raster.height as f64 / doc.height as f64)
                                            .floor()
                                            .max(0.) as u32,
                                    ])?
                                } else {
                                    predicted.selection()
                                };
                                let selection = scale_selection(&selection, doc.width, doc.height);
                                doc.selection = Some(selection::combine(
                                    doc.selection.as_ref(),
                                    &selection,
                                    mode,
                                ));
                            }
                            SegmentOperation::Remove { layer, settings } => {
                                let original = doc
                                    .layer(layer)
                                    .and_then(|l| l.image.clone())
                                    .ok_or_else(|| anyhow::anyhow!("Select a pixel layer"))?;
                                let predicted = engine.predict(&original)?;
                                let alpha =
                                    crate::matte::refine(&predicted.alpha, &original, &settings)?;
                                crate::matte::apply_subject_mask(&mut doc, layer, &alpha)?;
                            }
                            SegmentOperation::Preview { .. } => {
                                unreachable!("Preview was handled above")
                            }
                        }
                        Ok(doc)
                    })()
                    .map_err(|e| format!("{e:#}"))
                });
                let _ = sender.send(Worker::Edit {
                    id: request.session,
                    revision: request.revision,
                    serial: request.serial,
                    name,
                    before: request.before,
                    result,
                });
            }
        });
    if let Err(error) = worker {
        // A failed spawn drops the receiver. Requests report the unavailable
        // worker through their existing error path without aborting startup.
        log::error!("Start subject segmentation worker: {error}");
    }
    tx
}
fn effective_selection_mode(choice: SelectionMode, modifiers: egui::Modifiers) -> SelectionMode {
    if modifiers.alt {
        SelectionMode::Subtract
    } else if modifiers.shift {
        SelectionMode::Add
    } else {
        choice
    }
}
fn selection_drag_box(drag: &Drag, shift: bool) -> [f64; 4] {
    selection::drag_box(
        drag.start.map(f64::round),
        drag.last.map(f64::round),
        drag.constrain_armed && shift,
        false,
    )
}
fn camera_raw_preview_is_clean(settings: &Value, clean: &Value) -> bool {
    let raw = settings.get("cameraRaw").unwrap_or(settings);
    let clean = clean.get("cameraRaw").unwrap_or(clean);
    crate::filters::number(raw, "previewScale", 1.)
        == crate::filters::number(clean, "previewScale", 1.)
        && crate::filters::number(raw, "clipping", 0.) == 0.
        && !crate::filters::boolean(raw, "showShadowClipping", false)
        && !crate::filters::boolean(raw, "showHighlightClipping", false)
        && !crate::filters::boolean(&raw["detail"], "showSharpenMask", false)
        && !raw["mixer"]["points"].as_array().is_some_and(|points| {
            points
                .iter()
                .any(|point| crate::filters::boolean(point, "visualize", false))
        })
}

fn committed_filter_settings(kind: &str, mut settings: Value) -> Value {
    if kind != "Camera Raw Filter" {
        return settings;
    }
    settings = crate::filter_panel::effective_camera_raw_settings(&settings);
    clear_raw_preview_settings(&mut settings);
    if let Some(raw) = settings.get_mut("cameraRaw") {
        clear_raw_preview_settings(raw);
    }
    settings
}
fn clear_raw_preview_settings(raw: &mut Value) {
    if let Some(object) = raw.as_object_mut() {
        for key in [
            "previewScale",
            "clipping",
            "showShadowClipping",
            "showHighlightClipping",
        ] {
            object.remove(key);
        }
    }
    if let Some(detail) = raw.get_mut("detail").and_then(Value::as_object_mut) {
        detail.remove("showSharpenMask");
    }
    if let Some(points) = raw
        .get_mut("mixer")
        .and_then(|m| m.get_mut("points"))
        .and_then(Value::as_array_mut)
    {
        for point in points {
            if let Some(point) = point.as_object_mut() {
                point.remove("visualize");
            }
        }
    }
}
fn tool_bindings() -> [(&'static str, Tool); 16] {
    [
        ("Move tool", Tool::Move),
        ("Marquee tool", Tool::Rectangle),
        ("Lasso tool", Tool::Lasso),
        ("Magic tool", Tool::Wand),
        ("Crop tool", Tool::Crop),
        ("Brush tool", Tool::Brush),
        ("Eraser tool", Tool::Eraser),
        ("Healing tool", Tool::Heal),
        ("Clone tool", Tool::Clone),
        ("Blur tool", Tool::Blur),
        ("Gradient tool", Tool::Gradient),
        ("Shape tool", Tool::Shape),
        ("Text tool", Tool::Text),
        ("Eyedropper tool", Tool::Eyedropper),
        ("Hand tool", Tool::Hand),
        ("Zoom tool", Tool::Zoom),
    ]
}
fn load_shortcuts() -> BTreeMap<String, String> {
    let mut shortcuts: BTreeMap<String, String> = std::fs::read(shortcuts_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    for (name, chord) in [
        ("New", "Ctrl+N"),
        ("Open", "Ctrl+O"),
        ("Save", "Ctrl+S"),
        ("Save As", "Ctrl+Shift+S"),
        ("Undo", "Ctrl+Z"),
        ("Redo", "Ctrl+Shift+Z"),
        ("Copy", "Ctrl+C"),
        ("Cut", "Ctrl+X"),
        ("Paste", "Ctrl+V"),
        ("Select All", "Ctrl+A"),
        ("Deselect", "Ctrl+D"),
        ("Close", "Ctrl+W"),
        ("Swap colors", "X"),
        ("Default colors", "D"),
        ("Cycle tool mode", "Tab"),
    ] {
        shortcuts.entry(name.into()).or_insert(chord.into());
    }
    for ((name, _), chord) in tool_bindings().into_iter().zip([
        "V", "M", "L", "W", "C", "B", "E", "J", "S", "R", "G", "U", "T", "I", "H", "Z",
    ]) {
        shortcuts.entry(name.into()).or_insert(chord.into());
    }
    shortcuts
}
fn adapt_native_clipboard(
    input: &mut egui::InputState,
    paste_key_handled: &mut bool,
    accepts_clipboard: bool,
) {
    let mut native = [false; 3];
    let mut paste_release = None;
    let mut accepts_paste_release = input.focused;
    let paste_key_pressed = input.raw.events.iter().any(|event| {
        matches!(
            event,
            egui::Event::Key {
                key: Key::V,
                pressed: true,
                ..
            }
        )
    });
    for event in &input.events {
        match event {
            egui::Event::Cut => native[0] = true,
            egui::Event::Copy => native[1] = true,
            egui::Event::Paste(_) => {
                native[2] = true;
                *paste_key_handled = true;
            }
            egui::Event::Key {
                key: Key::V,
                pressed,
                modifiers,
                ..
            } => {
                if *pressed {
                    // Raw events exclude our synthetic presses and retain real
                    // presses even when their release arrives in the same frame.
                    *paste_key_handled |= paste_key_pressed;
                } else {
                    // winit swallows Ctrl+V presses when the clipboard has no text.
                    // Recover its release for image/internal pastes, once per key cycle.
                    if !*paste_key_handled && modifiers.ctrl && accepts_paste_release {
                        paste_release = Some(*modifiers);
                    }
                    *paste_key_handled = false;
                }
            }
            egui::Event::WindowFocused(false) => {
                *paste_key_handled = false;
                paste_release = None;
                accepts_paste_release = false;
            }
            _ => {}
        }
    }
    if !accepts_clipboard {
        return;
    }
    input.events.retain(|event| {
        !matches!(
            event,
            egui::Event::Cut | egui::Event::Copy | egui::Event::Paste(_)
        ) && !matches!(event, egui::Event::Key { key: Key::V, pressed: false, modifiers, .. }
                if modifiers.ctrl)
    });
    for (index, key) in [Key::X, Key::C, Key::V].into_iter().enumerate() {
        if !native[index] && !(key == Key::V && paste_release.is_some()) {
            continue;
        }
        if input.events.iter().any(|event| {
            matches!(event, egui::Event::Key { key: other, pressed: true, modifiers, .. }
                if *other == key && modifiers.ctrl)
        }) {
            continue;
        }
        // Feed the normal binding dispatcher, so remapped/disabled shortcuts and
        // more specific modifiers retain their existing priority.
        let modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..if key == Key::V {
                paste_release.unwrap_or(input.modifiers)
            } else {
                input.modifiers
            }
        };
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        });
    }
}

fn parse_shortcut(s: &str) -> Option<egui::KeyboardShortcut> {
    let mut m = egui::Modifiers::NONE;
    let mut key = None;
    for part in s.split('+') {
        match part.trim().to_lowercase().as_str() {
            "ctrl" => {
                m.ctrl = true;
                m.command = true;
            }
            "shift" => m.shift = true,
            "alt" => m.alt = true,
            k => {
                key = Key::ALL
                    .iter()
                    .copied()
                    .find(|candidate| candidate.name().eq_ignore_ascii_case(k));
            }
        }
    }
    Some(egui::KeyboardShortcut::new(m, key?))
}

impl EditorApp {
    fn segment(&mut self, operation: SegmentOperation) {
        if self.session().edit_busy {
            self.message = Some("This document is already processing an operation".into());
            return;
        }
        let request = SegmentRequest {
            session: self.session().id,
            before: self.session().doc.clone(),
            revision: self.session().history.revision(),
            serial: self.session().serial,
            operation,
        };
        match self.segment_sender.send(request) {
            Ok(()) => self.session_mut().edit_busy = true,
            Err(e) => self.message = Some(e.to_string()),
        }
    }
    fn drop_layers(&mut self, payload: &LayerPayload, parent: Option<Uuid>, above: Option<Uuid>) {
        let source = self
            .tabs
            .iter()
            .find(|s| s.id == payload.document)
            .map(|s| s.doc.clone());
        let Some(source) = source else {
            return;
        };
        self.prepare_canvas_for_layer_drop(&source);
        if payload.document == self.session().id {
            let ids: Vec<_> = source
                .layers
                .iter()
                .filter(|l| {
                    payload.ids.contains(&l.id)
                        && !payload.ids.iter().any(|&id| {
                            id != l.id && crate::edit::descendants(&source, id).contains(&l.id)
                        })
                })
                .map(|l| l.id)
                .collect();
            if above.is_some_and(|id| ids.contains(&id)) {
                return;
            }
            self.apply("Reorder layers", |d| {
                let mut anchor = above;
                for id in ids {
                    crate::edit::place_layer(d, id, parent, anchor)?;
                    anchor = Some(id);
                }
                Ok(())
            });
        } else {
            self.apply("Copy layers from project", |d| {
                let copied = crate::edit::copy_layers_to(&source, d, &payload.ids, None)?;
                for id in copied {
                    if d.layer(id).is_some_and(|l| l.parent_id.is_none()) {
                        crate::edit::place_layer(d, id, parent, above)?;
                    }
                }
                Ok(())
            });
        }
    }
    fn background_edit(
        &mut self,
        name: &str,
        operation: impl FnOnce(&mut Document) -> anyhow::Result<()> + Send + 'static,
    ) {
        if self.session().edit_busy {
            self.message = Some("This document is already processing an operation".into());
            return;
        }
        let before = self.session().doc.clone();
        let mut d = before.clone();
        let id = self.session().id;
        let revision = self.session().history.revision();
        let serial = self.session().serial;
        let name = name.to_owned();
        let sender = self.sender.clone();
        match gui_threads::spawn("后台编辑", move || {
            let result = gui_threads::run("后台编辑", || {
                operation(&mut d).map(|_| d).map_err(|e| format!("{e:#}"))
            });
            let _ = sender.send(Worker::Edit {
                id,
                revision,
                serial,
                name,
                before,
                result,
            });
        }) {
            Ok(_) => self.session_mut().edit_busy = true,
            Err(error) => self.message = Some(error),
        }
    }
    fn apply(&mut self, name: &str, f: impl FnOnce(&mut Document) -> anyhow::Result<()>) {
        let before = self.session().doc.clone();
        let result = f(&mut self.session_mut().doc);
        let result = result.and_then(|_| self.check_workspace(&self.session().doc));
        match result {
            Ok(()) => {
                let s = self.session_mut();
                if s.history.record(name, before, &s.doc) {
                    s.changed();
                }
            }
            Err(e) => {
                self.session_mut().doc = before;
                self.message = Some(format!("{e:#}"));
            }
        }
    }
    fn apply_crop(&mut self, rect: [f64; 4]) {
        self.apply("Crop", |d| crate::edit::crop(d, rect));
        self.session_mut().zoom = 0.;
    }
    fn copy(&mut self, merged: bool, cut: bool) {
        let d = self.session().doc.clone();
        let ids = self.session().selected.iter().copied().collect::<Vec<_>>();
        if d.selection.is_some() || merged || self.mask_target {
            let clipboard = match crate::clipboard_pixels::copy_pixels(&d, merged, self.mask_target)
            {
                Ok(Some(clipboard)) => clipboard,
                Ok(None) => return,
                Err(e) => {
                    self.message = Some(e.to_string());
                    return;
                }
            };
            if let Ok(mut system) = arboard::Clipboard::new() {
                let _ = system.set_image(arboard::ImageData {
                    width: clipboard.0.width as usize,
                    height: clipboard.0.height as usize,
                    bytes: std::borrow::Cow::Owned(clipboard.0.to_rgba()),
                });
            }
            self.pixel_clipboard = Some(clipboard);
            self.layer_clipboard = None;
            if cut {
                let mask = self.mask_target;
                if let Some(id) = d.active_layer_id {
                    self.apply("Cut pixels", |d| crate::edit::clear(d, id, mask));
                }
            }
        } else {
            self.layer_clipboard = Some((d, ids.clone()));
            self.pixel_clipboard = None;
            if cut {
                self.apply("Cut layers", |d| crate::edit::delete_layers(d, &ids));
            }
        }
        self.clipboard_sequence = clipboard_sequence();
    }
    fn paste(&mut self) {
        if self.clipboard_sequence != clipboard_sequence() {
            self.layer_clipboard = None;
            self.pixel_clipboard = None;
        }
        if let Some((source, ids)) = self.layer_clipboard.clone() {
            if self.is_welcome() {
                self.install_canvas_session(Session::new(
                    Document::new(source.width, source.height),
                    None,
                ));
            }
            self.apply("Paste layers", |d| {
                crate::edit::copy_layers_to(&source, d, &ids, None).map(|_| ())
            });
            return;
        }
        let clipboard = self.pixel_clipboard.clone().or_else(|| {
            let mut clipboard = arboard::Clipboard::new().ok()?;
            let image = clipboard.get_image().ok()?;
            if !Document::valid_size(image.width as u32, image.height as u32) {
                return None;
            }
            Some((
                Raster::from_rgba(
                    image.width as u32,
                    image.height as u32,
                    image.bytes.into_owned(),
                ),
                [0., 0.],
            ))
        });
        if let Some((raster, origin)) = clipboard {
            if self.is_welcome() {
                self.install_canvas_session(Session::new(
                    Document::new(raster.width, raster.height),
                    None,
                ));
            }
            self.apply("Paste pixels", |d| {
                let mut l = Layer::raster("粘贴的像素", raster);
                l.transform.origin = origin;
                d.active_layer_id = Some(l.id);
                d.layers.push(l);
                Ok(())
            });
        }
    }
    fn merge(&mut self, down: bool) {
        let document = &self.session().doc;
        let mut ids = self.session().selected.iter().copied().collect::<Vec<_>>();
        if down && let Some(id) = document.active_layer_id {
            ids = vec![id];
            if let Some(index) = document.layers.iter().position(|l| l.id == id)
                && let Some(layer) = document.layers[..index]
                    .iter()
                    .rev()
                    .find(|l| l.parent_id == document.layers[index].parent_id)
            {
                ids.push(layer.id);
            }
        }
        self.background_edit("Merge layers", move |document| {
            crate::edit::merge_layers(document, &ids).map(|_| ())
        });
    }
    fn begin_brush(&mut self, point: [f64; 2]) {
        use crate::brush::{BrushMode, BrushSettings, BrushStroke, HealingMode};
        let Some(id) = self.session().doc.active_layer_id else {
            return;
        };
        let mode = match self.tool {
            Tool::Eraser => BrushMode::Erase,
            Tool::Clone => {
                let Some(source) = self.clone_source else {
                    self.message = Some("Alt-click to set a clone source".into());
                    return;
                };
                let offset = if self.clone_aligned {
                    *self
                        .clone_offset
                        .get_or_insert([source[0] - point[0], source[1] - point[1]])
                } else {
                    [source[0] - point[0], source[1] - point[1]]
                };
                BrushMode::Clone { offset }
            }
            Tool::Heal => BrushMode::Heal(match self.healing_mode {
                1 => HealingMode::CreateTexture,
                2 => HealingMode::ProximityMatch,
                _ => HealingMode::ContentAware,
            }),
            Tool::Blur => BrushMode::Blur {
                radius: self.blur_radius,
            },
            Tool::Smudge => BrushMode::Smudge,
            Tool::Liquify => BrushMode::Liquify,
            _ => BrushMode::Paint,
        };
        let sample = (self.tool == Tool::Clone && self.clone_all)
            .then(|| Arc::new(render::render_document(&self.session().doc)));
        let settings = BrushSettings {
            diameter: self.diameter,
            hardness: self.hardness,
            opacity: self.opacity,
            color: self.palette_color(false),
            smoothing: self.smoothing * 100.,
            mode,
        };
        match BrushStroke::new(&self.session().doc, id, self.mask_target, settings, sample) {
            Ok(mut stroke) => {
                if let Err(e) = stroke.append(point) {
                    self.message = Some(e.to_string());
                }
                self.brush_stroke = Some(stroke);
                self.show_brush();
            }
            Err(e) => self.message = Some(e.to_string()),
        }
    }
    fn finish_brush(&mut self, before: Document, last: [f64; 2]) {
        let Some(mut stroke) = self.brush_stroke.take() else {
            return;
        };
        let owner = (self.session().id, stroke.layer_id, stroke.mask, last);
        self.session_mut().doc = before.clone();
        match stroke
            .finish_at(last)
            .and_then(|_| stroke.commit(&mut self.session_mut().doc))
        {
            Ok(_) => {
                let session = self.session_mut();
                session.history.record("Brush stroke", before, &session.doc);
                session.changed();
                self.last_brush = Some(owner);
            }
            Err(e) => {
                self.session_mut().doc = before;
                self.session_mut().changed();
                self.message = Some(e.to_string());
            }
        }
    }
    fn show_brush(&mut self) {
        if let Some(stroke) = &self.brush_stroke {
            let (id, mask, raster, transform) = (
                stroke.layer_id,
                stroke.mask,
                stroke.raster.clone(),
                stroke.transform,
            );
            if let Some(l) = self.session_mut().doc.layer_mut(id) {
                if mask {
                    if let Some(m) = &mut l.mask {
                        m.raster = Arc::new(raster);
                        m.placement = Some(transform);
                    }
                } else {
                    if let Some(m) = &mut l.mask
                        && m.placement.is_none()
                    {
                        m.placement = Some(l.transform);
                    }
                    l.image = Some(Arc::new(raster));
                    l.transform = transform;
                    l.text = None;
                    l.shape = None;
                }
            }
            self.session_mut().changed();
        }
    }
    fn snap_options(&self, include_centers: bool) -> crate::crop_interaction::SnapOptions {
        crate::crop_interaction::SnapOptions {
            canvas: self.snap_targets[0],
            layers: self.snap_targets[1],
            guides: self.snap_targets[2],
            grid_spacing: (self.grid && self.snap_targets[3])
                .then_some(self.grid_spacing / self.grid_subdivisions.max(1) as f64),
            include_centers,
        }
    }
    fn transform_snap(
        &self,
        doc: &Document,
        include_centers: bool,
    ) -> crate::crop_interaction::CropSnap {
        let excluding = self
            .session()
            .selected
            .iter()
            .flat_map(|id| crate::edit::descendants(doc, *id))
            .collect();
        crate::crop_interaction::targets(
            doc,
            self.snap_options(include_centers),
            &excluding,
            if self.snap {
                (6. * self.session().pixels_per_point / self.session().zoom.max(0.01)) as f64
            } else {
                0.
            },
        )
    }
    fn snap_delta(&self, delta: [f64; 2], before: &Document) -> [f64; 2] {
        if !self.snap {
            return delta;
        }
        let ids: Vec<_> = self.session().selected.iter().copied().collect();
        let bounds = if self.mask_target {
            transform_frame(before, &self.session().selected, true).map(|t| {
                let p = t.corners();
                let min = [
                    p.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min),
                    p.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min),
                ];
                let max = [
                    p.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max),
                    p.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max),
                ];
                [min[0], min[1], max[0] - min[0], max[1] - min[1]]
            })
        } else {
            before
                .selection
                .as_ref()
                .and_then(|s| s.bounds())
                .map(|[x0, y0, x1, y1]| [x0 as f64, y0 as f64, (x1 - x0) as f64, (y1 - y0) as f64])
        }
        .unwrap_or_else(|| crate::edit::selected_bounds(before, &ids));
        let snap = self.transform_snap(before, true);
        std::array::from_fn(|axis| {
            let targets = if axis == 0 { &snap.xs } else { &snap.ys };
            let mut offset: Option<f64> = None;
            for fraction in [0., 0.5, 1.] {
                let value = bounds[axis] + bounds[axis + 2] * fraction + delta[axis];
                for target in targets {
                    let shift = target - value;
                    if shift.abs() <= snap.tolerance
                        && offset.is_none_or(|old| shift.abs() < old.abs())
                    {
                        offset = Some(shift);
                    }
                }
            }
            delta[axis] + offset.unwrap_or(0.)
        })
    }
    fn canvas(&mut self, ui: &mut egui::Ui) {
        if self.tool == Tool::Crop && self.crop_session != Some(self.session().id) {
            self.initialize_crop();
        }
        let (space, response) =
            ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        ui.painter().rect_filled(
            space,
            0.,
            crate::chrome::theme::palette(ui.ctx(), ui.visuals()).canvas,
        );
        let (w, h) = (
            self.session().doc.width as f32,
            self.session().doc.height as f32,
        );
        let pixels_per_point = ui.ctx().pixels_per_point();
        self.session_mut().pixels_per_point = pixels_per_point;
        if self.session().zoom == 0. {
            self.session_mut().zoom = (((space.width() - 60.) / w).min((space.height() - 60.) / h)
                * pixels_per_point)
                .clamp(0.005, 8.);
        }
        let zoom = self.session().zoom / pixels_per_point;
        let size = Vec2::new(w * zoom, h * zoom);
        let mut rect = Rect::from_center_size(space.center() + self.session().pan, size);
        if self.session().zoom >= 1.0 {
            // Native texel edges share the physical pixel grid, including at fractional DPI.
            let origin = (rect.min.to_vec2() * pixels_per_point).round() / pixels_per_point;
            rect = Rect::from_min_size(origin.to_pos2(), size);
        }
        let painter = ui.painter().with_clip_rect(space);
        let visible = rect.intersect(space);
        let view = crate::preview::ViewRequest::new(
            [self.session().doc.width, self.session().doc.height],
            [
                ((visible.left() - rect.left()) / zoom) as f64,
                ((visible.top() - rect.top()) / zoom) as f64,
                (visible.width() / zoom) as f64,
                (visible.height() / zoom) as f64,
            ],
            self.session().zoom as f64,
        );
        self.session_mut().preview_view = view;
        if let Some(view) = view {
            self.render_canvas(ui.ctx(), view);
        }
        crate::canvas_chrome::document_backdrop(&painter, rect, visible);
        if let Some(texture) = self.filter_canvas_preview_texture() {
            painter.image(
                texture,
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)),
                Color32::WHITE,
            );
        } else if let Some(view) = self.session().rendered_view
            && let Some(texture) = self
                .session()
                .gpu_id
                .or_else(|| self.session().texture.as_ref().map(|texture| texture.id()))
        {
            let [x, y, width, height] = view.region;
            painter.image(
                texture,
                Rect::from_min_size(
                    rect.min + Vec2::new(x as f32 * zoom, y as f32 * zoom),
                    Vec2::new(width as f32 * zoom, height as f32 * zoom),
                ),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)),
                Color32::WHITE,
            );
        }
        crate::canvas_chrome::document_edge(&painter, rect);
        let doc_to_screen =
            |p: [f64; 2]| rect.min + Vec2::new(p[0] as f32 * zoom, p[1] as f32 * zoom);
        if self.grid {
            let spacing = (self.grid_spacing / self.grid_subdivisions.max(1) as f64) as f32 * zoom;
            if spacing >= 4. {
                let xs = (w * zoom / spacing).ceil() as u32;
                let ys = (h * zoom / spacing).ceil() as u32;
                if xs + ys < 5000 {
                    for i in 0..=xs {
                        let x = rect.left() + i as f32 * spacing;
                        painter.line_segment(
                            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                            Stroke::new(
                                0.5_f32,
                                Color32::from_rgba_unmultiplied(
                                    self.grid_color[0],
                                    self.grid_color[1],
                                    self.grid_color[2],
                                    self.grid_color[3],
                                ),
                            ),
                        );
                    }
                    for i in 0..=ys {
                        let y = rect.top() + i as f32 * spacing;
                        painter.line_segment(
                            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                            Stroke::new(
                                0.5_f32,
                                Color32::from_rgba_unmultiplied(
                                    self.grid_color[0],
                                    self.grid_color[1],
                                    self.grid_color[2],
                                    self.grid_color[3],
                                ),
                            ),
                        );
                    }
                }
            }
        }
        if self.pixel_grid && self.session().zoom >= 8. {
            for x in 0..=(w as u32).min(30000) {
                let sx = rect.left() + x as f32 * zoom;
                if sx >= space.left() && sx <= space.right() {
                    painter.line_segment(
                        [Pos2::new(sx, rect.top()), Pos2::new(sx, rect.bottom())],
                        Stroke::new(0.5_f32, Color32::from_white_alpha(30)),
                    );
                }
            }
            for y in 0..=(h as u32).min(30000) {
                let sy = rect.top() + y as f32 * zoom;
                if sy >= space.top() && sy <= space.bottom() {
                    painter.line_segment(
                        [Pos2::new(rect.left(), sy), Pos2::new(rect.right(), sy)],
                        Stroke::new(0.5_f32, Color32::from_white_alpha(30)),
                    );
                }
            }
        }
        for guide in &self.session().doc.guides {
            if guide.axis == "vertical" {
                let x = rect.left() + guide.position as f32 * zoom;
                painter.line_segment(
                    [Pos2::new(x, space.top()), Pos2::new(x, space.bottom())],
                    Stroke::new(1.0_f32, Color32::from_rgb(45, 190, 210)),
                );
            } else {
                let y = rect.top() + guide.position as f32 * zoom;
                painter.line_segment(
                    [Pos2::new(space.left(), y), Pos2::new(space.right(), y)],
                    Stroke::new(1.0_f32, Color32::from_rgb(45, 190, 210)),
                );
            }
        }
        if self.rulers {
            crate::canvas_chrome::rulers(&painter, space, rect, zoom);
        }
        if let Some([x, y, w, h]) = self.crop_rect {
            let bounds = Rect::from_min_max(doc_to_screen([x, y]), doc_to_screen([x + w, y + h]));
            for fraction in crate::crop_interaction::HANDLES {
                let point = doc_to_screen([x + fraction[0] * w, y + fraction[1] * h]);
                painter.rect_filled(
                    Rect::from_center_size(point, Vec2::splat(7.)),
                    0.,
                    Color32::WHITE,
                );
            }
            painter.rect_stroke(
                bounds,
                0.,
                Stroke::new(2_f32, Color32::WHITE),
                egui::StrokeKind::Inside,
            );
            for fraction in [1_f32 / 3., 2_f32 / 3.] {
                let sx = bounds.left() + bounds.width() * fraction;
                let sy = bounds.top() + bounds.height() * fraction;
                painter.line_segment(
                    [Pos2::new(sx, bounds.top()), Pos2::new(sx, bounds.bottom())],
                    Stroke::new(1_f32, Color32::from_white_alpha(100)),
                );
                painter.line_segment(
                    [Pos2::new(bounds.left(), sy), Pos2::new(bounds.right(), sy)],
                    Stroke::new(1_f32, Color32::from_white_alpha(100)),
                );
            }
        }
        if self.tool == Tool::Gradient
            && let Some(g) = &self.gradient
        {
            let start = doc_to_screen(g.start);
            let end = doc_to_screen(g.end);
            painter.line_segment([start, end], Stroke::new(1_f32, Color32::WHITE));
            for point in [start, end] {
                painter.circle_filled(point, 5., Color32::WHITE);
                painter.circle_stroke(point, 5., Stroke::new(1_f32, Color32::BLACK));
            }
        }
        if let Some(selection) = self.session().doc.selection.clone() {
            let side = (self.session().doc.width.max(self.session().doc.height) as f32 * zoom)
                .clamp(1000., 4000.) as u32;
            let phase = (ui.input(|i| i.time) * 4.) as usize;
            for (index, &(a, b)) in self
                .session_mut()
                .outline
                .lines_at(&selection, side)
                .iter()
                .enumerate()
            {
                painter.line_segment(
                    [doc_to_screen(a), doc_to_screen(b)],
                    Stroke::new(
                        1_f32,
                        if (index + phase).is_multiple_of(2) {
                            Color32::WHITE
                        } else {
                            Color32::BLACK
                        },
                    ),
                );
            }
        }
        if self.tool == Tool::Move
            && self.show_transform
            && let Some(transform) = transform_frame(
                &self.session().doc,
                &self.session().selected,
                self.mask_target,
            )
        {
            let corners = geometric_transform(transform).corners().map(doc_to_screen);
            painter.add(egui::Shape::closed_line(
                corners.to_vec(),
                Stroke::new(1.0_f32, Color32::from_rgb(77, 170, 240)),
            ));
            for p in transform_handles(transform, zoom).map(doc_to_screen) {
                painter.rect_filled(
                    Rect::from_center_size(p, Vec2::splat(6.)),
                    0.,
                    Color32::WHITE,
                );
            }
        }
        if let Some(drag) = &self.drag {
            if matches!(
                drag.mode,
                Tool::Rectangle | Tool::Ellipse | Tool::Crop | Tool::Shape | Tool::Gradient
            ) {
                let bounds = if matches!(drag.mode, Tool::Rectangle | Tool::Ellipse) {
                    let [x, y, w, h] = selection_drag_box(drag, ui.input(|i| i.modifiers.shift));
                    Rect::from_two_pos(doc_to_screen([x, y]), doc_to_screen([x + w, y + h]))
                } else {
                    Rect::from_two_pos(doc_to_screen(drag.start), doc_to_screen(drag.last))
                };
                if drag.mode == Tool::Ellipse {
                    painter.add(egui::Shape::ellipse_stroke(
                        bounds.center(),
                        bounds.size() / 2.,
                        Stroke::new(1.0_f32, Color32::WHITE),
                    ));
                } else {
                    painter.rect_stroke(
                        bounds,
                        0.,
                        Stroke::new(1.0_f32, Color32::WHITE),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            if drag.mode == Tool::Lasso {
                painter.add(egui::Shape::line(
                    drag.points.iter().copied().map(doc_to_screen).collect(),
                    Stroke::new(1.0_f32, Color32::WHITE),
                ));
            }
        }
        if self.polygon.len() > 1 {
            painter.add(egui::Shape::line(
                self.polygon.iter().copied().map(doc_to_screen).collect(),
                Stroke::new(1.0_f32, Color32::WHITE),
            ));
        }
        self.sample_text_color_canvas(ui, &response, rect);
        if let Some(Dialog::Text { layer, mut origin }) = self.dialog.clone()
            && self
                .dialog_target
                .is_some_and(|(target, _)| target == self.session().id)
        {
            let style = match crate::text::TextStyle::from_value(&self.dialog_value) {
                Ok(style) => style,
                Err(error) => {
                    ui.colored_label(Color32::LIGHT_RED, tr(&error.to_string()));
                    return;
                }
            };
            let existing = layer.and_then(|id| self.session().doc.layer(id));
            let native_size = style.box_size.unwrap_or_else(|| {
                existing
                    .and_then(|l| l.image.as_ref().map(|r| [r.width as f64, r.height as f64]))
                    .unwrap_or([400., 160.])
            });
            let layer_scale = existing
                .and_then(|l| {
                    l.image.as_ref().map(|r| {
                        (l.transform.size[0] / r.width as f64)
                            .min(l.transform.size[1] / r.height as f64)
                    })
                })
                .unwrap_or(1.);
            let text_scale = zoom * layer_scale as f32;
            let bounds = Rect::from_min_size(
                doc_to_screen(origin),
                Vec2::new(
                    native_size[0] as f32 * text_scale,
                    native_size[1] as f32 * text_scale,
                ),
            );
            let editor_id = egui::Id::new(("canvas-text", self.dialog_target.map(|(id, _)| id)));
            painter.rect_stroke(
                bounds,
                0.,
                Stroke::new(1_f32, Color32::LIGHT_BLUE),
                egui::StrokeKind::Outside,
            );
            let handle = Rect::from_center_size(bounds.right_bottom(), Vec2::splat(10.));
            let header =
                Rect::from_min_max(bounds.left_top() - Vec2::new(0., 10.), bounds.right_top());
            painter.rect_filled(
                header,
                0.,
                Color32::from_rgba_unmultiplied(60, 130, 180, 170),
            );
            painter.rect_filled(handle, 0., Color32::LIGHT_BLUE);
            for (area, resize) in [(header, false), (handle, true)] {
                let response = ui.interact(
                    area,
                    editor_id.with(if resize { "resize" } else { "move" }),
                    if self.text_color_target.is_some() {
                        Sense::hover()
                    } else {
                        Sense::click_and_drag()
                    },
                );
                if response.drag_started() {
                    let start = ui
                        .input(|i| i.pointer.press_origin())
                        .unwrap_or(area.center());
                    self.text_box_drag = Some((
                        [start.x as f64, start.y as f64],
                        origin,
                        native_size,
                        resize,
                    ));
                }
                if response.dragged()
                    && let Some((start, original, size, resizing)) = self.text_box_drag
                    && let Some(pointer) = response.interact_pointer_pos()
                {
                    let delta = [pointer.x as f64 - start[0], pointer.y as f64 - start[1]];
                    if resizing {
                        self.dialog_value["boxSize"] = json!([
                            (size[0] + delta[0] / text_scale as f64).clamp(25., 30_000.),
                            (size[1] + delta[1] / text_scale as f64).clamp(25., 30_000.)
                        ]);
                    } else {
                        origin = [
                            original[0] + delta[0] / zoom as f64,
                            original[1] + delta[1] / zoom as f64,
                        ];
                        if let Some(Dialog::Text { origin: value, .. }) = &mut self.dialog {
                            *value = origin;
                        }
                    }
                }
                if response.drag_stopped() {
                    self.text_box_drag = None;
                }
            }
            if let Err(error) = crate::text_panel::show_inline_with_interaction(
                ui,
                &mut self.dialog_value,
                editor_id,
                bounds.shrink((12. * text_scale).min(bounds.width().min(bounds.height()) / 3.)),
                text_scale,
                self.text_color_target.is_none(),
            ) {
                ui.colored_label(Color32::LIGHT_RED, tr(&error.to_string()));
            }
            return;
        }
        if self.specialized_filter_dialog()
            && response
                .interact_pointer_pos()
                .or_else(|| response.hover_pos())
                .is_none()
            && ui.input(|i| !i.pointer.primary_down())
        {
            self.filter_ui.pointer_end();
        }
        if let Some(pos) = response
            .interact_pointer_pos()
            .or_else(|| response.hover_pos())
        {
            let point = [
                ((pos.x - rect.left()) / zoom) as f64,
                ((pos.y - rect.top()) / zoom) as f64,
            ];
            if self.tool.paints() {
                painter.circle_stroke(
                    pos,
                    (self.diameter as f32 * zoom / 2.).max(2.),
                    Stroke::new(1.0_f32, Color32::WHITE),
                );
            }
            if response.hovered() {
                let wheel = ui.input(|i| i.smooth_scroll_delta.y);
                if wheel != 0. {
                    let old_zoom = self.session().zoom;
                    let new_zoom = (old_zoom * (wheel * 0.002).exp()).clamp(0.005, 64.);
                    let delta = pos - space.center();
                    self.session_mut().pan =
                        delta - (delta - self.session().pan) * (new_zoom / old_zoom);
                    self.session_mut().zoom = new_zoom;
                }
            }
            if self.filter_canvas_gesture(ui, &response, rect) {
                return;
            }
            let alt = ui.input(|i| i.modifiers.alt);
            let shift = ui.input(|i| i.modifiers.shift);
            let ctrl = ui.input(|i| i.modifiers.ctrl);
            let space_key = ui.input(|i| i.key_down(Key::Space));
            if response.is_pointer_button_down_on() && ui.input(|i| i.pointer.any_pressed()) {
                self.selection_press = Some(ui.input(|i| i.modifiers));
            }
            if !shift && let Some(drag) = &mut self.drag {
                drag.constrain_armed = true;
            }
            if self.tool == Tool::Crop && !space_key {
                use crate::crop_interaction::{CropDrag, CropMode};
                if response.drag_started() {
                    let press = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
                    let start = [
                        ((press.x - rect.left()) / zoom) as f64,
                        ((press.y - rect.top()) / zoom) as f64,
                    ];
                    let original = self.crop_rect.unwrap_or([0., 0., w as f64, h as f64]);
                    let handle = crate::crop_interaction::HANDLES
                        .iter()
                        .position(|fraction| {
                            doc_to_screen([
                                original[0] + fraction[0] * original[2],
                                original[1] + fraction[1] * original[3],
                            ])
                            .distance(press)
                                < 10.
                        });
                    let mode = if let Some(handle) = handle {
                        CropMode::Resize(handle)
                    } else if start[0] >= original[0]
                        && start[1] >= original[1]
                        && start[0] <= original[0] + original[2]
                        && start[1] <= original[1] + original[3]
                    {
                        CropMode::Move
                    } else {
                        CropMode::Create
                    };
                    self.crop_drag = Some(CropDrag {
                        start,
                        original,
                        mode,
                    });
                }
                if (response.dragged() || response.drag_stopped())
                    && let Some(drag) = &self.crop_drag
                {
                    let ratio = self.crop_aspect_ratio();
                    if let Some(frame) = drag.updated(point, ratio, alt) {
                        let snap = crate::crop_interaction::targets(
                            &self.session().doc,
                            self.snap_options(false),
                            &HashSet::new(),
                            if self.snap { 6. / zoom as f64 } else { 0. },
                        );
                        self.crop_rect = Some(snap.apply(frame, drag, point, ratio, alt));
                    }
                }
                if response.drag_stopped() {
                    self.crop_drag = None;
                }
                return;
            }
            if (self.tool == Tool::Eyedropper
                || self.color_picker.is_some()
                || (alt
                    && matches!(self.tool, Tool::Brush | Tool::Heal | Tool::Gradient)
                    && self.brush_stroke.is_none()
                    && self.drag.is_none()))
                && !space_key
            {
                if response.is_pointer_button_down_on() || response.clicked() || response.dragged()
                {
                    self.sample_point = Some(point);
                    if self
                        .sample_drag
                        .as_ref()
                        .is_none_or(|sample| sample.session != self.session().id)
                    {
                        let original = if self
                            .color_picker
                            .as_ref()
                            .is_some_and(|picker| picker.background)
                        {
                            self.background
                        } else {
                            self.foreground
                        };
                        self.sample_drag = Some(SampleDrag {
                            original,
                            session: self.session().id,
                        });
                    }
                    if self.sample_cache.as_ref().is_none_or(|(id, sampler)| {
                        *id != self.session().id || !sampler.matches(&self.session().doc)
                    }) {
                        self.sample_cache = None;
                        let key = (self.session().id, self.session().serial);
                        if self.sample_busy != Some(key) {
                            self.start_color_sampler(key);
                        }
                    }
                    let color = self
                        .sample_cache
                        .as_ref()
                        .and_then(|(_, sampler)| sampler.sample(point))
                        .filter(|color| color[3] > 0);
                    if let Some(color) = color {
                        self.apply_sampled_color(color);
                        if self.show_sample_ring
                            && ui.input(|i| i.pointer.primary_down())
                            && let Some(sample) = &self.sample_drag
                        {
                            crate::palette::sample_ring(&painter, pos, sample.original, color);
                        }
                    }
                }
                return;
            }
            if response.clicked() {
                match self.tool {
                    Tool::Move if !self.mask_target => {
                        self.pick_move_target(point, ctrl, shift);
                    }
                    t if t.paints() && !(t == Tool::Clone && alt) => {
                        let before = self.session().doc.clone();
                        let start = if shift {
                            self.brush_line_start(point)
                        } else {
                            point
                        };
                        self.begin_brush(start);
                        self.finish_brush(before, point);
                    }
                    Tool::Eyedropper => {
                        let r = render::render_preview(&self.session().doc, 2048);
                        self.foreground = render::sample(
                            &r,
                            point[0] * r.width as f64 / w as f64 - 0.5,
                            point[1] * r.height as f64 / h as f64 - 0.5,
                        );
                    }
                    Tool::Wand | Tool::Object => {
                        let mode = effective_selection_mode(
                            self.selection_mode,
                            ui.input(|i| i.modifiers),
                        );
                        if self.tool == Tool::Object {
                            self.segment(SegmentOperation::Selection {
                                point: Some(point),
                                mode,
                            });
                            return;
                        }
                        let all = self.wand_all;
                        let settings = selection::WandSettings {
                            tolerance: self.tolerance,
                            sample_radius: self.wand_radius as u32,
                            contiguous: self.contiguous,
                        };
                        self.background_edit("Magic selection", move |doc| {
                            let mut source = doc.clone();
                            if !all {
                                let id = doc.active_layer_id;
                                source.layers.retain(|l| Some(l.id) == id);
                                if let Some(l) = source.layers.first_mut() {
                                    l.parent_id = None;
                                    l.mask_source_id = None;
                                }
                            }
                            let raster = render::render_document(&source);
                            let incoming = selection::wand(&raster, point, settings);
                            doc.selection =
                                Some(selection::combine(doc.selection.as_ref(), &incoming, mode));
                            Ok(())
                        });
                    }
                    Tool::Polygon => {
                        if self.polygon.is_empty() {
                            self.polygon_options = Some((
                                effective_selection_mode(
                                    self.selection_mode,
                                    ui.input(|i| i.modifiers),
                                ),
                                self.antialias,
                            ));
                        }
                        self.polygon.push(point);
                    }
                    Tool::Rectangle | Tool::Ellipse | Tool::Lasso => {
                        if effective_selection_mode(self.selection_mode, ui.input(|i| i.modifiers))
                            == SelectionMode::Replace
                            && self.session().doc.selection.as_ref().is_some_and(|s| {
                                point[0] < 0.
                                    || point[1] < 0.
                                    || s.get(point[0].floor() as u32, point[1].floor() as u32) == 0
                            })
                        {
                            self.session_mut().edit("Deselect", |d| d.selection = None);
                        }
                    }
                    Tool::Text => {
                        let existing = hit_layer(&self.session().doc, point)
                            .and_then(|id| self.session().doc.layer(id))
                            .and_then(|l| {
                                l.text
                                    .as_ref()
                                    .map(|text| (l.id, l.transform.origin, text.clone()))
                            });
                        self.begin_dialog(Dialog::Text {
                            layer: existing.as_ref().map(|l| l.0),
                            origin: existing.as_ref().map_or(point, |l| l.1),
                        });
                        self.dialog_value = existing.map(|l|l.2).unwrap_or_else(||json!({"content":"文字","fontName":"Segoe UI","fontSize":48,"red":self.foreground[0]as f64/255.,"green":self.foreground[1]as f64/255.,"blue":self.foreground[2]as f64/255.,"alignment":"Left","tracking":0,"leading":0,"boxSize":[400,160]}));
                    }
                    Tool::Clone if alt => {
                        self.clone_source = Some(point);
                        self.clone_offset = None;
                    }
                    Tool::Zoom => {
                        let current_zoom = self.session().zoom;
                        self.session_mut().zoom =
                            (current_zoom * if alt { 0.5 } else { 2. }).clamp(0.005, 64.);
                    }
                    _ => {}
                }
            }
            if response.drag_started() {
                let press = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
                let start = [
                    ((press.x - rect.left()) / zoom) as f64,
                    ((press.y - rect.top()) / zoom) as f64,
                ];
                let mode = if space_key { Tool::Hand } else { self.tool };
                let gradient_handle = if mode == Tool::Gradient {
                    self.gradient.as_ref().and_then(|g| {
                        [g.start, g.end]
                            .iter()
                            .position(|&p| doc_to_screen(p).distance(press) < 10.)
                    })
                } else {
                    None
                };
                let existing = (!self.guides_locked && mode == Tool::Move)
                    .then(|| {
                        self.session()
                            .doc
                            .guides
                            .iter()
                            .find(|g| {
                                let (value, target) = if g.axis == "horizontal" {
                                    (g.position, start[1])
                                } else {
                                    (g.position, start[0])
                                };
                                (value - target).abs() * (zoom as f64) < 5.
                            })
                            .cloned()
                    })
                    .flatten();
                if let Some(handle) = gradient_handle {
                    self.gradient_handle = Some(handle);
                } else if !self.guides_locked
                    && self.rulers
                    && (pos.y < space.top() + crate::chrome::metrics::RULER_THICKNESS
                        || pos.x < space.left() + crate::chrome::metrics::RULER_THICKNESS)
                {
                    let horizontal = pos.y < space.top() + crate::chrome::metrics::RULER_THICKNESS;
                    let before = self.session().doc.clone();
                    let position = if horizontal { start[1] } else { start[0] };
                    match crate::edit::add_guide(&mut self.session_mut().doc, horizontal, position)
                    {
                        Ok(id) => {
                            self.guide_drag = Some(GuideDrag {
                                before,
                                id,
                                horizontal,
                            });
                            self.session_mut().changed();
                        }
                        Err(e) => self.message = Some(e.to_string()),
                    }
                } else if let Some(g) = existing {
                    self.guide_drag = Some(GuideDrag {
                        before: self.session().doc.clone(),
                        id: g.id,
                        horizontal: g.axis == "horizontal",
                    });
                } else if !(mode == Tool::Clone && alt) {
                    if mode == Tool::Gradient {
                        self.gradient = None;
                    }
                    let mut before = self.session().doc.clone();
                    let mut handle = None;
                    if mode == Tool::Move
                        && self.show_transform
                        && let Some(transform) =
                            transform_frame(&before, &self.session().selected, self.mask_target)
                    {
                        let corners = transform_handles(transform, zoom);
                        handle = corners
                            .iter()
                            .position(|p| doc_to_screen(*p).distance(press) < 10.);
                    }
                    if mode == Tool::Move && handle.is_none() {
                        self.pick_move_target(start, ctrl, shift);
                        before = self.session().doc.clone();
                    }
                    let mut work_before = before.clone();
                    let selected_pixels = before.selection.as_ref().is_some_and(|s| {
                        start[0] >= 0.
                            && start[1] >= 0.
                            && s.get(start[0] as u32, start[1] as u32) > 0
                    });
                    if mode == Tool::Move && handle.is_none() && alt && !selected_pixels {
                        let ids = self.session().selected.iter().copied().collect::<Vec<_>>();
                        match crate::edit::duplicate_layers(&mut work_before, &ids) {
                            Ok(ids) => {
                                self.session_mut().selected = ids.into_iter().collect();
                                self.session_mut().doc = work_before.clone();
                            }
                            Err(e) => self.message = Some(e.to_string()),
                        }
                    }
                    self.drag = Some(Drag {
                        target: Some((self.session().id, self.session().history.revision())),
                        before: before.clone(),
                        work_before,
                        start,
                        last: start,
                        points: vec![start],
                        mode,
                        selection_mode: effective_selection_mode(
                            self.selection_mode,
                            self.selection_press
                                .unwrap_or_else(|| ui.input(|i| i.modifiers)),
                        ),
                        selection_antialias: self.antialias,
                        constrain_armed: !self
                            .selection_press
                            .unwrap_or_else(|| ui.input(|i| i.modifiers))
                            .shift,
                        handle,
                        pan: self.session().pan,
                        distort: ctrl && handle.is_some_and(|h| h < 8),
                        rotate: handle == Some(8),
                        centered: alt,
                        pixel_move: mode == Tool::Move
                            && handle.is_none()
                            && before.selection.as_ref().is_some_and(|s| {
                                start[0] >= 0.
                                    && start[1] >= 0.
                                    && s.get(start[0] as u32, start[1] as u32) > 0
                            }),
                        outline_move: matches!(
                            mode,
                            Tool::Rectangle
                                | Tool::Ellipse
                                | Tool::Lasso
                                | Tool::Polygon
                                | Tool::Wand
                                | Tool::Object
                        ) && effective_selection_mode(
                            self.selection_mode,
                            self.selection_press
                                .unwrap_or_else(|| ui.input(|i| i.modifiers)),
                        ) == SelectionMode::Replace
                            && before.selection.as_ref().is_some_and(|s| {
                                !s.is_empty()
                                    && s.outline().get_unclipped(
                                        start[0].floor() as i64,
                                        start[1].floor() as i64,
                                    ) > 0
                            }),
                        duplicate_pixels: alt,
                    });
                    if mode.paints() {
                        self.begin_brush(start);
                    }
                }
            }
            if response.dragged() {
                if let Some(handle) = self.gradient_handle {
                    if let Some(g) = &mut self.gradient {
                        if handle == 0 {
                            g.start = point;
                        } else {
                            g.end = point;
                        }
                    }
                } else if let Some(g) = &self.guide_drag {
                    let id = g.id;
                    let position = if g.horizontal { point[1] } else { point[0] };
                    if let Some(g) = self
                        .session_mut()
                        .doc
                        .guides
                        .iter_mut()
                        .find(|g| g.id == id)
                    {
                        g.position = position;
                    }
                    self.session_mut().changed();
                } else if let Some(mut drag) = self.drag.take() {
                    if !self.drag_is_current(&drag) {
                        self.brush_stroke = None;
                        return;
                    }
                    drag.last = point;
                    if drag.mode == Tool::Crop
                        && let Some(ratio) = self.crop_aspect_ratio()
                    {
                        let dx = point[0] - drag.start[0];
                        let dy = point[1] - drag.start[1];
                        if dx.abs() / dy.abs().max(1e-9) > ratio {
                            drag.last[1] =
                                drag.start[1] + dx.abs() / ratio * if dy < 0. { -1. } else { 1. };
                        } else {
                            drag.last[0] =
                                drag.start[0] + dy.abs() * ratio * if dx < 0. { -1. } else { 1. };
                        }
                    }
                    drag.points.push(point);
                    if drag.outline_move {
                        if let Some(selection) = &drag.before.selection {
                            self.session_mut().doc.selection = Some(selection::translated(
                                selection,
                                (point[0] - drag.start[0]).round() as i32,
                                (point[1] - drag.start[1]).round() as i32,
                                drag.before.width,
                                drag.before.height,
                            ));
                            self.session_mut().changed();
                        }
                        self.drag = Some(drag);
                        return;
                    }
                    match drag.mode {
                        Tool::Hand => {
                            self.session_mut().pan = drag.pan
                                + Vec2::new(
                                    (point[0] - drag.start[0]) as f32 * zoom,
                                    (point[1] - drag.start[1]) as f32 * zoom,
                                )
                        }
                        Tool::Move => {
                            let ids: Vec<_> = self.session().selected.iter().copied().collect();
                            let mut delta = [point[0] - drag.start[0], point[1] - drag.start[1]];
                            if shift {
                                if delta[0].abs() > delta[1].abs() {
                                    delta[1] = 0.;
                                } else {
                                    delta[0] = 0.;
                                }
                            }
                            let locked = if shift {
                                Some(if delta[0] == 0. { 0 } else { 1 })
                            } else {
                                None
                            };
                            let mut delta = if drag.handle.is_none() {
                                self.snap_delta(delta, &drag.work_before)
                            } else {
                                delta
                            };
                            if drag.distort {
                                let snap = self.transform_snap(&drag.work_before, false);
                                let p = crate::edit::snap_point(
                                    [drag.start[0] + delta[0], drag.start[1] + delta[1]],
                                    &snap.xs,
                                    &snap.ys,
                                    snap.tolerance,
                                );
                                delta = [p[0] - drag.start[0], p[1] - drag.start[1]];
                            }
                            if let Some(axis) = locked {
                                delta[axis] = 0.;
                            }
                            if drag.distort {
                                drag.last = [drag.start[0] + delta[0], drag.start[1] + delta[1]];
                            }
                            self.session_mut().doc = drag.work_before.clone();
                            if drag.pixel_move {
                                if let Some(s) = &drag.before.selection {
                                    self.session_mut().doc.selection = Some(selection::translated(
                                        s,
                                        delta[0].round() as i32,
                                        delta[1].round() as i32,
                                        drag.before.width,
                                        drag.before.height,
                                    ));
                                }
                            } else if let Some(handle) = drag.handle {
                                if let Some(l) = drag.work_before.active() {
                                    let proportional = self.locks_transform_ratio != shift;
                                    let old = transform_frame(
                                        &drag.work_before,
                                        &self.session().selected,
                                        self.mask_target,
                                    )
                                    .unwrap_or(l.transform);
                                    let resize_point = if !drag.distort && !drag.rotate {
                                        let snap = self.transform_snap(&drag.work_before, true);
                                        crate::crop_interaction::snap_resize_point(
                                            old,
                                            handle,
                                            drag.start,
                                            point,
                                            proportional,
                                            &snap,
                                            |p| {
                                                resize_transform(
                                                    old,
                                                    handle,
                                                    drag.start,
                                                    p,
                                                    drag.centered,
                                                    proportional,
                                                )
                                            },
                                        )
                                        .0
                                    } else {
                                        point
                                    };
                                    let mut new = resize_transform(
                                        old,
                                        handle,
                                        drag.start,
                                        resize_point,
                                        drag.centered,
                                        proportional,
                                    );
                                    if drag.rotate {
                                        let center = [
                                            old.origin[0] + old.size[0] / 2.,
                                            old.origin[1] + old.size[1] / 2.,
                                        ];
                                        new = old;
                                        new.rotation = old.rotation
                                            + ((point[1] - center[1]).atan2(point[0] - center[0])
                                                - (drag.start[1] - center[1])
                                                    .atan2(drag.start[0] - center[0]))
                                            .to_degrees();
                                    }
                                    if !drag.distort {
                                        let result = if self.mask_target {
                                            crate::edit::set_transform(
                                                &mut self.session_mut().doc,
                                                l.id,
                                                new,
                                                true,
                                            )
                                        } else {
                                            crate::edit::transform_layers(
                                                &mut self.session_mut().doc,
                                                &ids,
                                                old,
                                                new,
                                            )
                                        };
                                        if let Err(e) = result {
                                            self.message = Some(e.to_string());
                                        }
                                    }
                                }
                            } else {
                                let result = if self.mask_target {
                                    if let Some(layer) = drag.work_before.active() {
                                        let mut transform = layer
                                            .mask
                                            .as_ref()
                                            .and_then(|m| m.placement)
                                            .unwrap_or(layer.transform);
                                        transform.origin[0] += delta[0];
                                        transform.origin[1] += delta[1];
                                        crate::edit::set_transform(
                                            &mut self.session_mut().doc,
                                            layer.id,
                                            transform,
                                            true,
                                        )
                                    } else {
                                        Ok(())
                                    }
                                } else {
                                    crate::edit::move_layers(
                                        &mut self.session_mut().doc,
                                        &ids,
                                        delta,
                                    )
                                };
                                if let Err(e) = result {
                                    self.message = Some(e.to_string());
                                }
                            }
                            self.session_mut().changed();
                        }
                        t if t.paints() => {
                            if let Some(stroke) = &mut self.brush_stroke
                                && let Err(e) = stroke.append(point)
                            {
                                self.message = Some(e.to_string());
                            }
                            self.show_brush();
                        }
                        _ => {}
                    }
                    self.drag = Some(drag);
                }
            }
        }
        if response.drag_stopped() {
            if self.gradient_handle.take().is_some()
                && let Some(state) = self.gradient.clone()
            {
                self.update_gradient(state);
            }
            let pos = ui
                .input(|i| i.pointer.latest_pos())
                .unwrap_or(space.center());
            let alt = ui.input(|i| i.modifiers.alt);
            let shift = ui.input(|i| i.modifiers.shift);
            if let Some(g) = self.guide_drag.take() {
                if !space.contains(pos) {
                    self.session_mut()
                        .doc
                        .guides
                        .retain(|guide| guide.id != g.id);
                }
                let s = self.session_mut();
                if s.history.record("Guide", g.before, &s.doc) {
                    s.changed();
                }
            }
            if let Some(drag) = self.drag.take() {
                if !self.drag_is_current(&drag) {
                    self.brush_stroke = None;
                    return;
                }
                if drag.outline_move {
                    let session = self.session_mut();
                    session
                        .history
                        .record("Move selection outline", drag.before, &session.doc);
                    session.changed();
                    return;
                }
                match drag.mode {
                    t if t.paints() => {
                        self.finish_brush(drag.before, drag.last);
                    }
                    Tool::Move => {
                        if drag.distort
                            && let Some((id, corners)) = distort_corners(&drag, self.mask_target)
                        {
                            self.session_mut().doc = drag.before;
                            let mask = self.mask_target;
                            self.background_edit("Free distort", move |doc| {
                                crate::edit::free_distort(doc, id, corners, mask)
                            });
                            return;
                        }
                        if drag.pixel_move
                            && let Some(id) = drag.before.active_layer_id
                        {
                            self.session_mut().doc = drag.before.clone();
                            let offset =
                                [drag.last[0] - drag.start[0], drag.last[1] - drag.start[1]];
                            let mask = self.mask_target;
                            if let Err(e) = crate::edit::move_selected_pixels(
                                &mut self.session_mut().doc,
                                id,
                                mask,
                                offset,
                                drag.duplicate_pixels,
                            ) {
                                self.message = Some(e.to_string());
                            }
                        }
                        let s = self.session_mut();
                        s.history.record("Transform", drag.before, &s.doc);
                        s.changed();
                    }
                    Tool::Rectangle | Tool::Ellipse | Tool::Lasso => {
                        let mode = drag.selection_mode;
                        let ellipse = drag.mode == Tool::Ellipse;
                        let aa = drag.selection_antialias;
                        let incoming = if drag.mode == Tool::Lasso {
                            selection::polygon(
                                self.session().doc.width,
                                self.session().doc.height,
                                &drag.points,
                                aa,
                            )
                        } else {
                            selection::marquee(
                                self.session().doc.width,
                                self.session().doc.height,
                                selection_drag_box(&drag, shift),
                                ellipse,
                                aa,
                            )
                        };
                        self.session_mut().edit("Selection", |d| {
                            d.selection =
                                Some(selection::combine(d.selection.as_ref(), &incoming, mode))
                        });
                    }
                    Tool::Crop => {
                        self.crop_rect =
                            Some(selection::drag_box(drag.start, drag.last, false, alt));
                    }
                    Tool::Text => {
                        let [x, y, w, h] = selection::drag_box(drag.start, drag.last, false, false);
                        self.begin_dialog(Dialog::Text {
                            layer: None,
                            origin: [x, y],
                        });
                        self.dialog_value = json!({"content":"文字","fontName":"Segoe UI","fontSize":48,"red":self.foreground[0]as f64/255.,"green":self.foreground[1]as f64/255.,"blue":self.foreground[2]as f64/255.,"alignment":"Left","tracking":0,"leading":0,"boxSize":[w.max(25.),h.max(25.)]});
                    }
                    Tool::Shape => {
                        let kind = match self.shape_kind.as_str() {
                            "Ellipse" => crate::edit::ShapeKind::Ellipse,
                            "Line" => crate::edit::ShapeKind::Line,
                            _ => crate::edit::ShapeKind::Rectangle,
                        };
                        let color = self.foreground;
                        let radius = self.corner_radius;
                        let width = self.line_width;
                        let (rect, ends) = crate::edit::shape_drag_geometry(
                            kind, drag.start, drag.last, shift, alt, width,
                        );
                        self.background_edit("New shape", move |d| {
                            crate::edit::add_shape(d, kind, rect, color, radius, width, ends)
                                .map(|_| ())
                        });
                    }
                    Tool::Gradient => {
                        let Some(id) = self.session().doc.active_layer_id else {
                            return;
                        };
                        self.update_gradient(GradientState {
                            session: self.session().id,
                            request: Uuid::new_v4(),
                            base_revision: self.session().history.revision(),
                            before: drag.before,
                            start: drag.start,
                            end: drag.last,
                            layer: id,
                            mask: self.mask_target,
                            revision: None,
                            parameters: (
                                self.gradient_kind.clone(),
                                self.foreground,
                                self.background,
                                self.opacity,
                            ),
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    fn dialogs(&mut self, ctx: &egui::Context) {
        self.pdf_dialogs(ctx);
        if let Some(dialog) = self.dialog.clone() {
            if self.dialog_target.is_none() {
                self.dialog_target = Some(self.dialog_destination(&dialog));
                self.dialog_mask = self.mask_target;
            }
            if let Dialog::Raw { path, .. } = &dialog
                && !self.raw_busy
                && self.raw_key.as_ref()
                    != Some(&(path.clone(), raw_settings_key(self.raw_settings)))
            {
                self.raw_preview(path.clone());
            }
            self.dialog_preview(ctx, &dialog);
            let title = match &dialog {
                Dialog::CanvasSize => "Canvas Size".into(),
                Dialog::ImageSize => "Image Size".into(),
                Dialog::Grid => "Grid Settings".into(),
                Dialog::Guides => "Guides".into(),
                Dialog::ColorRange => "Color Range".into(),
                Dialog::Jpeg => "Export JPEG".into(),
                Dialog::Trim => "Trim".into(),
                Dialog::Text { .. } => "Editable Text".into(),
                Dialog::Adjustment { kind } => format!("New {kind} Adjustment"),
                Dialog::Filter { kind } => kind.clone(),
                Dialog::Effect { kind } => humanize_english(kind),
                Dialog::CameraRaw => "Camera Raw".into(),
                Dialog::Raw { .. } => "Develop Camera RAW".into(),
            };
            let mut apply = matches!(dialog, Dialog::Text { .. }) && self.text_apply_requested;
            self.text_apply_requested = false;
            let mut cancel = false;
            let caption = match &dialog {
                Dialog::Adjustment { kind } => format!("新建{}调整图层", tr(kind)),
                _ => tr(&title).into_owned(),
            };
            let docked = matches!(dialog, Dialog::CameraRaw);
            let outer_width = match &dialog {
                Dialog::CameraRaw => 440.,
                Dialog::Filter { kind } | Dialog::Adjustment { kind } if kind == "Levels" => 440.,
                Dialog::Filter { kind } | Dialog::Adjustment { kind }
                    if kind == "Hue/Saturation" =>
                {
                    460.
                }
                Dialog::Filter { .. } | Dialog::Adjustment { .. } => 380.,
                Dialog::Effect { .. } | Dialog::ColorRange => 340.,
                Dialog::Trim => 320.,
                Dialog::Grid => 360.,
                Dialog::CanvasSize => 450.,
                Dialog::ImageSize => 430.,
                _ => 400.,
            };
            let padding = if matches!(dialog, Dialog::Effect { .. }) {
                20
            } else {
                24
            };
            let mut panel_open = true;
            let window = crate::dialog_layout::window(
                ctx,
                &caption,
                egui::Id::new(("editor-dialog", &title)),
                outer_width,
                docked,
                ctx.available_rect().center(),
            )
            .open(&mut panel_open);
            if !matches!(dialog, Dialog::Text { .. }) {
                window.show(ctx, |ui| {
                    crate::dialog_layout::padded(ui, padding, |ui| {
                        crate::dialog_layout::body(ui, docked, |ui| {
                            if matches!(dialog, Dialog::Jpeg) {
                                ui.horizontal(|ui| {
                                    if ui.button("适合窗口").clicked() {
                                        self.jpeg_zoom = 0.;
                                    }
                                    if ui.button("100%").clicked() {
                                        self.jpeg_zoom = 1.;
                                    }
                                    if ui.button("+").clicked() {
                                        self.jpeg_zoom = (self.jpeg_zoom.max(0.25) * 2.).min(8.);
                                    }
                                    if ui.button("−").clicked() {
                                        self.jpeg_zoom = (self.jpeg_zoom.max(0.25) / 2.).max(0.125);
                                    }
                                });
                                if let Some(t) = &self.dialog_texture {
                                    let size = t.size_vec2();
                                    let scale = if self.jpeg_zoom == 0. {
                                        (600. / size.x.max(size.y)).min(1.)
                                    } else {
                                        self.jpeg_zoom
                                    };
                                    egui::ScrollArea::both().max_height(420.).show(ui, |ui| {
                                        ui.image((t.id(), size * scale));
                                    });
                                }
                                ui.add(
                                    egui::Slider::new(&mut self.jpeg_quality, 1..=100).text("品质"),
                                );
                                ui.horizontal(|ui| {
                                    ui.label("透明区域背景色");
                                    ui.color_edit_button_srgba_unmultiplied(&mut self.jpeg_matte);
                                });
                                ui.label(format!(
                                    "JPEG 预览：{} 字节 · 导出 {} × {} 像素",
                                    self.dialog_preview_bytes,
                                    self.session().doc.width,
                                    self.session().doc.height
                                ));
                            } else if let Dialog::Raw { path, .. } = &dialog {
                                if let Some(t) = &self.raw_texture {
                                    let size = t.size_vec2();
                                    ui.image((t.id(), size * (500. / size.x.max(size.y)).min(1.)));
                                }
                                let mut changed = false;
                                changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut self.raw_settings.exposure,
                                            -5.0..=5.0,
                                        )
                                        .text("曝光"),
                                    )
                                    .changed();
                                changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut self.raw_settings.temperature,
                                            2000.0..=12000.0,
                                        )
                                        .text("色温"),
                                    )
                                    .changed();
                                changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut self.raw_settings.tint,
                                            -100.0..=100.0,
                                        )
                                        .text("色调"),
                                    )
                                    .changed();
                                changed |= ui
                                    .add(
                                        egui::Slider::new(&mut self.raw_settings.boost, 0.0..=2.0)
                                            .text("增强"),
                                    )
                                    .changed();
                                if self.raw_busy {
                                    ui.spinner();
                                }
                                if changed && !self.raw_busy {
                                    self.raw_preview(path.clone());
                                }
                                if ui.button("刷新预览").clicked() {
                                    self.raw_preview(path.clone());
                                }
                            } else if matches!(dialog, Dialog::CanvasSize | Dialog::ImageSize) {
                                egui::ComboBox::from_id_salt("size-units")
                                    .selected_text(tr(&self.size_units))
                                    .show_ui(ui, |ui| {
                                        for unit in [
                                            "Pixels",
                                            "Percent",
                                            "Inches",
                                            "Centimeters",
                                            "Millimeters",
                                        ] {
                                            ui.selectable_value(
                                                &mut self.size_units,
                                                unit.into(),
                                                tr(unit),
                                            );
                                        }
                                    });
                                if matches!(dialog, Dialog::CanvasSize) {
                                    ui.checkbox(&mut self.size_relative, "相对尺寸");
                                }
                                if matches!(dialog, Dialog::ImageSize) {
                                    ui.checkbox(&mut self.size_proportional, "保持比例");
                                }
                                for axis in 0..2 {
                                    let factor = size_unit_factor(
                                        &self.size_units,
                                        self.new_resolution,
                                        self.size_original[axis],
                                    );
                                    let pixels = if self.size_relative {
                                        self.size_delta[axis] as f64
                                    } else {
                                        self.new_size[axis] as f64
                                    };
                                    let mut display = pixels / factor;
                                    let low = if self.size_relative {
                                        -30_000.0 / factor
                                    } else {
                                        1.0 / factor
                                    };
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut display)
                                                .range(low..=30_000.0 / factor)
                                                .speed((1.0 / factor).max(0.01))
                                                .max_decimals(if self.size_units == "Pixels" {
                                                    0
                                                } else {
                                                    3
                                                })
                                                .prefix(if axis == 0 {
                                                    "宽度 "
                                                } else {
                                                    "高度 "
                                                }),
                                        )
                                        .changed()
                                    {
                                        if self.size_relative {
                                            self.size_delta[axis] =
                                                (display * factor).round() as i32;
                                        } else {
                                            self.new_size[axis] =
                                                (display * factor).round().clamp(1.0, 30_000.0)
                                                    as u32;
                                            if matches!(dialog, Dialog::ImageSize)
                                                && self.size_proportional
                                            {
                                                let other = 1 - axis;
                                                self.new_size[other] = (self.new_size[axis] as f64
                                                    * self.size_original[other] as f64
                                                    / self.size_original[axis].max(1) as f64)
                                                    .round()
                                                    .clamp(1.0, 30_000.0)
                                                    as u32;
                                            }
                                        }
                                    }
                                }
                                if self.size_relative {
                                    self.new_size = std::array::from_fn(|axis| {
                                        (self.size_original[axis] as i64
                                            + self.size_delta[axis] as i64)
                                            .clamp(1, 30_000)
                                            as u32
                                    });
                                }
                                ui.label(format!(
                                    "结果：{} × {} 像素",
                                    self.new_size[0], self.new_size[1]
                                ));
                                if !Document::valid_size(self.new_size[0], self.new_size[1]) {
                                    ui.colored_label(Color32::LIGHT_RED, "尺寸超过文档像素上限");
                                }
                                if matches!(dialog, Dialog::ImageSize) {
                                    ui.add(
                                        egui::DragValue::new(&mut self.new_resolution)
                                            .range(1.0..=9600.0)
                                            .prefix("分辨率 ")
                                            .suffix(" ppi"),
                                    );
                                }
                                if matches!(dialog, Dialog::CanvasSize) {
                                    ui.horizontal(|ui| {
                                        for (i, label) in [(0, "水平锚点"), (1, "垂直锚点")]
                                        {
                                            egui::ComboBox::from_id_salt(label)
                                                .selected_text(format!(
                                                    "{:.0}%",
                                                    self.canvas_anchor[i] * 100.
                                                ))
                                                .show_ui(ui, |ui| {
                                                    for (v, label) in
                                                        [(0., "起点"), (0.5, "中心"), (1., "终点")]
                                                    {
                                                        ui.selectable_value(
                                                            &mut self.canvas_anchor[i],
                                                            v,
                                                            label,
                                                        );
                                                    }
                                                });
                                        }
                                    });
                                }
                                if matches!(dialog, Dialog::ImageSize) {
                                    egui::ComboBox::from_id_salt("resize-sampling")
                                        .selected_text(tr(&format!("{:?}", self.resize_sampling)))
                                        .show_ui(ui, |ui| {
                                            for mode in [
                                                Sampling::Nearest,
                                                Sampling::Smooth,
                                                Sampling::High,
                                            ] {
                                                ui.selectable_value(
                                                    &mut self.resize_sampling,
                                                    mode,
                                                    tr(&format!("{mode:?}")),
                                                );
                                            }
                                        });
                                }
                            } else if matches!(dialog, Dialog::Grid) {
                                ui.color_edit_button_srgba_unmultiplied(&mut self.grid_color);
                                ui.add(
                                    egui::DragValue::new(&mut self.grid_spacing)
                                        .range(1.0..=10_000.0)
                                        .prefix("间距 "),
                                );
                                ui.add(
                                    egui::DragValue::new(&mut self.grid_subdivisions)
                                        .range(1..=32)
                                        .prefix("细分 "),
                                );
                            } else if matches!(dialog, Dialog::ColorRange) {
                                let mut fuzziness =
                                    crate::filters::number(&self.dialog_value, "fuzziness", 40.)
                                        as u8;
                                if ui
                                    .add(
                                        egui::Slider::new(&mut fuzziness, 0..=255).text("颜色容差"),
                                    )
                                    .changed()
                                {
                                    self.dialog_value["fuzziness"] = json!(fuzziness);
                                }
                                ui.label("单击预览取样 · Shift 添加取样 · Alt 移除取样");
                                let mut colors = color_range_colors(&self.dialog_value);
                                let mut remove = None;
                                ui.horizontal_wrapped(|ui| {
                                    for (i, color) in colors.iter_mut().enumerate() {
                                        ui.color_edit_button_srgba_unmultiplied(color);
                                        if ui.small_button("×").clicked() {
                                            remove = Some(i);
                                        }
                                    }
                                    if ui.button("添加前景色").clicked() {
                                        colors.push(self.foreground);
                                    }
                                });
                                if let Some(i) = remove {
                                    colors.remove(i);
                                }
                                self.dialog_value["colors"] = json!(colors);
                                if let (Some(texture), Some(sample)) =
                                    (&self.dialog_texture, &self.dialog_sample)
                                {
                                    let size = texture.size_vec2();
                                    let response = ui.add(
                                        egui::Image::new((
                                            texture.id(),
                                            size * (600. / size.x.max(size.y)).min(1.),
                                        ))
                                        .sense(Sense::click()),
                                    );
                                    if response.clicked()
                                        && let Some(p) = response.interact_pointer_pos()
                                    {
                                        let x = ((p.x - response.rect.left())
                                            / response.rect.width()
                                            * sample.width as f32)
                                            .floor()
                                            .clamp(0., sample.width.saturating_sub(1) as f32)
                                            as u32;
                                        let y = ((p.y - response.rect.top())
                                            / response.rect.height()
                                            * sample.height as f32)
                                            .floor()
                                            .clamp(0., sample.height.saturating_sub(1) as f32)
                                            as u32;
                                        let color = sample.get(x, y);
                                        let mut colors = color_range_colors(&self.dialog_value);
                                        if ui.input(|i| i.modifiers.alt) {
                                            if let Some(index) = colors
                                                .iter()
                                                .enumerate()
                                                .min_by_key(|(_, c)| {
                                                    (0..3)
                                                        .map(|i| {
                                                            (c[i] as i32 - color[i] as i32).pow(2)
                                                        })
                                                        .sum::<i32>()
                                                })
                                                .map(|(i, _)| i)
                                            {
                                                colors.remove(index);
                                            }
                                        } else if ui.input(|i| i.modifiers.shift) {
                                            colors.push(color);
                                        } else {
                                            colors = vec![color];
                                        }
                                        self.dialog_value["colors"] = json!(colors);
                                    }
                                }
                            } else if matches!(dialog, Dialog::Trim) {
                                egui::ComboBox::from_id_salt("trim-basis")
                                    .selected_text(tr(self.dialog_value["basis"]
                                        .as_str()
                                        .unwrap_or("Transparent")))
                                    .show_ui(ui, |ui| {
                                        for basis in ["Transparent", "Top Left", "Bottom Right"] {
                                            if ui
                                                .selectable_label(
                                                    self.dialog_value["basis"] == basis,
                                                    tr(basis),
                                                )
                                                .clicked()
                                            {
                                                self.dialog_value["basis"] = json!(basis);
                                            }
                                        }
                                    });
                                json_widgets(ui, &mut self.dialog_value, "trim");
                            } else if matches!(dialog, Dialog::Guides) {
                                ui.checkbox(&mut self.guides_locked, "锁定参考线");
                                ui.checkbox(&mut self.new_guide_horizontal, "水平");
                                ui.add(
                                    egui::DragValue::new(&mut self.new_guide_position)
                                        .prefix("位置 ")
                                        .suffix(" px"),
                                );
                                if ui.button("添加参考线").clicked() {
                                    let horizontal = self.new_guide_horizontal;
                                    let position = self.new_guide_position;
                                    self.apply("Add guide", |d| {
                                        crate::edit::add_guide(d, horizontal, position).map(|_| ())
                                    });
                                }
                                for guide in self.session().doc.guides.clone() {
                                    ui.horizontal(|ui| {
                                        ui.label(tr(&guide.axis));
                                        let mut p = guide.position;
                                        if ui
                                            .add_enabled(
                                                !self.guides_locked,
                                                egui::DragValue::new(&mut p),
                                            )
                                            .changed()
                                        {
                                            self.session_mut().edit("Move guide", |d| {
                                                if let Some(g) =
                                                    d.guides.iter_mut().find(|g| g.id == guide.id)
                                                {
                                                    g.position = p;
                                                }
                                            });
                                        }
                                        if ui
                                            .add_enabled(
                                                !self.guides_locked,
                                                egui::Button::new("删除"),
                                            )
                                            .clicked()
                                        {
                                            self.session_mut().edit("Delete guide", |d| {
                                                d.guides.retain(|g| g.id != guide.id)
                                            });
                                        }
                                    });
                                }
                                if ui
                                    .add_enabled(
                                        !self.guides_locked,
                                        egui::Button::new("清除参考线"),
                                    )
                                    .clicked()
                                {
                                    self.session_mut()
                                        .edit("Clear guides", |d| d.guides.clear());
                                }
                            } else if matches!(dialog, Dialog::Text { .. }) {
                                let editor_id = egui::Id::new((
                                    "canvas-text",
                                    self.dialog_target.map(|(id, _)| id),
                                ));
                                if let Err(e) = crate::text_panel::show_controls(
                                    ui,
                                    &mut self.dialog_value,
                                    editor_id,
                                ) {
                                    ui.colored_label(Color32::LIGHT_RED, tr(&e.to_string()));
                                }
                            } else {
                                match &dialog {
                                    Dialog::Filter { kind } if kind == "Remove Background" => {
                                        let mut quality = self.dialog_value["backgroundQuality"]
                                            .as_str()
                                            .unwrap_or("Basic")
                                            .to_owned();
                                        egui::ComboBox::from_id_salt("background-quality")
                                            .selected_text(tr(&quality))
                                            .show_ui(ui, |ui| {
                                                for choice in ["Basic", "Advanced"] {
                                                    ui.selectable_value(
                                                        &mut quality,
                                                        choice.into(),
                                                        tr(choice),
                                                    );
                                                }
                                            });
                                        self.dialog_value["backgroundQuality"] = json!(quality);
                                        if quality == "Advanced" {
                                            for (key, label, low, high, fallback) in [
                                                ("refineEdges", "细化边缘", 0.0, 40.0, 12.0),
                                                ("matteContrast", "蒙版对比度", 0.0, 100.0, 25.0),
                                                ("shiftEdge", "移动边缘", -10.0, 10.0, 0.0),
                                            ] {
                                                let mut amount = crate::filters::number(
                                                    &self.dialog_value,
                                                    key,
                                                    fallback,
                                                );
                                                if ui
                                                    .add(
                                                        egui::Slider::new(&mut amount, low..=high)
                                                            .text(label),
                                                    )
                                                    .changed()
                                                {
                                                    self.dialog_value[key] = json!(amount);
                                                }
                                            }
                                        }
                                    }
                                    Dialog::Filter { kind } | Dialog::Adjustment { kind } => {
                                        crate::filter_panel::show_dialog_controls(
                                            ui,
                                            kind,
                                            &mut self.dialog_value,
                                            self.dialog_sample.as_deref(),
                                            self.dialog_sample.as_deref(),
                                            &mut self.filter_ui,
                                        );
                                    }
                                    Dialog::Effect { kind } => {
                                        crate::filter_panel::show_effect(
                                            ui,
                                            kind,
                                            &mut self.dialog_value,
                                        );
                                    }
                                    Dialog::CameraRaw => {
                                        crate::filter_panel::show_with_original_sample(
                                            ui,
                                            "Camera Raw Filter",
                                            &mut self.dialog_value,
                                            self.dialog_graded_sample
                                                .as_deref()
                                                .or(self.dialog_sample.as_deref()),
                                            self.dialog_sample.as_deref(),
                                        );
                                        ui.collapsing("色彩分级色轮", |ui| {
                                            crate::raw_interaction::show_grading_wheels(
                                                ui,
                                                &mut self.dialog_value,
                                            );
                                        });
                                    }
                                    _ => json_widgets(ui, &mut self.dialog_value, "dialog"),
                                }
                            }
                            if !matches!(dialog, Dialog::Jpeg)
                                && !self.specialized_filter_dialog()
                                && let Some(t) = self.dialog_texture.clone()
                                && matches!(
                                    dialog,
                                    Dialog::Filter { .. }
                                        | Dialog::Adjustment { .. }
                                        | Dialog::Effect { .. }
                                        | Dialog::CameraRaw
                                )
                            {
                                if matches!(dialog, Dialog::CameraRaw) {
                                    self.raw_pointer_controls(ui);
                                    ui.horizontal(|ui| {
                                        ui.selectable_value(
                                            &mut self.raw_native_view,
                                            false,
                                            "适合窗口",
                                        );
                                        ui.selectable_value(
                                            &mut self.raw_native_view,
                                            true,
                                            "100% 原图",
                                        );
                                    });
                                }
                                let size = t.size_vec2();
                                let response = if matches!(dialog, Dialog::CameraRaw)
                                    && self.raw_native_view
                                    && let Some(source) = &self.dialog_display_sample
                                {
                                    crate::filter_panel::camera_raw_native_preview(
                                        ui,
                                        source,
                                        &mut self.raw_view_texture,
                                    )
                                } else {
                                    ui.add(
                                        egui::Image::new((
                                            t.id(),
                                            size * (ui.available_width().min(400.)
                                                / size.x.max(size.y))
                                            .min(1.),
                                        ))
                                        .sense(Sense::click_and_drag()),
                                    )
                                };
                                if matches!(dialog, Dialog::CameraRaw)
                                    && let Some(source) = self.dialog_sample.clone()
                                {
                                    self.raw_preview_gesture(ui, &response, &source);
                                }
                            }
                            if self.dialog_preview_busy {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label("正在更新预览…");
                                });
                            }
                            if let Some(error) = &self.dialog_preview_error {
                                ui.colored_label(Color32::LIGHT_RED, tr(error));
                            }
                        });
                        let (requested_cancel, requested_apply) = crate::dialog_layout::footer(
                            ui,
                            docked,
                            !matches!(dialog, Dialog::Jpeg)
                                || (!self.dialog_preview_busy
                                    && self.dialog_preview_error.is_none()
                                    && self.dialog_texture.is_some()),
                            if matches!(dialog, Dialog::Jpeg) {
                                "导出…"
                            } else {
                                "应用"
                            },
                        );
                        cancel |= requested_cancel;
                        apply |= requested_apply;
                    });
                });
                cancel |= !panel_open;
            }
            if cancel {
                self.cancel_dialog();
            }
            if apply {
                let original_current = self.session().id;
                if let Some((target, layer)) = self.dialog_target {
                    let Some(index) = self.tabs.iter().position(|s| s.id == target) else {
                        self.dialog = None;
                        self.dialog_target = None;
                        self.message = Some("The project for this dialog was closed".into());
                        return;
                    };
                    self.activate_tab(index);
                    if layer.is_some_and(|id| self.session().doc.layer(id).is_none()) {
                        self.current = self
                            .tabs
                            .iter()
                            .position(|s| s.id == original_current)
                            .unwrap_or(self.current);
                        self.dialog = None;
                        self.dialog_target = None;
                        self.message = Some("The layer for this dialog was deleted".into());
                        return;
                    }
                    if layer.is_some() {
                        self.session_mut().doc.active_layer_id = layer;
                    }
                }
                if matches!(dialog, Dialog::Text { .. }) && self.text_color_target.is_some() {
                    self.finish_color_picker(true);
                }
                let value = self.dialog_value.clone();
                match dialog {
                    Dialog::CanvasSize => {
                        let [w, h] = self.new_size;
                        let anchor = self.canvas_anchor;
                        self.apply("Canvas size", |d| {
                            crate::edit::resize_canvas(d, w, h, anchor)
                        });
                        self.session_mut().zoom = 0.;
                    }
                    Dialog::ImageSize => {
                        let [w, h] = self.new_size;
                        let resolution = self.new_resolution;
                        let sampling = self.resize_sampling;
                        self.background_edit("Image size", move |d| {
                            crate::edit::resize_image(d, w, h, sampling)?;
                            d.resolution = resolution;
                            Ok(())
                        });
                        self.session_mut().zoom = 0.;
                    }
                    Dialog::Grid => {}
                    Dialog::Guides => {}
                    Dialog::Jpeg => self.export("jpg"),
                    Dialog::Trim => {
                        let basis = match value["basis"].as_str() {
                            Some("Top Left") => crate::edit::TrimBasis::TopLeft,
                            Some("Bottom Right") => crate::edit::TrimBasis::BottomRight,
                            _ => crate::edit::TrimBasis::Transparent,
                        };
                        let tolerance = value["tolerance"].as_u64().unwrap_or(0).min(255) as u8;
                        let sides = ["left", "top", "right", "bottom"]
                            .map(|k| value[k].as_bool().unwrap_or(true));
                        self.background_edit("Trim", move |d| {
                            crate::edit::trim(d, basis, tolerance, sides)
                        });
                    }
                    Dialog::ColorRange => {
                        let colors = color_range_colors(&value);
                        let fuzziness =
                            crate::filters::number(&value, "fuzziness", 40.).clamp(0., 255.) as u8;
                        let mode = self.selection_mode;
                        self.background_edit("Color range", move |d| {
                            let preview = crate::preview::PreviewCache::default().render(d, 1600);
                            let selected = selection::color_range(&preview, &colors, fuzziness);
                            let incoming = scale_selection(&selected, d.width, d.height);
                            d.selection =
                                Some(selection::combine(d.selection.as_ref(), &incoming, mode));
                            Ok(())
                        });
                    }
                    Dialog::Text { layer, origin } => {
                        self.background_edit("Edit text", move |d| {
                            let style = crate::text::TextStyle::from_value(&value)?;
                            if layer.is_none() && style.content.trim().is_empty() {
                                return Ok(());
                            }
                            if let Some(id) = layer {
                                crate::text::update_text(d, id, style)?;
                                let mut transform = d.layer(id).unwrap().transform;
                                transform.origin = origin;
                                crate::edit::set_transform(d, id, transform, false)
                            } else {
                                crate::text::add_text(d, style, origin).map(|_| ())
                            }
                        });
                    }
                    Dialog::Adjustment { kind } => self.apply("New adjustment", |d| {
                        let mut l = Layer::new(kind, d.width, d.height);
                        l.adjustment = Some(project::canonical_adjustment(&value)?);
                        l.parent_id = d
                            .active()
                            .and_then(|l| if l.is_group { Some(l.id) } else { l.parent_id });
                        let index = d
                            .active_layer_id
                            .and_then(|id| d.layers.iter().position(|l| l.id == id))
                            .map_or(d.layers.len(), |i| i + 1);
                        d.active_layer_id = Some(l.id);
                        d.layers.insert(index, l);
                        Ok(())
                    }),
                    Dialog::Effect { kind } => self.session_mut().edit("Layer effect", |d| {
                        if let Some(l) = d.active_mut() {
                            let mut effects = l.effects.take().unwrap_or_else(|| json!({}));
                            effects[&kind] = value;
                            l.effects = Some(effects);
                        }
                    }),
                    Dialog::Filter { kind } => self.filter(&kind, value, self.dialog_mask),
                    Dialog::CameraRaw => self.filter("Camera Raw Filter", value, self.dialog_mask),
                    Dialog::Raw { path, target } => self.import_raw(path, target),
                }
                self.cancel_dialog();
                if let Some(index) = self.tabs.iter().position(|s| s.id == original_current) {
                    self.activate_tab(index);
                }
            }
        }
        if self
            .close_pending
            .is_some_and(|id| self.tabs.iter().all(|s| s.id != id))
        {
            self.close_pending = None;
        }
        if let Some(i) = self
            .close_pending
            .and_then(|id| self.tabs.iter().position(|s| s.id == id))
        {
            let title = self.tabs.get(i).map(Session::title).unwrap_or_default();
            let mut action = 0;
            let busy = self.tabs[i].save_busy
                || self.tabs[i].edit_busy
                || self.tabs[i].reload_busy
                || (i == self.current
                    && (self.drag.is_some()
                        || self.guide_drag.is_some()
                        || self.gradient_handle.is_some()
                        || self.property_edit.is_some()));
            egui::Window::new("保存修改？")
                .id(egui::Id::new("Save changes?"))
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("关闭前保存“{title}”的修改吗？"));
                    if busy {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("正在等待当前操作完成…");
                        });
                    }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!busy, egui::Button::new("保存")).clicked() {
                            action = 1;
                        }
                        if ui.add_enabled(!busy, egui::Button::new("不保存")).clicked() {
                            action = 2;
                        }
                        if ui.button("取消").clicked() {
                            action = 3;
                        }
                    });
                });
            match action {
                1 => {
                    self.activate_tab(i);
                    self.save(false);
                }
                2 => {
                    self.close_pending = None;
                    self.close_tab(i);
                }
                3 => {
                    self.close_pending = None;
                    self.exit_pending = false;
                }
                _ => {}
            }
            if self.tabs.get(i).is_some_and(|s| {
                self.close_pending == Some(s.id)
                    && s.path.is_some()
                    && !s.history.is_modified()
                    && !s.save_busy
                    && !s.edit_busy
                    && !s.reload_busy
            }) && !busy
            {
                self.close_pending = None;
                self.close_tab(i);
            }
        }
        if let Some((id, stamp)) = self.external_pending {
            let mut action = 0;
            egui::Window::new("磁盘上的项目已更改")
                .id(egui::Id::new("Project changed on disk"))
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("外部修改与未保存的编辑冲突。请重新载入磁盘项目，或保留当前编辑。");
                    ui.horizontal(|ui| {
                        if ui.button("重新载入").clicked() {
                            action = 1;
                        }
                        if ui.button("保留编辑").clicked() {
                            action = 2;
                        }
                    });
                });
            if action > 0 {
                if let Some(s) = self.tabs.iter_mut().find(|s| s.id == id) {
                    s.stamp = stamp;
                    if action == 1
                        && let Some(path) = s.path.clone()
                    {
                        let sender = self.sender.clone();
                        let revision = s.history.revision();
                        let serial = s.serial;
                        s.reload_busy = true;
                        if let Err(error) = gui_threads::spawn("重新载入", move || {
                            let result = gui_threads::run("重新载入", || {
                                project::load(&path).map_err(|e| format!("{e:#}"))
                            });
                            let _ = sender.send(Worker::Reload {
                                id,
                                stamp,
                                revision,
                                serial,
                                force: true,
                                result,
                            });
                        }) {
                            s.reload_busy = false;
                            self.message = Some(error);
                        }
                    }
                }
                self.external_pending = None;
            }
        }
        if !self.report.is_empty() {
            let mut open = true;
            let mut accepted = false;
            let mut cancel = false;
            egui::Window::new("导入转换报告")
                .id(egui::Id::new("Import conversion report"))
                .open(&mut open)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(400.)
                        .show(ui, |ui| {
                            for line in &self.report {
                                ui.label(tr(line));
                            }
                        });
                    if self.pending_import.is_some() {
                        ui.horizontal(|ui| {
                            accepted = ui.button("导入").clicked();
                            cancel = ui.button("取消").clicked();
                        });
                    }
                });
            if accepted {
                if let Some((path, import, target)) = self.pending_import.take() {
                    self.install_import(path, import, target);
                }
                self.report.clear();
            }
            if !open || cancel {
                self.pending_import = None;
                self.report.clear();
            }
        }
        if self.pending_import.is_none()
            && let Some(next) = self.import_queue.pop_front()
        {
            self.report = next.1.conversions.clone();
            self.pending_import = Some(next);
        }
        if self.dialog.is_none()
            && self.pending_import.is_none()
            && let Some((path, target)) = self.raw_queue.pop_front()
        {
            self.open_target(path, target);
        }
        if self.shortcuts_open {
            let mut open = true;
            egui::Window::new("键盘快捷键")
                .id(egui::Id::new("Keyboard Shortcuts"))
                .open(&mut open)
                .show(ctx, |ui| {
                    for (name, shortcut) in &mut self.shortcuts {
                        ui.horizontal(|ui| {
                            ui.label(tr(name));
                            ui.text_edit_singleline(shortcut);
                            if parse_shortcut(shortcut).is_none() {
                                ui.colored_label(Color32::LIGHT_RED, "无效");
                            }
                        });
                    }
                    if ui.button("保存快捷键").clicked() {
                        let p = shortcuts_path();
                        if let Some(parent) = p.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        if let Ok(bytes) = serde_json::to_vec_pretty(&self.shortcuts)
                            && let Err(e) = image_io::write_atomic(&p, &bytes)
                        {
                            self.message = Some(e.to_string());
                        }
                    }
                });
            self.shortcuts_open = open;
        }
    }
    fn filter(&mut self, kind: &str, value: Value, mask: bool) {
        let Some(id) = self.session().doc.active_layer_id else {
            return;
        };
        if kind == "Remove Background" {
            self.segment(SegmentOperation::Remove {
                layer: id,
                settings: value,
            });
            return;
        }
        let value = committed_filter_settings(kind, value);
        let kind = kind.to_owned();
        self.background_edit(&kind.clone(), move |d| {
            crate::filter_edit::apply(d, id, mask, &kind, &value)
        });
    }
    fn watch(&mut self) {
        self.disk_changes.retain(|id, (target, _)| {
            self.tabs.iter().any(|s| {
                s.id == *id && s.path.as_ref() == Some(&target.path) && s.stamp == target.stamp
            })
        });
        if self.external_pending.is_some() || self.drag.is_some() || self.guide_drag.is_some() {
            return;
        }
        for s in &mut self.tabs {
            if s.save_busy || s.reload_busy || s.edit_busy {
                continue;
            }
            if let Some((target, _)) = self.disk_changes.remove(&s.id) {
                // Dialogs and gestures may delay notification handling; recheck before replacing edits.
                let stamp = package_stamp(&target.path);
                if stamp == s.stamp {
                    continue;
                }
                if s.history.is_modified() {
                    self.external_pending = Some((s.id, stamp));
                    break;
                }
                let path = target.path;
                let id = s.id;
                let revision = s.history.revision();
                let serial = s.serial;
                let sender = self.sender.clone();
                s.reload_busy = true;
                if let Err(error) = gui_threads::spawn("重新载入", move || {
                    let result = gui_threads::run("重新载入", || {
                        project::load(&path).map_err(|e| format!("{e:#}"))
                    });
                    let _ = sender.send(Worker::Reload {
                        id,
                        stamp,
                        revision,
                        serial,
                        force: false,
                        result,
                    });
                }) {
                    s.reload_busy = false;
                    self.message = Some(error);
                }
            }
        }
    }
    fn sync_watch_targets(&mut self) {
        let suspended =
            self.external_pending.is_some() || self.drag.is_some() || self.guide_drag.is_some();
        let targets = self
            .tabs
            .iter()
            .filter(|s| !suspended && !s.save_busy && !s.reload_busy && !s.edit_busy)
            .filter_map(|s| {
                s.path.as_ref().map(|path| WatchTarget {
                    id: s.id,
                    path: path.clone(),
                    stamp: s.stamp,
                })
            })
            .collect();
        if let Err(error) = self.watcher.sync(targets) {
            self.message = Some(error);
        }
    }
    fn repaint_delay(&self, time: f64) -> Option<Duration> {
        let animation = self
            .session()
            .doc
            .selection
            .as_ref()
            .map(|_| selection_repaint_delay(time));
        let busy = self.busy_since.and_then(|start| {
            let remaining = Duration::from_millis(250).saturating_sub(start.elapsed());
            (!remaining.is_zero()).then_some(remaining)
        });
        let smoke = (self.smoke && !self.smoke_done)
            .then(|| Duration::from_secs(2).saturating_sub(self.started.elapsed()));
        [animation, busy, smoke].into_iter().flatten().min()
    }
    fn schedule_repaint(&self, ctx: &egui::Context, busy_shown: bool) {
        // Rendering can cross the feedback deadline after this frame's status bar was painted.
        if !busy_shown
            && self.operation_busy()
            && self
                .busy_since
                .is_some_and(|start| start.elapsed() >= Duration::from_millis(250))
        {
            ctx.request_repaint();
        } else if let Some(delay) = self.repaint_delay(ctx.input(|i| i.time)) {
            ctx.request_repaint_after(delay);
        }
    }
    fn record_qa_ready(&mut self) {
        if let Some((path, started)) = self.qa_ready.take() {
            let marker = json!({
                "stage": "first update completed",
                "elapsed_ms": started.elapsed().as_secs_f64() * 1000.,
                "gpu_presentation_measured": false,
                "adapter": self.gpu.as_ref().map(GpuCanvas::adapter_info),
            });
            if let Err(error) = image_io::write_atomic(&path, &serde_json::to_vec(&marker).unwrap())
            {
                eprintln!("Could not write UI readiness marker: {error}");
            }
        }
    }
    fn smoke_test(&mut self, ctx: &egui::Context) {
        if !self.smoke || self.smoke_done || self.started.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.smoke_done = true;
        let output_root = std::env::var_os("COMPOSITOR_SMOKE_OUTPUT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("build/verification")
            });
        let result = (|| -> anyhow::Result<Value> {
            std::fs::create_dir_all(&output_root)?;
            let mut d = Document::new(640, 400);
            let id = d.layers[0].id;
            crate::brush::paint(
                &mut d,
                id,
                false,
                &[[40., 60.], [240., 180.], [500., 80.]],
                crate::brush::BrushSettings {
                    diameter: 30.,
                    color: [31, 154, 222, 255],
                    ..Default::default()
                },
                None,
            )?;
            crate::edit::add_shape(
                &mut d,
                crate::edit::ShapeKind::Ellipse,
                [360., 190., 160., 160.],
                [241, 155, 41, 255],
                0.,
                4.,
                None,
            )?;
            let path = output_root.join("gui-smoke.comp");
            let mut h = History::default();
            let before = d.clone();
            d.layers[0].opacity = 0.6;
            h.record("Opacity", before, &d);
            let changed = d.clone();
            h.undo(&mut d);
            anyhow::ensure!(d.layers[0].opacity == 1., "Undo failed");
            h.redo(&mut d);
            anyhow::ensure!(d == changed, "Redo failed");
            project::save(&d, &path)?;
            let reopened = project::load(&path)?;
            anyhow::ensure!(
                reopened.layers.len() == 2 && reopened.layers[0].opacity == 0.6,
                "Project round trip failed"
            );
            let raster = render::render_document(&reopened);
            image_io::export_png(&raster, &path.with_extension("png"), 72.)?;
            self.tabs.push(Session::new(reopened, Some(path)));
            self.activate_tab(self.tabs.len() - 1);
            Ok(
                json!({"passed":true,"checks":["paint","shape","undo","redo","save","reopen","flattened PNG"],"adapter":self.gpu.as_ref().map(GpuCanvas::adapter_info)}),
            )
        })();
        let value = match result {
            Ok(v) => v,
            Err(e) => {
                self.message = Some(e.to_string());
                json!({"passed":false,"error":format!("{e:#}")})
            }
        };
        let path = output_root.join("gui-smoke.json");
        let _ = image_io::write_atomic(&path, &serde_json::to_vec_pretty(&value).unwrap());
        ctx.request_repaint();
    }
    fn handle_window_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_exit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.exit_pending = true;
        }
        if self.exit_pending && self.dialog.is_some() {
            if matches!(self.dialog, Some(Dialog::Text { .. })) {
                self.text_apply_requested = true;
            } else {
                self.message = Some("Apply or cancel the open dialog to finish closing".into());
            }
        }
        if self.exit_pending {
            self.pdf_ui.cancel();
        }
        if self.exit_pending && self.pending_io > 0 {
            self.message = Some("正在等待文件读写完成后关闭…".into());
        }
        if self.exit_pending
            && self.close_pending.is_none()
            && self.dialog.is_none()
            && self.pending_io == 0
        {
            if self.drag.is_some()
                || self.guide_drag.is_some()
                || self.gradient_handle.is_some()
                || self.property_edit.is_some()
            {
                self.close_pending = Some(self.session().id);
            } else if let Some(i) = self.tabs.iter().position(|s| {
                s.history.is_modified() || s.save_busy || s.edit_busy || s.reload_busy
            }) {
                self.close_pending = Some(self.tabs[i].id);
            } else {
                self.window_motion.begin_close(ctx.input(|i| i.time));
            }
        }
        self.window_motion.suppress_input(ctx);
    }

    fn window_close_ready(&self) -> bool {
        self.pending_io == 0
            && self.close_pending.is_none()
            && self.dialog.is_none()
            && self.drag.is_none()
            && self.guide_drag.is_none()
            && self.gradient_handle.is_none()
            && self.property_edit.is_none()
            && self
                .tabs
                .iter()
                .all(|s| !s.history.is_modified() && !s.save_busy && !s.edit_busy && !s.reload_busy)
    }
}

impl eframe::App for EditorApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        let requested = input
            .viewports
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|viewport| viewport.close_requested());
        // Clear events before egui computes clicked/dragged widget IDs for this frame.
        if self.window_motion.is_closing()
            || ((requested || self.exit_pending) && self.window_close_ready())
        {
            input.events.clear();
            input.dropped_files.clear();
            input.hovered_files.clear();
        }
    }
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(
            storage,
            "editor",
            &json!({"themePreference":self.theme_preference,"customTheme":self.custom_theme,"customThemeEnabled":self.custom_theme_enabled,"floatingLayers":self.floating_layers,"floatingTools":self.floating_tools,"gridSpacing":self.grid_spacing,"gridSubdivisions":self.grid_subdivisions,"rulers":self.rulers,"grid":self.grid,"snap":self.snap,"snapTargets":self.snap_targets,"sampleRing":self.show_sample_ring,"transformAutoSelect":self.transform_auto_select,"locksTransformRatio":self.locks_transform_ratio,"gridColor":self.grid_color,"jpegQuality":self.jpeg_quality}),
        );
    }
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.restore_window_after_first_frame(ctx);
        self.window_motion.refresh_system_preference();
        crate::chrome::theme::set_animations(ctx, self.window_motion.animations_enabled());
        self.window_motion.suppress_input(ctx);
        if self.sample_cache.as_ref().is_some_and(|(id, sampler)| {
            *id != self.session().id || !sampler.matches(&self.session().doc)
        }) {
            self.sample_cache = None;
        }
        if !ctx.input(|i| i.pointer.primary_down()) {
            self.sample_drag = None;
        }
        if !ctx.input(|i| i.pointer.primary_down())
            && let Some((id, before)) = self.property_edit.take()
            && let Some(session) = self.tabs.iter_mut().find(|s| s.id == id)
            && session
                .history
                .record("Layer properties", before, &session.doc)
        {
            session.changed();
        }
        if !self.window_motion.is_closing() {
            self.poll(ctx);
            self.poll_agent();
            self.watch();
            self.smoke_test(ctx);
        }
        if self.operation_busy() {
            self.busy_since.get_or_insert_with(Instant::now);
        } else {
            self.busy_since = None;
        }
        let show_busy = self
            .busy_since
            .is_some_and(|start| start.elapsed() >= Duration::from_millis(250));
        self.handle_window_close(ctx);
        let before_theme = self.theme_preference;
        let mut commands = self.menu(ctx);
        self.show_theme_settings(ctx);
        self.open_source_licenses.show(ctx);
        if before_theme != self.theme_preference || self.theme_settings_dirty {
            if let Some(storage) = frame.storage_mut() {
                eframe::App::save(self, storage);
                storage.flush();
            }
            self.theme_settings_dirty = false;
        }
        commands.extend(self.workspace_toolbar(ctx));
        commands.extend(self.tool_header(ctx));
        self.status_ui(ctx, show_busy);
        commands.extend(self.shortcuts(ctx));
        if self.floating_tools {
            egui::Window::new("工具")
                .id(egui::Id::new("Tools"))
                .default_pos([12., 150.])
                .show(ctx, |ui| self.tools_ui(ui));
        } else {
            egui::SidePanel::left("tools")
                .exact_width(crate::chrome::metrics::RAIL_WIDTH)
                .frame(egui::Frame::NONE.fill(ctx.style().visuals.panel_fill))
                .resizable(false)
                .show(ctx, |ui| self.tools_ui(ui));
        }
        if self.floating_layers {
            egui::Window::new("图层")
                .id(egui::Id::new("Layers"))
                .default_pos([970., 150.])
                .default_width(crate::chrome::metrics::LAYERS_DEFAULT_WIDTH)
                .frame(egui::Frame::window(&ctx.style()).inner_margin(0.))
                .show(ctx, |ui| {
                    commands.extend(self.layers_ui(ui));
                });
        } else {
            egui::SidePanel::right("layers")
                .default_width(crate::chrome::metrics::LAYERS_DEFAULT_WIDTH)
                .min_width(crate::chrome::metrics::LAYERS_MIN_WIDTH)
                .frame(egui::Frame::NONE.fill(ctx.style().visuals.panel_fill))
                .max_width(crate::chrome::metrics::LAYERS_MAX_WIDTH)
                .show(ctx, |ui| {
                    commands.extend(self.layers_ui(ui));
                });
        }
        let mut save_actions = vec![];
        for c in commands {
            if matches!(c, Command::Save | Command::SaveAs) {
                save_actions.push(c);
            } else {
                self.command(c);
            }
        }
        if let Some(g) = self.gradient.clone() {
            if self.tool != Tool::Gradient
                || g.session != self.session().id
                || g.revision.unwrap_or(g.base_revision) != self.session().history.revision()
                || g.mask != self.mask_target
                || self.session().doc.active_layer_id != Some(g.layer)
            {
                self.gradient = None;
                self.gradient_handle = None;
            } else if self.gradient_handle.is_none()
                && !self.session().edit_busy
                && g.parameters != self.gradient_parameters()
            {
                self.update_gradient(g);
            }
        }
        self.update_warp_preview();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                if self.is_welcome() {
                    self.welcome_ui(ui);
                } else {
                    self.canvas(ui);
                }
            });
        // A same-frame pointer release must finalize the stroke before taking a save snapshot.
        for command in save_actions {
            self.command(command);
        }
        self.dialogs(ctx);
        self.show_selection_amount_dialog(ctx);
        self.show_color_picker(ctx);
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            if let Some(path) = f.path {
                self.open(path, ctx.input(|i| i.modifiers.shift));
            }
        }
        if self.operation_busy() {
            self.busy_since.get_or_insert_with(Instant::now);
        }
        self.sync_watch_targets();
        self.schedule_repaint(ctx, show_busy);
        if self.window_motion.paint(ctx) {
            self.allow_exit = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.record_qa_ready();
    }
}
