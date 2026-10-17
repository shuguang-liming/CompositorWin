use super::*;
use crate::chrome::{Icon, capsule, icon_button_sized};

impl EditorApp {
    pub(super) fn cancel_gradient(&mut self) {
        if let Some(state) = self.gradient.take()
            && let Some(session) = self
                .tabs
                .iter_mut()
                .find(|session| session.id == state.session)
        {
            cancel_gradient_state(session, &state);
        }
        self.gradient_handle = None;
    }

    pub(super) fn apply_gradient(&mut self) {
        // A pending result still commits, but must not reopen its editing handles.
        self.gradient = None;
        self.gradient_handle = None;
    }

    pub(super) fn palette_color(&self, background: bool) -> [u8; 4] {
        effective_palette_color(
            self.foreground,
            self.background,
            self.mask_target.then_some(self.session().mask_paint_white),
            background,
        )
    }

    pub(super) fn swap_palette_colors(&mut self) {
        if self.mask_target {
            let white = self.session().mask_paint_white;
            self.session_mut().mask_paint_white = !white;
        } else {
            std::mem::swap(&mut self.foreground, &mut self.background);
        }
    }

    pub(super) fn reset_palette_colors(&mut self) {
        if self.mask_target {
            self.session_mut().mask_paint_white = false;
        } else {
            self.foreground = [0, 0, 0, 255];
            self.background = [255; 4];
        }
    }

    pub(super) fn palette_swatch_interaction(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        background: bool,
    ) {
        if !ui.is_enabled() {
            return;
        }
        if self.mask_target {
            egui::Popup::menu(response).show(|ui| {
                for (white, label) in [(false, "黑色 · 隐藏"), (true, "白色 · 显示")] {
                    let current = self.session().mask_paint_white != background;
                    if ui.selectable_label(current == white, label).clicked() {
                        self.session_mut().mask_paint_white = white != background;
                        ui.close();
                    }
                }
            });
        } else if response.clicked() {
            self.finish_color_picker(false);
            self.color_picker = Some(crate::palette::ColorPicker::new(
                background,
                self.palette_color(background),
            ));
        }
    }

    pub(super) fn shape_header_contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 12.;
            ui.label(egui::RichText::new("形状").size(13.).strong());
            for kind in ["Rectangle", "Ellipse", "Line"] {
                if capsule(ui, &tr(kind), self.shape_kind == kind).clicked() {
                    if let Some(drag) = self.drag.take() {
                        self.cancel_drag(drag);
                    }
                    self.shape_kind = kind.into();
                }
            }
            ui.label("填充");
            let (rect, response) = ui.allocate_exact_size(Vec2::new(36., 18.), Sense::click());
            ui.painter().rect_filled(
                rect,
                3.,
                Color32::from_rgba_unmultiplied(
                    self.foreground[0],
                    self.foreground[1],
                    self.foreground[2],
                    self.foreground[3],
                ),
            );
            ui.painter().rect_stroke(
                rect,
                3.,
                Stroke::new(1.0_f32, Color32::from_black_alpha(128)),
                egui::StrokeKind::Inside,
            );
            if response.on_hover_text("使用前景色填充形状").clicked() {
                self.finish_color_picker(false);
                self.color_picker = Some(crate::palette::ColorPicker::new(false, self.foreground));
            }
            if self.shape_kind == "Rectangle" {
                ui.label("半径");
                let mut slider = self.corner_radius.min(200.);
                if ui
                    .add(egui::Slider::new(&mut slider, 0. ..=200.).show_value(false))
                    .changed()
                {
                    self.corner_radius = slider.round();
                }
                ui.add_sized(
                    [48., 26.],
                    egui::DragValue::new(&mut self.corner_radius)
                        .range(0. ..=5000.)
                        .speed(1.),
                );
                ui.label("px");
                ui.checkbox(&mut self.shape_smoothing, "平滑圆角")
                    .on_hover_text("让圆角与直边更平滑地衔接；空间不足时自动降低平滑度");
            } else if self.shape_kind == "Line" {
                ui.label("宽度");
                let mut slider = self.line_width.min(100.);
                if ui
                    .add(egui::Slider::new(&mut slider, 1. ..=100.).show_value(false))
                    .changed()
                {
                    self.line_width = slider.round();
                }
                ui.add_sized(
                    [48., 26.],
                    egui::DragValue::new(&mut self.line_width)
                        .range(1. ..=5000.)
                        .speed(1.),
                );
                ui.label("px");
            }
        });
    }

    pub(super) fn crop_header_contents(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Command>) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 14.;
            ui.label(egui::RichText::new("裁剪").size(13.).strong());
            let previous = self.crop_ratio;
            egui::ComboBox::from_id_salt("original-crop-ratio")
                .width(170.)
                .selected_text(match self.crop_ratio {
                    [0, 0] => "自由".into(),
                    [1, 0] => "原始".into(),
                    [w, h] => format!("{w}:{h}"),
                })
                .show_ui(ui, |ui| {
                    for (ratio, label) in [
                        ([0, 0], "自由"),
                        ([1, 0], "原始"),
                        ([1, 1], "1:1"),
                        ([4, 3], "4:3"),
                        ([3, 4], "3:4"),
                        ([16, 9], "16:9"),
                        ([9, 16], "9:16"),
                    ] {
                        ui.selectable_value(&mut self.crop_ratio, ratio, label);
                    }
                });
            if previous != self.crop_ratio {
                self.change_crop_ratio();
            }
            if let Some([_, _, w, h]) = self.crop_rect {
                ui.label(format!("{} × {} px", w.round() as u32, h.round() as u32));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(self.crop_rect.is_some(), egui::Button::new("应用裁剪"))
                    .clicked()
                {
                    actions.push(Command::CropCommit);
                }
                if ui
                    .add_enabled(self.crop_rect.is_some(), egui::Button::new("取消"))
                    .clicked()
                {
                    actions.push(Command::CropCancel);
                }
            });
        });
    }

    pub(super) fn navigation_header_contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 12.;
            ui.label(
                egui::RichText::new(if self.tool == Tool::Hand {
                    "平移"
                } else {
                    "缩放"
                })
                .size(13.)
                .strong(),
            );
            if self.tool == Tool::Zoom {
                let mut percent = self.session().zoom.max(0.001) * 100.;
                if ui
                    .add_sized(
                        [72., 26.],
                        egui::DragValue::new(&mut percent)
                            .range(0.1..=6400.)
                            .speed(1.),
                    )
                    .changed()
                {
                    self.session_mut().zoom = percent / 100.;
                }
                ui.label("%");
            }
        });
    }

    pub(super) fn gradient_header_contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 12.;
            ui.label(egui::RichText::new("渐变").size(13.).strong());
            for kind in ["Linear", "Radial", "Angle", "Reflected", "Diamond"] {
                if capsule(ui, &tr(kind), self.gradient_kind == kind).clicked() {
                    self.gradient_kind = kind.into();
                }
            }
            let (_, from, to, _) = self.gradient_parameters();
            let (rect, _) = ui.allocate_exact_size(Vec2::new(56., 18.), Sense::hover());
            let colors = crate::chrome::theme::palette(ui.ctx(), ui.visuals());
            let painter = ui.painter().with_clip_rect(rect);
            painter.rect_filled(rect, 3., colors.thumb_checker_light);
            for y in 0..5 {
                for x in 0..14 {
                    if (x + y) % 2 == 0 {
                        painter.rect_filled(
                            Rect::from_min_size(
                                rect.min + Vec2::new(x as f32 * 4., y as f32 * 4.),
                                Vec2::splat(4.),
                            ),
                            0.,
                            colors.thumb_checker_dark,
                        );
                    }
                }
            }
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(
                rect.left_top(),
                Color32::from_rgba_unmultiplied(from[0], from[1], from[2], from[3]),
            );
            mesh.colored_vertex(
                rect.left_bottom(),
                Color32::from_rgba_unmultiplied(from[0], from[1], from[2], from[3]),
            );
            mesh.colored_vertex(
                rect.right_top(),
                Color32::from_rgba_unmultiplied(to[0], to[1], to[2], to[3]),
            );
            mesh.colored_vertex(
                rect.right_bottom(),
                Color32::from_rgba_unmultiplied(to[0], to[1], to[2], to[3]),
            );
            mesh.indices.extend([0, 1, 2, 1, 2, 3]);
            painter.add(egui::Shape::mesh(mesh));
            egui::ComboBox::from_id_salt("gradient-colors")
                .selected_text(if self.gradient_transparent {
                    "前景色到透明"
                } else {
                    "前景色到背景色"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.gradient_transparent, false, "前景色到背景色");
                    ui.selectable_value(&mut self.gradient_transparent, true, "前景色到透明");
                });
            ui.checkbox(&mut self.gradient_reverse, "反向");
            percentage_control(ui, "不透明度", &mut self.opacity);
            if self.gradient.is_some() {
                if ui
                    .button("取消")
                    .on_hover_text("取消本次渐变 (Esc)")
                    .clicked()
                {
                    self.cancel_gradient();
                }
                if ui
                    .button("应用")
                    .on_hover_text("应用本次渐变 (Enter)")
                    .clicked()
                {
                    self.apply_gradient();
                }
            }
        });
    }
    pub(super) fn paint_header_contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 12.;
            let title = if matches!(self.tool, Tool::Blur | Tool::Smudge | Tool::Liquify) {
                "涂抹"
            } else {
                self.tool
                    .label()
                    .split('(')
                    .next()
                    .unwrap_or(self.tool.label())
                    .trim()
            };
            ui.label(egui::RichText::new(title).size(13.).strong());
            if self.tool.rail_group() < 5 {
                let group = self.tool.rail_group();
                for tool in Tool::ALL {
                    if tool.rail_group() == group {
                        let label = tool
                            .label()
                            .split('(')
                            .next()
                            .unwrap_or(tool.label())
                            .trim();
                        if capsule(ui, label, self.tool == tool).clicked() {
                            self.set_tool(tool);
                        }
                    }
                }
            }
            if self.tool == Tool::Heal {
                egui::ComboBox::from_id_salt("healing-mode")
                    .selected_text(["内容识别", "创建纹理", "邻近匹配"][self.healing_mode])
                    .show_ui(ui, |ui| {
                        for (index, label) in
                            ["内容识别", "创建纹理", "邻近匹配"].iter().enumerate()
                        {
                            ui.selectable_value(&mut self.healing_mode, index, *label);
                        }
                    });
            }
            if self.tool == Tool::Clone {
                ui.checkbox(&mut self.clone_aligned, "对齐");
                for (all, label) in [(false, "当前图层"), (true, "所有图层")] {
                    if capsule(ui, label, self.clone_all == all).clicked() {
                        self.clone_all = all;
                    }
                }
            }
            ui.label("大小");
            ui.add_sized(
                [48., 26.],
                egui::DragValue::new(&mut self.diameter)
                    .range(1. ..=2500.)
                    .speed(1.),
            );
            ui.label("px");
            percentage_control(ui, "硬度", &mut self.hardness);
            percentage_control(
                ui,
                if matches!(self.tool, Tool::Blur | Tool::Smudge | Tool::Liquify) {
                    "强度"
                } else {
                    "不透明度"
                },
                &mut self.opacity,
            );
            if self.tool == Tool::Blur {
                ui.label("半径");
                ui.add(egui::Slider::new(&mut self.blur_radius, 0.5..=20.).show_value(false));
                ui.add_sized(
                    [48., 26.],
                    egui::DragValue::new(&mut self.blur_radius)
                        .range(0.5..=50.)
                        .speed(0.5),
                );
                ui.label("px");
            }
            if matches!(self.tool, Tool::Brush | Tool::Eraser) {
                percentage_control(ui, "平滑", &mut self.smoothing);
            } else {
                ui.menu_button("更多", |ui| {
                    percentage_control(ui, "平滑", &mut self.smoothing);
                });
            }
            if !matches!(
                self.tool,
                Tool::Clone | Tool::Blur | Tool::Smudge | Tool::Liquify
            ) {
                ui.label("颜色");
                let color = self.palette_color(false);
                let (rect, response) = ui.allocate_exact_size(Vec2::new(34., 18.), Sense::click());
                ui.painter().rect_filled(
                    rect,
                    4.,
                    Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]),
                );
                ui.painter().rect_stroke(
                    rect,
                    4.,
                    Stroke::new(1.0_f32, Color32::BLACK),
                    egui::StrokeKind::Inside,
                );
                self.palette_swatch_interaction(ui, &response.on_hover_text("前景色"), false);
            }
            if self.mask_target {
                ui.weak("蒙版");
            }
        });
    }

    pub(super) fn selection_header_contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 12.;
            ui.label(
                egui::RichText::new(
                    self.tool
                        .label()
                        .split('(')
                        .next()
                        .unwrap_or(self.tool.label())
                        .trim(),
                )
                .size(13.)
                .strong(),
            );
            let group = self.tool.rail_group();
            for tool in Tool::ALL {
                if tool.rail_group() == group {
                    let label = tool
                        .label()
                        .split('(')
                        .next()
                        .unwrap_or(tool.label())
                        .trim();
                    if capsule(ui, label, self.tool == tool).clicked() {
                        self.set_tool(tool);
                    }
                }
            }
            for (mode, label) in [
                (SelectionMode::Replace, "替换"),
                (SelectionMode::Add, "添加"),
                (SelectionMode::Subtract, "减去"),
                (SelectionMode::Intersect, "交集"),
            ] {
                if capsule(ui, label, self.selection_mode == mode).clicked() {
                    self.selection_mode = mode;
                }
            }
            ui.checkbox(&mut self.antialias, "抗锯齿");
            if self.tool == Tool::Wand {
                ui.label("容差");
                ui.add_sized(
                    [48., 26.],
                    egui::DragValue::new(&mut self.tolerance).range(0..=255),
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
            if self.tool == Tool::Object
                && icon_button_sized(ui, Icon::Object, false, "选择主体", Vec2::splat(26.))
                    .clicked()
            {
                self.command(Command::Subject);
            }
        });
    }
}

fn effective_palette_color(
    foreground: [u8; 4],
    background: [u8; 4],
    mask_white: Option<bool>,
    use_background: bool,
) -> [u8; 4] {
    if let Some(white) = mask_white {
        if white != use_background {
            [255; 4]
        } else {
            [0, 0, 0, 255]
        }
    } else if use_background {
        background
    } else {
        foreground
    }
}

fn cancel_gradient_state(session: &mut Session, state: &GradientState) {
    if session.id != state.session {
        return;
    }
    if let Some((request, cancelled)) = &mut session.gradient_task
        && *request == state.request
    {
        *cancelled = true;
    }
    if state.revision == Some(session.history.revision()) && session.history.undo(&mut session.doc)
    {
        session.changed();
        session.selected = session.doc.active_layer_id.into_iter().collect();
    }
}

pub(super) fn finish_gradient_result(
    session: &mut Session,
    state: &mut GradientState,
    revision: Uuid,
    serial: u64,
    result: Result<Document, String>,
) -> Result<bool, String> {
    let Some((request, cancelled)) = session.gradient_task else {
        return Ok(false);
    };
    if request != state.request || session.id != state.session {
        return Ok(false);
    }
    session.gradient_task = None;
    session.edit_busy = false;
    if cancelled {
        return Ok(false);
    }
    if session.history.revision() != revision || session.serial != serial {
        return Err("The document changed during gradient processing. Draw a new gradient.".into());
    }
    let doc = result?;
    if let Some(owned) = state.revision {
        if owned != revision {
            return Err("渐变历史已改变，请重新绘制渐变。".into());
        }
        if doc == session.doc {
            return Ok(true);
        }
        if !session.history.amend(owned, &doc) {
            return Err("本次渐变的历史记录已释放，请重新绘制渐变。".into());
        }
    } else if !session
        .history
        .record("Gradient", state.before.clone(), &doc)
    {
        // A zero-length or transparent gradient must never own a preceding edit.
        return Ok(false);
    }
    session.doc = doc;
    state.revision = Some(session.history.revision());
    session.changed();
    Ok(true)
}

pub(super) fn header_transform_target(
    layer: &Layer,
    mask: bool,
) -> Option<(crate::model::Transform, [f64; 2])> {
    let (transform, raster) = if mask {
        let mask = layer.mask.as_ref()?;
        (
            mask.placement.unwrap_or(layer.transform),
            Some(&mask.raster),
        )
    } else {
        (layer.transform, layer.image.as_ref())
    };
    let original = raster.map_or(transform.size, |raster| {
        [raster.width as f64, raster.height as f64]
    });
    Some((transform, original))
}

pub(super) fn write_header_transform(
    document: &mut Document,
    id: Uuid,
    transform: crate::model::Transform,
    mask: bool,
) -> anyhow::Result<()> {
    if mask {
        crate::edit::set_transform(document, id, transform, true)
    } else {
        anyhow::ensure!(transform.is_valid(), "变换参数无效");
        document
            .layer_mut(id)
            .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?
            .transform = transform;
        Ok(())
    }
}

fn percentage_control(ui: &mut egui::Ui, label: &str, value: &mut f64) {
    ui.label(label);
    ui.add(egui::Slider::new(value, 0. ..=1.).show_value(false));
    let mut percent = *value * 100.;
    if ui
        .add_sized(
            [42., 26.],
            egui::DragValue::new(&mut percent)
                .range(0. ..=100.)
                .speed(1.),
        )
        .changed()
    {
        *value = percent / 100.;
    }
    ui.label("%");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient_state(session: &Session) -> GradientState {
        GradientState {
            session: session.id,
            request: Uuid::new_v4(),
            base_revision: session.history.revision(),
            before: session.doc.clone(),
            start: [0., 0.],
            end: [4., 0.],
            layer: session.doc.active_layer_id.unwrap(),
            mask: false,
            revision: None,
            parameters: ("Linear".into(), [0, 0, 0, 255], [255; 4], 1.),
        }
    }

    fn calculate_gradient(state: &GradientState) -> Document {
        let mut document = state.before.clone();
        crate::brush::gradient(
            &mut document,
            state.layer,
            state.mask,
            state.start,
            state.end,
            state.parameters.1,
            state.parameters.2,
            crate::brush::GradientShape::Linear,
            state.parameters.3,
        )
        .unwrap();
        document
    }

    fn submit(
        session: &mut Session,
        state: &mut GradientState,
        document: Document,
    ) -> Result<bool, String> {
        let revision = session.history.revision();
        let serial = session.serial;
        session.gradient_task = Some((state.request, false));
        session.edit_busy = true;
        finish_gradient_result(session, state, revision, serial, Ok(document))
    }

    #[test]
    fn noop_gradient_does_not_own_or_undo_a_previous_edit() {
        let mut session = Session::new(Document::new(4, 4), None);
        session.edit("Earlier edit", |document| {
            document.layers[0].name = "Renamed".into()
        });
        let previous_revision = session.history.revision();
        let previous_document = session.doc.clone();
        let mut state = gradient_state(&session);
        state.end = state.start;
        let result = calculate_gradient(&state);
        assert!(!submit(&mut session, &mut state, result).unwrap());
        assert!(state.revision.is_none());
        cancel_gradient_state(&mut session, &state);
        assert_eq!(session.doc, previous_document);
        assert_eq!(session.history.revision(), previous_revision);
        assert_eq!(session.history.undo_name(), Some("Earlier edit"));
    }

    #[test]
    fn cancelling_an_owned_gradient_restores_only_its_before_document() {
        let mut session = Session::new(Document::new(4, 4), None);
        session.edit("Earlier edit", |document| {
            document.layers[0].name = "Renamed".into()
        });
        let before = session.doc.clone();
        let mut state = gradient_state(&session);
        let result = calculate_gradient(&state);
        assert!(submit(&mut session, &mut state, result).unwrap());
        assert_ne!(session.doc, before);
        cancel_gradient_state(&mut session, &state);
        assert_eq!(session.doc, before);
        assert_eq!(session.history.undo_name(), Some("Earlier edit"));
    }

    #[test]
    fn cancelling_an_inflight_gradient_discards_its_result_and_releases_busy() {
        let mut session = Session::new(Document::new(4, 4), None);
        let before = session.doc.clone();
        let mut state = gradient_state(&session);
        let result = calculate_gradient(&state);
        let revision = session.history.revision();
        let serial = session.serial;
        session.gradient_task = Some((state.request, false));
        session.edit_busy = true;
        cancel_gradient_state(&mut session, &state);
        assert!(session.edit_busy);
        assert!(
            !finish_gradient_result(&mut session, &mut state, revision, serial, Ok(result))
                .unwrap()
        );
        assert!(!session.edit_busy);
        assert!(session.gradient_task.is_none());
        assert_eq!(session.doc, before);
        assert!(session.history.undo_name().is_none());
    }

    #[test]
    fn cancelled_amend_does_not_reapply_the_last_preview() {
        let mut session = Session::new(Document::new(4, 4), None);
        let before = session.doc.clone();
        let mut state = gradient_state(&session);
        let result = calculate_gradient(&state);
        assert!(submit(&mut session, &mut state, result).unwrap());
        state.request = Uuid::new_v4();
        state.end = [0., 4.];
        let result = calculate_gradient(&state);
        let revision = session.history.revision();
        let serial = session.serial;
        session.gradient_task = Some((state.request, false));
        session.edit_busy = true;
        cancel_gradient_state(&mut session, &state);
        assert!(
            !finish_gradient_result(&mut session, &mut state, revision, serial, Ok(result))
                .unwrap()
        );
        assert_eq!(session.doc, before);
        assert!(!session.edit_busy);
    }

    #[test]
    fn stale_gradient_neither_clears_new_busy_state_nor_changes_pixels() {
        let mut session = Session::new(Document::new(4, 4), None);
        let before = session.doc.clone();
        let mut state = gradient_state(&session);
        let result = calculate_gradient(&state);
        let revision = session.history.revision();
        let serial = session.serial;
        let new_request = Uuid::new_v4();
        session.gradient_task = Some((new_request, false));
        session.edit_busy = true;
        assert!(
            !finish_gradient_result(&mut session, &mut state, revision, serial, Ok(result))
                .unwrap()
        );
        assert!(session.edit_busy);
        assert_eq!(session.gradient_task, Some((new_request, false)));
        assert_eq!(session.doc, before);
    }

    #[test]
    fn failed_history_amend_keeps_the_last_valid_document() {
        let mut session = Session::new(Document::new(4, 4), None);
        session.history.entry_limit = 0;
        let mut state = gradient_state(&session);
        let result = calculate_gradient(&state);
        assert!(submit(&mut session, &mut state, result).unwrap());
        let valid = session.doc.clone();
        state.request = Uuid::new_v4();
        state.end = [0., 4.];
        let result = calculate_gradient(&state);
        assert!(submit(&mut session, &mut state, result).is_err());
        assert_eq!(session.doc, valid);
        assert!(!session.edit_busy);
    }

    #[test]
    fn mask_header_reads_mask_dimensions_and_changes_only_mask_transform() {
        let mut document = Document::new(20, 30);
        let id = document.active_layer_id.unwrap();
        let placement = crate::model::Transform::new(2., 3., 8., 9.);
        document.active_mut().unwrap().mask = Some(Mask {
            raster: Arc::new(Raster::solid(7, 11, [255; 4])),
            enabled: true,
            linked: false,
            placement: Some(placement),
        });
        let before_layer = document.active().unwrap().transform;
        let (mut target, dimensions) =
            header_transform_target(document.active().unwrap(), true).unwrap();
        assert_eq!(target, placement);
        assert_eq!(dimensions, [7., 11.]);
        target.origin = [14., 15.];
        target.rotation = 45.;
        target.flip_x = true;
        target.sampling = Sampling::Nearest;
        write_header_transform(&mut document, id, target, true).unwrap();
        assert_eq!(document.active().unwrap().transform, before_layer);
        assert_eq!(
            document.active().unwrap().mask.as_ref().unwrap().placement,
            Some(target)
        );
    }

    #[test]
    fn mask_without_placement_starts_at_layer_transform_and_missing_mask_is_ignored() {
        let mut document = Document::new(20, 30);
        assert!(header_transform_target(document.active().unwrap(), true).is_none());
        document.active_mut().unwrap().mask = Some(Mask::white());
        let (target, dimensions) =
            header_transform_target(document.active().unwrap(), true).unwrap();
        assert_eq!(target, document.active().unwrap().transform);
        assert_eq!(dimensions, [1., 1.]);
    }

    #[test]
    fn normal_header_keeps_mask_placement_for_the_existing_property_commit() {
        let mut document = Document::new(20, 30);
        let id = document.active_layer_id.unwrap();
        let placement = crate::model::Transform::new(2., 3., 8., 9.);
        let mut mask = Mask::white();
        mask.placement = Some(placement);
        document.active_mut().unwrap().mask = Some(mask);
        let mut target = document.active().unwrap().transform;
        target.origin = [14., 15.];
        write_header_transform(&mut document, id, target, false).unwrap();
        assert_eq!(document.active().unwrap().transform, target);
        assert_eq!(
            document.active().unwrap().mask.as_ref().unwrap().placement,
            Some(placement)
        );
    }

    #[test]
    fn mask_palette_uses_opaque_black_and_white_and_preserves_normal_alpha() {
        let foreground = [45, 159, 231, 80];
        let background = [240, 35, 92, 130];
        assert_eq!(
            effective_palette_color(foreground, background, Some(false), false),
            [0, 0, 0, 255]
        );
        assert_eq!(
            effective_palette_color(foreground, background, Some(false), true),
            [255; 4]
        );
        assert_eq!(
            effective_palette_color(foreground, background, Some(true), false),
            [255; 4]
        );
        assert_eq!(
            effective_palette_color(foreground, background, Some(true), true),
            [0, 0, 0, 255]
        );
        assert_eq!(
            effective_palette_color(foreground, background, None, false),
            foreground
        );
        assert_eq!(
            effective_palette_color(foreground, background, None, true),
            background
        );
    }

    #[test]
    fn mask_palette_state_belongs_to_its_document_session() {
        let mut first = Session::new(Document::new(4, 4), None);
        let second = Session::new(Document::new(4, 4), None);
        first.mask_paint_white = true;
        assert!(first.mask_paint_white);
        assert!(!second.mask_paint_white);
    }
}
