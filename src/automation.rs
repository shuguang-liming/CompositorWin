//! Headless editing operations shared by the CLI and AI agents. The caller owns history and transactions.
use crate::{
    brush::{self, BrushMode, BrushSettings, GradientShape, HealingMode},
    edit::{self, ShapeKind, TrimBasis},
    filter_edit, filters, image_io, matte,
    model::{BlendMode, Document, Layer, Mask, Raster, Sampling, Selection, Transform},
    project, render,
    segmentation::SegmentationEngine,
    selection::{self, SelectionMode, WandSettings},
    text::{self, TextStyle},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf, sync::Arc};
use uuid::Uuid;

fn yes() -> bool {
    true
}
fn one() -> f64 {
    1.
}
fn center() -> [f64; 2] {
    [0.5; 2]
}
fn all_sides() -> [bool; 4] {
    [true; 4]
}
fn empty_object() -> Value {
    json!({})
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    AddLayer {
        name: String,
        #[serde(default)]
        color: Option<[u8; 4]>,
    },
    SetActiveLayer {
        #[serde(default)]
        layer: Option<Uuid>,
    },
    DeleteLayers {
        layers: Vec<Uuid>,
    },
    DuplicateLayers {
        layers: Vec<Uuid>,
    },
    GroupLayers {
        layers: Vec<Uuid>,
        name: String,
    },
    Ungroup {
        #[serde(default)]
        layer: Option<Uuid>,
    },
    PlaceLayer {
        #[serde(default)]
        layer: Option<Uuid>,
        #[serde(default)]
        parent: Option<Uuid>,
        #[serde(default)]
        above: Option<Uuid>,
    },
    SetLayer {
        #[serde(default)]
        layer: Option<Uuid>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        visible: Option<bool>,
        #[serde(default)]
        opacity: Option<f64>,
        #[serde(default)]
        blend_mode: Option<BlendMode>,
    },
    MoveLayers {
        layers: Vec<Uuid>,
        delta: [f64; 2],
    },
    SetTransform {
        #[serde(default)]
        layer: Option<Uuid>,
        #[serde(deserialize_with = "deserialize_transform")]
        transform: Transform,
        #[serde(default)]
        mask: bool,
    },
    TransformLayers {
        layers: Vec<Uuid>,
        #[serde(deserialize_with = "deserialize_transform")]
        transform: Transform,
    },
    FlipLayers {
        layers: Vec<Uuid>,
        horizontal: bool,
    },
    FlipCanvas {
        horizontal: bool,
    },
    RotateCanvas {
        clockwise: bool,
    },
    ConvertDepth {
        #[serde(default)]
        layer: Option<Uuid>,
        depth: String,
    },
    ResizeCanvas {
        width: u32,
        height: u32,
        #[serde(default = "center")]
        anchor: [f64; 2],
    },
    ResizeImage {
        width: u32,
        height: u32,
        #[serde(default)]
        sampling: Sampling,
    },
    Crop {
        rect: [f64; 4],
    },
    Trim {
        #[serde(default)]
        basis: TrimSource,
        #[serde(default)]
        tolerance: u8,
        #[serde(default = "all_sides")]
        sides: [bool; 4],
    },
    AddGuide {
        horizontal: bool,
        position: f64,
    },
    DeleteGuide {
        guide: Uuid,
    },
    ClearGuides {},
    Fill {
        #[serde(default)]
        layer: Option<Uuid>,
        color: [u8; 4],
        #[serde(default)]
        mask: bool,
    },
    Clear {
        #[serde(default)]
        layer: Option<Uuid>,
        #[serde(default)]
        mask: bool,
    },
    InvertPixels {
        #[serde(default)]
        layer: Option<Uuid>,
        #[serde(default)]
        mask: bool,
    },
    ContentAwareFill {
        #[serde(default)]
        layer: Option<Uuid>,
    },
    Brush {
        #[serde(default)]
        layer: Option<Uuid>,
        points: Vec<[f64; 2]>,
        #[serde(default)]
        mask: bool,
        #[serde(default)]
        settings: StrokeSettings,
    },
    Gradient {
        #[serde(default)]
        layer: Option<Uuid>,
        start: [f64; 2],
        end: [f64; 2],
        from: [u8; 4],
        to: [u8; 4],
        #[serde(default)]
        shape: GradientKind,
        #[serde(default = "one")]
        opacity: f64,
        #[serde(default)]
        mask: bool,
    },
    Select {
        selection: SelectionAction,
    },
    Mask {
        #[serde(default)]
        layer: Option<Uuid>,
        action: MaskAction,
    },
    SubjectMask {
        #[serde(default)]
        layer: Option<Uuid>,
        #[serde(default = "yes")]
        replace_existing: bool,
        #[serde(default = "empty_object")]
        settings: Value,
    },
    Filter {
        #[serde(default)]
        layer: Option<Uuid>,
        kind: String,
        #[serde(default = "empty_object")]
        settings: Value,
        #[serde(default)]
        mask: bool,
    },
    Adjustment {
        #[serde(default)]
        layer: Option<Uuid>,
        kind: String,
        #[serde(default = "empty_object")]
        settings: Value,
        #[serde(default = "yes")]
        create: bool,
    },
    Effects {
        #[serde(default)]
        layer: Option<Uuid>,
        #[serde(default)]
        effects: Option<Value>,
    },
    AddShape {
        kind: Shape,
        rect: [f64; 4],
        color: [u8; 4],
        #[serde(default)]
        corner_radius: f64,
        #[serde(default)]
        smoothing: bool,
        #[serde(default = "one")]
        line_width: f64,
        #[serde(default)]
        line_ends: Option<[[f64; 2]; 2]>,
    },
    UpdateShape {
        #[serde(default)]
        layer: Option<Uuid>,
        style: Value,
    },
    AddText {
        style: Value,
        origin: [f64; 2],
    },
    UpdateText {
        #[serde(default)]
        layer: Option<Uuid>,
        style: Value,
    },
    ImportLayers {
        paths: Vec<PathBuf>,
        #[serde(default)]
        point: Option<[f64; 2]>,
    },
    MergeLayers {
        layers: Vec<Uuid>,
    },
    Flatten {
        #[serde(default)]
        name: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Combine {
    #[default]
    Replace,
    Add,
    Subtract,
    Intersect,
}
impl From<Combine> for SelectionMode {
    fn from(value: Combine) -> Self {
        match value {
            Combine::Replace => Self::Replace,
            Combine::Add => Self::Add,
            Combine::Subtract => Self::Subtract,
            Combine::Intersect => Self::Intersect,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum SelectionAction {
    Rectangle {
        rect: [f64; 4],
        #[serde(default)]
        mode: Combine,
        #[serde(default = "yes")]
        antialiased: bool,
    },
    Ellipse {
        rect: [f64; 4],
        #[serde(default)]
        mode: Combine,
        #[serde(default = "yes")]
        antialiased: bool,
    },
    Polygon {
        points: Vec<[f64; 2]>,
        #[serde(default)]
        mode: Combine,
        #[serde(default = "yes")]
        antialiased: bool,
    },
    Wand {
        point: [f64; 2],
        #[serde(default)]
        layer: Option<Uuid>,
        #[serde(default)]
        sample_merged: bool,
        #[serde(default = "wand_tolerance")]
        tolerance: u8,
        #[serde(default)]
        sample_radius: u32,
        #[serde(default = "yes")]
        contiguous: bool,
        #[serde(default)]
        mode: Combine,
    },
    ColorRange {
        colors: Vec<[u8; 4]>,
        #[serde(default)]
        layer: Option<Uuid>,
        #[serde(default)]
        sample_merged: bool,
        fuzziness: u8,
        #[serde(default)]
        mode: Combine,
    },
    Alpha {
        #[serde(default)]
        layer: Option<Uuid>,
        #[serde(default)]
        mask: bool,
        #[serde(default)]
        invert: bool,
        #[serde(default)]
        mode: Combine,
    },
    Subject {
        #[serde(default)]
        layer: Option<Uuid>,
        #[serde(default)]
        point: Option<[f64; 2]>,
        #[serde(default)]
        mode: Combine,
    },
    All {},
    None {},
    Invert {},
    Feather {
        radius: f64,
    },
    Expand {
        amount: u32,
    },
    Contract {
        amount: u32,
    },
    Translate {
        delta: [i32; 2],
    },
}
fn wand_tolerance() -> u8 {
    32
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum MaskAction {
    Add {
        #[serde(default = "yes")]
        revealing: bool,
        #[serde(default)]
        from_selection: bool,
    },
    Apply {},
    Delete {},
    Link {
        linked: bool,
    },
    Enable {
        enabled: bool,
    },
    ReplaceFromFile {
        path: PathBuf,
        #[serde(default)]
        invert: bool,
    },
    ClipTo {
        #[serde(default)]
        source: Option<Uuid>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrimSource {
    #[default]
    Transparent,
    TopLeft,
    BottomRight,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    Rectangle,
    Ellipse,
    Line,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GradientKind {
    #[default]
    Linear,
    Radial,
    Angle,
    Reflected,
    Diamond,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum StrokeMode {
    Paint {},
    Erase {},
    Smudge {},
    Liquify {},
    Clone {
        offset: [f64; 2],
        #[serde(default)]
        sample_merged: bool,
    },
    Heal {
        #[serde(default)]
        method: HealMethod,
    },
    Blur {
        radius: f64,
    },
}
impl Default for StrokeMode {
    fn default() -> Self {
        Self::Paint {}
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HealMethod {
    #[default]
    ContentAware,
    CreateTexture,
    ProximityMatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StrokeSettings {
    pub diameter: f64,
    pub hardness: f64,
    pub opacity: f64,
    pub color: [u8; 4],
    pub smoothing: f64,
    pub brush: StrokeMode,
}
impl Default for StrokeSettings {
    fn default() -> Self {
        Self {
            diameter: 40.,
            hardness: 1.,
            opacity: 1.,
            color: [0, 0, 0, 255],
            smoothing: 0.,
            brush: StrokeMode::Paint {},
        }
    }
}

#[derive(Default)]
pub struct Engine {
    segmentation: Option<SegmentationEngine>,
}
impl Engine {
    fn segmentation(&mut self) -> Result<&SegmentationEngine> {
        if self.segmentation.is_none() {
            self.segmentation = Some(SegmentationEngine::load_default()?);
        }
        Ok(self.segmentation.as_ref().unwrap())
    }

    pub fn apply(&mut self, doc: &mut Document, operation: &Operation) -> Result<Value> {
        self.apply_with_workspace(doc, operation, &[], 0)
    }

    /// Include transaction snapshots and retained history even while importing multiple sources.
    pub fn apply_with_workspace(
        &mut self,
        doc: &mut Document,
        operation: &Operation,
        retained_documents: &[&Document],
        retained_history_bytes: u64,
    ) -> Result<Value> {
        ensure_edit_budget(doc, retained_documents, retained_history_bytes)?;
        let result = match operation {
            Operation::AddLayer { name, color } => {
                validate_name(name)?;
                let id = edit::add_layer(doc, name);
                if let Some(color) = color {
                    doc.layer_mut(id).unwrap().image =
                        Some(Arc::new(Raster::solid(doc.width, doc.height, *color)));
                }
                json!({"layer":id})
            }
            Operation::SetActiveLayer { layer } => {
                let id = layer_id(doc, *layer)?;
                doc.active_layer_id = Some(id);
                json!({"layer":id})
            }
            Operation::DeleteLayers { layers } => {
                validate_ids(doc, layers)?;
                edit::delete_layers(doc, layers)?;
                json!({"deleted":layers})
            }
            Operation::DuplicateLayers { layers } => {
                validate_ids(doc, layers)?;
                json!({"layers":edit::duplicate_layers(doc,layers)?})
            }
            Operation::GroupLayers { layers, name } => {
                validate_ids(doc, layers)?;
                validate_name(name)?;
                json!({"layer":edit::add_group(doc,name,layers)?})
            }
            Operation::Ungroup { layer } => {
                let id = layer_id(doc, *layer)?;
                edit::ungroup(doc, id)?;
                json!({"ungrouped":id})
            }
            Operation::PlaceLayer {
                layer,
                parent,
                above,
            } => {
                let id = layer_id(doc, *layer)?;
                if let Some(above) = above {
                    let target = doc.layer(layer_id(doc, Some(*above))?).unwrap();
                    ensure!(target.parent_id == *parent, "above 必须与目标父组相同");
                }
                edit::place_layer(doc, id, *parent, *above)?;
                json!({"layer":id})
            }
            Operation::SetLayer {
                layer,
                name,
                visible,
                opacity,
                blend_mode,
            } => {
                let id = layer_id(doc, *layer)?;
                ensure!(
                    name.is_some()
                        || visible.is_some()
                        || opacity.is_some()
                        || blend_mode.is_some(),
                    "至少指定一个图层属性"
                );
                if let Some(name) = name {
                    validate_name(name)?;
                }
                if let Some(opacity) = opacity {
                    unit(*opacity, "opacity")?;
                }
                let target = doc.layer_mut(id).unwrap();
                if let Some(name) = name {
                    target.name.clone_from(name);
                }
                if let Some(visible) = visible {
                    target.visible = *visible;
                }
                if let Some(opacity) = opacity {
                    target.opacity = *opacity;
                }
                if let Some(mode) = blend_mode {
                    target.blend_mode = *mode;
                }
                json!({"layer":id})
            }
            Operation::MoveLayers { layers, delta } => {
                validate_ids(doc, layers)?;
                finite(delta, "delta")?;
                edit::move_layers(doc, layers, *delta)?;
                json!({"layers":layers})
            }
            Operation::SetTransform {
                layer,
                transform,
                mask,
            } => {
                let id = layer_id(doc, *layer)?;
                validate_transform(*transform)?;
                if !mask {
                    ensure!(
                        !doc.layer(id).unwrap().is_group,
                        "组变换请使用 transform_layers"
                    );
                }
                edit::set_transform(doc, id, *transform, *mask)?;
                json!({"layer":id,"transform":transform})
            }
            Operation::TransformLayers { layers, transform } => {
                validate_ids(doc, layers)?;
                validate_transform(*transform)?;
                let bounds = edit::selected_bounds(doc, layers);
                let old = Transform::new(bounds[0], bounds[1], bounds[2], bounds[3]);
                edit::transform_layers(doc, layers, old, *transform)?;
                json!({"layers":layers})
            }
            Operation::FlipLayers { layers, horizontal } => {
                validate_ids(doc, layers)?;
                edit::flip_layers(doc, layers, *horizontal)?;
                json!({"layers":layers})
            }
            Operation::FlipCanvas { horizontal } => {
                edit::flip_canvas(doc, *horizontal);
                json!({})
            }
            Operation::RotateCanvas { clockwise } => {
                edit::rotate_canvas(doc, *clockwise);
                json!({"width":doc.width,"height":doc.height})
            }
            Operation::ConvertDepth { layer, depth } => {
                let id = layer.or(doc.active_layer_id).context("请先选择图层")?;
                let target = match depth.as_str() {
                    "8-bit" => None,
                    "16-bit" => Some(crate::deep_pixels::Depth::U16),
                    "HDR" => Some(crate::deep_pixels::Depth::Hdr),
                    _ => bail!("depth 必须为 8-bit、16-bit 或 HDR"),
                };
                edit::convert_layer_depth(doc, id, target)?;
                json!({"layer":id,"depth":depth})
            }
            Operation::ResizeCanvas {
                width,
                height,
                anchor,
            } => {
                edit::resize_canvas(doc, *width, *height, *anchor)?;
                json!({"width":width,"height":height})
            }
            Operation::ResizeImage {
                width,
                height,
                sampling,
            } => {
                edit::resize_image(doc, *width, *height, *sampling)?;
                json!({"width":width,"height":height})
            }
            Operation::Crop { rect } => {
                validate_rect(*rect)?;
                edit::crop(doc, *rect)?;
                json!({"width":doc.width,"height":doc.height})
            }
            Operation::Trim {
                basis,
                tolerance,
                sides,
            } => {
                ensure!(sides.iter().any(|v| *v), "至少选择一条修边方向");
                let basis = match basis {
                    TrimSource::Transparent => TrimBasis::Transparent,
                    TrimSource::TopLeft => TrimBasis::TopLeft,
                    TrimSource::BottomRight => TrimBasis::BottomRight,
                };
                edit::trim(doc, basis, *tolerance, *sides)?;
                json!({"width":doc.width,"height":doc.height})
            }
            Operation::AddGuide {
                horizontal,
                position,
            } => {
                ensure!(position.is_finite(), "参考线位置无效");
                json!({"guide":edit::add_guide(doc,*horizontal,*position)?})
            }
            Operation::DeleteGuide { guide } => {
                ensure!(doc.guides.iter().any(|g| g.id == *guide), "参考线已不存在");
                doc.guides.retain(|g| g.id != *guide);
                json!({"deleted":guide})
            }
            Operation::ClearGuides {} => {
                let count = doc.guides.len();
                doc.guides.clear();
                json!({"deleted_count":count})
            }
            Operation::Fill { layer, color, mask } => {
                let id = pixel_layer(doc, *layer, *mask)?;
                edit::fill(doc, id, *mask, *color)?;
                json!({"layer":id})
            }
            Operation::Clear { layer, mask } => {
                let id = pixel_layer(doc, *layer, *mask)?;
                edit::clear(doc, id, *mask)?;
                json!({"layer":id})
            }
            Operation::InvertPixels { layer, mask } => {
                let id = pixel_layer(doc, *layer, *mask)?;
                edit::invert_pixels(doc, id, *mask)?;
                json!({"layer":id})
            }
            Operation::ContentAwareFill { layer } => {
                let id = pixel_layer(doc, *layer, false)?;
                ensure!(doc.selection.is_some(), "内容识别填充需要选区");
                edit::content_aware_fill_selected(doc, id)?;
                json!({"layer":id})
            }
            Operation::Brush {
                layer,
                points,
                mask,
                settings,
            } => {
                let id = pixel_layer(doc, *layer, *mask)?;
                ensure!(
                    !points.is_empty() && points.len() <= 100_000,
                    "笔刷路径需要 1–100000 个点"
                );
                for point in points {
                    finite(point, "points")?;
                }
                ensure!(
                    settings.diameter.is_finite() && (0.01..=5000.).contains(&settings.diameter),
                    "diameter 超出 0.01–5000"
                );
                unit(settings.hardness, "hardness")?;
                unit(settings.opacity, "opacity")?;
                unit(settings.smoothing, "smoothing")?;
                let mut sample = None;
                let mode = match settings.brush {
                    StrokeMode::Paint {} => BrushMode::Paint,
                    StrokeMode::Erase {} => BrushMode::Erase,
                    StrokeMode::Smudge {} => BrushMode::Smudge,
                    StrokeMode::Liquify {} => BrushMode::Liquify,
                    StrokeMode::Blur { radius } => {
                        ensure!(
                            radius.is_finite() && (0.1..=250.).contains(&radius),
                            "模糊半径超出 0.1–250"
                        );
                        BrushMode::Blur { radius }
                    }
                    StrokeMode::Clone {
                        offset,
                        sample_merged,
                    } => {
                        finite(&offset, "offset")?;
                        if sample_merged {
                            sample = Some(Arc::new(render::composite(doc)));
                        }
                        BrushMode::Clone { offset }
                    }
                    StrokeMode::Heal { method } => BrushMode::Heal(match method {
                        HealMethod::ContentAware => HealingMode::ContentAware,
                        HealMethod::CreateTexture => HealingMode::CreateTexture,
                        HealMethod::ProximityMatch => HealingMode::ProximityMatch,
                    }),
                };
                let painted = brush::paint(
                    doc,
                    id,
                    *mask,
                    points,
                    BrushSettings {
                        diameter: settings.diameter,
                        hardness: settings.hardness,
                        opacity: settings.opacity,
                        color: settings.color,
                        smoothing: settings.smoothing,
                        mode,
                    },
                    sample,
                )?;
                json!({"layer":id,"changed_pixels":painted.changed_pixels,"bounds":painted.bounds})
            }
            Operation::Gradient {
                layer,
                start,
                end,
                from,
                to,
                shape,
                opacity,
                mask,
            } => {
                let id = pixel_layer(doc, *layer, *mask)?;
                unit(*opacity, "opacity")?;
                finite(start, "start")?;
                finite(end, "end")?;
                ensure!(
                    (end[0] - start[0]).hypot(end[1] - start[1]) >= 0.5,
                    "渐变端点至少相距 0.5 像素"
                );
                let shape = match shape {
                    GradientKind::Linear => GradientShape::Linear,
                    GradientKind::Radial => GradientShape::Radial,
                    GradientKind::Angle => GradientShape::Angle,
                    GradientKind::Reflected => GradientShape::Reflected,
                    GradientKind::Diamond => GradientShape::Diamond,
                };
                brush::gradient(doc, id, *mask, *start, *end, *from, *to, shape, *opacity)?;
                json!({"layer":id})
            }
            Operation::Select { selection } => self.select(doc, selection)?,
            Operation::Mask { layer, action } => self.mask(doc, *layer, action)?,
            Operation::SubjectMask {
                layer,
                replace_existing,
                settings,
            } => {
                let id = pixel_layer(doc, *layer, false)?;
                let image = doc
                    .layer(id)
                    .unwrap()
                    .image
                    .clone()
                    .context("图层没有源像素")?;
                let settings = matte_settings(settings)?;
                let segmentation = self.segmentation()?.predict(&image)?;
                let alpha = matte::refine(&segmentation.alpha, &image, &settings)?;
                apply_subject_alpha(doc, id, &alpha, *replace_existing)?;
                json!({"layer":id,"model":"U2Netp","inference_millis":segmentation.inference_millis})
            }
            Operation::Filter {
                layer,
                kind,
                settings,
                mask,
            } => {
                let id = pixel_layer(doc, *layer, *mask)?;
                let mut settings = filter_settings(kind, settings)?;
                if kind == "Camera Raw Filter" && settings["whiteBalance"].as_str() == Some("Auto")
                {
                    let layer = doc.layer(id).unwrap();
                    let source = if *mask {
                        layer.mask.as_ref().map(|mask| &mask.raster)
                    } else {
                        layer.image.as_ref()
                    }
                    .context("自动白平衡需要源像素")?;
                    let [temperature, tint] = crate::filter_panel::auto_white_balance(source)
                        .context("源图像不足以估计自动白平衡，请使用 Custom")?;
                    settings["temperature"] = json!(temperature.clamp(-100., 100.));
                    settings["tint"] = json!(tint.clamp(-100., 100.));
                }
                filter_edit::apply(doc, id, *mask, kind, &settings)?;
                json!({"layer":id,"kind":kind})
            }
            Operation::Adjustment {
                layer,
                kind,
                settings,
                create,
            } => {
                let settings = project::canonical_adjustment(&filter_settings(kind, settings)?)?;
                let id = if *create {
                    if let Some(layer) = layer {
                        doc.active_layer_id = Some(layer_id(doc, Some(*layer))?);
                    }
                    edit::add_layer(doc, kind)
                } else {
                    let id = layer_id(doc, *layer)?;
                    ensure!(
                        doc.layer(id).unwrap().adjustment.is_some(),
                        "更新调整需要选择已有调整图层"
                    );
                    id
                };
                doc.layer_mut(id).unwrap().adjustment = Some(settings);
                json!({"layer":id,"kind":kind})
            }
            Operation::Effects { layer, effects } => {
                let id = layer_id(doc, *layer)?;
                let effects = effects.as_ref().map(validate_effects).transpose()?;
                doc.layer_mut(id).unwrap().effects = effects;
                json!({"layer":id})
            }
            Operation::AddShape {
                kind,
                rect,
                color,
                corner_radius,
                smoothing,
                line_width,
                line_ends,
            } => {
                validate_rect(*rect)?;
                validate_shape_arguments(*kind, *rect, *corner_radius, *line_width, *line_ends)?;
                let kind = match kind {
                    Shape::Rectangle => ShapeKind::Rectangle,
                    Shape::Ellipse => ShapeKind::Ellipse,
                    Shape::Line => ShapeKind::Line,
                };
                json!({"layer":edit::add_shape_with_smoothing(doc,kind,*rect,*color,*corner_radius,*line_width,*line_ends,*smoothing)?})
            }
            Operation::UpdateShape { layer, style } => {
                let id = layer_id(doc, *layer)?;
                let target = doc.layer(id).unwrap();
                let old = target
                    .shape
                    .as_ref()
                    .context("更新形状需要选择已有形状图层")?;
                let merged = shape_style(old, style)?;
                let raster = edit::shape_raster(
                    &merged,
                    target.transform.size[0].round() as u32,
                    target.transform.size[1].round() as u32,
                )?;
                let target = doc.layer_mut(id).unwrap();
                target.shape = Some(merged);
                target.image = Some(Arc::new(raster));
                json!({"layer":id})
            }
            Operation::AddText { style, origin } => {
                finite(origin, "origin")?;
                json!({"layer":text::add_text(doc,text_style(None,style)?,*origin)?})
            }
            Operation::UpdateText { layer, style } => {
                let id = layer_id(doc, *layer)?;
                let old = doc
                    .layer(id)
                    .unwrap()
                    .text
                    .as_ref()
                    .context("更新文字需要选择已有文字图层")?;
                let style = text_style(Some(old), style)?;
                text::update_text(doc, id, style)?;
                json!({"layer":id})
            }
            Operation::ImportLayers { paths, point } => {
                ensure!(
                    !paths.is_empty() && paths.len() <= 1000,
                    "paths 需要 1–1000 个文件"
                );
                if let Some(point) = point {
                    finite(point, "point")?;
                }
                let mut ids = Vec::new();
                let mut conversions = Vec::new();
                for path in paths {
                    let imported = image_io::import(path)
                        .with_context(|| format!("导入 {}", path.display()))?;
                    project::validate_document(&imported.document)?;
                    crate::workspace_limits::ensure_workspace_budget(
                        retained_documents
                            .iter()
                            .copied()
                            .chain([&*doc, &imported.document]),
                        retained_history_bytes,
                    )?;
                    let layers: Vec<_> = imported.document.layers.iter().map(|l| l.id).collect();
                    ids.extend(edit::copy_layers_to(
                        &imported.document,
                        doc,
                        &layers,
                        *point,
                    )?);
                    project::validate_document(doc)?;
                    ensure_edit_budget(doc, retained_documents, retained_history_bytes)?;
                    conversions.extend(imported.conversions);
                }
                json!({"layers":ids,"conversions":conversions})
            }
            Operation::MergeLayers { layers } => {
                validate_ids(doc, layers)?;
                json!({"layer":edit::merge_layers(doc,layers)?})
            }
            Operation::Flatten { name } => {
                if let Some(name) = name {
                    validate_name(name)?;
                }
                let raster = render::composite(doc);
                let layer = Layer::raster(name.as_deref().unwrap_or("合并图像"), raster);
                let id = layer.id;
                doc.layers = vec![layer];
                doc.active_layer_id = Some(id);
                json!({"layer":id})
            }
        };
        project::validate_document(doc)?;
        ensure_edit_budget(doc, retained_documents, retained_history_bytes)?;
        Ok(result)
    }

    fn select(&mut self, doc: &mut Document, action: &SelectionAction) -> Result<Value> {
        let (incoming, mode) = match action {
            SelectionAction::Rectangle {
                rect,
                mode,
                antialiased,
            }
            | SelectionAction::Ellipse {
                rect,
                mode,
                antialiased,
            } => {
                validate_rect(*rect)?;
                (
                    selection::marquee(
                        doc.width,
                        doc.height,
                        *rect,
                        matches!(action, SelectionAction::Ellipse { .. }),
                        *antialiased,
                    ),
                    *mode,
                )
            }
            SelectionAction::Polygon {
                points,
                mode,
                antialiased,
            } => {
                ensure!(
                    (3..=100_000).contains(&points.len()),
                    "多边形需要 3–100000 个点"
                );
                for point in points {
                    finite(point, "points")?;
                }
                (
                    selection::polygon(doc.width, doc.height, points, *antialiased),
                    *mode,
                )
            }
            SelectionAction::Wand {
                point,
                layer,
                sample_merged,
                tolerance,
                sample_radius,
                contiguous,
                mode,
            } => {
                validate_point(doc, *point)?;
                ensure!(*sample_radius <= 2, "魔棒采样半径应为 0–2");
                let source = sample_canvas(doc, *layer, *sample_merged)?;
                (
                    selection::wand(
                        &source,
                        *point,
                        WandSettings {
                            tolerance: *tolerance,
                            sample_radius: *sample_radius,
                            contiguous: *contiguous,
                        },
                    ),
                    *mode,
                )
            }
            SelectionAction::ColorRange {
                colors,
                layer,
                sample_merged,
                fuzziness,
                mode,
            } => {
                ensure!(
                    !colors.is_empty() && colors.len() <= 1024,
                    "颜色范围需要 1–1024 个颜色"
                );
                let source = sample_canvas(doc, *layer, *sample_merged)?;
                (selection::color_range(&source, colors, *fuzziness), *mode)
            }
            SelectionAction::Alpha {
                layer,
                mask,
                invert,
                mode,
            } => {
                let id = layer_id(doc, *layer)?;
                let alpha = edit::layer_selection(doc, id, *mask)?;
                let alpha = if *invert {
                    selection::inverse(&alpha)
                        .unwrap_or_else(|| Selection::empty(doc.width, doc.height))
                } else {
                    alpha
                };
                (alpha, *mode)
            }
            SelectionAction::Subject { layer, point, mode } => {
                let id = pixel_layer(doc, *layer, false)?;
                let target = doc.layer(id).unwrap();
                let source = target.image.clone().context("主体选择需要源像素")?;
                let transform = target.transform;
                let predicted = self.segmentation()?.predict(&source)?;
                let native = if let Some(point) = point {
                    validate_point(doc, *point)?;
                    let uv = transform.from_document(*point);
                    ensure!(
                        uv.iter().all(|v| (0. ..1.).contains(v)),
                        "对象点不在该图层范围内"
                    );
                    predicted.object_at([
                        (uv[0] * source.width as f64) as u32,
                        (uv[1] * source.height as f64) as u32,
                    ])?
                } else {
                    predicted.selection()
                };
                (
                    selection_to_canvas(&native, transform, doc.width, doc.height),
                    *mode,
                )
            }
            SelectionAction::All {} => {
                doc.selection = Some(Selection::full(doc.width, doc.height));
                return Ok(selection_result(doc));
            }
            SelectionAction::None {} => {
                doc.selection = None;
                return Ok(selection_result(doc));
            }
            SelectionAction::Invert {} => {
                doc.selection = Some(doc.selection.as_ref().map_or_else(
                    || Selection::full(doc.width, doc.height),
                    |s| {
                        selection::inverse(s)
                            .unwrap_or_else(|| Selection::empty(doc.width, doc.height))
                    },
                ));
                return Ok(selection_result(doc));
            }
            SelectionAction::Feather { radius } => {
                ensure!(
                    radius.is_finite() && (0. ..=250.).contains(radius),
                    "羽化半径应为 0–250"
                );
                doc.selection = Some(selection::feather(
                    doc.selection.as_ref().context("当前没有选区")?,
                    *radius,
                ));
                return Ok(selection_result(doc));
            }
            SelectionAction::Expand { amount } | SelectionAction::Contract { amount } => {
                ensure!(*amount <= 500, "扩展/收缩量应为 0–500");
                let source = doc.selection.as_ref().context("当前没有选区")?;
                doc.selection = Some(if matches!(action, SelectionAction::Expand { .. }) {
                    selection::expand(source, *amount)
                } else {
                    selection::contract(source, *amount)
                });
                return Ok(selection_result(doc));
            }
            SelectionAction::Translate { delta } => {
                let source = doc.selection.as_ref().context("当前没有选区")?;
                doc.selection = Some(selection::translated(
                    source, delta[0], delta[1], doc.width, doc.height,
                ));
                return Ok(selection_result(doc));
            }
        };
        doc.selection = Some(selection::combine(
            doc.selection.as_ref(),
            &incoming,
            mode.into(),
        ));
        Ok(selection_result(doc))
    }

    fn mask(
        &mut self,
        doc: &mut Document,
        layer: Option<Uuid>,
        action: &MaskAction,
    ) -> Result<Value> {
        let id = layer_id(doc, layer)?;
        match action {
            MaskAction::Add {
                revealing,
                from_selection,
            } => {
                ensure!(
                    doc.layer(id).unwrap().mask.is_none(),
                    "图层已有蒙版，请先删除或替换"
                );
                if *from_selection {
                    let selected = doc
                        .selection
                        .as_ref()
                        .context("从选区建立蒙版需要当前选区")?;
                    let pixels = selected
                        .canvas_coverage()
                        .iter()
                        .flat_map(|&a| {
                            let a = if *revealing { a } else { 255 - a };
                            [a, a, a, 255]
                        })
                        .collect();
                    let placement =
                        Some(Transform::new(0., 0., doc.width as f64, doc.height as f64));
                    doc.layer_mut(id).unwrap().mask = Some(Mask {
                        raster: Arc::new(Raster::from_rgba(doc.width, doc.height, pixels)),
                        enabled: true,
                        linked: true,
                        placement,
                    });
                } else {
                    brush::add_mask(doc, id, *revealing)?;
                }
            }
            MaskAction::Apply {} => {
                ensure!(
                    doc.layer(id).unwrap().adjustment.is_none(),
                    "调整图层的蒙版请保留为非破坏蒙版"
                );
                edit::apply_mask(doc, id)?;
            }
            MaskAction::Delete {} => {
                ensure!(doc.layer(id).unwrap().mask.is_some(), "图层没有蒙版");
                doc.layer_mut(id).unwrap().mask = None;
            }
            MaskAction::Link { linked } => edit::set_mask_linked(doc, id, *linked)?,
            MaskAction::Enable { enabled } => {
                doc.layer_mut(id)
                    .unwrap()
                    .mask
                    .as_mut()
                    .context("图层没有蒙版")?
                    .enabled = *enabled;
            }
            MaskAction::ReplaceFromFile { path, invert } => {
                let source = image_io::decode_image(path)?;
                let mut pixels = source.to_rgba();
                for p in pixels.as_chunks_mut::<4>().0 {
                    let gray = ((p[0] as u32 * 54 + p[1] as u32 * 183 + p[2] as u32 * 19)
                        * p[3] as u32
                        / (256 * 255)) as u8;
                    let gray = if *invert { 255 - gray } else { gray };
                    *p = [gray, gray, gray, 255];
                }
                doc.layer_mut(id).unwrap().mask = Some(Mask {
                    raster: Arc::new(Raster::from_rgba(source.width, source.height, pixels)),
                    enabled: true,
                    linked: true,
                    placement: None,
                });
            }
            MaskAction::ClipTo { source } => {
                if let Some(source) = source {
                    layer_id(doc, Some(*source))?;
                    ensure!(*source != id, "图层不可剪贴到自身");
                    let target = doc.layer(id).unwrap();
                    let base = doc.layer(*source).unwrap();
                    ensure!(target.parent_id == base.parent_id, "剪贴源必须处于同一组");
                    let source_index = doc.layers.iter().position(|l| l.id == *source).unwrap();
                    let target_index = doc.layers.iter().position(|l| l.id == id).unwrap();
                    ensure!(source_index < target_index, "剪贴源必须位于目标图层下方");
                }
                doc.layer_mut(id).unwrap().mask_source_id = *source;
            }
        }
        Ok(json!({"layer":id}))
    }
}

fn ensure_edit_budget(
    doc: &Document,
    retained_documents: &[&Document],
    retained_history_bytes: u64,
) -> Result<()> {
    crate::workspace_limits::ensure_workspace_budget(
        retained_documents.iter().copied().chain([doc]),
        retained_history_bytes,
    )?;
    Ok(())
}

fn layer_id(doc: &Document, supplied: Option<Uuid>) -> Result<Uuid> {
    let id = supplied
        .or(doc.active_layer_id)
        .context("没有活动图层；请指定 layer")?;
    ensure!(doc.layer(id).is_some(), "图层 {id} 已不存在");
    Ok(id)
}
fn deserialize_transform<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Transform, D::Error> {
    let value = Value::deserialize(deserializer)?;
    let object = value
        .as_object()
        .ok_or_else(|| serde::de::Error::custom("transform 必须是对象"))?;
    for key in object.keys() {
        if !["origin", "size", "rotation", "flipX", "flipY", "sampling"].contains(&key.as_str()) {
            return Err(serde::de::Error::custom(format!(
                "未知变换参数 transform.{key}"
            )));
        }
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}
fn validate_ids(doc: &Document, ids: &[Uuid]) -> Result<()> {
    ensure!(!ids.is_empty(), "layers 至少需要一个图层 UUID");
    let mut seen = HashSet::new();
    for id in ids {
        layer_id(doc, Some(*id))?;
        ensure!(seen.insert(*id), "layers 中存在重复 UUID：{id}");
    }
    Ok(())
}
fn pixel_layer(doc: &Document, layer: Option<Uuid>, mask: bool) -> Result<Uuid> {
    let id = layer_id(doc, layer)?;
    let layer = doc.layer(id).unwrap();
    if mask {
        ensure!(layer.mask.is_some(), "图层没有蒙版");
    } else {
        ensure!(
            !layer.is_group && layer.adjustment.is_none(),
            "该操作需要像素、文字或形状图层"
        );
    }
    Ok(id)
}
fn finite(values: &[f64], field: &str) -> Result<()> {
    ensure!(
        values
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 1_000_000.),
        "{field} 包含无效坐标或超出 ±1000000"
    );
    Ok(())
}
fn unit(value: f64, field: &str) -> Result<()> {
    ensure!(
        value.is_finite() && (0. ..=1.).contains(&value),
        "{field} 必须在 0–1 之间"
    );
    Ok(())
}
fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty() && name.chars().count() <= 10_000,
        "名称应为 1–10000 个字符"
    );
    Ok(())
}
fn validate_transform(value: Transform) -> Result<()> {
    ensure!(
        value.is_valid() && value.size.iter().all(|v| (1. ..=300_000.).contains(v)),
        "transform 尺寸应为 1–300000 且所有值有限"
    );
    Ok(())
}
fn validate_rect(rect: [f64; 4]) -> Result<()> {
    finite(&rect, "rect")?;
    ensure!(rect[2] >= 1. && rect[3] >= 1., "rect 的宽高至少为 1");
    Ok(())
}
fn validate_point(doc: &Document, point: [f64; 2]) -> Result<()> {
    finite(&point, "point")?;
    ensure!(
        point[0] >= 0.
            && point[1] >= 0.
            && point[0] < doc.width as f64
            && point[1] < doc.height as f64,
        "point 位于画布外"
    );
    Ok(())
}
fn selection_result(doc: &Document) -> Value {
    json!({"selection":doc.selection.is_some(),"bounds":doc.selection.as_ref().and_then(Selection::bounds)})
}
fn sample_canvas(doc: &Document, layer: Option<Uuid>, merged: bool) -> Result<Raster> {
    if let Some(layer) = layer {
        layer_id(doc, Some(layer))?;
    }
    if merged {
        ensure!(layer.is_none(), "sample_merged=true 时不要同时指定 layer");
        return Ok(render::composite(doc));
    }
    let id = layer_id(doc, layer)?;
    let ids = edit::descendants(doc, id);
    let mut subset = doc.clone();
    subset.layers.retain(|l| ids.contains(&l.id));
    for layer in &mut subset.layers {
        if layer.parent_id.is_some_and(|id| !ids.contains(&id)) {
            layer.parent_id = None;
        }
        if layer.mask_source_id.is_some_and(|id| !ids.contains(&id)) {
            layer.mask_source_id = None;
        }
    }
    Ok(render::composite(&subset))
}
fn selection_to_canvas(
    native: &Selection,
    transform: Transform,
    width: u32,
    height: u32,
) -> Selection {
    let mut coverage = vec![0; width as usize * height as usize];
    for y in 0..height {
        for x in 0..width {
            let uv = transform.from_document([x as f64 + 0.5, y as f64 + 0.5]);
            if uv.iter().all(|v| (0. ..1.).contains(v)) {
                coverage[(y * width + x) as usize] = native.get(
                    (uv[0] * native.width as f64) as u32,
                    (uv[1] * native.height as f64) as u32,
                );
            }
        }
    }
    Selection::from_coverage(width, height, coverage)
}

fn apply_subject_alpha(doc: &mut Document, id: Uuid, alpha: &[u8], replace: bool) -> Result<()> {
    if !replace {
        return matte::apply_subject_mask(doc, id, alpha);
    }
    let layer = doc.layer(id).context("图层已不存在")?;
    let source = layer.image.as_ref().context("图层没有源像素")?;
    ensure!(
        alpha.len() == source.width as usize * source.height as usize,
        "主体蒙版尺寸与源图像不一致"
    );
    if doc.selection.as_ref().is_some_and(Selection::is_empty) {
        return Ok(());
    }
    let width = source.width;
    let height = source.height;
    let transform = layer.transform;
    let old = layer
        .mask
        .as_ref()
        .map(|mask| (mask, render::mask_background(&mask.raster)));
    let linked = old.is_none_or(|(mask, _)| mask.linked);
    let mut selected = false;
    let mut pixels = Vec::with_capacity(alpha.len() * 4);
    for y in 0..height {
        for x in 0..width {
            let point = transform.to_document([
                (x as f64 + 0.5) / width as f64,
                (y as f64 + 0.5) / height as f64,
            ]);
            let coverage = doc.selection.as_ref().map_or(1., |selection| {
                if point[0] >= 0.
                    && point[1] >= 0.
                    && point[0] < doc.width as f64
                    && point[1] < doc.height as f64
                {
                    selection.get(point[0].floor() as u32, point[1].floor() as u32) as f64 / 255.
                } else {
                    0.
                }
            });
            selected |= coverage > 0.;
            let base = old.map_or(1., |(mask, background)| {
                let uv = mask.placement.unwrap_or(transform).from_document(point);
                render::sample_mask(
                    &mask.raster,
                    uv[0] * mask.raster.width as f64 - 0.5,
                    uv[1] * mask.raster.height as f64 - 0.5,
                    background,
                )
            });
            let subject = alpha[(y * width + x) as usize] as f64 / 255.;
            let value = ((base * (1. - coverage) + subject * coverage) * 255.)
                .round()
                .clamp(0., 255.) as u8;
            pixels.extend_from_slice(&[value, value, value, 255]);
        }
    }
    if selected {
        doc.layer_mut(id).unwrap().mask = Some(Mask {
            raster: Arc::new(Raster::from_rgba(width, height, pixels)),
            enabled: true,
            linked,
            placement: Some(transform),
        });
    }
    Ok(())
}

/// Merge settings by their actual defaults; reject typos and wrong JSON types rather than dropping them.
fn merge_settings(base: &Value, patch: &Value, path: &str) -> Result<Value> {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            let mut result = base.clone();
            for (key, value) in patch {
                let template = base
                    .get(key)
                    .with_context(|| format!("未知参数 {path}.{key}"))?;
                result.insert(
                    key.clone(),
                    merge_settings(template, value, &format!("{path}.{key}"))?,
                );
            }
            Ok(Value::Object(result))
        }
        (Value::Array(base), Value::Array(patch)) => {
            if path.ends_with(".ranges") || path.ends_with(".channels") {
                ensure!(patch.len() == 4, "{path} 需要四个通道");
            }
            if [
                "settings.mixer.hue",
                "settings.mixer.saturation",
                "settings.mixer.luminance",
            ]
            .contains(&path)
            {
                ensure!(patch.len() == 8, "{path} 需要八个色域");
            }
            if let Some(template) = base.first() {
                Ok(Value::Array(
                    patch
                        .iter()
                        .map(|value| merge_settings(template, value, path))
                        .collect::<Result<_>>()?,
                ))
            } else if let Some(template) = array_item_schema(path) {
                let max = if path == "settings.mixer.points" {
                    8
                } else {
                    2
                };
                ensure!(patch.len() <= max, "{path} 超出 {max} 项");
                Ok(Value::Array(
                    patch
                        .iter()
                        .map(|value| merge_settings(&template, value, path))
                        .collect::<Result<_>>()?,
                ))
            } else {
                ensure!(patch.is_empty(), "{path} 的当前实现只接受空数组");
                Ok(json!([]))
            }
        }
        (Value::Number(_), Value::Number(value)) => {
            ensure!(
                value.as_f64().is_some_and(f64::is_finite),
                "{path} 数值无效"
            );
            Ok(patch.clone())
        }
        (Value::Bool(_), Value::Bool(_)) | (Value::String(_), Value::String(_)) => {
            Ok(patch.clone())
        }
        _ => bail!("{path} 的 JSON 类型无效"),
    }
}
fn filter_settings(kind: &str, settings: &Value) -> Result<Value> {
    ensure!(filters::FILTERS.contains(&kind), "未知滤镜：{kind}");
    ensure!(settings.is_object(), "settings 必须是对象");
    if let Some(value) = settings.get("kind") {
        ensure!(value.as_str() == Some(kind), "settings.kind 与 kind 不一致");
    }
    let mut result = merge_settings(&filters::default_settings(kind), settings, "settings")?;
    if kind == "Hue/Saturation" {
        for key in ["hue", "saturation", "lightness"] {
            if let Some(value) = settings.get(key) {
                ensure!(
                    settings
                        .pointer(&format!("/hsvSettings/adjustments/Master/{key}"))
                        .is_none(),
                    "顶层 {key} 与 hsvSettings.adjustments.Master.{key} 请只指定一处"
                );
                result["hsvSettings"]["adjustments"]["Master"][key] = value.clone();
            }
        }
        if let Some(value) = settings.get("colorize") {
            ensure!(
                settings.pointer("/hsvSettings/colorize").is_none(),
                "colorize 请只指定一处"
            );
            result["hsvSettings"]["colorize"] = value.clone();
        }
    }
    if [
        "Hue/Saturation",
        "Levels",
        "Curves",
        "Exposure",
        "Gradient Map",
        "Grain",
        "Invert",
        "Black & White",
        "Color Balance",
    ]
    .contains(&kind)
    {
        project::canonical_adjustment(&result)?;
    }
    validate_filter_choices(&result)?;
    validate_parameter_ranges(&result)?;
    Ok(result)
}
fn array_item_schema(path: &str) -> Option<Value> {
    match path {
        "settings.mixer.points" => Some(
            json!({"hue":0,"saturation":0,"luminance":0,"hueShift":0,"saturationShift":0,"luminanceShift":0,"hueRange":30,"saturationRange":0.4,"luminanceRange":0.4,"visualize":false}),
        ),
        "settings.geometry.guides" => Some(json!({"startX":0,"startY":0,"endX":1,"endY":1})),
        _ => None,
    }
}
fn validate_filter_choices(value: &Value) -> Result<()> {
    for (pointer, allowed) in [
        ("/levels/channel", &["RGB", "Red", "Green", "Blue"][..]),
        ("/curves/channel", &["RGB", "Red", "Green", "Blue"][..]),
        ("/whiteBalance", &["Custom", "Auto"][..]),
        ("/glowStyle", &["Diffusion", "Bloom", "Halation"][..]),
        (
            "/vignetteStyle",
            &["Highlight Priority", "Color Priority", "Paint Overlay"][..],
        ),
        ("/geometry/upright", &["Off", "Guided"][..]),
        ("/geometry/projection", &["Perspective", "Rectilinear"][..]),
        (
            "/calibration/process",
            &[
                "Version 1",
                "Version 2",
                "Version 3",
                "Version 4",
                "Version 5",
                "Version 6",
            ][..],
        ),
        (
            "/dither/style",
            &[
                "Atkinson (Classic Mac)",
                "Floyd–Steinberg",
                "Bayer 2 × 2",
                "Bayer 4 × 4",
                "Bayer 8 × 8",
                "Halftone Dots",
                "Halftone Lines",
                "Halftone Diamonds",
                "Mac Patterns",
                "ASCII",
                "Scanlines (CRT)",
            ][..],
        ),
        ("/dither/pixelShape", &["Square", "Dot"][..]),
        (
            "/dither/colors",
            &["Black & White", "Two Colors", "Original"][..],
        ),
    ] {
        if let Some(field) = value.pointer(pointer) {
            ensure!(
                field.as_str().is_some_and(|v| allowed.contains(&v)),
                "参数 {pointer} 值无效；可选 {allowed:?}"
            );
        }
    }
    if let Some(points) = value.pointer("/mixer/points").and_then(Value::as_array) {
        for point in points {
            for key in [
                "saturation",
                "luminance",
                "saturationRange",
                "luminanceRange",
            ] {
                unit(point[key].as_f64().context("点颜色范围无效")?, key)?;
            }
        }
    }
    if let Some(guides) = value.pointer("/geometry/guides").and_then(Value::as_array) {
        for guide in guides {
            for key in ["startX", "startY", "endX", "endY"] {
                unit(guide[key].as_f64().context("引导线坐标无效")?, key)?;
            }
        }
    }
    Ok(())
}
fn validate_parameter_ranges(value: &Value) -> Result<()> {
    if let Some(object) = value.as_object() {
        for (key, value) in object {
            let range = match key.as_str() {
                "red" | "green" | "blue" | "opacity" => Some((0., 1.)),
                "blurRadius" => Some((0., 250.)),
                "motionDistance" => Some((0., 2000.)),
                "motionAngle" => Some((-360., 360.)),
                "bloomRadius" => Some((0., 150.)),
                "bloomAmount" => Some((0., 200.)),
                "noiseAmount" => Some((0., 400.)),
                "vignetteAmount" => Some((-100., 100.)),
                "vignetteMidpoint" | "vignetteFeather" | "vignetteHighlights" => Some((0., 100.)),
                "hue" => Some((-360., 360.)),
                "saturation" | "lightness" => Some((-100., 100.)),
                "gamma" => Some((0.01, 9.99)),
                "previewScale" => Some((0.001, 1.)),
                _ => None,
            };
            if let Some((lo, hi)) = range
                && let Some(number) = value.as_f64()
            {
                ensure!((lo..=hi).contains(&number), "{key} 超出 {lo}–{hi}");
            }
            validate_parameter_ranges(value)?;
        }
    } else if let Some(array) = value.as_array() {
        for value in array {
            validate_parameter_ranges(value)?;
        }
    }
    Ok(())
}
fn matte_settings(settings: &Value) -> Result<Value> {
    let value = merge_settings(
        &json!({"backgroundQuality":"Advanced","refineEdges":12.,"shiftEdge":0.,"matteContrast":25.}),
        settings,
        "settings",
    )?;
    ensure!(
        ["Basic", "Advanced"].contains(&value["backgroundQuality"].as_str().unwrap_or("")),
        "backgroundQuality 应为 Basic 或 Advanced"
    );
    for (key, lo, hi) in [
        ("refineEdges", 0., 40.),
        ("shiftEdge", -10., 10.),
        ("matteContrast", 0., 100.),
    ] {
        ensure!(
            value[key].as_f64().is_some_and(|v| (lo..=hi).contains(&v)),
            "{key} 超出 {lo}–{hi}"
        );
    }
    Ok(value)
}
fn effect_defaults(kind: &str) -> Result<Value> {
    Ok(match kind {
        "stroke" => {
            json!({"enabled":true,"size":4,"red":0,"green":0,"blue":0,"opacity":1,"inside":false})
        }
        "shadow" | "innerShadow" => {
            json!({"enabled":true,"angle":120,"distance":12,"blur":8,"red":0,"green":0,"blue":0,"opacity":0.5})
        }
        "colorOverlay" => json!({"enabled":true,"red":1,"green":0,"blue":0,"opacity":0.5}),
        "innerGlow" | "outerGlow" => {
            json!({"enabled":true,"size":12,"red":1,"green":1,"blue":1,"opacity":0.7})
        }
        _ => bail!("未知图层效果：{kind}"),
    })
}
fn validate_effects(effects: &Value) -> Result<Value> {
    let object = effects.as_object().context("effects 必须是对象或 null")?;
    let mut result = serde_json::Map::new();
    for (key, value) in object {
        let value = merge_settings(&effect_defaults(key)?, value, &format!("effects.{key}"))?;
        validate_parameter_ranges(&value)?;
        for (field, lo, hi) in [
            ("size", 0., 500.),
            ("blur", 0., 500.),
            ("distance", 0., 5000.),
            ("angle", -360., 360.),
        ] {
            if let Some(value) = value.get(field) {
                ensure!(
                    value.as_f64().is_some_and(|v| (lo..=hi).contains(&v)),
                    "effects.{key}.{field} 超出 {lo}–{hi}"
                );
            }
        }
        result.insert(key.clone(), value);
    }
    Ok(Value::Object(result))
}
fn validate_shape_arguments(
    kind: Shape,
    rect: [f64; 4],
    radius: f64,
    width: f64,
    ends: Option<[[f64; 2]; 2]>,
) -> Result<()> {
    ensure!(
        Document::valid_size(rect[2].round() as u32, rect[3].round() as u32),
        "形状尺寸超出上限"
    );
    ensure!(
        radius.is_finite() && radius >= 0. && radius <= rect[2].min(rect[3]) / 2.,
        "corner_radius 超出形状范围"
    );
    ensure!(
        width.is_finite() && (0.1..=5000.).contains(&width),
        "line_width 应为 0.1–5000"
    );
    if let Some(ends) = ends {
        ensure!(matches!(kind, Shape::Line), "line_ends 只适用于 line");
        for value in ends.iter().flatten() {
            unit(*value, "line_ends")?;
        }
    }
    Ok(())
}
fn shape_style(old: &Value, patch: &Value) -> Result<Value> {
    let kind = old["kind"].as_str().context("原形状类型无效")?;
    let template = if kind == "Line" {
        json!({"kind":"Line","red":0,"green":0,"blue":0,"cornerRadius":0,"lineWidth":1,"start":[0,0],"end":[1,1]})
    } else if kind == "Rectangle" {
        json!({"kind":kind,"red":0,"green":0,"blue":0,"cornerRadius":0,"smoothing":false})
    } else {
        json!({"kind":kind,"red":0,"green":0,"blue":0,"cornerRadius":0})
    };
    let base = merge_settings(&template, old, "style")?;
    let mut result = merge_settings(&base, patch, "style")?;
    ensure!(
        result["kind"].as_str() == Some(kind),
        "update_shape 保持已有形状类型；新类型请使用 add_shape"
    );
    validate_parameter_ranges(&result)?;
    ensure!(
        result["cornerRadius"]
            .as_f64()
            .is_some_and(|v| (0. ..=300_000.).contains(&v)),
        "cornerRadius 超出上限"
    );
    if kind == "Line" {
        ensure!(
            result["lineWidth"]
                .as_f64()
                .is_some_and(|v| (0.1..=5000.).contains(&v)),
            "lineWidth 超出上限"
        );
        for key in ["start", "end"] {
            let points = result[key].as_array().context("端点应为数组")?;
            ensure!(points.len() == 2, "端点应含两个坐标");
            for value in points {
                unit(value.as_f64().context("端点应为数值")?, key)?;
            }
        }
    }
    // Match the original optional field: absent/false means ordinary corners.
    if result["smoothing"] == false {
        result.as_object_mut().unwrap().remove("smoothing");
    }
    Ok(result)
}
fn text_style(old: Option<&Value>, patch: &Value) -> Result<TextStyle> {
    let object = patch.as_object().context("style 必须是对象")?;
    let mut base = old
        .cloned()
        .unwrap_or_else(|| TextStyle::default().to_value());
    for (key, value) in object {
        ensure!(
            [
                "content",
                "fontName",
                "fontSize",
                "red",
                "green",
                "blue",
                "alignment",
                "tracking",
                "leading",
                "boxSize",
                "colorRuns",
                "fontRuns"
            ]
            .contains(&key.as_str()),
            "未知文字参数 style.{key}"
        );
        if ["colorRuns", "fontRuns"].contains(&key.as_str()) && !value.is_null() {
            let runs = value.as_array().context("字符样式应为数组")?;
            let allowed: &[&str] = if key == "colorRuns" {
                &["location", "length", "red", "green", "blue"]
            } else {
                &["location", "length", "fontName"]
            };
            for run in runs {
                for key in run.as_object().context("字符样式应为对象")?.keys() {
                    ensure!(allowed.contains(&key.as_str()), "未知字符样式参数 {key}");
                }
            }
        }
        if key == "boxSize" && !value.is_null() {
            let size = value
                .as_array()
                .context("boxSize 应为 [width,height] 或 null")?;
            ensure!(
                size.len() == 2 && size.iter().all(|v| v.as_f64().is_some()),
                "boxSize 应包含两个数值"
            );
        }
        base.as_object_mut()
            .context("原文字样式无效")?
            .insert(key.clone(), value.clone());
    }
    TextStyle::from_value(&base)
}

fn descriptor(example: Value, description: &str, required: &[&str]) -> Value {
    let operation: Operation = serde_json::from_value(example.clone())
        .expect("Built-in operation example must deserialize");
    let expanded = serde_json::to_value(operation).expect("Operation is serializable");
    let mut fields = serde_json::Map::new();
    for (key, value) in expanded.as_object().unwrap() {
        if key == "op" {
            continue;
        }
        fields.insert(
            key.clone(),
            json!({"required":required.contains(&key.as_str()),"example_or_default":value}),
        );
    }
    json!({"op":expanded["op"],"description":description,"fields":fields,"example":example,"expanded_example":expanded})
}

/// Every example is decoded by the same enum as execution, so discovery cannot drift to invented commands.
pub fn capabilities() -> Value {
    let id = "00000000-0000-0000-0000-000000000001";
    let transform = json!({"origin":[100,80],"size":[640,480],"rotation":0,"flipX":false,"flipY":false,"sampling":"High quality"});
    let operations = vec![
        descriptor(
            json!({"op":"add_layer","name":"背景"}),
            "在活动图层上方建立透明像素图层；color 可选 RGBA8 整层初始颜色。",
            &["name"],
        ),
        descriptor(
            json!({"op":"set_active_layer","layer":id}),
            "设置活动图层；后续省略 layer 的操作使用该图层。",
            &[],
        ),
        descriptor(
            json!({"op":"delete_layers","layers":[id]}),
            "删除指定图层及其组后代。",
            &["layers"],
        ),
        descriptor(
            json!({"op":"duplicate_layers","layers":[id]}),
            "复制图层、组后代和内部蒙版引用，返回全部新 UUID。",
            &["layers"],
        ),
        descriptor(
            json!({"op":"group_layers","layers":[id],"name":"组"}),
            "将指定图层放入新组，返回组 UUID。",
            &["layers", "name"],
        ),
        descriptor(json!({"op":"ungroup"}), "解散组并保留其内容。", &[]),
        descriptor(
            json!({"op":"place_layer"}),
            "重排图层或组；parent=null 为根级，above=null 放到该父级最上方；above 必须与 parent 同级。",
            &[],
        ),
        descriptor(
            json!({"op":"set_layer","opacity":0.75}),
            "更新名称、visible、0–1 opacity 或 blend_mode；至少指定一个属性。",
            &[],
        ),
        descriptor(
            json!({"op":"move_layers","layers":[id],"delta":[20,-10]}),
            "按文档像素位移图层及组后代；按链接状态移动蒙版。",
            &["layers", "delta"],
        ),
        descriptor(
            json!({"op":"set_transform","transform":transform}),
            "设置单图层或独立蒙版的完整变换；rotation 单位为度，正数沿屏幕顺时针。",
            &["transform"],
        ),
        descriptor(
            json!({"op":"transform_layers","layers":[id],"transform":transform}),
            "以所选图层当前包围框为基准缩放/旋转/移动图层集合及组后代。",
            &["layers", "transform"],
        ),
        descriptor(
            json!({"op":"flip_layers","layers":[id],"horizontal":true}),
            "围绕所选图层包围框翻转图层及蒙版；false 为垂直。",
            &["layers", "horizontal"],
        ),
        descriptor(
            json!({"op":"flip_canvas","horizontal":true}),
            "翻转整个文档的图层、蒙版、选区和参考线。",
            &["horizontal"],
        ),
        descriptor(
            json!({"op":"rotate_canvas","clockwise":true}),
            "将画布顺时针旋转90°；false为逆时针，宽高互换，所有图层、蒙版、参考线和选区随画布转动，原始图像不重采样。",
            &["clockwise"],
        ),
        descriptor(
            json!({"op":"convert_depth","depth":"16-bit"}),
            "转换当前像素图层为8-bit、16-bit或HDR；8-bit转换会量化低位和裁切超白颜色，可撤销。",
            &["depth"],
        ),
        descriptor(
            json!({"op":"resize_canvas","width":1920,"height":1080}),
            "调整画布边界；anchor 为旧内容在新画布中 0–1 的保留锚点。",
            &["width", "height"],
        ),
        descriptor(
            json!({"op":"resize_image","width":1280,"height":720}),
            "缩放图像、图层、蒙版、选区与参考线，重采样原始像素。",
            &["width", "height"],
        ),
        descriptor(
            json!({"op":"crop","rect":[20,30,800,600]}),
            "以 [x,y,width,height] 裁剪画布，保留图层画布外像素。",
            &["rect"],
        ),
        descriptor(
            json!({"op":"trim"}),
            "按透明度、左上色或右下色修边；sides 顺序为左、上、右、下。",
            &[],
        ),
        descriptor(
            json!({"op":"add_guide","horizontal":true,"position":100}),
            "增加参考线；horizontal=true 的 position 为 y，否则为 x。",
            &["horizontal", "position"],
        ),
        descriptor(
            json!({"op":"delete_guide","guide":id}),
            "通过 UUID 删除参考线；不存在则报错。",
            &["guide"],
        ),
        descriptor(json!({"op":"clear_guides"}), "移除全部参考线。", &[]),
        descriptor(
            json!({"op":"fill","color":[255,255,255,255]}),
            "按当前选区填充像素或蒙版；无选区时填充整层。",
            &["color"],
        ),
        descriptor(json!({"op":"clear"}), "按选区擦除像素或蒙版。", &[]),
        descriptor(
            json!({"op":"invert_pixels"}),
            "反相像素 RGB 或蒙版灰度，尊重当前选区。",
            &[],
        ),
        descriptor(
            json!({"op":"content_aware_fill"}),
            "使用现有离线内容识别算法修补所选像素，要求选区。",
            &[],
        ),
        descriptor(
            json!({"op":"brush","points":[[10,20],[60,45]]}),
            "一次完整笔划，复用 GUI 稀疏像素编辑；详见 brush_modes 与 brush_defaults。",
            &["points"],
        ),
        descriptor(
            json!({"op":"gradient","start":[0,0],"end":[500,0],"from":[0,0,0,255],"to":[255,255,255,255]}),
            "线性、径向、角度、反射或菱形渐变；尊重当前选区与 0–1 opacity。",
            &["start", "end", "from", "to"],
        ),
        descriptor(
            json!({"op":"select","selection":{"action":"rectangle","rect":[20,30,200,150]}}),
            "创建、组合或修改选区；详见 selection_actions。",
            &["selection"],
        ),
        descriptor(
            json!({"op":"mask","action":{"action":"add"}}),
            "建立、应用、删除、链接、启用、文件替换或剪贴蒙版；详见 mask_actions。",
            &["action"],
        ),
        descriptor(
            json!({"op":"subject_mask"}),
            "内嵌 U²-Netp 离线真实主体分割和边缘精修；默认替换旧蒙版，replace_existing=false 相乘；当前选区限制修改区域。",
            &[],
        ),
        descriptor(
            json!({"op":"filter","kind":"Gaussian Blur","settings":{"blurRadius":4}}),
            "对原生像素网格应用破坏性滤镜；参数严格按 filter_defaults 的同名模板校验，省略项取默认。",
            &["kind"],
        ),
        descriptor(
            json!({"op":"adjustment","kind":"Exposure","settings":{"exposureSettings":{"exposure":1}}}),
            "默认新建非破坏调整层；create=false 更新已有调整层；支持的种类见 adjustment_kinds。",
            &["kind"],
        ),
        descriptor(
            json!({"op":"effects","effects":{"shadow":{"opacity":0.5}}}),
            "替换该层的非破坏图层效果集合；effects=null 清除；参数见 effect_defaults。",
            &[],
        ),
        descriptor(
            json!({"op":"add_shape","kind":"rectangle","rect":[10,20,200,100],"color":[40,130,255,255]}),
            "增加可编辑矩形、椭圆或线条；smoothing=true 为矩形启用平滑圆角，line_ends 为 0–1 相对端点，RGBA alpha 转为图层透明度。",
            &["kind", "rect", "color"],
        ),
        descriptor(
            json!({"op":"update_shape","style":{"red":1,"green":0,"blue":0}}),
            "更新已有形状样式，保留未指定字段；矩形支持布尔 smoothing 平滑圆角，kind 不变，样式使用原项目 camelCase 字段。",
            &["style"],
        ),
        descriptor(
            json!({"op":"add_text","style":{"content":"你好","fontSize":48,"red":1},"origin":[20,30]}),
            "增加可编辑文字，样式使用 text_defaults 的 camelCase 字段；支持 UTF-16 字符颜色和字体区段。",
            &["style", "origin"],
        ),
        descriptor(
            json!({"op":"update_text","style":{"content":"新的文字"}}),
            "更新已有文字图层；保留未指定样式，重新排版并保持文字锚点。",
            &["style"],
        ),
        descriptor(
            json!({"op":"import_layers","paths":["D:/Photos/input.png"]}),
            "将图片、PSD 或 .comp 图层导入当前文档；point 可选，将每份导入内容包围框中心放到此处；返回新 UUID 与兼容性转换说明。",
            &["paths"],
        ),
        descriptor(
            json!({"op":"merge_layers","layers":[id]}),
            "合并所选图层；仅一个普通层时合并同组下方普通图层，仅一个组时合并组内容。",
            &["layers"],
        ),
        descriptor(
            json!({"op":"flatten"}),
            "将当前可见合成渲染为一个像素层；隐藏图层也从文档移除，透明背景保持透明。",
            &[],
        ),
    ];
    let selection_examples = vec![
        json!({"action":"rectangle","rect":[20,30,200,150]}),
        json!({"action":"ellipse","rect":[20,30,200,150],"mode":"add"}),
        json!({"action":"polygon","points":[[10,10],[100,10],[80,80]]}),
        json!({"action":"wand","point":[10,10],"tolerance":32}),
        json!({"action":"color_range","colors":[[255,255,255,255]],"fuzziness":40}),
        json!({"action":"alpha"}),
        json!({"action":"subject"}),
        json!({"action":"all"}),
        json!({"action":"none"}),
        json!({"action":"invert"}),
        json!({"action":"feather","radius":8}),
        json!({"action":"expand","amount":3}),
        json!({"action":"contract","amount":3}),
        json!({"action":"translate","delta":[10,-5]}),
    ];
    let selection_actions: Vec<Value> = selection_examples
        .into_iter()
        .map(|value| {
            let parsed: SelectionAction =
                serde_json::from_value(value).expect("Built-in selection example must deserialize");
            serde_json::to_value(parsed).unwrap()
        })
        .collect();
    let mask_examples = vec![
        json!({"action":"add"}),
        json!({"action":"apply"}),
        json!({"action":"delete"}),
        json!({"action":"link","linked":false}),
        json!({"action":"enable","enabled":true}),
        json!({"action":"replace_from_file","path":"D:/Photos/mask.png"}),
        json!({"action":"clip_to","source":id}),
    ];
    let mask_actions: Vec<Value> = mask_examples
        .into_iter()
        .map(|value| {
            let parsed: MaskAction =
                serde_json::from_value(value).expect("Built-in mask example must deserialize");
            serde_json::to_value(parsed).unwrap()
        })
        .collect();
    let filter_defaults: serde_json::Map<String, Value> = filters::FILTERS
        .iter()
        .map(|kind| ((*kind).into(), filters::default_settings(kind)))
        .collect();
    let effect_defaults: serde_json::Map<String, Value> = [
        "stroke",
        "shadow",
        "colorOverlay",
        "innerShadow",
        "outerGlow",
        "innerGlow",
    ]
    .into_iter()
    .map(|kind| (kind.into(), effect_defaults(kind).unwrap()))
    .collect();
    json!({
        "operations":operations,
        "coordinates":{"origin":"top_left","x":"right","y":"down","units":"document_pixels","rect":"[x,y,width,height]","rotation":"degrees_clockwise","raster":"straight_alpha_sRGB_RGBA8_top_to_bottom"},
        "layer_references":"layer omitted/null means active; explicit missing UUID is an error; layers is a nonempty unique UUID array; parent/above/source null have operation-specific meanings",
        "layer_order":"layers are stored bottom-to-top; group descendants are included by multi-layer transforms, grouping, deletion and duplication",
        "selection_actions":selection_actions,
        "selection_notes":{"modes":["replace","add","subtract","intersect"],"all":"full canvas selection","none":"no selection; pixel edits affect whole layer","invert":"inverts within canvas; no selection becomes full","feather_radius":[0,250],"morphology_amount":[0,500],"subject":"offline model on source image, transformed into document selection; point selects connected subject under document point","wand":"point is in document pixels; sample_merged=false samples active/explicit layer rendered to canvas; true samples composite and forbids explicit layer"},
        "mask_actions":mask_actions,
        "mask_notes":{"add":"existing mask is an error; from_selection maps current selection to document-space mask; revealing=false inverts coverage","replace_from_file":"scaled to layer transform; luminance times alpha; white reveals, black hides","subject_mask":"segmentation uses source raster; original pixels remain unchanged; selection restricts replacement; false replace_existing multiplies old coverage"},
        "brush_defaults":StrokeSettings::default(),
        "brush_modes":[{"mode":"paint"},{"mode":"erase"},{"mode":"blur","radius":4},{"mode":"smudge"},{"mode":"liquify"},{"mode":"clone","offset":[-20,0],"sample_merged":false},{"mode":"heal","method":"content_aware"},{"mode":"heal","method":"create_texture"},{"mode":"heal","method":"proximity_match"}],
        "brush_ranges":{"diameter":[0.01,5000],"warp_max_diameter":2000,"hardness":[0,1],"opacity":[0,1],"smoothing":[0,1],"blur_radius":[0.1,250],"clone_offset":"sample position minus painted position in document coordinates"},
        "gradient_shapes":["linear","radial","angle","reflected","diamond"],
        "trim_basis":["transparent","top_left","bottom_right"],
        "filter_defaults":filter_defaults,
        "filter_array_item_schemas":{"mixer.points":array_item_schema("settings.mixer.points").unwrap(),"geometry.guides":array_item_schema("settings.geometry.guides").unwrap()},
        "filter_notes":{"Hue/Saturation":"top-level hue/saturation/lightness are Master shortcuts; prefer hsvSettings.adjustments for color bands","Camera Raw Filter":"whiteBalance=Auto computes temperature/tint from source; geometry guides use normalized lower-left coordinates, up to two guides; mixer points allow up to eight entries"},
        "adjustment_kinds":["Hue/Saturation","Levels","Curves","Exposure","Gradient Map","Grain","Invert","Black & White","Color Balance","Gaussian Blur","Motion Blur","Add Noise"],
        "effect_defaults":effect_defaults,
        "text_defaults":TextStyle::default(),
        "text_optional_fields":{"boxSize":[320,120],"colorRuns":[{"location":0,"length":1,"red":1,"green":0,"blue":0}],"fontRuns":[{"location":0,"length":1,"fontName":"Arial"}]},
        "subject_mask_defaults":matte_settings(&json!({})).unwrap(),
        "blend_modes":BlendMode::ALL.map(BlendMode::label),
        "sampling":["Nearest","Smooth","High quality"],
        "limits":{"max_side":crate::model::MAX_SIDE,"max_pixels":crate::model::MAX_PIXELS,"transform_size":[1,300000],"coordinate_absolute_max":1000000},
        "transactions":"The caller applies operations to a cloned document and validates before committing. These operations never save/export output files."
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(doc: &mut Document, value: Value) -> Value {
        let operation: Operation = serde_json::from_value(value).unwrap();
        Engine::default().apply(doc, &operation).unwrap()
    }

    #[test]
    fn capabilities_examples_use_real_strict_operations() {
        let caps = capabilities();
        assert_eq!(caps["operations"].as_array().unwrap().len(), 41);
        let mut names = HashSet::new();
        for item in caps["operations"].as_array().unwrap() {
            let parsed: Operation = serde_json::from_value(item["example"].clone()).unwrap();
            assert!(names.insert(item["op"].as_str().unwrap()));
            assert_eq!(
                serde_json::to_value(parsed).unwrap(),
                item["expanded_example"]
            );
            let mut unknown = item["example"].clone();
            unknown["misspelled_field"] = json!(true);
            assert!(serde_json::from_value::<Operation>(unknown).is_err());
        }
    }

    #[test]
    fn smooth_shape_operations_keep_defaults_and_preserve_optional_style() {
        let mut doc = Document::new(120, 100);
        let value = json!({"op":"add_shape","kind":"rectangle","rect":[0,0,100,80],"color":[255,0,0,255],"corner_radius":20});
        let ordinary: Operation = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&ordinary).unwrap()["smoothing"], false);
        Engine::default().apply(&mut doc, &ordinary).unwrap();
        let original = doc.active().unwrap().image.clone().unwrap();
        assert!(
            doc.active()
                .unwrap()
                .shape
                .as_ref()
                .unwrap()
                .get("smoothing")
                .is_none()
        );

        apply(
            &mut doc,
            json!({"op":"update_shape","style":{"smoothing":true}}),
        );
        let smoothed = doc.active().unwrap().image.clone().unwrap();
        assert_ne!(smoothed, original);
        assert_eq!(
            doc.active().unwrap().shape.as_ref().unwrap()["smoothing"],
            true
        );
        apply(&mut doc, json!({"op":"update_shape","style":{"red":0.5}}));
        assert_eq!(
            doc.active().unwrap().shape.as_ref().unwrap()["smoothing"],
            true
        );
        apply(
            &mut doc,
            json!({"op":"update_shape","style":{"smoothing":false,"red":1.}}),
        );
        assert_eq!(doc.active().unwrap().image.as_ref(), Some(&original));
        assert!(
            doc.active()
                .unwrap()
                .shape
                .as_ref()
                .unwrap()
                .get("smoothing")
                .is_none()
        );

        let mut smooth = value.clone();
        smooth["smoothing"] = json!(true);
        apply(&mut doc, smooth);
        assert_eq!(doc.active().unwrap().image.as_ref(), Some(&smoothed));
        for invalid in [json!("true"), json!(1), Value::Null] {
            let mut wrong = value.clone();
            wrong["smoothing"] = invalid.clone();
            assert!(serde_json::from_value::<Operation>(wrong).is_err());
            assert!(
                shape_style(
                    doc.active().unwrap().shape.as_ref().unwrap(),
                    &json!({"smoothing":invalid})
                )
                .is_err()
            );
        }
        for kind in ["Ellipse", "Line"] {
            let style = if kind == "Line" {
                json!({"kind":kind,"red":1.,"green":0.,"blue":0.,"cornerRadius":0.,"lineWidth":1.,"start":[0.,0.],"end":[1.,1.]})
            } else {
                json!({"kind":kind,"red":1.,"green":0.,"blue":0.,"cornerRadius":0.})
            };
            assert!(shape_style(&style, &json!({"smoothing":true})).is_err());
        }
    }

    #[test]
    fn rotate_canvas_operation_swaps_dimensions_and_inverse_restores_document() {
        let mut doc = Document::new(8, 6);
        apply(&mut doc, json!({"op":"fill","color":[20,60,90,255]}));
        apply(
            &mut doc,
            json!({"op":"add_guide","horizontal":false,"position":2.}),
        );
        let original = doc.clone();
        let result = apply(&mut doc, json!({"op":"rotate_canvas","clockwise":true}));
        assert_eq!(result["width"], 6);
        assert_eq!(result["height"], 8);
        assert_eq!(doc.guides[0].axis, "horizontal");
        apply(&mut doc, json!({"op":"rotate_canvas","clockwise":false}));
        assert_eq!(doc, original);
        assert!(
            serde_json::from_value::<Operation>(json!({"op":"rotate_canvas","clockwise":"true"}))
                .is_err()
        );
    }

    #[test]
    fn missing_explicit_ids_never_fall_back_to_active_layer() {
        let mut doc = Document::new(8, 8);
        let before = doc.clone();
        let operation = Operation::MoveLayers {
            layers: vec![Uuid::new_v4()],
            delta: [1., 2.],
        };
        assert!(Engine::default().apply(&mut doc, &operation).is_err());
        assert_eq!(doc, before);
        let operation = Operation::Fill {
            layer: Some(Uuid::new_v4()),
            color: [255; 4],
            mask: false,
        };
        assert!(Engine::default().apply(&mut doc, &operation).is_err());
        assert_eq!(doc, before);
    }

    #[test]
    fn selected_fill_clear_and_mask_use_document_coordinates() {
        let mut doc = Document::new(8, 6);
        apply(&mut doc, json!({"op":"fill","color":[40,80,120,255]}));
        apply(
            &mut doc,
            json!({"op":"select","selection":{"action":"rectangle","rect":[1,2,3,2],"antialiased":false}}),
        );
        apply(&mut doc, json!({"op":"clear"}));
        let image = doc.active().unwrap().image.as_ref().unwrap();
        assert_eq!(image.get(1, 2), [0; 4]);
        assert_eq!(image.get(1, 0), [40, 80, 120, 255]);
        apply(
            &mut doc,
            json!({"op":"mask","action":{"action":"add","from_selection":true}}),
        );
        let mask = doc.active().unwrap().mask.as_ref().unwrap();
        assert_eq!(mask.raster.get(1, 2), [255; 4]);
        assert_eq!(mask.raster.get(0, 0), [0, 0, 0, 255]);
        project::validate_document(&doc).unwrap();
    }

    #[test]
    fn brush_uses_the_real_pixel_editing_engine() {
        let mut doc = Document::new(24, 24);
        let result = apply(
            &mut doc,
            json!({"op":"brush","points":[[12,12]],"settings":{"diameter":8,"color":[255,30,10,255]}}),
        );
        assert!(result["changed_pixels"].as_u64().unwrap() > 0);
        assert_eq!(
            doc.active().unwrap().image.as_ref().unwrap().get(12, 12),
            [255, 30, 10, 255]
        );
        apply(
            &mut doc,
            json!({"op":"brush","points":[[12,12]],"settings":{"diameter":8,"brush":{"mode":"erase"}}}),
        );
        assert_eq!(
            doc.active().unwrap().image.as_ref().unwrap().get(12, 12)[3],
            0
        );
        project::validate_document(&doc).unwrap();
    }

    #[test]
    fn group_move_and_duplicate_preserve_structure() {
        let mut doc = Document::new(16, 16);
        let first = doc.active_layer_id.unwrap();
        let second: Uuid = serde_json::from_value(
            apply(
                &mut doc,
                json!({"op":"add_layer","name":"二","color":[1,2,3,255]}),
            )["layer"]
                .clone(),
        )
        .unwrap();
        let group: Uuid = serde_json::from_value(
            apply(
                &mut doc,
                json!({"op":"group_layers","layers":[first,second],"name":"组"}),
            )["layer"]
                .clone(),
        )
        .unwrap();
        apply(
            &mut doc,
            json!({"op":"move_layers","layers":[group],"delta":[3,5]}),
        );
        assert_eq!(doc.layer(first).unwrap().transform.origin, [3., 5.]);
        assert_eq!(doc.layer(second).unwrap().transform.origin, [3., 5.]);
        let copied = apply(&mut doc, json!({"op":"duplicate_layers","layers":[group]}));
        assert_eq!(copied["layers"].as_array().unwrap().len(), 3);
        project::validate_document(&doc).unwrap();
    }

    #[test]
    fn strict_style_and_filter_parameters_reject_silent_typos() {
        assert!(filter_settings("Gaussian Blur", &json!({"blurRaduis":3})).is_err());
        assert!(filter_settings("Gaussian Blur", &json!({"blurRadius":"3"})).is_err());
        assert!(filter_settings("Gaussian Blur", &json!({"blurRadius":500})).is_err());
        assert!(matte_settings(&json!({"refineEdges":-1})).is_err());
        assert!(validate_effects(&json!({"dropShadow":{}})).is_err());
        assert!(text_style(None, &json!({"fontSzie":36})).is_err());
        assert!(
            serde_json::from_value::<Operation>(
                json!({"op":"brush","points":[[1,1]],"settings":{"brush":{"mode":"clone"}}})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<Operation>(json!({"op":"set_transform","transform":{"origin":[0,0],"size":[8,8],"flip_x":true}})).is_err());
    }

    #[test]
    fn no_argument_tagged_variants_reject_unknown_nested_fields() {
        for action in ["all", "none", "invert"] {
            let valid = json!({"op":"select","selection":{"action":action}});
            assert!(serde_json::from_value::<Operation>(valid.clone()).is_ok());
            let mut invalid = valid;
            invalid["selection"]["misspelled_field"] = json!(true);
            assert!(
                serde_json::from_value::<Operation>(invalid).is_err(),
                "selection {action}"
            );
        }
        for action in ["apply", "delete"] {
            let valid = json!({"op":"mask","action":{"action":action}});
            assert!(serde_json::from_value::<Operation>(valid.clone()).is_ok());
            let mut invalid = valid;
            invalid["action"]["misspelled_field"] = json!(true);
            assert!(
                serde_json::from_value::<Operation>(invalid).is_err(),
                "mask {action}"
            );
        }
        for mode in ["paint", "erase", "smudge", "liquify"] {
            let valid = json!({"op":"brush","points":[[1,1]],"settings":{"brush":{"mode":mode}}});
            assert!(serde_json::from_value::<Operation>(valid.clone()).is_ok());
            let mut invalid = valid;
            invalid["settings"]["brush"]["misspelled_field"] = json!(true);
            assert!(
                serde_json::from_value::<Operation>(invalid).is_err(),
                "brush {mode}"
            );
        }
    }

    #[test]
    fn replacement_subject_mask_preserves_unselected_old_mask_and_empty_selection() {
        let mut doc = Document::new(4, 2);
        let id = doc.active_layer_id.unwrap();
        doc.active_mut().unwrap().image = Some(Arc::new(Raster::solid(4, 2, [120, 70, 30, 255])));
        brush::add_mask(&mut doc, id, false).unwrap();
        doc.selection = Some(selection::marquee(4, 2, [1., 0., 2., 2.], false, false));
        apply_subject_alpha(&mut doc, id, &[255; 8], true).unwrap();
        let mask = doc.active().unwrap().mask.as_ref().unwrap();
        assert_eq!(mask.raster.get(0, 0)[0], 0);
        assert_eq!(mask.raster.get(1, 0)[0], 255);
        assert_eq!(mask.raster.get(3, 0)[0], 0);
        doc.selection = Some(Selection::empty(4, 2));
        let before = doc.clone();
        apply_subject_alpha(&mut doc, id, &[0; 8], true).unwrap();
        assert_eq!(doc, before);
        doc.selection = None;
        apply_subject_alpha(&mut doc, id, &[200; 8], false).unwrap();
        assert_eq!(
            doc.active()
                .unwrap()
                .mask
                .as_ref()
                .unwrap()
                .raster
                .get(0, 0)[0],
            0
        );
        assert_eq!(
            doc.active()
                .unwrap()
                .mask
                .as_ref()
                .unwrap()
                .raster
                .get(1, 0)[0],
            200
        );
        project::validate_document(&doc).unwrap();
    }

    #[test]
    fn text_update_does_not_convert_an_arbitrary_raster() {
        let mut doc = Document::new(8, 8);
        let before = doc.clone();
        let operation: Operation =
            serde_json::from_value(json!({"op":"update_text","style":{"content":"x"}})).unwrap();
        assert!(Engine::default().apply(&mut doc, &operation).is_err());
        assert_eq!(doc, before);
    }

    #[test]
    fn importing_uses_codecs_and_never_saves_an_output() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.png");
        image_io::export_png(&Raster::solid(2, 3, [20, 60, 90, 255]), &path, 72.).unwrap();
        let mut doc = Document::new(10, 10);
        let result = apply(
            &mut doc,
            json!({"op":"import_layers","paths":[path],"point":[5,5]}),
        );
        assert_eq!(result["layers"].as_array().unwrap().len(), 1);
        assert_eq!(doc.active().unwrap().transform.origin, [4., 3.5]);
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
        project::validate_document(&doc).unwrap();
    }

    #[test]
    fn adjustment_and_effects_render_real_pixels() {
        let mut doc = Document::new(4, 4);
        apply(&mut doc, json!({"op":"fill","color":[20,40,80,255]}));
        apply(&mut doc, json!({"op":"adjustment","kind":"Invert"}));
        assert_eq!(render::composite(&doc).get(2, 2), [235, 215, 175, 255]);
        apply(&mut doc, json!({"op":"flatten"}));
        apply(
            &mut doc,
            json!({"op":"effects","effects":{"colorOverlay":{"red":1,"green":0,"blue":0,"opacity":1}}}),
        );
        assert_eq!(render::composite(&doc).get(2, 2), [255, 0, 0, 255]);
        project::validate_document(&doc).unwrap();
    }

    #[test]
    fn retained_history_budget_is_checked_before_applying_an_operation() {
        let mut doc = Document::new(8, 8);
        let before = doc.clone();
        let operation = Operation::AddLayer {
            name: "over budget".into(),
            color: None,
        };
        let err = Engine::default()
            .apply_with_workspace(
                &mut doc,
                &operation,
                &[&before],
                crate::workspace_limits::workspace_budget_bytes() + 1,
            )
            .unwrap_err();
        assert!(err.to_string().contains("工作区内存上限"));
        assert_eq!(doc, before);
    }

    #[test]
    fn import_validates_each_source_before_decoding_the_next_path() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("one.png");
        image_io::export_png(&Raster::solid(1, 1, [255; 4]), &first, 72.).unwrap();
        let mut doc = Document::new(1, 1);
        doc.layers.clear();
        // Compact uniform rasters reach the logical pixel limit without allocating those pixels.
        let mut remaining = crate::model::document_pixel_budget();
        while remaining > 0 {
            let (width, height) = if remaining >= 10_000 {
                (10_000, (remaining / 10_000).min(20_000) as u32)
            } else {
                (remaining as u32, 1)
            };
            doc.layers.push(Layer::raster(
                "compact",
                Raster::solid(width, height, [0; 4]),
            ));
            remaining -= u64::from(width) * u64::from(height);
        }
        doc.active_layer_id = doc.layers.last().map(|layer| layer.id);
        project::validate_document(&doc).unwrap();
        let operation = Operation::ImportLayers {
            paths: vec![first, temp.path().join("must-not-be-opened.png")],
            point: None,
        };
        let err = Engine::default().apply(&mut doc, &operation).unwrap_err();
        assert!(err.to_string().contains("文档像素预算"), "{err:#}");
    }
}
