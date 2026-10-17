//! Search existing editor actions without maintaining a second execution path.
use super::*;
use std::sync::OnceLock;

#[derive(Default)]
pub(super) struct CommandPalette {
    open: bool,
    query: String,
    selected: usize,
    request_focus: bool,
    composing: bool,
}

impl CommandPalette {
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    // A candidate-confirming Enter belongs to the IME, including the commit frame.
    fn update_composition(&mut self, events: &[egui::Event]) -> bool {
        let mut composition_event = false;
        for event in events {
            if let egui::Event::Ime(event) = event {
                match event {
                    egui::ImeEvent::Preedit(text) => {
                        self.composing = !text.is_empty();
                        composition_event = true;
                    }
                    egui::ImeEvent::Commit(_) => {
                        self.composing = false;
                        composition_event = true;
                    }
                    egui::ImeEvent::Disabled => self.composing = false,
                    egui::ImeEvent::Enabled => {}
                }
            }
        }
        self.composing || composition_event
    }
}

#[derive(Clone, Copy)]
enum Action {
    Command(Command),
    Tool(Tool),
    Filter(&'static str),
    Adjustment(&'static str),
    Effect(&'static str),
    Snap,
    SnapTarget(usize),
    PixelGrid,
    FloatingLayers,
    FloatingTools,
    ThemeSettings,
    Theme(egui::ThemePreference),
    Licenses,
}

struct Entry {
    title: String,
    category: &'static str,
    search: Vec<Vec<char>>,
    action: Action,
}

impl Entry {
    fn new(
        title: impl Into<String>,
        category: &'static str,
        aliases: &str,
        action: Action,
    ) -> Self {
        let title = title.into();
        let search = std::iter::once(title.clone())
            .chain(std::iter::once(format!("{category}{title}")))
            .chain(aliases.split('|').map(str::to_owned))
            .map(|text| normalized(&text))
            .filter(|text| !text.is_empty())
            .collect();
        Self {
            title,
            category,
            search,
            action,
        }
    }

    fn shortcut<'a>(&self, shortcuts: &'a BTreeMap<String, String>) -> &'a str {
        let key = match self.action {
            Action::Command(command) => match command {
                Command::New => "New",
                Command::Open => "Open",
                Command::Save => "Save",
                Command::SaveAs => "Save As",
                Command::Undo => "Undo",
                Command::Redo => "Redo",
                Command::Copy => "Copy",
                Command::Cut => "Cut",
                Command::Paste => "Paste",
                Command::SelectAll => "Select All",
                Command::Deselect => "Deselect",
                Command::Close => "Close",
                Command::RepeatFilter => "Repeat Last Filter",
                Command::Search => "Search Commands",
                Command::CanvasOnly => return "F",
                _ => return "",
            },
            Action::Tool(tool) => {
                let Some((key, _)) = tool_bindings()
                    .into_iter()
                    .find(|(_, value)| *value == tool)
                else {
                    return "";
                };
                key
            }
            _ => return "",
        };
        shortcuts.get(key).map(String::as_str).unwrap_or("")
    }

    fn disabled_reason(&self, app: &EditorApp) -> Option<&'static str> {
        if app.pdf_ui.is_open() {
            return Some("请先关闭 PDF 页面选择窗口");
        }
        match self.action {
            Action::Command(command) => {
                if !app.command_available(command) {
                    return Some(if app.is_welcome() {
                        "请先新建或打开图像"
                    } else if matches!(command, Command::RepeatFilter) && app.last_filter.is_none()
                    {
                        "还没有应用过滤镜"
                    } else {
                        "请先完成当前操作"
                    });
                }
                match command {
                    Command::Undo if app.session().history.undo_name().is_none() => {
                        Some("没有可撤销的操作")
                    }
                    Command::Redo if app.session().history.redo_name().is_none() => {
                        Some("没有可重做的操作")
                    }
                    Command::Deselect
                    | Command::Expand
                    | Command::Contract
                    | Command::Feather
                    | Command::CropSelection
                        if app.session().doc.selection.is_none() =>
                    {
                        Some("请先建立选区")
                    }
                    Command::InvertMask
                    | Command::ApplyMask
                    | Command::DeleteMask
                    | Command::CopyMask
                    | Command::LoadMask
                    | Command::BlurMask
                        if app
                            .session()
                            .doc
                            .active()
                            .is_none_or(|layer| layer.mask.is_none()) =>
                    {
                        Some("当前图层没有蒙版")
                    }
                    _ => None,
                }
            }
            Action::Tool(_) | Action::Filter(_) | Action::Adjustment(_) | Action::Effect(_) => {
                if app.is_welcome() {
                    Some("请先新建或打开图像")
                } else if app.dialog.is_some() || app.session().edit_busy {
                    Some("请先完成当前操作")
                } else if matches!(self.action, Action::Filter(_) | Action::Effect(_))
                    && app.session().doc.active().is_none()
                {
                    Some("请先选择图层")
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

fn entries() -> &'static [Entry] {
    static ENTRIES: OnceLock<Vec<Entry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        use Command::*;
        let mut entries = Vec::new();
        for (title, category, aliases, command) in [
            ("新建画布…", "文件", "New|New Canvas|Create", New),
            ("打开…", "文件", "Open|Open Image|Open Project", Open),
            (
                "导入为图层…",
                "文件",
                "Import|Import Image|Import Layer",
                Import,
            ),
            (
                "导入图像文件夹…",
                "文件",
                "Import Folder|Open Folder|Batch Import|批量导入",
                ImportFolder,
            ),
            ("保存", "文件", "Save|Save Project", Save),
            ("另存为…", "文件", "Save As|Save Project As", SaveAs),
            ("导出 PNG…", "文件", "Export PNG|透明图片", ExportPng),
            ("导出 JPEG…", "文件", "Export JPEG|JPG", ExportJpeg),
            ("导出 TIFF…", "文件", "Export TIFF|TIF", ExportTiff),
            (
                "导出…（格式／尺寸／位深）",
                "文件",
                "Export Other|Export Image|BMP|WebP|GIF|EXR|HDR|CMYK",
                ExportOther,
            ),
            ("导出 PDF…", "文件", "Export PDF|合并多页", ExportPdf),
            ("关闭项目", "文件", "Close|Close Tab|Close Project", Close),
            ("撤销", "编辑", "Undo", Undo),
            ("重做", "编辑", "Redo", Redo),
            ("复制", "编辑", "Copy", Copy),
            ("合并复制", "编辑", "Copy Merged", CopyMerged),
            ("剪切", "编辑", "Cut", Cut),
            ("粘贴", "编辑", "Paste", Paste),
            ("填充前景色", "编辑", "Fill Foreground", FillForeground),
            ("填充背景色", "编辑", "Fill Background", FillBackground),
            (
                "清除所选像素",
                "编辑",
                "Clear Pixels|Erase Selection",
                ClearPixels,
            ),
            ("反相所选像素", "编辑", "Invert Pixels", InvertPixels),
            (
                "内容识别填充",
                "编辑",
                "Content Aware Fill|Content Fill",
                ContentFill,
            ),
            ("新建图层", "图层", "Add Layer|New Layer", AddLayer),
            ("将所选图层编组", "图层", "Group Layers", Group),
            ("取消编组", "图层", "Ungroup Layers", Ungroup),
            ("复制图层", "图层", "Duplicate Layer", Duplicate),
            ("删除图层", "图层", "Delete Layer", Delete),
            ("合并所选图层", "图层", "Merge Layers|Merge Selected", Merge),
            ("向下合并", "图层", "Merge Down", MergeDown),
            (
                "转换为 8 位图层",
                "图层",
                "Convert Depth|8-bit",
                ConvertDepth(None),
            ),
            (
                "转换为 16 位图层",
                "图层",
                "Convert Depth|16-bit",
                ConvertDepth(Some(crate::deep_pixels::Depth::U16)),
            ),
            (
                "转换为 HDR 图层",
                "图层",
                "Convert Depth|Float|HDR",
                ConvertDepth(Some(crate::deep_pixels::Depth::Hdr)),
            ),
            ("添加图层蒙版", "图层", "Add Layer Mask", Mask),
            ("反相蒙版", "图层", "Invert Mask", InvertMask),
            ("应用蒙版", "图层", "Apply Mask", ApplyMask),
            ("删除蒙版", "图层", "Delete Mask", DeleteMask),
            ("复制蒙版", "图层", "Copy Mask", CopyMask),
            ("粘贴蒙版", "图层", "Paste Mask", PasteMask),
            ("选择蒙版区域", "图层", "Load Mask|Mask Selection", LoadMask),
            (
                "模糊／羽化蒙版…",
                "图层",
                "Blur Mask|Feather Mask",
                BlurMask,
            ),
            ("创建／释放剪贴蒙版", "图层", "Clipping Mask|Clip", Clip),
            ("全选", "选择", "Select All", SelectAll),
            ("取消选择", "选择", "Deselect|Select None", Deselect),
            ("反选", "选择", "Invert Selection", InvertSelection),
            ("选择主体", "选择", "Select Subject", Subject),
            ("色彩范围…", "选择", "Color Range|Colour Range", ColorRange),
            (
                "载入图层透明度选区",
                "选择",
                "Load Alpha|Transparency Selection",
                LoadAlpha,
            ),
            ("扩展选区…", "选择", "Expand Selection", Expand),
            ("收缩选区…", "选择", "Contract Selection", Contract),
            ("羽化选区…", "选择", "Feather Selection", Feather),
            ("画布大小…", "图像", "Canvas Size", CanvasSize),
            ("图像大小…", "图像", "Image Size|Resize Image", ImageSize),
            (
                "裁剪到选区",
                "图像",
                "Crop Selection|Crop to Selection",
                CropSelection,
            ),
            ("修剪透明边缘", "图像", "Trim|Trim Transparent Edges", Trim),
            (
                "水平翻转画布",
                "图像",
                "Flip Canvas Horizontal|Flip X",
                FlipCanvasX,
            ),
            (
                "垂直翻转画布",
                "图像",
                "Flip Canvas Vertical|Flip Y",
                FlipCanvasY,
            ),
            (
                "顺时针旋转画布 90°",
                "图像",
                "Rotate Canvas Clockwise|Rotate Right|CW",
                RotateCanvas(true),
            ),
            (
                "逆时针旋转画布 90°",
                "图像",
                "Rotate Canvas Counterclockwise|Rotate Left|CCW",
                RotateCanvas(false),
            ),
            (
                "重复上次滤镜",
                "滤镜",
                "Repeat Last Filter|Repeat Filter",
                RepeatFilter,
            ),
            ("适合窗口", "视图", "Fit|Fit Canvas|Fit to Window", Fit),
            ("实际大小 100%", "视图", "Actual Size|100 Percent", Actual),
            ("切换网格", "视图", "Toggle Grid|Show Grid", Grid),
            ("切换标尺", "视图", "Toggle Rulers|Show Rulers", Rulers),
            ("网格设置…", "视图", "Grid Settings", GridSettings),
            ("参考线…", "视图", "Guides|Guide Settings", Guides),
            (
                "全屏画布",
                "视图",
                "Canvas Only|Full Screen|Fullscreen",
                CanvasOnly,
            ),
            ("应用裁剪", "工具", "Commit Crop|Apply Crop", CropCommit),
            ("取消裁剪", "工具", "Cancel Crop", CropCancel),
            (
                "快捷键设置…",
                "帮助",
                "Shortcuts|Keyboard Shortcuts",
                Shortcuts,
            ),
            (
                "搜索命令…",
                "帮助",
                "Search Commands|Command Palette|Find Action",
                Search,
            ),
        ] {
            entries.push(Entry::new(
                title,
                category,
                aliases,
                Action::Command(command),
            ));
        }
        for tool in Tool::ALL {
            let title = tool.label().split('(').next().unwrap().trim();
            let alias = tool_bindings()
                .into_iter()
                .find(|(_, value)| *value == tool)
                .map(|(name, _)| name.to_owned())
                .unwrap_or_else(|| format!("{tool:?} tool"));
            entries.push(Entry::new(title, "工具", &alias, Action::Tool(tool)));
        }
        for &kind in crate::filters::FILTERS
            .iter()
            .chain(std::iter::once(&"Remove Background"))
        {
            let title = if kind == "Camera Raw Filter" {
                "Camera Raw 调色".into()
            } else {
                tr(kind).into_owned()
            };
            entries.push(Entry::new(title, "滤镜", kind, Action::Filter(kind)));
        }
        for &kind in ADJUSTMENTS {
            entries.push(Entry::new(
                format!("新建{}调整图层", tr(kind)),
                "调整图层",
                &format!("Adjustment {kind}|{kind} Adjustment"),
                Action::Adjustment(kind),
            ));
        }
        for kind in [
            "stroke",
            "shadow",
            "colorOverlay",
            "innerShadow",
            "outerGlow",
            "innerGlow",
        ] {
            entries.push(Entry::new(
                format!("{}图层效果", humanize(kind)),
                "图层效果",
                &humanize_english(kind),
                Action::Effect(kind),
            ));
        }
        for (title, aliases, action) in [
            ("切换吸附", "Snap|Toggle Snapping", Action::Snap),
            ("吸附到画布边界", "Snap Canvas Edges", Action::SnapTarget(0)),
            ("吸附到图层", "Snap Layers", Action::SnapTarget(1)),
            ("吸附到参考线", "Snap Guides", Action::SnapTarget(2)),
            ("吸附到网格", "Snap Grid", Action::SnapTarget(3)),
            ("切换像素网格", "Pixel Grid", Action::PixelGrid),
            (
                "切换浮动图层面板",
                "Floating Layers",
                Action::FloatingLayers,
            ),
            ("切换浮动工具面板", "Floating Tools", Action::FloatingTools),
            (
                "主题设置…",
                "Theme Settings|Custom Theme",
                Action::ThemeSettings,
            ),
            (
                "主题跟随系统",
                "System Theme|Automatic Theme",
                Action::Theme(egui::ThemePreference::System),
            ),
            (
                "日间模式",
                "Light Theme|Day Mode",
                Action::Theme(egui::ThemePreference::Light),
            ),
            (
                "夜间模式",
                "Dark Theme|Night Mode",
                Action::Theme(egui::ThemePreference::Dark),
            ),
            (
                "开源软件许可证",
                "Open Source Licenses|Copyright",
                Action::Licenses,
            ),
        ] {
            entries.push(Entry::new(title, "设置", aliases, action));
        }
        entries
    })
}

fn normalized(text: &str) -> Vec<char> {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn match_score(query: &[char], candidate: &[char]) -> Option<usize> {
    if query.is_empty() || query == candidate {
        return Some(0);
    }
    if query.len() > candidate.len() {
        return None;
    }
    if candidate.starts_with(query) {
        return Some(10 + candidate.len() - query.len());
    }
    if let Some(start) = candidate
        .windows(query.len())
        .position(|part| part == query)
    {
        return Some(40 + start + candidate.len() - query.len());
    }
    let mut found = 0;
    let mut first = 0;
    for (index, &character) in candidate.iter().enumerate() {
        if query[found] == character {
            if found == 0 {
                first = index;
            }
            found += 1;
            if found == query.len() {
                return Some(200 + first * 2 + index + 1 - query.len());
            }
        }
    }
    None
}

fn results(query: &str) -> Vec<usize> {
    let query = normalized(query);
    let mut ranked: Vec<_> = entries()
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            entry
                .search
                .iter()
                .filter_map(|alias| match_score(&query, alias))
                .min()
                .map(|score| (score, index))
        })
        .collect();
    ranked.sort_unstable();
    ranked.into_iter().map(|(_, index)| index).collect()
}

impl EditorApp {
    pub(super) fn open_command_palette(&mut self) {
        self.settle_gestures();
        self.command_palette = CommandPalette {
            open: true,
            request_focus: true,
            ..Default::default()
        };
    }

    fn run_palette_action(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Command(command) => self.command(command),
            Action::Tool(tool) => self.set_tool(tool),
            Action::Filter("Camera Raw Filter") => {
                self.begin_dialog(Dialog::CameraRaw);
                self.dialog_value = camera_raw_defaults();
            }
            Action::Filter(kind) => {
                self.begin_dialog(Dialog::Filter { kind: kind.into() });
                self.dialog_value = adjustment_defaults(kind);
            }
            Action::Adjustment(kind) => {
                self.begin_dialog(Dialog::Adjustment { kind: kind.into() });
                self.dialog_value = adjustment_defaults(kind);
            }
            Action::Effect(kind) => {
                self.begin_dialog(Dialog::Effect { kind: kind.into() });
                self.dialog_value = effect_defaults(kind);
            }
            Action::Snap => self.snap = !self.snap,
            Action::SnapTarget(index) => self.snap_targets[index] = !self.snap_targets[index],
            Action::PixelGrid => self.pixel_grid = !self.pixel_grid,
            Action::FloatingLayers => self.floating_layers = !self.floating_layers,
            Action::FloatingTools => self.floating_tools = !self.floating_tools,
            Action::ThemeSettings => self.theme_settings_open = true,
            Action::Theme(preference) => {
                self.theme_preference = preference;
                ctx.set_theme(preference);
            }
            Action::Licenses => self.open_source_licenses.open = true,
        }
    }

    pub(super) fn command_palette_ui(&mut self, ctx: &egui::Context) {
        if !self.command_palette.open {
            return;
        }
        let composing = ctx.input(|input| self.command_palette.update_composition(&input.events));
        let (up, down, enter) = if composing {
            (false, false, false)
        } else {
            ctx.input_mut(|input| {
                (
                    input.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                    input.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                    input.consume_key(egui::Modifiers::NONE, Key::Enter),
                )
            })
        };
        let mut execute = None;
        let response = egui::Modal::new(egui::Id::new("command-palette")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 40.).clamp(180., 590.));
            ui.label(egui::RichText::new("搜索命令").strong().size(17.));
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.command_palette.query)
                    .id(egui::Id::new("command-palette-query"))
                    .hint_text("输入中文或英文，例如：旋转、Camera Raw、Scanlines")
                    .desired_width(f32::INFINITY),
            );
            if self.command_palette.request_focus {
                search.request_focus();
                self.command_palette.request_focus = false;
            }
            if search.changed() {
                self.command_palette.selected = 0;
            }
            let matches = results(&self.command_palette.query);
            if matches.is_empty() {
                ui.add_space(12.);
                ui.weak("没有匹配的命令");
                ui.add_space(12.);
            } else {
                self.command_palette.selected =
                    self.command_palette.selected.min(matches.len() - 1);
                if up {
                    self.command_palette.selected =
                        (self.command_palette.selected + matches.len() - 1) % matches.len();
                } else if down {
                    self.command_palette.selected =
                        (self.command_palette.selected + 1) % matches.len();
                }
                egui::ScrollArea::vertical()
                    .id_salt("command-palette-results")
                    .max_height((ctx.content_rect().height() - 200.).clamp(80., 390.))
                    .show(ui, |ui| {
                        for (position, &index) in matches.iter().enumerate() {
                            let entry = &entries()[index];
                            let disabled = entry.disabled_reason(self);
                            let selected = position == self.command_palette.selected;
                            let row = ui
                                .add_enabled_ui(disabled.is_none(), |ui| {
                                    let (rect, response) = ui.allocate_exact_size(
                                        Vec2::new(ui.available_width(), 31.),
                                        Sense::click(),
                                    );
                                    if selected || response.hovered() {
                                        ui.painter().rect_filled(
                                            rect,
                                            5.,
                                            if selected {
                                                ui.visuals().selection.bg_fill
                                            } else {
                                                ui.visuals().widgets.hovered.bg_fill
                                            },
                                        );
                                    }
                                    let color = if disabled.is_some() {
                                        ui.visuals().weak_text_color()
                                    } else if selected {
                                        ui.visuals().selection.stroke.color
                                    } else {
                                        ui.visuals().text_color()
                                    };
                                    ui.painter().text(
                                        rect.left_center() + Vec2::new(8., 0.),
                                        egui::Align2::LEFT_CENTER,
                                        format!("{}  ·  {}", entry.title, entry.category),
                                        egui::FontId::proportional(13.),
                                        color,
                                    );
                                    ui.painter().text(
                                        rect.right_center() - Vec2::new(8., 0.),
                                        egui::Align2::RIGHT_CENTER,
                                        entry.shortcut(&self.shortcuts),
                                        egui::FontId::proportional(12.),
                                        color,
                                    );
                                    response
                                })
                                .inner;
                            if selected && (up || down || search.changed()) {
                                row.scroll_to_me(Some(egui::Align::Center));
                            }
                            if let Some(reason) = disabled {
                                row.on_disabled_hover_text(reason);
                            } else if row.clicked() || (enter && selected) {
                                execute = Some(entry.action);
                            }
                        }
                    });
            }
            ui.separator();
            ui.weak("↑ ↓ 选择　Enter 执行　Esc 关闭");
        });
        if execute.is_some()
            || response.backdrop_response.clicked()
            || (!composing && response.should_close())
        {
            self.command_palette.open = false;
            ctx.memory_mut(|memory| memory.surrender_focus(egui::Id::new("command-palette-query")));
        }
        if let Some(action) = execute {
            self.run_palette_action(action, ctx);
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

    fn key(key: Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn frame(app: &mut EditorApp, ctx: &egui::Context, events: Vec<egui::Event>) {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
                events,
                ..Default::default()
            },
            |ctx| app.command_palette_ui(ctx),
        );
    }

    #[test]
    fn search_ranks_chinese_english_and_ordered_abbreviations() {
        assert_eq!(entries()[results("保存")[0]].title, "保存");
        assert_eq!(entries()[results("gAuSsIaN bLuR")[0]].category, "滤镜");
        assert_eq!(entries()[results("sclns")[0]].title, tr("Scanlines"));
        assert!(results("从来没有的命令xyz").is_empty());
        assert_eq!(match_score(&normalized("ccw"), &normalized("CW")), None);
        assert!(
            match_score(&normalized("旋转"), &normalized("旋转画布")).unwrap()
                < match_score(&normalized("旋转"), &normalized("顺时针旋转画布")).unwrap()
        );
    }

    #[test]
    fn palette_displays_current_remapped_shortcuts() {
        let mut app = editor();
        app.shortcuts.insert("Brush tool".into(), "F6".into());
        app.shortcuts.insert("Save".into(), "Ctrl+Alt+S".into());
        assert_eq!(
            entries()
                .iter()
                .find(|entry| matches!(entry.action, Action::Tool(Tool::Brush)))
                .unwrap()
                .shortcut(&app.shortcuts),
            "F6"
        );
        assert_eq!(
            entries()
                .iter()
                .find(|entry| matches!(entry.action, Action::Command(Command::Save)))
                .unwrap()
                .shortcut(&app.shortcuts),
            "Ctrl+Alt+S"
        );
    }

    #[test]
    fn palette_executes_existing_tool_filter_effect_and_command_paths() {
        let mut app = editor();
        let ctx = egui::Context::default();
        app.run_palette_action(Action::Tool(Tool::Ellipse), &ctx);
        assert_eq!(app.tool, Tool::Ellipse);
        app.run_palette_action(Action::Filter("Scanlines"), &ctx);
        assert!(matches!(app.dialog, Some(Dialog::Filter { ref kind }) if kind == "Scanlines"));
        assert_eq!(app.dialog_value["scanlines"]["thickness"], 70);
        app.cancel_dialog();
        app.run_palette_action(Action::Effect("stroke"), &ctx);
        assert!(matches!(app.dialog, Some(Dialog::Effect { ref kind }) if kind == "stroke"));
        assert_eq!(app.dialog_value["size"], 4);
        app.cancel_dialog();
        app.grid = false;
        app.run_palette_action(Action::Command(Command::Grid), &ctx);
        assert!(app.grid);
    }

    #[test]
    fn disabled_enter_does_not_run_or_close_the_palette() {
        let mut app = editor();
        app.session_mut().placeholder = true;
        app.open_command_palette();
        app.command_palette.query = "Gaussian Blur".into();
        let before = app.session().doc.clone();
        let ctx = egui::Context::default();
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![key(Key::Enter)]);
        assert!(app.command_palette.is_open());
        assert!(app.dialog.is_none());
        assert_eq!(app.session().doc, before);
        assert!(app.command_available(Command::Search));
    }

    #[test]
    fn ime_commit_enter_does_not_execute_and_navigation_does_not_change_the_document() {
        let mut app = editor();
        let before = app.session().doc.clone();
        app.open_command_palette();
        app.command_palette.query = "椭圆".into();
        let ctx = egui::Context::default();
        frame(&mut app, &ctx, vec![]);
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::Ime(egui::ImeEvent::Preedit("椭圆".into())),
                key(Key::Enter),
            ],
        );
        assert!(app.command_palette.is_open());
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::Ime(egui::ImeEvent::Commit("椭圆".into())),
                key(Key::Enter),
            ],
        );
        assert!(app.command_palette.is_open());
        assert_eq!(app.session().doc, before);
        frame(&mut app, &ctx, vec![key(Key::Escape)]);
        assert!(!app.command_palette.is_open());
        app.open_command_palette();
        app.command_palette.query = "Ellipse tool".into();
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![key(Key::Enter)]);
        assert!(!app.command_palette.is_open());
        assert_eq!(app.tool, Tool::Ellipse);
        assert_eq!(app.session().doc, before);
    }

    #[test]
    fn opening_frame_blocks_canvas_strokes_zoom_and_text_editor_input() {
        let mut app = editor();
        app.set_tool(Tool::Brush);
        app.session_mut().zoom = 4.;
        let before = app.session().doc.clone();
        let ctx = egui::Context::default();
        app.open_command_palette();
        let pointer = Pos2::new(430., 310.);
        let canvas_frame = |app: &mut EditorApp, events| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    assert!(app.shortcuts(ctx).is_empty());
                    egui::CentralPanel::default().show(ctx, |ui| app.canvas(ui));
                    app.command_palette_ui(ctx);
                },
            );
        };
        canvas_frame(
            &mut app,
            vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert!(app.drag.is_none());
        assert!(app.brush_stroke.is_none());
        assert_eq!(app.session().doc, before);
        assert_eq!(app.session().zoom, 4.);
        app.command_palette.open = false;
        app.begin_dialog(Dialog::Text {
            layer: app.session().doc.active_layer_id,
            origin: [0., 0.],
        });
        app.dialog_value = json!({"content":"原来的文字","fontName":"Segoe UI","fontSize":24,"red":0,"green":0,"blue":0,"alignment":"Left","tracking":0,"leading":0,"boxSize":[200,80]});
        let text_before = app.dialog_value.clone();
        app.open_command_palette();
        canvas_frame(
            &mut app,
            vec![egui::Event::Text("fZ".into()), key(Key::F), key(Key::Z)],
        );
        assert_eq!(app.dialog_value, text_before);
        assert_eq!(app.tool, Tool::Brush);
        assert!(!app.canvas_only);
        assert_eq!(app.session().doc, before);
    }
}
