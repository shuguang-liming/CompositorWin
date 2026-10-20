use super::*;
use crate::chrome::{Icon, capsule, icon_button, icon_button_sized, metrics};

impl Tool {
    pub(super) fn rail_group(self) -> usize {
        match self {
            Self::Rectangle | Self::Ellipse => 0,
            Self::Lasso | Self::Polygon => 1,
            Self::Wand | Self::Object => 2,
            Self::Brush | Self::Eraser => 3,
            Self::Blur | Self::Smudge | Self::Liquify => 4,
            _ => 5,
        }
    }
    pub(super) fn icon(self) -> Icon {
        match self {
            Self::Move => Icon::Move,
            Self::Rectangle => Icon::MarqueeRect,
            Self::Ellipse => Icon::MarqueeEllipse,
            Self::Lasso => Icon::Lasso,
            Self::Polygon => Icon::PolygonLasso,
            Self::Wand => Icon::Wand,
            Self::Object => Icon::Object,
            Self::Crop => Icon::Crop,
            Self::Brush => Icon::Brush,
            Self::Eraser => Icon::Eraser,
            Self::Heal => Icon::Heal,
            Self::Clone => Icon::Clone,
            Self::Blur | Self::Smudge | Self::Liquify => Icon::Smear,
            Self::Gradient => Icon::Gradient,
            Self::Shape => Icon::Shape,
            Self::Text => Icon::Text,
            Self::Eyedropper => Icon::Eyedropper,
            Self::Hand => Icon::Hand,
            Self::Zoom => Icon::Zoom,
        }
    }
    pub(super) fn hint(self) -> &'static str {
        match self {
            Self::Move => "拖动移动 · 控制点缩放 · 圆点旋转 · 空格平移",
            Self::Rectangle | Self::Ellipse => "拖动选择 · Shift 添加 · Alt 相减 · Ctrl+D 取消选择",
            Self::Lasso => "拖动选择 · Shift 添加 · Alt 相减 · 空格平移",
            Self::Polygon => "单击添加顶点 · 双击或 Enter 闭合 · Esc 取消",
            Self::Wand | Self::Object => "单击选择 · Tab 切换模式 · Shift 添加 · Alt 相减",
            Self::Brush | Self::Eraser | Self::Heal | Self::Blur | Self::Smudge | Self::Liquify => {
                "拖动绘画 · [ ] 大小 · Shift+[ ] 硬度 · 数字键强度 · 空格平移"
            }
            Self::Clone => "Alt 单击设置取样源 · 拖动仿制 · 空格平移",
            Self::Gradient => "拖动绘制渐变 · 拖动端点调整 · Enter 应用 · Esc 取消",
            Self::Shape => "拖动绘制形状 · Shift 约束 · Alt 从中心 · Tab 切换形状",
            Self::Text => "拖动文本框 · 单击文字编辑 · Ctrl+Enter 完成 · Esc 取消",
            Self::Crop => "拖动裁剪 · Enter 应用 · Esc 取消 · 空格平移",
            Self::Eyedropper => "单击取样颜色 · Alt 设置背景色",
            Self::Hand => "拖动平移 · 滚轮缩放",
            Self::Zoom => "单击放大 · Alt 单击缩小 · 上下拖动连续缩放 · Ctrl+中键缩放",
        }
    }
}

impl EditorApp {
    pub(super) fn is_welcome(&self) -> bool {
        self.session().placeholder
    }

    pub(super) fn command_available(&self, command: Command) -> bool {
        !self.pdf_ui.is_open()
            && self.export_dialog.is_none()
            && (!matches!(command, Command::RepeatFilter)
                || (self.last_filter.is_some()
                    && !self.is_welcome()
                    && self.dialog.is_none()
                    && !self.session().edit_busy))
            && (!self.is_welcome()
                || matches!(
                    command,
                    Command::New
                        | Command::Open
                        | Command::OpenPdf
                        | Command::Import
                        | Command::ImportFolder
                        | Command::Close
                        | Command::Shortcuts
                        | Command::Search
                        | Command::Paste
                ))
    }

    fn project_tab_title(&self, index: usize) -> String {
        let session = &self.tabs[index];
        if session.placeholder {
            return "新建画布".into();
        }
        session
            .path
            .as_ref()
            .and_then(|path| path.file_stem())
            .map(|name| name.to_string_lossy().into_owned())
            .or_else(|| session.display_name.clone())
            .unwrap_or_else(|| "未命名".into())
    }

    // Keep the selected tab visible, replacing the oldest visible slot when necessary.
    pub(super) fn tabs_strip_ui(&mut self, ui: &mut egui::Ui) {
        let titles: Vec<_> = (0..self.tabs.len())
            .map(|index| self.project_tab_title(index))
            .collect();
        let widths: Vec<_> = titles
            .iter()
            .enumerate()
            .map(|(index, title)| {
                let dirty = !self.tabs[index].placeholder && self.tabs[index].history.is_modified();
                let text = project_tab_text(title, index == self.current).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::FontId::proportional(12.),
                );
                (text.size().x.ceil() + if dirty { 10. } else { 0. }).clamp(35., 155.) + 40.
            })
            .collect();
        let available = ui.available_width();
        let mut first = 0;
        let mut span = widths.iter().sum::<f32>()
            + metrics::TAB_SPACING * widths.len().saturating_sub(1) as f32;
        while first + 1 < widths.len() && span + overflow_tab_width(ui, first) > available {
            span -= widths[first] + metrics::TAB_SPACING;
            first += 1;
        }
        let mut visible: Vec<_> = (first..self.tabs.len()).collect();
        let mut hidden: Vec<_> = (0..first).collect();
        if first > 0 && self.current < first {
            let bumped = visible[0];
            visible[0] = self.current;
            span += widths[self.current] - widths[bumped];
            hidden.retain(|&index| index != self.current);
            hidden.push(bumped);
            while visible.len() > 1 && span + overflow_tab_width(ui, hidden.len()) > available {
                let removed = visible.remove(1);
                span -= widths[removed] + metrics::TAB_SPACING;
                hidden.push(removed);
            }
            hidden.sort_unstable();
        }
        ui.spacing_mut().item_spacing.x = metrics::TAB_SPACING;
        if !hidden.is_empty() {
            let label = overflow_tab_label(hidden.len());
            let response = ui.add(
                egui::Button::new(egui::RichText::new(&label).size(12.))
                    .fill(crate::chrome::theme::tab_fill(ui.visuals(), false))
                    .stroke(crate::chrome::theme::tab_stroke(ui.visuals(), false))
                    .corner_radius(14)
                    .min_size(Vec2::new(
                        overflow_tab_width(ui, hidden.len()) - metrics::TAB_SPACING,
                        metrics::TAB_HEIGHT,
                    )),
            );
            egui::Popup::menu(&response).show(|ui| {
                for &index in &hidden {
                    let title = if self.tabs[index].history.is_modified() {
                        format!("• {}", titles[index])
                    } else {
                        titles[index].clone()
                    };
                    if ui.selectable_label(index == self.current, title).clicked() {
                        self.activate_tab(index);
                        ui.close();
                    }
                }
            });
            response.on_hover_text(label);
        }
        let mut tab_move = None;
        for index in visible {
            let tab_id = self.tabs[index].id;
            let dirty = !self.tabs[index].placeholder && self.tabs[index].history.is_modified();
            let (response, close) = project_tab_capsule(
                ui,
                tab_id,
                &titles[index],
                dirty,
                index == self.current,
                widths[index],
            );
            response.dnd_set_drag_payload(TabPayload(tab_id));
            if response.drag_started() {
                self.activate_tab(index);
            }
            let drop_response = response.clone() | close.clone();
            // egui takes a payload before downcasting; a wrong release type consumes it.
            if drop_response.dnd_hover_payload::<TabPayload>().is_some() {
                if let Some(source) = drop_response.dnd_release_payload::<TabPayload>()
                    && let Some(from) = self.tabs.iter().position(|session| session.id == source.0)
                {
                    let after = ui
                        .input(|input| input.pointer.interact_pos())
                        .is_some_and(|point| point.x > drop_response.rect.center().x);
                    let mut to = index + usize::from(after);
                    if from < to {
                        to -= 1;
                    }
                    tab_move = Some((from, to));
                }
            } else if drop_response.dnd_hover_payload::<LayerPayload>().is_some()
                && let Some(payload) = drop_response.dnd_release_payload::<LayerPayload>()
            {
                self.copy_layers_to_tab(index, &payload);
            }
            if close.clicked() {
                self.request_close(index);
                break;
            }
            if response.clicked() {
                self.activate_tab(index);
            }
        }
        if let Some((from, to)) = tab_move {
            let active = self.session().id;
            let tab = self.tabs.remove(from);
            self.tabs.insert(to, tab);
            self.current = self
                .tabs
                .iter()
                .position(|session| session.id == active)
                .unwrap_or(0);
        }
    }

    pub(super) fn prepare_canvas_for_layer_drop(&mut self, source: &Document) {
        if self.is_welcome() {
            let mut document = Document::new(source.width, source.height);
            document.resolution = source.resolution;
            document.layers.clear();
            document.active_layer_id = None;
            self.install_canvas_session(Session::new(document, None));
        }
    }

    fn copy_layers_to_tab(&mut self, index: usize, payload: &LayerPayload) {
        if payload.document == self.tabs[index].id {
            return;
        }
        let Some(source) = self
            .tabs
            .iter()
            .find(|session| session.id == payload.document)
            .map(|session| session.doc.clone())
        else {
            return;
        };
        if !payload.ids.iter().any(|&id| source.layer(id).is_some()) {
            return;
        }
        self.activate_tab(index);
        self.prepare_canvas_for_layer_drop(&source);
        self.apply("Copy layers from project", |document| {
            crate::edit::copy_layers_to(&source, document, &payload.ids, None).map(|_| ())
        });
    }

    pub(super) fn tool_rail_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("original-tool-rail")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.;
                ui.add_space(16.);
                for base in [
                    Tool::Move,
                    Tool::Rectangle,
                    Tool::Lasso,
                    Tool::Wand,
                    Tool::Crop,
                    Tool::Brush,
                    Tool::Heal,
                    Tool::Clone,
                    Tool::Blur,
                    Tool::Gradient,
                    Tool::Shape,
                    Tool::Text,
                    Tool::Eyedropper,
                    Tool::Hand,
                    Tool::Zoom,
                ] {
                    let group = base.rail_group();
                    let tool = if group < 5 {
                        self.tool_modes[group]
                    } else {
                        base
                    };
                    ui.horizontal(|ui| {
                        ui.add_space(10.);
                        let response =
                            icon_button(ui, tool.icon(), self.tool == tool, tool.label());
                        if response.clicked() {
                            self.set_tool(tool);
                        }
                        response.context_menu(|ui| {
                            for candidate in Tool::ALL {
                                if (candidate == base
                                    || (group < 5 && candidate.rail_group() == group))
                                    && ui
                                        .selectable_label(self.tool == candidate, candidate.label())
                                        .clicked()
                                {
                                    self.set_tool(candidate);
                                    ui.close();
                                }
                            }
                        });
                    });
                }
                ui.add_space(8.);
                self.palette_rail_ui(ui);
                ui.add_space(12.);
            });
    }

    pub(super) fn palette_rail_ui(&mut self, ui: &mut egui::Ui) {
        let (space, _) = ui.allocate_exact_size(Vec2::new(56., 39.), Sense::hover());
        let origin = space.min + Vec2::new(10., 0.);
        for (background, offset, color) in [
            (true, Vec2::splat(12.), self.palette_color(true)),
            (false, Vec2::ZERO, self.palette_color(false)),
        ] {
            let rect = Rect::from_min_size(origin + offset, Vec2::splat(24.));
            ui.painter().rect_filled(
                rect,
                6.,
                Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]),
            );
            ui.painter().rect_stroke(
                rect.shrink(1.),
                5.,
                Stroke::new(1.5_f32, Color32::WHITE),
                egui::StrokeKind::Inside,
            );
            ui.painter().rect_stroke(
                rect,
                6.,
                Stroke::new(1.0_f32, Color32::BLACK),
                egui::StrokeKind::Inside,
            );
            let response = ui
                .interact(rect, ui.id().with(("palette", background)), Sense::click())
                .on_hover_text(if background { "背景色" } else { "前景色" });
            self.palette_swatch_interaction(ui, &response, background);
        }
        for (icon, offset, label) in [
            (Icon::Swap, Vec2::new(27., -3.), "交换前景和背景色 (X)"),
            (Icon::Reset, Vec2::new(-1., 27.), "复位为黑白 (D)"),
        ] {
            let rect = Rect::from_min_size(origin + offset, Vec2::splat(12.));
            let response = ui.interact(
                rect,
                ui.id().with(("palette-action", label)),
                Sense::click(),
            );
            let color = if response.hovered() {
                crate::chrome::theme::palette(ui.ctx(), ui.visuals()).text
            } else {
                crate::chrome::theme::palette(ui.ctx(), ui.visuals()).secondary_text
            };
            crate::chrome::paint_icon(ui.painter(), icon, rect.shrink(1.), color);
            if response.on_hover_text(label).clicked() {
                if matches!(icon, Icon::Swap) {
                    self.swap_palette_colors();
                } else {
                    self.reset_palette_colors();
                }
            }
        }
    }

    pub(super) fn workspace_toolbar(&mut self, ctx: &egui::Context) -> Vec<Command> {
        let mut actions = vec![];
        egui::TopBottomPanel::top("workspace-toolbar")
            .exact_height(42.)
            .frame(
                egui::Frame::NONE
                    .fill(ctx.style().visuals.panel_fill)
                    .inner_margin(egui::Margin::symmetric(12, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    if icon_button_sized(
                        ui,
                        Icon::Add,
                        false,
                        "新建画布 (Ctrl+N)",
                        Vec2::splat(30.),
                    )
                    .clicked()
                    {
                        actions.push(Command::New);
                    }
                    let tabs_width = (ui.available_width() - 242.).max(100.);
                    ui.allocate_ui_with_layout(
                        Vec2::new(tabs_width, metrics::TAB_STRIP_HEIGHT),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            self.tabs_strip_ui(ui);
                        },
                    );
                    ui.add_enabled_ui(!self.is_welcome(), |ui| {
                        if capsule(ui, "适合", false).clicked() {
                            actions.push(Command::Fit);
                        }
                        if capsule(ui, "100%", false).clicked() {
                            actions.push(Command::Actual);
                        }
                        if icon_button_sized(ui, Icon::ZoomIn, false, "放大", Vec2::new(28., 28.))
                            .clicked()
                        {
                            self.zoom_toolbar(1.25);
                        }
                        if icon_button_sized(ui, Icon::ZoomOut, false, "缩小", Vec2::new(28., 28.))
                            .clicked()
                        {
                            self.zoom_toolbar(0.8);
                        }
                    });
                });
            });
        actions
    }

    pub(super) fn zoom_toolbar(&mut self, factor: f32) {
        let session = self.session_mut();
        session.zoom = (session.zoom.max(0.01) * factor).clamp(0.01, 64.);
    }

    pub(super) fn tool_header(&mut self, ctx: &egui::Context) -> Vec<Command> {
        let mut actions = vec![];
        egui::TopBottomPanel::top("tool-header")
            .exact_height(metrics::TOOL_HEADER_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(ctx.style().visuals.panel_fill)
                    .inner_margin(egui::Margin::symmetric(18, 0)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().slider_width = 100.;
                egui::ScrollArea::horizontal()
                    .id_salt("tool-header-scroll")
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        if self.is_welcome() {
                            ui.label("选择工具");
                            return;
                        }
                        if self.tool == Tool::Text
                            || matches!(self.dialog, Some(Dialog::Text { .. }))
                        {
                            self.text_header_contents(ui);
                            return;
                        }
                        if self.tool == Tool::Move {
                            self.transform_controls_ui(ui);
                            return;
                        }
                        if self.tool.paints() {
                            self.paint_header_contents(ui);
                            return;
                        }
                        if self.tool.rail_group() < 3 {
                            self.selection_header_contents(ui);
                            return;
                        }
                        if self.tool == Tool::Shape {
                            self.shape_header_contents(ui);
                            return;
                        }
                        if self.tool == Tool::Crop {
                            self.crop_header_contents(ui, &mut actions);
                            return;
                        }
                        if self.tool == Tool::Gradient {
                            self.gradient_header_contents(ui);
                            return;
                        }
                        if matches!(self.tool, Tool::Hand | Tool::Zoom) {
                            self.navigation_header_contents(ui);
                            return;
                        }
                        ui.horizontal_centered(|ui| {
                            let group = self.tool.rail_group();
                            if group < 5 {
                                let mut chosen = self.tool;
                                egui::ComboBox::from_id_salt("tool-subtype")
                                    .selected_text(self.tool.label())
                                    .show_ui(ui, |ui| {
                                        for tool in Tool::ALL {
                                            if tool.rail_group() == group {
                                                ui.selectable_value(
                                                    &mut chosen,
                                                    tool,
                                                    tool.label(),
                                                );
                                            }
                                        }
                                    });
                                if chosen != self.tool {
                                    self.set_tool(chosen);
                                }
                            }
                            self.parameter_controls_ui(ui, &mut actions);
                        });
                    });
            });
        actions
    }

    pub(super) fn status_ui(&mut self, ctx: &egui::Context, show_busy: bool) {
        egui::TopBottomPanel::bottom("status")
            .exact_height(metrics::STATUS_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(ctx.style().visuals.panel_fill)
                    .inner_margin(egui::Margin::symmetric(18, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.style_mut().override_font_id = Some(egui::FontId::proportional(11.));
                    if self.is_welcome() {
                        ui.weak("准备就绪");
                    } else {
                        ui.add_sized(
                            [62., 18.],
                            egui::Label::new(format!("{:.1}%", self.session().zoom * 100.)),
                        );
                        ui.weak(format!(
                            "{} × {} px",
                            self.session().doc.width,
                            self.session().doc.height
                        ));
                        ui.add_space(16.);
                        ui.weak("sRGB · 透明背景");
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if show_busy {
                            ui.label(if self.pending_io > 0 {
                                "正在打开／导出…"
                            } else if self.tabs.iter().any(|s| s.save_busy) {
                                "正在保存…"
                            } else {
                                "正在处理…"
                            });
                            ui.spinner();
                        } else if let Some(message) = self.message.clone() {
                            if icon_button_sized(
                                ui,
                                Icon::Close,
                                false,
                                "关闭消息",
                                Vec2::splat(16.),
                            )
                            .clicked()
                            {
                                self.message = None;
                            }
                            ui.add(egui::Label::new(tr(&message)).truncate());
                        } else if !self.is_welcome() {
                            ui.add(egui::Label::new(self.tool.hint()).truncate());
                        }
                    });
                });
            });
    }

    pub(super) fn welcome_ui(&mut self, ui: &mut egui::Ui) {
        self.original_welcome_ui(ui);
    }

    pub(super) fn install_canvas_session(&mut self, session: Session) {
        if self.session().placeholder {
            self.settle_gestures();
            self.cancel_dialog();
            self.tabs[self.current] = session;
        } else {
            self.tabs.push(session);
            self.activate_tab(self.tabs.len() - 1);
        }
    }
}

fn project_tab_text(title: &str, active: bool) -> egui::WidgetText {
    let text = egui::RichText::new(title).size(12.);
    if active {
        text.strong().into()
    } else {
        text.into()
    }
}

fn overflow_tab_label(hidden: usize) -> String {
    format!("另有 {hidden} 个项目  ⌄")
}

fn overflow_tab_width(ui: &egui::Ui, hidden: usize) -> f32 {
    if hidden == 0 {
        return 0.;
    }
    ui.painter()
        .layout_no_wrap(
            overflow_tab_label(hidden),
            egui::FontId::proportional(12.),
            crate::chrome::theme::palette(ui.ctx(), ui.visuals()).text,
        )
        .size()
        .x
        .ceil()
        + 22.
        + metrics::TAB_SPACING
}

fn project_tab_capsule(
    ui: &mut egui::Ui,
    id: Uuid,
    title: &str,
    dirty: bool,
    active: bool,
    width: f32,
) -> (egui::Response, egui::Response) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, metrics::TAB_HEIGHT), Sense::hover());
    let close_rect = Rect::from_min_max(
        Pos2::new(rect.right() - 21., rect.top()),
        Pos2::new(rect.right() - 5., rect.bottom()),
    );
    let label_rect = Rect::from_min_max(rect.min, Pos2::new(close_rect.left(), rect.bottom()));
    let response = ui.interact(
        label_rect,
        ui.id().with(("project-tab", id)),
        Sense::click_and_drag(),
    );
    let close = ui.interact(
        close_rect,
        ui.id().with(("project-tab-close", id)),
        Sense::click(),
    );
    let targeted = response.dnd_hover_payload::<LayerPayload>().is_some()
        || close.dnd_hover_payload::<LayerPayload>().is_some();
    let fill = if targeted {
        Color32::from_rgba_unmultiplied(80, 151, 223, 77)
    } else {
        crate::chrome::theme::tab_fill(ui.visuals(), active)
    };
    let stroke = if targeted {
        Stroke::new(
            2.0_f32,
            crate::chrome::theme::palette(ui.ctx(), ui.visuals()).accent,
        )
    } else {
        crate::chrome::theme::tab_stroke(ui.visuals(), active)
    };
    ui.painter().rect_filled(rect, 14., fill);
    ui.painter()
        .rect_stroke(rect, 14., stroke, egui::StrokeKind::Inside);
    let mut text_x = rect.left() + 11.;
    if dirty {
        ui.painter().circle_filled(
            Pos2::new(text_x + 2.5, rect.center().y),
            2.5,
            crate::chrome::theme::palette(ui.ctx(), ui.visuals()).text,
        );
        text_x += 10.;
    }
    let text = project_tab_text(title, active).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        (close_rect.left() - 8. - text_x).max(1.),
        egui::FontId::proportional(12.),
    );
    ui.painter().galley(
        Pos2::new(text_x, rect.center().y - text.size().y / 2.),
        text,
        crate::chrome::theme::palette(ui.ctx(), ui.visuals()).text,
    );
    if close.hovered() {
        ui.painter().circle_filled(
            close_rect.center(),
            7.,
            crate::chrome::theme::overlay(ui.visuals(), 15),
        );
    }
    crate::chrome::paint_icon(
        ui.painter(),
        Icon::Close,
        Rect::from_center_size(close_rect.center(), Vec2::splat(9.)),
        crate::chrome::theme::palette(ui.ctx(), ui.visuals()).secondary_text,
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), active, title)
    });
    close.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("关闭 {title}"),
        )
    });
    (
        response.on_hover_text(title),
        close.on_hover_text(format!("关闭 {title}")),
    )
}

impl EditorApp {
    fn parameter_controls_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Command>) {
        ui.label(self.tool.label());
        ui.separator();
        if self.tool.paints() {
            ui.add(
                egui::DragValue::new(&mut self.diameter)
                    .range(1.0..=2500.0)
                    .speed(0.5)
                    .prefix("大小 "),
            );
            ui.add(egui::Slider::new(&mut self.hardness, 0.0..=1.0).text("硬度"));
            ui.add(egui::Slider::new(&mut self.opacity, 0.0..=1.0).text("不透明度"));
            ui.add(egui::Slider::new(&mut self.smoothing, 0.0..=1.0).text("平滑"));
        }
        if matches!(
            self.tool,
            Tool::Rectangle
                | Tool::Ellipse
                | Tool::Lasso
                | Tool::Polygon
                | Tool::Wand
                | Tool::Object
        ) {
            egui::ComboBox::from_id_salt("selection-mode")
                .selected_text(tr(&format!("{:?}", self.selection_mode)))
                .show_ui(ui, |ui| {
                    for mode in [
                        SelectionMode::Replace,
                        SelectionMode::Add,
                        SelectionMode::Subtract,
                        SelectionMode::Intersect,
                    ] {
                        ui.selectable_value(
                            &mut self.selection_mode,
                            mode,
                            tr(&format!("{mode:?}")),
                        );
                    }
                });
            ui.checkbox(&mut self.antialias, "抗锯齿");
        }
        if self.tool == Tool::Wand {
            ui.add(
                egui::DragValue::new(&mut self.tolerance)
                    .range(0..=255)
                    .prefix("容差 "),
            );
            ui.checkbox(&mut self.contiguous, "连续");
            ui.checkbox(&mut self.wand_all, "所有图层");
            egui::ComboBox::from_id_salt("wand-radius")
                .selected_text(format!(
                    "{} × {}",
                    self.wand_radius * 2 + 1,
                    self.wand_radius * 2 + 1
                ))
                .show_ui(ui, |ui| {
                    for radius in [0, 1, 2] {
                        ui.selectable_value(
                            &mut self.wand_radius,
                            radius,
                            format!("{} × {}", radius * 2 + 1, radius * 2 + 1),
                        );
                    }
                });
        }
        if self.tool == Tool::Clone {
            ui.checkbox(&mut self.clone_aligned, "对齐");
            ui.checkbox(&mut self.clone_all, "取样所有图层");
            ui.label("按住 Alt 单击设置取样源");
        }
        if self.tool == Tool::Heal {
            egui::ComboBox::from_id_salt("healing-mode")
                .selected_text(["内容识别", "创建纹理", "邻近匹配"][self.healing_mode])
                .show_ui(ui, |ui| {
                    for (i, label) in ["内容识别", "创建纹理", "邻近匹配"].iter().enumerate()
                    {
                        ui.selectable_value(&mut self.healing_mode, i, *label);
                    }
                });
        }
        if self.tool == Tool::Shape {
            egui::ComboBox::from_id_salt("shape-kind")
                .selected_text(tr(&self.shape_kind))
                .show_ui(ui, |ui| {
                    for k in ["Rectangle", "Ellipse", "Line"] {
                        ui.selectable_value(&mut self.shape_kind, k.into(), tr(k));
                    }
                });
            ui.add(
                egui::DragValue::new(&mut self.corner_radius)
                    .range(0.0..=500.0)
                    .prefix("半径 "),
            );
            ui.add(
                egui::DragValue::new(&mut self.line_width)
                    .range(1.0..=500.0)
                    .prefix("线宽 "),
            );
        }
        if self.tool == Tool::Gradient {
            egui::ComboBox::from_id_salt("gradient-kind")
                .selected_text(tr(&self.gradient_kind))
                .show_ui(ui, |ui| {
                    for k in ["Linear", "Radial", "Angle", "Reflected", "Diamond"] {
                        ui.selectable_value(&mut self.gradient_kind, k.into(), tr(k));
                    }
                });
            ui.add(egui::Slider::new(&mut self.opacity, 0.0..=1.0).text("不透明度"));
            ui.checkbox(&mut self.gradient_transparent, "渐变到透明");
            ui.checkbox(&mut self.gradient_reverse, "反向");
            ui.label("拖动端点调整 · Enter 提交");
        }
        if self.tool == Tool::Crop {
            let previous_ratio = self.crop_ratio;
            egui::ComboBox::from_id_salt("crop-ratio")
                .selected_text(match self.crop_ratio {
                    [0, 0] => tr("Free").into_owned(),
                    [1, 0] => tr("Original").into_owned(),
                    [width, height] => format!("{width} : {height}"),
                })
                .show_ui(ui, |ui| {
                    for (ratio, label) in [
                        ([0, 0], "Free"),
                        ([1, 0], "Original"),
                        ([1, 1], "1:1"),
                        ([4, 3], "4:3"),
                        ([3, 4], "3:4"),
                        ([16, 9], "16:9"),
                        ([9, 16], "9:16"),
                    ] {
                        ui.selectable_value(&mut self.crop_ratio, ratio, tr(label));
                    }
                });
            if previous_ratio != self.crop_ratio {
                self.change_crop_ratio();
            }
            ui.label("Alt：从中心调整 · Enter：裁剪 · Esc：取消");
            if ui
                .add_enabled(self.crop_rect.is_some(), egui::Button::new("应用裁剪"))
                .clicked()
            {
                actions.push(Command::CropCommit);
            }
            if ui.button("取消裁剪").clicked() {
                actions.push(Command::CropCancel);
            }
        }
        if self.tool == Tool::Eyedropper {
            ui.checkbox(&mut self.show_sample_ring, "取样环");
        }
    }
}

impl EditorApp {
    fn text_header_contents(&mut self, ui: &mut egui::Ui) {
        let editing_current = matches!(self.dialog, Some(Dialog::Text { .. }))
            && self
                .dialog_target
                .is_some_and(|(target, _)| target == self.session().id);
        let mut cancel = false;
        let mut done = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("文字").size(13.).strong());
            if editing_current {
                let editor_id =
                    egui::Id::new(("canvas-text", self.dialog_target.map(|(id, _)| id)));
                let controls_width =
                    (ui.available_width() - 120. - ui.spacing().item_spacing.x * 2.).max(0.);
                ui.allocate_ui_with_layout(
                    egui::vec2(controls_width, 26.),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        if let Err(error) = crate::text_panel::show_header_controls(
                            ui,
                            &mut self.dialog_value,
                            editor_id,
                        ) {
                            self.message = Some(error.to_string());
                        }
                    },
                );
                cancel = ui
                    .add_sized([60., 26.], egui::Button::new("取消"))
                    .clicked();
                done = ui
                    .add_sized([60., 26.], egui::Button::new("完成"))
                    .clicked();
            } else {
                let editable = self.session().doc.active().and_then(|layer| {
                    Some((layer.text.clone()?, layer.id, layer.transform.origin))
                });
                if ui
                    .add_enabled(
                        editable.is_some() && self.dialog.is_none(),
                        egui::Button::new("编辑文字"),
                    )
                    .clicked()
                    && let Some((value, id, origin)) = editable
                {
                    self.dialog_value = value;
                    self.begin_dialog(Dialog::Text {
                        layer: Some(id),
                        origin,
                    });
                }
            }
        });
        if cancel {
            self.cancel_dialog();
        } else if done {
            // Let dialogs() apply after the canvas has consumed this frame's final text edit.
            self.text_apply_requested = true;
        }
    }
}
