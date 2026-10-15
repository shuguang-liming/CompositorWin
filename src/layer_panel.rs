//! Original Compositor layer layout, using the existing commands and thumbnail worker.
use super::*;

const ROW_HEIGHT: f32 = 52.;

fn property_drag_value(value: &mut f64) -> egui::DragValue<'_> {
    egui::DragValue::new(value).update_while_editing(false)
}

const EFFECT_KINDS: [&str; 6] = [
    "stroke",
    "shadow",
    "innerShadow",
    "colorOverlay",
    "outerGlow",
    "innerGlow",
];

#[derive(Clone)]
struct Rename {
    layer: Uuid,
    text: String,
    focus: bool,
}

#[derive(Clone)]
struct EffectTarget {
    layer: Uuid,
    kind: String,
}

struct LayerDrop {
    payload: Arc<LayerPayload>,
    parent: Option<Uuid>,
    above: Option<Uuid>,
}

struct LayerRow {
    id: Uuid,
    name: String,
    visible: bool,
    parent: Option<Uuid>,
    group: bool,
    text: bool,
    adjustment: bool,
    transform: crate::model::Transform,
    image: Option<Arc<Raster>>,
    mask: Option<Mask>,
    clipped_to: Option<String>,
    effects: Vec<(String, bool)>,
}

#[derive(Clone, Copy)]
enum Icon {
    Eye(bool),
    Disclosure(bool),
    Folder,
    Add,
    Mask,
    Effects,
    Adjustment,
    Delete,
    Link(bool),
    Text,
}

impl Icon {
    fn chrome(self) -> crate::chrome::Icon {
        use crate::chrome::Icon as Shared;
        match self {
            Self::Eye(true) => Shared::Eye,
            Self::Eye(false) => Shared::EyeOff,
            Self::Disclosure(true) => Shared::ChevronRight,
            Self::Disclosure(false) => Shared::ChevronDown,
            Self::Folder => Shared::Group,
            Self::Add => Shared::AddLayer,
            Self::Mask => Shared::Mask,
            Self::Effects => Shared::Effects,
            Self::Adjustment => Shared::Adjustment,
            Self::Delete => Shared::Trash,
            Self::Link(true) => Shared::Link,
            Self::Link(false) => Shared::Unlink,
            Self::Text => Shared::Text,
        }
    }
}

fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let side = rect.width().min(rect.height()).min(18.);
    crate::chrome::paint_icon(
        painter,
        icon.chrome(),
        Rect::from_center_size(rect.center(), Vec2::splat(side)),
        color,
    );
}

fn icon_button(ui: &mut egui::Ui, icon: Icon, size: Vec2, help: &str) -> egui::Response {
    crate::chrome::icon_button_sized(ui, icon.chrome(), false, help, size)
}

struct ThumbnailView {
    transform: crate::model::Transform,
    canvas: Vec2,
    selected: bool,
    background: Option<u8>,
    opacity: f32,
}

fn thumbnail(ui: &egui::Ui, rect: Rect, texture: egui::TextureId, view: ThumbnailView) {
    let ThumbnailView {
        transform,
        canvas,
        selected,
        background,
        opacity,
    } = view;
    let scale = (rect.width() / canvas.x).min(rect.height() / canvas.y);
    let fitted = Rect::from_center_size(rect.center(), canvas * scale);
    let painter = ui
        .painter()
        .with_clip_rect(fitted.intersect(ui.clip_rect()));
    if let Some(background) = background {
        painter.rect_filled(
            fitted,
            2,
            Color32::from_gray(background).gamma_multiply(opacity),
        );
    } else {
        painter.rect_filled(
            fitted,
            2,
            crate::chrome::theme::palette(ui.ctx(), ui.visuals())
                .thumb_checker_dark
                .gamma_multiply(opacity),
        );
        for y in 0..((fitted.height() / 6.).ceil() as i32) {
            for x in 0..((fitted.width() / 6.).ceil() as i32) {
                if (x + y) % 2 == 0 {
                    painter.rect_filled(
                        Rect::from_min_size(
                            fitted.min + Vec2::new(x as f32 * 6., y as f32 * 6.),
                            Vec2::splat(6.),
                        ),
                        0,
                        crate::chrome::theme::palette(ui.ctx(), ui.visuals())
                            .thumb_checker_light
                            .gamma_multiply(opacity),
                    );
                }
            }
        }
    }
    let mut mesh = egui::Mesh::with_texture(texture);
    for (corner, uv) in transform.corners().into_iter().zip([
        Pos2::new(0., 0.),
        Pos2::new(1., 0.),
        Pos2::new(1., 1.),
        Pos2::new(0., 1.),
    ]) {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: fitted.min + Vec2::new(corner[0] as f32, corner[1] as f32) * scale,
            uv,
            color: Color32::WHITE.gamma_multiply(opacity),
        });
    }
    mesh.indices.extend([0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
    ui.painter().rect_stroke(
        fitted,
        2,
        Stroke::new(
            if selected { 2.0_f32 } else { 0.5_f32 },
            if selected {
                ui.visuals().selection.stroke.color.gamma_multiply(opacity)
            } else {
                ui.visuals().widgets.noninteractive.bg_stroke.color
            },
        ),
        egui::StrokeKind::Inside,
    );
}

impl EditorApp {
    fn layer_panel_key(&self, value: &str) -> egui::Id {
        egui::Id::new(("original-layer-panel", self.session().id, value))
    }

    fn layer_select_row(&mut self, id: Uuid, modifiers: egui::Modifiers) {
        let clear_mask_view = self.mask_alone;
        if self.mask_target {
            self.finish_color_picker(false);
        }
        self.mask_target = false;
        self.mask_alone = false;
        let session = self.session_mut();
        let anchor = session.doc.active_layer_id;
        session.doc.active_layer_id = Some(id);
        if modifiers.shift
            && let Some(anchor) = anchor
        {
            let indices = session
                .doc
                .layers
                .iter()
                .position(|layer| layer.id == id)
                .zip(
                    session
                        .doc
                        .layers
                        .iter()
                        .position(|layer| layer.id == anchor),
                );
            if let Some((a, b)) = indices {
                if !modifiers.ctrl {
                    session.selected.clear();
                }
                session.selected.extend(
                    session.doc.layers[a.min(b)..=a.max(b)]
                        .iter()
                        .map(|layer| layer.id),
                );
            }
        } else if modifiers.ctrl {
            if !session.selected.insert(id) {
                session.selected.remove(&id);
            }
        } else {
            session.selected = HashSet::from([id]);
        }
        if clear_mask_view {
            session.changed();
        }
    }

    fn layer_select_target(&mut self, id: Uuid, mask: bool) {
        if mask != self.mask_target {
            self.finish_color_picker(false);
        }
        let changed = self.mask_target != mask || self.session().doc.active_layer_id != Some(id);
        self.session_mut().doc.active_layer_id = Some(id);
        self.session_mut().selected = HashSet::from([id]);
        self.mask_target = mask;
        if !mask {
            self.mask_alone = false;
        }
        if changed {
            self.session_mut().changed();
        }
    }

    fn layer_load_selection(&mut self, id: Uuid, mask: bool, modifiers: egui::Modifiers) {
        let mode = if modifiers.alt {
            SelectionMode::Subtract
        } else if modifiers.shift {
            SelectionMode::Add
        } else {
            SelectionMode::Replace
        };
        self.background_edit(
            if mask {
                "Select masked area"
            } else {
                "Select layer opacity"
            },
            move |document| {
                let incoming = crate::edit::layer_selection(document, id, mask)?;
                document.selection = Some(selection::combine(
                    document.selection.as_ref(),
                    &incoming,
                    mode,
                ));
                Ok(())
            },
        );
    }

    fn layer_begin_rename(&self, ui: &egui::Ui, id: Uuid) {
        if let Some(layer) = self.session().doc.layer(id) {
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    self.layer_panel_key("rename"),
                    Rename {
                        layer: id,
                        text: layer.name.clone(),
                        focus: true,
                    },
                )
            });
            ui.ctx().request_repaint();
        }
    }

    fn layer_appearance_ui(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new().inner_margin(12).show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6., 8.);
            let active = self.session().doc.active_layer_id.is_some();
            ui.add_enabled_ui(active, |ui| {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [48., 20.],
                        egui::Label::new(egui::RichText::new("混合").size(11.)),
                    );
                    if let Some(layer) = self.session_mut().doc.active_mut() {
                        ui.add_enabled_ui(!layer.is_group, |ui| {
                            egui::ComboBox::from_id_salt("layer-blend")
                                .width(ui.available_width())
                                .selected_text(tr(&layer.blend_mode.label()))
                                .show_ui(ui, |ui| {
                                    for mode in BlendMode::ALL {
                                        ui.selectable_value(
                                            &mut layer.blend_mode,
                                            mode,
                                            tr(&mode.label()),
                                        );
                                    }
                                });
                        });
                    } else {
                        ui.label("正常");
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [48., 20.],
                        egui::Label::new(egui::RichText::new("不透明度").size(11.)),
                    );
                    ui.spacing_mut().slider_width = (ui.available_width() - 68.).max(20.);
                    if let Some(layer) = self.session_mut().doc.active_mut() {
                        ui.add(egui::Slider::new(&mut layer.opacity, 0.0..=1.0).show_value(false));
                        let mut percent = layer.opacity * 100.;
                        if ui
                            .add_sized(
                                [44., 20.],
                                property_drag_value(&mut percent)
                                    .range(0.0..=100.)
                                    .speed(1.)
                                    .max_decimals(0),
                            )
                            .changed()
                        {
                            layer.opacity = percent / 100.;
                        }
                    }
                    ui.label(egui::RichText::new("%").size(11.));
                });
            });
        });
    }

    pub(super) fn restored_layers_ui(&mut self, ui: &mut egui::Ui) -> Vec<Command> {
        let before = self.session().doc.clone();
        let mut actions = vec![];
        let mut layer_drops = vec![];
        let inspector_key = self.layer_panel_key("inspector");
        let welcome = self.is_welcome();
        let count = if welcome {
            0
        } else {
            self.session().doc.layers.len()
        };
        let (header, header_response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.), Sense::click());
        ui.painter().text(
            Pos2::new(header.left() + 18., header.center().y),
            egui::Align2::LEFT_CENTER,
            "图层",
            egui::FontId::proportional(12.),
            ui.visuals().text_color(),
        );
        ui.painter().text(
            Pos2::new(header.right() - 18., header.center().y),
            egui::Align2::RIGHT_CENTER,
            count.to_string(),
            egui::FontId::monospace(11.),
            ui.visuals().weak_text_color(),
        );
        header_response.context_menu(|ui| {
            if ui
                .add_enabled(!welcome, egui::Button::new("图层属性与蒙版设置…"))
                .clicked()
            {
                ui.ctx()
                    .data_mut(|data| data.insert_temp(inspector_key, true));
                ui.close();
            }
        });
        ui.separator();
        ui.add_enabled_ui(!welcome, |ui| self.layer_appearance_ui(ui));
        ui.separator();
        let list_height = (ui.available_height() - 49.).max(1.);
        egui::ScrollArea::vertical()
            .id_salt(self.layer_panel_key("list"))
            .auto_shrink([false, false])
            .max_height(list_height)
            .min_scrolled_height(list_height)
            .show(ui, |ui| {
                if welcome {
                    ui.vertical_centered(|ui| {
                        ui.add_space(30.);
                        ui.label("暂无图层");
                        ui.label(egui::RichText::new("创建画布或导入图像。").size(11.).weak());
                    });
                } else {
                    self.layer_rows_ui(ui, &before, &mut actions, &mut layer_drops);
                }
            });
        ui.separator();
        let create_mask = ui
            .add_enabled_ui(!welcome, |ui| self.layer_footer_ui(ui, &mut actions))
            .inner;
        let mut inspector_open = ui
            .ctx()
            .data(|data| data.get_temp::<bool>(inspector_key).unwrap_or(false));
        let mut shape_edit = None;
        if inspector_open && !welcome {
            egui::Window::new("图层属性")
                .id(inspector_key)
                .open(&mut inspector_open)
                .default_width(270.)
                .resizable(true)
                .show(ui.ctx(), |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(500.)
                        .show(ui, |ui| {
                            shape_edit = self.layer_details_ui(ui);
                        });
                });
            ui.ctx()
                .data_mut(|data| data.insert_temp(inspector_key, inspector_open));
        }
        self.finish_layer_property_edit(ui, before, shape_edit);
        for drop in layer_drops {
            self.drop_layers(&drop.payload, drop.parent, drop.above);
        }
        if let Some(revealing) = create_mask {
            self.layer_add_mask(revealing);
        }
        actions
    }

    fn layer_rows_ui(
        &mut self,
        ui: &mut egui::Ui,
        before: &Document,
        actions: &mut Vec<Command>,
        layer_drops: &mut Vec<LayerDrop>,
    ) {
        let collapsed_key = self.layer_panel_key("collapsed");
        let rename_key = self.layer_panel_key("rename");
        let effect_key = self.layer_panel_key("effect");
        let mut collapsed = ui.ctx().data(|data| {
            data.get_temp::<HashSet<Uuid>>(collapsed_key)
                .unwrap_or_default()
        });
        collapsed.retain(|id| {
            self.session()
                .doc
                .layer(*id)
                .is_some_and(|layer| layer.is_group)
        });
        let nodes: Vec<_> = self
            .session()
            .doc
            .layers
            .iter()
            .rev()
            .map(|layer| LayerRow {
                id: layer.id,
                name: layer.name.clone(),
                visible: layer.visible,
                parent: layer.parent_id,
                group: layer.is_group,
                text: layer.text.is_some(),
                adjustment: layer.adjustment.is_some(),
                transform: layer.transform,
                image: layer.image.clone(),
                mask: layer.mask.clone(),
                clipped_to: layer.mask_source_id.map(|id| {
                    self.session()
                        .doc
                        .layer(id)
                        .map_or_else(|| "缺失图层".into(), |source| source.name.clone())
                }),
                effects: EFFECT_KINDS
                    .into_iter()
                    .filter_map(|kind| {
                        layer
                            .effects
                            .as_ref()
                            .and_then(|effects| effects.get(kind))
                            .map(|effect| {
                                (kind.to_owned(), effect["enabled"].as_bool().unwrap_or(true))
                            })
                    })
                    .collect(),
            })
            .collect();
        if nodes.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(30.);
                ui.label("暂无图层");
                ui.label(
                    egui::RichText::new("导入图像或添加空白图层。")
                        .size(11.)
                        .weak(),
                );
            });
        }
        ui.spacing_mut().item_spacing.y = 2.;
        for node in nodes {
            let mut depth = 0;
            let mut parent = node.parent;
            let mut hidden = false;
            while let Some(id) = parent {
                hidden |= collapsed.contains(&id);
                depth += 1;
                if depth >= 64 {
                    break;
                }
                parent = self
                    .session()
                    .doc
                    .layer(id)
                    .and_then(|layer| layer.parent_id);
            }
            if hidden {
                continue;
            }
            let (rect, response) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), ROW_HEIGHT),
                Sense::click_and_drag(),
            );
            let mut drag_response = response.clone();
            let selected = self.session().selected.contains(&node.id);
            if selected {
                ui.painter()
                    .rect_filled(rect, 0, ui.visuals().selection.bg_fill);
            } else if response.hovered() {
                ui.painter()
                    .rect_filled(rect, 0, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            ui.painter().hline(
                rect.x_range(),
                rect.bottom(),
                Stroke::new(0.5_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
            let color = if selected {
                ui.visuals().selection.stroke.color
            } else {
                ui.visuals().text_color()
            };
            let color = color.gamma_multiply(if node.visible { 1. } else { 0.35 });
            let mut row = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("layer-row", node.id))
                    .max_rect(rect),
            );
            let eye_rect = Rect::from_center_size(
                Pos2::new(rect.left() + 18., rect.center().y),
                Vec2::new(20., 32.),
            );
            let eye = row
                .interact(eye_rect, row.id().with("eye"), Sense::click_and_drag())
                .on_hover_text(if node.visible {
                    "隐藏图层；拖过眼睛可批量切换"
                } else {
                    "显示图层；拖过眼睛可批量切换"
                });
            paint_icon(row.painter(), eye_rect, Icon::Eye(node.visible), color);
            let mut show = if eye.clicked() {
                !node.visible
            } else {
                node.visible
            };
            if eye.drag_started() {
                self.visibility_sweep = Some((!node.visible, before.clone(), HashSet::new()));
            }
            if let Some((desired, _, visited)) = &mut self.visibility_sweep {
                if eye.contains_pointer() {
                    visited.insert(node.id);
                }
                if visited.contains(&node.id) {
                    show = *desired;
                }
            }
            if show != node.visible
                && let Some(layer) = self.session_mut().doc.layer_mut(node.id)
            {
                layer.visible = show;
            }
            let indent =
                depth.min(8) as f32 * 24. + if node.clipped_to.is_some() { 24. } else { 0. };
            let disclosure_rect = Rect::from_center_size(
                Pos2::new(rect.left() + 36. + indent, rect.center().y),
                Vec2::new(16., 24.),
            );
            if node.group {
                let disclosure = row
                    .interact(disclosure_rect, row.id().with("disclosure"), Sense::click())
                    .on_hover_text("展开／折叠图层组");
                paint_icon(
                    row.painter(),
                    disclosure_rect,
                    Icon::Disclosure(collapsed.contains(&node.id)),
                    color,
                );
                if disclosure.clicked() && !collapsed.insert(node.id) {
                    collapsed.remove(&node.id);
                }
            }
            let image_rect = Rect::from_min_size(
                Pos2::new(disclosure_rect.right() - 2., rect.center().y - 18.),
                Vec2::splat(36.),
            );
            let image = row
                .interact(image_rect, row.id().with("image"), Sense::click_and_drag())
                .on_hover_text(
                    "选择图层像素；Ctrl 点击载入透明度选区；Ctrl+Shift 添加；Ctrl+Alt 相减",
                );
            drag_response = drag_response.union(image.clone());
            let canvas = Vec2::new(
                self.session().doc.width.max(1) as f32,
                self.session().doc.height.max(1) as f32,
            );
            let active = self.session().doc.active_layer_id == Some(node.id)
                && self.session().selected.len() == 1;
            if node.group || node.text || node.adjustment {
                paint_icon(
                    row.painter(),
                    image_rect,
                    if node.group {
                        Icon::Folder
                    } else if node.text {
                        Icon::Text
                    } else {
                        Icon::Adjustment
                    },
                    color,
                );
                if active && !self.mask_target {
                    row.painter().rect_stroke(
                        image_rect,
                        3,
                        Stroke::new(2.0_f32, ui.visuals().selection.stroke.color),
                        egui::StrokeKind::Inside,
                    );
                }
            } else if let Some(pixels) = &node.image {
                let texture = self.session_mut().thumbnails.get(ui.ctx(), node.id, pixels);
                thumbnail(
                    &row,
                    image_rect,
                    texture,
                    ThumbnailView {
                        transform: node.transform,
                        canvas,
                        selected: active && !self.mask_target,
                        background: None,
                        opacity: if node.visible { 1. } else { 0.35 },
                    },
                );
            } else {
                row.painter().rect_stroke(
                    image_rect,
                    3,
                    Stroke::new(0.5_f32, color.gamma_multiply(0.4)),
                    egui::StrokeKind::Inside,
                );
            }
            if image.clicked() {
                let modifiers = ui.input(|input| input.modifiers);
                if modifiers.ctrl && node.image.is_some() {
                    self.layer_load_selection(node.id, false, modifiers);
                } else {
                    self.layer_select_target(node.id, false);
                    ui.ctx()
                        .data_mut(|data| data.remove::<EffectTarget>(effect_key));
                }
            }
            if image.double_clicked() {
                self.layer_open_editor(ui, node.id);
            }
            let mut text_left = image_rect.right() + 10.;
            if let Some(mask) = &node.mask {
                let link_rect = Rect::from_center_size(
                    Pos2::new(image_rect.right() + 6.5, rect.center().y),
                    Vec2::new(9., 20.),
                );
                let link = row
                    .interact(link_rect, row.id().with("link"), Sense::click())
                    .on_hover_text(if mask.linked {
                        "解除图层与蒙版链接"
                    } else {
                        "链接图层与蒙版"
                    });
                paint_icon(row.painter(), link_rect, Icon::Link(mask.linked), color);
                if link.clicked() {
                    self.layer_select_target(node.id, true);
                    if let Some(target) = self
                        .session_mut()
                        .doc
                        .layer_mut(node.id)
                        .and_then(|layer| layer.mask.as_mut())
                    {
                        target.linked = !mask.linked;
                    }
                }
                let mask_rect = Rect::from_min_size(
                    Pos2::new(image_rect.right() + 13., rect.center().y - 15.),
                    Vec2::splat(30.),
                );
                let mask_response = row
                    .interact(mask_rect, row.id().with("mask"), Sense::click())
                    .on_hover_text(
                        "选择蒙版；Alt 点击单独显示；Shift 点击启用／停用；Ctrl 点击载入遮挡选区",
                    );
                let texture =
                    self.session_mut()
                        .mask_thumbnails
                        .get(ui.ctx(), node.id, &mask.raster);
                thumbnail(
                    &row,
                    mask_rect,
                    texture,
                    ThumbnailView {
                        transform: mask.placement.unwrap_or(node.transform),
                        canvas,
                        selected: active && self.mask_target,
                        background: Some(render::cached_mask_background(&mask.raster)),
                        opacity: if node.visible { 1. } else { 0.35 },
                    },
                );
                if active && self.mask_target && self.mask_alone {
                    row.painter().rect_stroke(
                        mask_rect,
                        2,
                        Stroke::new(2.0_f32, Color32::WHITE),
                        egui::StrokeKind::Inside,
                    );
                }
                if !mask.enabled {
                    row.painter().line_segment(
                        [mask_rect.left_top(), mask_rect.right_bottom()],
                        Stroke::new(2.0_f32, Color32::from_rgb(220, 70, 70)),
                    );
                    row.painter().line_segment(
                        [mask_rect.right_top(), mask_rect.left_bottom()],
                        Stroke::new(2.0_f32, Color32::from_rgb(220, 70, 70)),
                    );
                }
                if mask_response.clicked() {
                    let modifiers = ui.input(|input| input.modifiers);
                    if modifiers.ctrl {
                        self.layer_load_selection(node.id, true, modifiers);
                    } else {
                        self.layer_select_target(node.id, true);
                        ui.ctx()
                            .data_mut(|data| data.remove::<EffectTarget>(effect_key));
                        if modifiers.shift
                            && let Some(target) = self
                                .session_mut()
                                .doc
                                .active_mut()
                                .and_then(|layer| layer.mask.as_mut())
                        {
                            target.enabled = !mask.enabled;
                        }
                        if modifiers.alt {
                            self.mask_alone = !self.mask_alone;
                            self.session_mut().changed();
                        }
                    }
                }
                text_left = mask_rect.right() + 5.;
            }
            let name_rect = Rect::from_min_max(
                Pos2::new(text_left.min(rect.right() - 9.), rect.top() + 7.),
                Pos2::new(rect.right() - 8., rect.top() + 27.),
            );
            let mut rename = ui
                .ctx()
                .data(|data| data.get_temp::<Rename>(rename_key))
                .filter(|rename| rename.layer == node.id);
            if let Some(state) = &mut rename {
                let edit = row.put(
                    name_rect,
                    egui::TextEdit::singleline(&mut state.text)
                        .font(egui::FontId::proportional(13.))
                        .id(row.id().with("rename")),
                );
                if state.focus {
                    edit.request_focus();
                    state.focus = false;
                }
                let cancel = ui.input(|input| input.key_pressed(Key::Escape));
                let commit = ui.input(|input| input.key_pressed(Key::Enter)) || edit.lost_focus();
                if cancel || commit {
                    if !cancel && let Some(layer) = self.session_mut().doc.layer_mut(node.id) {
                        layer.name = state.text.clone();
                    }
                    edit.surrender_focus();
                    ui.ctx().data_mut(|data| data.remove::<Rename>(rename_key));
                } else {
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(rename_key, state.clone()));
                }
            } else {
                let name = if node.clipped_to.is_some() {
                    format!("↳ {}", node.name)
                } else {
                    node.name.clone()
                };
                let label = row.put(
                    name_rect,
                    egui::Label::new(egui::RichText::new(name).size(13.).color(color))
                        .truncate()
                        .sense(Sense::click_and_drag()),
                );
                drag_response = drag_response.union(label.clone());
                if label.clicked() {
                    self.layer_select_row(node.id, ui.input(|input| input.modifiers));
                    ui.ctx()
                        .data_mut(|data| data.remove::<EffectTarget>(effect_key));
                }
                if label.double_clicked() {
                    self.layer_begin_rename(ui, node.id);
                }
                label.on_hover_text(&node.name);
            }
            let dimensions = if let Some(source) = &node.clipped_to {
                format!("剪贴至 {source}")
            } else if node.text {
                "文字 · 双击缩略图编辑".into()
            } else if node.adjustment {
                "调整 · 双击编辑".into()
            } else if node.group {
                "文件夹".into()
            } else {
                format!(
                    "{:.0} × {:.0}",
                    node.transform.size[0], node.transform.size[1]
                )
            };
            let dimensions_rect = Rect::from_min_max(
                Pos2::new(name_rect.left(), rect.top() + 29.),
                Pos2::new(name_rect.right(), rect.top() + 45.),
            );
            let dimensions_response = row.put(
                dimensions_rect,
                egui::Label::new(
                    egui::RichText::new(dimensions)
                        .size(10.)
                        .color(color.gamma_multiply(0.65)),
                )
                .truncate()
                .sense(Sense::click_and_drag()),
            );
            drag_response = drag_response.union(dimensions_response.clone());
            if dimensions_response.clicked() {
                ui.ctx()
                    .data_mut(|data| data.remove::<EffectTarget>(effect_key));
                if node.clipped_to.is_some() && ui.input(|input| input.modifiers.alt) {
                    self.layer_select_target(node.id, false);
                    actions.push(Command::Clip);
                } else {
                    self.layer_select_row(node.id, ui.input(|input| input.modifiers));
                }
            }
            if dimensions_response.double_clicked() && node.adjustment {
                self.layer_open_editor(ui, node.id);
            }
            if response.clicked() {
                self.layer_select_row(node.id, ui.input(|input| input.modifiers));
                ui.ctx()
                    .data_mut(|data| data.remove::<EffectTarget>(effect_key));
            }
            if response.double_clicked() && node.adjustment {
                self.layer_open_editor(ui, node.id);
            }
            drag_response.context_menu(|ui| {
                if ui.button("重命名").clicked() {
                    self.layer_select_target(node.id, false);
                    self.layer_begin_rename(ui, node.id);
                    ui.close();
                }
                ui.separator();
                for (label, command) in [
                    ("Duplicate", Command::Duplicate),
                    ("Delete", Command::Delete),
                    ("Group", Command::Group),
                    ("Merge Down", Command::MergeDown),
                    ("Add Mask", Command::Mask),
                    ("Clipping Mask", Command::Clip),
                ] {
                    if ui.button(tr(label)).clicked() {
                        self.session_mut().doc.active_layer_id = Some(node.id);
                        self.session_mut().selected = HashSet::from([node.id]);
                        actions.push(command);
                        ui.close();
                    }
                }
                if ui.button("图层属性…").clicked() {
                    self.layer_select_target(node.id, false);
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(self.layer_panel_key("inspector"), true));
                    ui.close();
                }
            });
            if rename.is_none() {
                let ids = if selected {
                    self.session().selected.iter().copied().collect()
                } else {
                    vec![node.id]
                };
                drag_response.dnd_set_drag_payload(LayerPayload {
                    document: self.session().id,
                    ids,
                });
                if let Some(payload) = drag_response.dnd_release_payload::<LayerPayload>() {
                    let parent = if node.group
                        && drag_response
                            .hover_pos()
                            .is_some_and(|point| point.y > rect.top() + rect.height() * 0.25)
                    {
                        Some(node.id)
                    } else {
                        node.parent
                    };
                    layer_drops.push(LayerDrop {
                        payload,
                        parent,
                        above: Some(node.id),
                    });
                }
            }
            self.layer_effect_rows_ui(ui, &node, indent, effect_key);
        }
        ui.ctx()
            .data_mut(|data| data.insert_temp(collapsed_key, collapsed));
    }

    fn layer_open_editor(&mut self, ui: &egui::Ui, id: Uuid) {
        self.layer_select_target(id, false);
        if let Some((value, origin)) = self.session().doc.layer(id).and_then(|layer| {
            layer
                .text
                .as_ref()
                .map(|value| (value.clone(), layer.transform.origin))
        }) {
            self.begin_dialog(Dialog::Text {
                layer: Some(id),
                origin,
            });
            self.dialog_value = value;
        } else {
            ui.ctx()
                .data_mut(|data| data.insert_temp(self.layer_panel_key("inspector"), true));
        }
    }

    fn layer_effect_rows_ui(
        &mut self,
        ui: &mut egui::Ui,
        node: &LayerRow,
        indent: f32,
        effect_key: egui::Id,
    ) {
        for (kind, enabled) in &node.effects {
            let selected = ui
                .ctx()
                .data(|data| data.get_temp::<EffectTarget>(effect_key))
                .is_some_and(|target| target.layer == node.id && target.kind == *kind);
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.), Sense::click());
            if selected {
                ui.painter()
                    .rect_filled(rect, 0, ui.visuals().selection.bg_fill);
            }
            let eye_rect = Rect::from_center_size(
                Pos2::new(rect.left() + 54. + indent, rect.center().y),
                Vec2::new(20., 22.),
            );
            let eye = ui.interact(eye_rect, response.id.with("effect-eye"), Sense::click());
            paint_icon(
                ui.painter(),
                eye_rect,
                Icon::Eye(*enabled),
                ui.visuals().weak_text_color(),
            );
            if eye.clicked()
                && let Some(effect) = self
                    .session_mut()
                    .doc
                    .layer_mut(node.id)
                    .and_then(|layer| layer.effects.as_mut())
                    .and_then(|effects| effects.get_mut(kind))
            {
                effect["enabled"] = json!(!*enabled);
            }
            let text_rect = Rect::from_min_max(
                Pos2::new(eye_rect.right() + 6., rect.top()),
                Pos2::new(rect.right() - 8., rect.bottom()),
            );
            let text_response = ui.put(
                text_rect,
                egui::Label::new(egui::RichText::new(humanize(kind)).size(11.).weak())
                    .truncate()
                    .sense(Sense::click()),
            );
            if response.clicked() || text_response.clicked() {
                self.layer_select_target(node.id, false);
                ui.ctx().data_mut(|data| {
                    data.insert_temp(
                        effect_key,
                        EffectTarget {
                            layer: node.id,
                            kind: kind.clone(),
                        },
                    )
                });
            }
            if response.double_clicked() || text_response.double_clicked() {
                self.layer_select_target(node.id, false);
                let value = self
                    .session()
                    .doc
                    .active()
                    .and_then(|layer| layer.effects.as_ref())
                    .and_then(|effects| effects.get(kind))
                    .cloned()
                    .unwrap_or_else(|| effect_defaults(kind));
                self.begin_dialog(Dialog::Effect { kind: kind.clone() });
                self.dialog_value = value;
            }
        }
    }

    fn layer_add_mask(&mut self, revealing: bool) {
        self.session_mut().edit("Add mask", |document| {
            let selection = document.selection.clone();
            let transform =
                crate::model::Transform::new(0., 0., document.width as f64, document.height as f64);
            if let Some(layer) = document.active_mut() {
                let mut mask = Mask::white();
                if let Some(selection) = selection {
                    mask.raster = Arc::new(Raster::from_rgba(
                        selection.width,
                        selection.height,
                        selection
                            .canvas_coverage()
                            .iter()
                            .flat_map(|&value| {
                                let value = if revealing { value } else { 255 - value };
                                [value, value, value, 255]
                            })
                            .collect(),
                    ));
                    mask.placement = Some(transform);
                } else if !revealing {
                    mask.raster = Arc::new(Raster::solid(1, 1, [0, 0, 0, 255]));
                }
                layer.mask = Some(mask);
            }
        });
    }

    fn layer_footer_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Command>) -> Option<bool> {
        let mut create_mask = None;
        let active = self.session().doc.active_layer_id.is_some();
        let has_mask = self
            .session()
            .doc
            .active()
            .is_some_and(|layer| layer.mask.is_some());
        let effect_key = self.layer_panel_key("effect");
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 0.;
                let button_size = Vec2::new((ui.available_width() / 6.).min(32.), 40.);
                ui.horizontal(|ui| {
                    if icon_button(ui, Icon::Add, button_size, "新建空白图层").clicked() {
                        actions.push(Command::AddLayer);
                    }
                    if icon_button(ui, Icon::Folder, button_size, "编组选中图层 (Ctrl+G)").clicked()
                    {
                        actions.push(Command::Group);
                    }
                    let mask_response = ui
                        .add_enabled_ui(active, |ui| {
                            icon_button(
                                ui,
                                Icon::Mask,
                                button_size,
                                if has_mask {
                                    "已有图层蒙版；右键管理蒙版"
                                } else {
                                    "添加图层蒙版；Alt 点击创建相反蒙版；右键管理"
                                },
                            )
                        })
                        .inner;
                    if mask_response.clicked() && !has_mask {
                        create_mask = Some(!ui.input(|input| input.modifiers.alt));
                    }
                    mask_response.context_menu(|ui| {
                        for (label, command, enabled) in [
                            ("添加图层蒙版", Command::Mask, !has_mask),
                            ("反相蒙版", Command::InvertMask, has_mask),
                            ("应用蒙版", Command::ApplyMask, has_mask),
                            ("删除蒙版", Command::DeleteMask, has_mask),
                            ("复制蒙版", Command::CopyMask, has_mask),
                            (
                                "粘贴蒙版",
                                Command::PasteMask,
                                self.mask_clipboard.is_some(),
                            ),
                            ("选择蒙版遮挡区域", Command::LoadMask, has_mask),
                            ("模糊／羽化蒙版…", Command::BlurMask, has_mask),
                            ("创建／释放剪贴蒙版", Command::Clip, active),
                        ] {
                            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                                actions.push(command);
                                ui.close();
                            }
                        }
                        if has_mask {
                            ui.separator();
                            let mut mask_target = self.mask_target;
                            if ui.checkbox(&mut mask_target, "编辑蒙版").changed() {
                                self.finish_color_picker(false);
                                self.mask_target = mask_target;
                            }
                            if ui.checkbox(&mut self.mask_alone, "仅显示蒙版").changed() {
                                self.session_mut().changed();
                            }
                        }
                    });
                    let effect_response = ui
                        .add_enabled_ui(active, |ui| {
                            icon_button(ui, Icon::Effects, button_size, "图层效果")
                        })
                        .inner;
                    egui::Popup::menu(&effect_response).show(|ui| {
                        for kind in EFFECT_KINDS {
                            if ui.button(humanize(kind)).clicked() {
                                let value = self
                                    .session()
                                    .doc
                                    .active()
                                    .and_then(|layer| layer.effects.as_ref())
                                    .and_then(|effects| effects.get(kind))
                                    .cloned()
                                    .unwrap_or_else(|| effect_defaults(kind));
                                self.begin_dialog(Dialog::Effect { kind: kind.into() });
                                self.dialog_value = value;
                                ui.close();
                            }
                        }
                    });
                    let adjustment_response =
                        icon_button(ui, Icon::Adjustment, button_size, "新建调整图层");
                    egui::Popup::menu(&adjustment_response).show(|ui| {
                        for kind in ADJUSTMENTS {
                            if ui.button(tr(kind)).clicked() {
                                self.begin_dialog(Dialog::Adjustment {
                                    kind: kind.to_string(),
                                });
                                self.dialog_value = adjustment_defaults(kind);
                                ui.close();
                            }
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let target = ui
                            .ctx()
                            .data(|data| data.get_temp::<EffectTarget>(effect_key))
                            .filter(|target| {
                                self.session().doc.active_layer_id == Some(target.layer)
                                    && self
                                        .session()
                                        .doc
                                        .layer(target.layer)
                                        .and_then(|layer| layer.effects.as_ref())
                                        .and_then(|effects| effects.get(&target.kind))
                                        .is_some()
                            });
                        let help = if target.is_some() {
                            "删除选中的效果"
                        } else if self.mask_target && has_mask {
                            "删除图层蒙版"
                        } else {
                            "删除选中的图层"
                        };
                        if ui
                            .add_enabled_ui(active, |ui| {
                                icon_button(ui, Icon::Delete, button_size, help)
                            })
                            .inner
                            .clicked()
                        {
                            if let Some(target) = target {
                                if let Some(effects) = self
                                    .session_mut()
                                    .doc
                                    .layer_mut(target.layer)
                                    .and_then(|layer| layer.effects.as_mut())
                                    .and_then(Value::as_object_mut)
                                {
                                    effects.remove(&target.kind);
                                }
                                ui.ctx()
                                    .data_mut(|data| data.remove::<EffectTarget>(effect_key));
                            } else {
                                actions.push(if self.mask_target && has_mask {
                                    Command::DeleteMask
                                } else {
                                    Command::Delete
                                });
                            }
                        }
                    });
                });
            });
        create_mask
    }

    /// The Move header owns the compact transform fields. The original history
    /// accumulator remains shared with layer appearance and mask placement.
    pub(super) fn transform_controls_ui(&mut self, ui: &mut egui::Ui) {
        if self.is_welcome() {
            return;
        }
        let before = self.session().doc.clone();
        let mask = self.mask_target;
        let Some(layer) = before.active() else {
            return;
        };
        let id = layer.id;
        let Some((mut transform, original_size)) =
            tool_header_ui::header_transform_target(layer, mask)
        else {
            return;
        };
        let initial_transform = transform;
        let percent = self.transform_percent;
        let lock_ratio = self.locks_transform_ratio;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.;
            {
                for (index, label) in [(0, "X"), (1, "Y")] {
                    ui.label(egui::RichText::new(label).size(11.));
                    ui.add_sized(
                        [50., 22.],
                        property_drag_value(&mut transform.origin[index]).speed(0.2),
                    );
                }
                for (index, label) in [(0, "宽"), (1, "高")] {
                    ui.label(egui::RichText::new(label).size(11.));
                    let original = original_size[index].max(1.);
                    let factor = if percent { original / 100. } else { 1. };
                    let mut displayed = transform.size[index] / factor;
                    if ui
                        .add_sized(
                            [65., 22.],
                            property_drag_value(&mut displayed)
                                .range(0.1 / factor..=30_000. / factor)
                                .speed(0.2)
                                .suffix(if percent { "%" } else { " px" }),
                        )
                        .changed()
                    {
                        let next = (displayed * factor).clamp(1., 30_000.);
                        if lock_ratio {
                            let other = 1 - index;
                            let minimum = 1. / transform.size[0].min(transform.size[1]).max(1.);
                            let maximum =
                                30_000. / transform.size[0].max(transform.size[1]).max(1.);
                            let ratio = (next / transform.size[index].max(1.))
                                .clamp(minimum, maximum.max(minimum));
                            transform.size[index] *= ratio;
                            transform.size[other] *= ratio;
                        } else {
                            transform.size[index] = next;
                        }
                    }
                }
                ui.label(egui::RichText::new("角度").size(11.));
                ui.add_sized(
                    [55., 22.],
                    property_drag_value(&mut transform.rotation)
                        .speed(0.2)
                        .suffix("°"),
                );
            }
            ui.menu_button("更多", |ui| {
                ui.checkbox(&mut self.transform_percent, "按百分比显示大小");
                ui.checkbox(&mut self.show_transform, "显示变换控件");
                let held = ui.input(|input| input.modifiers);
                let mut automatic = self.transform_auto_select != held.ctrl;
                if ui.checkbox(&mut automatic, "自动选择图层").changed() {
                    self.transform_auto_select = automatic != held.ctrl;
                }
                let mut lock_ratio = self.locks_transform_ratio != held.shift;
                if ui.checkbox(&mut lock_ratio, "保持宽高比").changed() {
                    self.locks_transform_ratio = lock_ratio != held.shift;
                }
                {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut transform.flip_x, "水平翻转");
                        ui.checkbox(&mut transform.flip_y, "垂直翻转");
                    });
                    egui::ComboBox::from_id_salt("transform-sampling")
                        .selected_text(tr(&format!("{:?}", transform.sampling)))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut transform.sampling,
                                Sampling::High,
                                tr("High quality"),
                            );
                            ui.selectable_value(
                                &mut transform.sampling,
                                Sampling::Smooth,
                                tr("Smooth"),
                            );
                            ui.selectable_value(
                                &mut transform.sampling,
                                Sampling::Nearest,
                                tr("Nearest"),
                            );
                        });
                }
            });
        });
        if transform != initial_transform
            && let Err(error) = tool_header_ui::write_header_transform(
                &mut self.session_mut().doc,
                id,
                transform,
                mask,
            )
        {
            self.message = Some(error.to_string());
        }
        // The layer rows process the final hovered eye on release, then consume
        // their own visibility sweep. The header must not close it first.
        if self.visibility_sweep.is_none() {
            self.finish_layer_property_edit(ui, before, None);
        }
    }

    fn layer_details_ui(&mut self, ui: &mut egui::Ui) -> Option<(Uuid, Value)> {
        let mut text_dialog = None;
        let mut shape_edit = None;
        if let Some(l) = self.session_mut().doc.active_mut() {
            if let Some(mask) = &mut l.mask {
                ui.separator();
                ui.strong("蒙版");
                ui.checkbox(&mut mask.enabled, "启用");
                let mut linked = mask.linked;
                if ui.checkbox(&mut linked, "链接到图层").changed() {
                    mask.linked = linked;
                }
                let mut transform = mask.placement.unwrap_or(l.transform);
                let previous = transform;
                if !mask.linked {
                    for (index, label) in [(0, "蒙版 X "), (1, "蒙版 Y ")] {
                        ui.add(property_drag_value(&mut transform.origin[index]).prefix(label));
                    }
                }
                for (index, label) in [(0, "蒙版宽度 "), (1, "蒙版高度 ")] {
                    ui.add(
                        property_drag_value(&mut transform.size[index])
                            .range(0.1..=30_000.)
                            .prefix(label),
                    );
                }
                ui.add(property_drag_value(&mut transform.rotation).prefix("蒙版旋转 "));
                ui.horizontal(|ui| {
                    ui.checkbox(&mut transform.flip_x, "水平翻转蒙版");
                    ui.checkbox(&mut transform.flip_y, "垂直翻转蒙版");
                });
                if transform != previous {
                    mask.placement = Some(transform);
                }
            }
            if let Some(style) = &l.shape {
                ui.collapsing("可编辑形状", |ui| {
                    let mut value = style.clone();
                    let mut color = ["red", "green", "blue"]
                        .map(|key| value[key].as_f64().unwrap_or(0.).clamp(0., 1.) as f32);
                    let mut changed = ui.color_edit_button_rgb(&mut color).changed();
                    for (key, channel) in ["red", "green", "blue"].into_iter().zip(color) {
                        value[key] = json!(channel);
                    }
                    let kind = value["kind"].as_str().unwrap_or("Rectangle");
                    let key = if kind == "Line" {
                        "lineWidth"
                    } else {
                        "cornerRadius"
                    };
                    if kind != "Ellipse" {
                        let mut amount = value[key].as_f64().unwrap_or(0.);
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut amount)
                                    .range(0.0..=500.0)
                                    .prefix(humanize(key)),
                            )
                            .changed();
                        value[key] = json!(amount);
                    }
                    if changed {
                        shape_edit = Some((l.id, value));
                    }
                });
            }
            if let Some(a) = &mut l.adjustment {
                ui.collapsing("调整参数", |ui| {
                    let kind = a["kind"].as_str().unwrap_or("Invert").to_owned();
                    crate::filter_panel::show(ui, &kind, a, None);
                });
            }
            if let Some(e) = &mut l.effects {
                ui.collapsing("图层效果", |ui| {
                    for kind in [
                        "stroke",
                        "shadow",
                        "innerShadow",
                        "colorOverlay",
                        "outerGlow",
                        "innerGlow",
                    ] {
                        if let Some(v) = e.get_mut(kind) {
                            ui.collapsing(humanize(kind), |ui| {
                                crate::filter_panel::show_effect(ui, kind, v);
                            });
                        }
                    }
                });
            }
            if l.text.is_some() && ui.button("编辑文字…").clicked() {
                text_dialog = Some((l.text.clone().unwrap(), l.id, l.transform.origin));
            }
        }
        if let Some((value, id, origin)) = text_dialog {
            self.begin_dialog(Dialog::Text {
                layer: Some(id),
                origin,
            });
            self.dialog_value = value;
        }
        shape_edit
    }

    fn finish_layer_property_edit(
        &mut self,
        ui: &egui::Ui,
        before: Document,
        shape_edit: Option<(Uuid, Value)>,
    ) {
        if let Some(id) = self.session().doc.active_layer_id
            && let (Some(old), Some(new)) = (before.layer(id), self.session().doc.layer(id))
        {
            let transform = new.transform;
            let opacity = new.opacity;
            let blend_mode = new.blend_mode;
            let changed_opacity = old.opacity != opacity;
            let changed_blend = old.blend_mode != blend_mode;
            let linked = new.mask.as_ref().map(|m| m.linked);
            if changed_opacity || changed_blend {
                let selected = self.session().selected.clone();
                for layer in &mut self.session_mut().doc.layers {
                    if selected.contains(&layer.id) {
                        if changed_opacity {
                            layer.opacity = opacity;
                        }
                        if changed_blend && !layer.is_group {
                            layer.blend_mode = blend_mode;
                        }
                    }
                }
            }
            if old.transform != transform {
                self.session_mut().doc.layer_mut(id).unwrap().transform = old.transform;
                let ids = self.session().selected.iter().copied().collect::<Vec<_>>();
                let result = if old.is_group || ids.len() > 1 {
                    crate::edit::transform_layers(
                        &mut self.session_mut().doc,
                        &ids,
                        old.transform,
                        transform,
                    )
                } else {
                    crate::edit::set_transform(&mut self.session_mut().doc, id, transform, false)
                };
                if let Err(e) = result {
                    self.message = Some(e.to_string());
                }
            }
            if let (Some(old_mask), Some(linked)) = (old.mask.as_ref(), linked)
                && old_mask.linked != linked
            {
                self.session_mut()
                    .doc
                    .layer_mut(id)
                    .unwrap()
                    .mask
                    .as_mut()
                    .unwrap()
                    .linked = old_mask.linked;
                if let Err(e) =
                    crate::edit::set_mask_linked(&mut self.session_mut().doc, id, linked)
                {
                    self.message = Some(e.to_string());
                }
            }
        }
        let sweep_before = if ui.input(|i| i.pointer.any_released()) {
            self.visibility_sweep.take().map(|(_, before, _)| before)
        } else {
            None
        };
        if self.visibility_sweep.is_some() {
            if before != self.session().doc {
                self.session_mut().changed();
            }
        } else if ui.input(|i| i.pointer.primary_down()) {
            if before != self.session().doc {
                if self.property_edit.is_none() {
                    self.property_edit = Some((self.session().id, before));
                }
                self.session_mut().changed();
            }
        } else {
            let original = self
                .property_edit
                .take()
                .filter(|(id, _)| *id == self.session().id)
                .map(|(_, document)| document);
            let s = self.session_mut();
            if s.history.record(
                "Layer properties",
                sweep_before.or(original).unwrap_or(before),
                &s.doc,
            ) {
                s.changed();
            }
        }
        if let Some((id, style)) = shape_edit {
            self.background_edit("Shape appearance", move |doc| {
                let layer = doc
                    .layer_mut(id)
                    .ok_or_else(|| anyhow::anyhow!("Shape layer was deleted"))?;
                layer.image = Some(Arc::new(crate::edit::shape_raster(
                    &style,
                    layer.transform.size[0].round().max(1.) as u32,
                    layer.transform.size[1].round().max(1.) as u32,
                )?));
                layer.shape = Some(style);
                Ok(())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask_x_frame(
        app: &mut EditorApp,
        context: &egui::Context,
        events: Vec<egui::Event>,
    ) -> Rect {
        let mut field_rect = Rect::NOTHING;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let before = app.session().doc.clone();
                    let layer = before.active().unwrap();
                    let id = layer.id;
                    let (mut target, _) =
                        tool_header_ui::header_transform_target(layer, true).unwrap();
                    let original = target;
                    field_rect = ui
                        .add_sized(
                            [50., 22.],
                            property_drag_value(&mut target.origin[0]).speed(0.2),
                        )
                        .rect;
                    if target != original {
                        tool_header_ui::write_header_transform(
                            &mut app.session_mut().doc,
                            id,
                            target,
                            true,
                        )
                        .unwrap();
                    }
                    app.finish_layer_property_edit(ui, before, None);
                });
            },
        );
        field_rect
    }

    #[test]
    fn typed_mask_position_commits_once_and_undo_restores_original_document() {
        let mut document = Document::new(8, 8);
        document.active_mut().unwrap().mask = Some(Mask {
            raster: Arc::new(Raster::solid(1, 1, [255; 4])),
            enabled: true,
            linked: false,
            placement: Some(crate::model::Transform::new(13., 9., 8., 8.)),
        });
        let original = document.clone();
        let mut app = EditorApp::create(None, None);
        app.tabs = vec![Session::new(document, None)];
        app.current = 0;
        let context = egui::Context::default();
        mask_x_frame(&mut app, &context, vec![]);
        let point = mask_x_frame(&mut app, &context, vec![]).center();
        for pressed in [true, false] {
            mask_x_frame(
                &mut app,
                &context,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        mask_x_frame(&mut app, &context, vec![]);
        let text_id = context.memory(|memory| memory.focused()).unwrap();
        assert!(egui::TextEdit::load_state(&context, text_id).is_some());
        assert!(context.wants_keyboard_input());
        mask_x_frame(
            &mut app,
            &context,
            vec![egui::Event::Key {
                key: Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                },
            }],
        );
        for character in ["3", "7", ".", "2", "5"] {
            mask_x_frame(
                &mut app,
                &context,
                vec![egui::Event::Text(character.into())],
            );
            assert_eq!(app.session().doc, original);
            assert!(app.session().history.undo_name().is_none());
        }
        mask_x_frame(
            &mut app,
            &context,
            vec![egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        mask_x_frame(&mut app, &context, vec![]);
        let mut expected = original.clone();
        expected
            .active_mut()
            .unwrap()
            .mask
            .as_mut()
            .unwrap()
            .placement
            .as_mut()
            .unwrap()
            .origin[0] = 37.25;
        assert_eq!(app.session().doc, expected);
        let session = app.session_mut();
        assert!(session.history.undo(&mut session.doc));
        assert_eq!(session.doc, original);
        assert!(session.history.undo_name().is_none());
        assert!(session.history.redo(&mut session.doc));
        assert_eq!(session.doc, expected);
    }
}
