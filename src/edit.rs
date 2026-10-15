use crate::model::{Document, Guide, Layer, MAX_PIXELS, Raster, Sampling, Selection, Transform};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use uuid::Uuid;

pub fn add_layer(doc: &mut Document, name: impl Into<String>) -> Uuid {
    let mut layer = Layer::new(name, doc.width, doc.height);
    let index = doc
        .active_layer_id
        .and_then(|id| doc.layers.iter().position(|l| l.id == id))
        .map_or(doc.layers.len(), |i| i + 1);
    layer.parent_id = doc
        .active()
        .and_then(|l| if l.is_group { Some(l.id) } else { l.parent_id });
    let id = layer.id;
    doc.layers.insert(index, layer);
    doc.active_layer_id = Some(id);
    id
}

pub fn descendants(doc: &Document, id: Uuid) -> HashSet<Uuid> {
    let mut set = HashSet::from([id]);
    loop {
        let old = set.len();
        for layer in &doc.layers {
            if layer.parent_id.is_some_and(|p| set.contains(&p)) {
                set.insert(layer.id);
            }
        }
        if set.len() == old {
            break;
        }
    }
    set
}

pub fn delete_layers(doc: &mut Document, ids: &[Uuid]) -> Result<()> {
    ensure!(
        ids.iter().all(|id| doc.layer(*id).is_some()),
        "选中的图层已不存在"
    );
    let remove = ids
        .iter()
        .flat_map(|&id| descendants(doc, id))
        .collect::<HashSet<_>>();
    doc.layers.retain(|l| !remove.contains(&l.id));
    for layer in &mut doc.layers {
        if layer.mask_source_id.is_some_and(|id| remove.contains(&id)) {
            layer.mask_source_id = None;
        }
    }
    if doc.active_layer_id.is_some_and(|id| remove.contains(&id)) {
        doc.active_layer_id = doc.layers.last().map(|l| l.id);
    }
    Ok(())
}

pub fn duplicate_layers(doc: &mut Document, ids: &[Uuid]) -> Result<Vec<Uuid>> {
    ensure!(
        ids.iter().all(|id| doc.layer(*id).is_some()),
        "选中的图层已不存在"
    );
    let selected = ids
        .iter()
        .flat_map(|&id| descendants(doc, id))
        .collect::<HashSet<_>>();
    let mapping = selected
        .iter()
        .map(|&id| (id, Uuid::new_v4()))
        .collect::<HashMap<_, _>>();
    let copies = doc
        .layers
        .iter()
        .filter(|l| selected.contains(&l.id))
        .map(|l| {
            let mut copy = l.clone();
            copy.id = mapping[&l.id];
            copy.name = format!("{} 副本", l.name);
            copy.parent_id = l.parent_id.map(|p| mapping.get(&p).copied().unwrap_or(p));
            copy.mask_source_id = l
                .mask_source_id
                .map(|p| mapping.get(&p).copied().unwrap_or(p));
            copy
        })
        .collect::<Vec<_>>();
    let insertion = doc
        .layers
        .iter()
        .rposition(|l| selected.contains(&l.id))
        .map_or(doc.layers.len(), |i| i + 1);
    let copy_ids = copies.iter().map(|l| l.id).collect::<Vec<_>>();
    doc.layers.splice(insertion..insertion, copies);
    doc.active_layer_id = ids.last().and_then(|id| mapping.get(id)).copied();
    Ok(copy_ids)
}

pub fn copy_layers_to(
    source: &Document,
    target: &mut Document,
    ids: &[Uuid],
    point: Option<[f64; 2]>,
) -> Result<Vec<Uuid>> {
    let selected = ids
        .iter()
        .flat_map(|&id| descendants(source, id))
        .collect::<HashSet<_>>();
    ensure!(!selected.is_empty(), "请先选择图层");
    let mapping = selected
        .iter()
        .map(|&id| (id, Uuid::new_v4()))
        .collect::<HashMap<_, _>>();
    let mut layers = source
        .layers
        .iter()
        .filter(|l| selected.contains(&l.id))
        .map(|l| {
            let mut copy = l.clone();
            copy.id = mapping[&l.id];
            copy.parent_id = l.parent_id.and_then(|p| mapping.get(&p).copied());
            copy.mask_source_id = l.mask_source_id.and_then(|p| mapping.get(&p).copied());
            copy
        })
        .collect::<Vec<_>>();
    if let Some(p) = point {
        let b = bounds_for(&layers);
        let delta = [p[0] - (b[0] + b[2] / 2.), p[1] - (b[1] + b[3] / 2.)];
        for l in &mut layers {
            offset_layer(l, delta);
        }
    }
    let out = layers.iter().map(|l| l.id).collect::<Vec<_>>();
    target.layers.extend(layers);
    target.active_layer_id = out.last().copied();
    Ok(out)
}

pub fn add_group(doc: &mut Document, name: impl Into<String>, ids: &[Uuid]) -> Result<Uuid> {
    ensure!(
        ids.iter().all(|id| doc.layer(*id).is_some()),
        "选中的图层已不存在"
    );
    let mut group = Layer::new(name, doc.width, doc.height);
    group.is_group = true;
    let selected = ids.iter().copied().collect::<HashSet<_>>();
    group.parent_id = ids
        .first()
        .and_then(|id| doc.layer(*id))
        .and_then(|l| l.parent_id);
    if ids
        .iter()
        .filter_map(|id| doc.layer(*id))
        .any(|l| l.parent_id != group.parent_id)
    {
        group.parent_id = None;
    }
    let id = group.id;
    let insertion = doc
        .layers
        .iter()
        .rposition(|l| selected.contains(&l.id))
        .map_or(doc.layers.len(), |i| i + 1);
    for l in &mut doc.layers {
        if selected.contains(&l.id) && !l.parent_id.is_some_and(|p| selected.contains(&p)) {
            l.parent_id = Some(id);
        }
    }
    doc.layers.insert(insertion, group);
    doc.active_layer_id = Some(id);
    Ok(id)
}

pub fn ungroup(doc: &mut Document, id: Uuid) -> Result<()> {
    let group = doc
        .layer(id)
        .ok_or_else(|| anyhow::anyhow!("该组已不存在"))?
        .clone();
    ensure!(group.is_group, "选中的图层不是组");
    for l in &mut doc.layers {
        if l.parent_id == Some(id) {
            l.parent_id = group.parent_id;
        }
    }
    doc.layers.retain(|l| l.id != id);
    if doc.active_layer_id == Some(id) {
        doc.active_layer_id = doc.layers.last().map(|l| l.id);
    }
    repair_clipping(doc);
    Ok(())
}

pub fn place_layer(
    doc: &mut Document,
    id: Uuid,
    parent: Option<Uuid>,
    above: Option<Uuid>,
) -> Result<()> {
    ensure!(doc.layer(id).is_some(), "图层已不存在");
    if let Some(p) = parent {
        ensure!(doc.layer(p).is_some_and(|l| l.is_group), "父图层不是组");
        ensure!(!descendants(doc, id).contains(&p), "组内不能包含自身");
    }
    let moving = descendants(doc, id);
    ensure!(
        above.is_none_or(|a| !moving.contains(&a)),
        "拖放目标位于正在移动的组内"
    );
    let mut layers = Vec::new();
    doc.layers.retain(|l| {
        if moving.contains(&l.id) {
            layers.push(l.clone());
            false
        } else {
            true
        }
    });
    layers.iter_mut().find(|l| l.id == id).unwrap().parent_id = parent;
    let index = above
        .and_then(|a| doc.layers.iter().position(|l| l.id == a))
        .map_or(doc.layers.len(), |i| i + 1);
    doc.layers.splice(index..index, layers);
    repair_clipping(doc);
    Ok(())
}

fn repair_clipping(doc: &mut Document) {
    let parent = doc
        .layers
        .iter()
        .map(|l| (l.id, l.parent_id))
        .collect::<HashMap<_, _>>();
    for l in &mut doc.layers {
        if l.mask_source_id
            .is_some_and(|id| parent.get(&id) != Some(&l.parent_id))
        {
            l.mask_source_id = None;
        }
    }
}

pub fn bounds_for(layers: &[Layer]) -> [f64; 4] {
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for l in layers.iter().filter(|l| !l.is_group) {
        for p in l.transform.corners() {
            x0 = x0.min(p[0]);
            y0 = y0.min(p[1]);
            x1 = x1.max(p[0]);
            y1 = y1.max(p[1]);
        }
    }
    if x0.is_finite() {
        [x0, y0, x1 - x0, y1 - y0]
    } else {
        [0., 0., 1., 1.]
    }
}
pub fn selected_bounds(doc: &Document, ids: &[Uuid]) -> [f64; 4] {
    let selected = ids
        .iter()
        .flat_map(|&id| descendants(doc, id))
        .collect::<HashSet<_>>();
    bounds_for(
        &doc.layers
            .iter()
            .filter(|l| selected.contains(&l.id))
            .cloned()
            .collect::<Vec<_>>(),
    )
}
fn offset_layer(layer: &mut Layer, delta: [f64; 2]) {
    layer.transform.origin[0] += delta[0];
    layer.transform.origin[1] += delta[1];
    if let Some(mask) = &mut layer.mask
        && let Some(t) = &mut mask.placement
    {
        t.origin[0] += delta[0];
        t.origin[1] += delta[1];
    }
}

pub fn move_layers(doc: &mut Document, ids: &[Uuid], delta: [f64; 2]) -> Result<()> {
    ensure!(delta.iter().all(|v| v.is_finite()), "移动参数无效");
    let selected = ids
        .iter()
        .flat_map(|&id| descendants(doc, id))
        .collect::<HashSet<_>>();
    for layer in &mut doc.layers {
        if selected.contains(&layer.id) {
            let old = layer.transform;
            layer.transform.origin[0] += delta[0];
            layer.transform.origin[1] += delta[1];
            if let Some(mask) = &mut layer.mask {
                if mask.linked {
                    if let Some(t) = &mut mask.placement {
                        t.origin[0] += delta[0];
                        t.origin[1] += delta[1];
                    }
                } else if mask.placement.is_none() {
                    mask.placement = Some(old);
                }
            }
        }
    }
    Ok(())
}
pub fn set_transform(
    doc: &mut Document,
    id: Uuid,
    value: Transform,
    mask_only: bool,
) -> Result<()> {
    ensure!(value.is_valid(), "变换参数无效");
    let layer = doc
        .layer_mut(id)
        .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?;
    if mask_only {
        let m = layer
            .mask
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("图层没有蒙版"))?;
        m.placement = Some(value);
    } else {
        let old = layer.transform;
        let redrawn = if old.size != value.size {
            layer
                .shape
                .as_ref()
                .map(|shape| {
                    shape_raster(
                        shape,
                        value.size[0].round().max(1.) as u32,
                        value.size[1].round().max(1.) as u32,
                    )
                })
                .transpose()?
        } else {
            None
        };
        if let Some(m) = &mut layer.mask {
            if m.linked {
                if let Some(placement) = m.placement {
                    m.placement = Some(follow_transform(placement, old, value));
                }
            } else if m.placement.is_none() {
                m.placement = Some(old);
            }
        }
        layer.transform = value;
        if let Some(image) = redrawn {
            layer.image = Some(Arc::new(image));
        }
    }
    Ok(())
}
pub fn follow_transform(value: Transform, old: Transform, new: Transform) -> Transform {
    if old.size == new.size
        && old.rotation == new.rotation
        && old.flip_x == new.flip_x
        && old.flip_y == new.flip_y
    {
        let mut out = value;
        out.origin[0] += new.origin[0] - old.origin[0];
        out.origin[1] += new.origin[1] - old.origin[1];
        return out;
    }
    let map = |unit| new.to_document(old.from_document(value.to_document(unit)));
    let origin = map([0., 0.]);
    let px = map([1., 0.]);
    let py = map([0., 1.]);
    let center = map([0.5, 0.5]);
    let vx = [px[0] - origin[0], px[1] - origin[1]];
    let vy = [py[0] - origin[0], py[1] - origin[1]];
    let sign = if value.flip_x { -1. } else { 1. };
    let angle = (vx[1] * sign).atan2(vx[0] * sign);
    let along = -vy[0] * angle.sin() + vy[1] * angle.cos();
    let mut out = value;
    out.size = [vx[0].hypot(vx[1]), along.abs()];
    out.origin = [center[0] - out.size[0] / 2., center[1] - out.size[1] / 2.];
    let degrees = angle.to_degrees();
    out.rotation = degrees + ((value.rotation - degrees) / 360.).round() * 360.;
    out.flip_y = along < 0.;
    out
}
pub fn transform_layers(
    doc: &mut Document,
    ids: &[Uuid],
    old: Transform,
    new: Transform,
) -> Result<()> {
    ensure!(old.is_valid() && new.is_valid(), "组变换参数无效");
    let selected = ids
        .iter()
        .flat_map(|&id| descendants(doc, id))
        .collect::<HashSet<_>>();
    let changes = doc
        .layers
        .iter()
        .filter(|l| selected.contains(&l.id))
        .map(|l| (l.id, follow_transform(l.transform, old, new)))
        .collect::<Vec<_>>();
    for (id, t) in changes {
        set_transform(doc, id, t, false)?;
    }
    Ok(())
}
pub fn flip_layers(doc: &mut Document, ids: &[Uuid], horizontal: bool) -> Result<()> {
    let b = selected_bounds(doc, ids);
    let center = if horizontal {
        b[0] + b[2] / 2.
    } else {
        b[1] + b[3] / 2.
    };
    let selected = ids
        .iter()
        .flat_map(|&id| descendants(doc, id))
        .collect::<HashSet<_>>();
    for l in &mut doc.layers {
        if selected.contains(&l.id) {
            let old = l.transform;
            mirror(&mut l.transform, horizontal, center);
            if let Some(m) = &mut l.mask {
                if m.linked {
                    if let Some(t) = &mut m.placement {
                        mirror(t, horizontal, center);
                    }
                } else if m.placement.is_none() {
                    m.placement = Some(old);
                }
            }
        }
    }
    Ok(())
}
fn mirror(t: &mut Transform, horizontal: bool, axis: f64) {
    let i = usize::from(!horizontal);
    t.origin[i] = 2. * axis - t.origin[i] - t.size[i];
    t.rotation = -t.rotation;
    if horizontal {
        t.flip_x = !t.flip_x;
    } else {
        t.flip_y = !t.flip_y;
    }
}
pub fn flip_canvas(doc: &mut Document, horizontal: bool) {
    let axis = if horizontal {
        doc.width as f64 / 2.
    } else {
        doc.height as f64 / 2.
    };
    for l in &mut doc.layers {
        mirror(&mut l.transform, horizontal, axis);
        if let Some(m) = &mut l.mask
            && let Some(t) = &mut m.placement
        {
            mirror(t, horizontal, axis);
        }
    }
    for guide in &mut doc.guides {
        if (horizontal && guide.axis == "vertical") || (!horizontal && guide.axis == "horizontal") {
            guide.position = 2. * axis - guide.position;
        }
    }
    if let Some(s) = &doc.selection {
        doc.selection = Some(crate::selection::flipped(s, horizontal));
    }
}

pub fn resize_canvas(doc: &mut Document, width: u32, height: u32, anchor: [f64; 2]) -> Result<()> {
    ensure!(Document::valid_size(width, height), "画布尺寸无效");
    ensure!(
        anchor.iter().all(|v| v.is_finite() && *v >= 0. && *v <= 1.),
        "锚点无效"
    );
    let delta = [
        ((width as f64 - doc.width as f64) * anchor[0]).round(),
        ((height as f64 - doc.height as f64) * anchor[1]).round(),
    ];
    for l in &mut doc.layers {
        offset_layer(l, delta);
    }
    for g in &mut doc.guides {
        g.position += delta[usize::from(g.axis == "horizontal")];
    }
    if let Some(s) = &doc.selection {
        doc.selection = Some(crate::selection::translated(
            s,
            delta[0] as i32,
            delta[1] as i32,
            width,
            height,
        ));
    }
    doc.width = width;
    doc.height = height;
    Ok(())
}
pub fn crop(doc: &mut Document, rect: [f64; 4]) -> Result<()> {
    ensure!(rect.iter().all(|v| v.is_finite()), "裁剪框无效");
    let x = rect[0].round();
    let y = rect[1].round();
    let w = rect[2].round();
    let h = rect[3].round();
    ensure!(
        w >= 1.
            && h >= 1.
            && w <= u32::MAX as f64
            && h <= u32::MAX as f64
            && Document::valid_size(w as u32, h as u32),
        "裁剪尺寸无效"
    );
    for l in &mut doc.layers {
        offset_layer(l, [-x, -y]);
    }
    for g in &mut doc.guides {
        g.position -= if g.axis == "horizontal" { y } else { x };
    }
    if let Some(s) = &doc.selection {
        doc.selection = Some(crate::selection::translated(
            s, -x as i32, -y as i32, w as u32, h as u32,
        ));
    }
    doc.width = w as u32;
    doc.height = h as u32;
    Ok(())
}
pub fn resize_image(doc: &mut Document, width: u32, height: u32, sampling: Sampling) -> Result<()> {
    ensure!(Document::valid_size(width, height), "图像尺寸无效");
    let sx = width as f64 / doc.width as f64;
    let sy = height as f64 / doc.height as f64;
    for l in &mut doc.layers {
        let old = l.transform;
        if let Some(m) = &mut l.mask {
            let mut t = m.placement.unwrap_or(old);
            t.origin[0] *= sx;
            t.origin[1] *= sy;
            t.size[0] *= sx;
            t.size[1] *= sy;
            m.placement = Some(t);
        }
        let corners = old.corners().map(|p| [p[0] * sx, p[1] * sy]);
        let minx = corners
            .iter()
            .map(|p| p[0])
            .fold(f64::INFINITY, f64::min)
            .floor();
        let miny = corners
            .iter()
            .map(|p| p[1])
            .fold(f64::INFINITY, f64::min)
            .floor();
        let w = (corners
            .iter()
            .map(|p| p[0])
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil()
            - minx)
            .max(1.) as u32;
        let h = (corners
            .iter()
            .map(|p| p[1])
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil()
            - miny)
            .max(1.) as u32;
        ensure!(
            w as u64 * h as u64 <= MAX_PIXELS,
            "调整后的图层超出图像大小上限"
        );
        if let Some(image) = &l.image {
            let mut data = vec![0; w as usize * h as usize * 4];
            for y in 0..h {
                for x in 0..w {
                    let unit = old.from_document([
                        (minx + x as f64 + 0.5) / sx,
                        (miny + y as f64 + 0.5) / sy,
                    ]);
                    let p = if sampling == Sampling::Nearest {
                        if unit.iter().all(|v| *v >= 0. && *v < 1.) {
                            image.get(
                                (unit[0] * image.width as f64) as u32,
                                (unit[1] * image.height as f64) as u32,
                            )
                        } else {
                            [0; 4]
                        }
                    } else {
                        crate::brush::sample(
                            image,
                            unit[0] * image.width as f64 - 0.5,
                            unit[1] * image.height as f64 - 0.5,
                        )
                    };
                    let i = ((y * w + x) * 4) as usize;
                    data[i..i + 4].copy_from_slice(&p);
                }
            }
            l.image = Some(Arc::new(Raster::from_rgba(w, h, data)));
            l.text = None;
            l.shape = None;
        }
        l.transform = Transform::new(minx, miny, w as f64, h as f64);
        l.transform.sampling = sampling;
    }
    for g in &mut doc.guides {
        g.position *= if g.axis == "horizontal" { sy } else { sx };
    }
    if let Some(s) = &doc.selection {
        doc.selection = Some(crate::selection::scaled(s, width, height));
    }
    doc.width = width;
    doc.height = height;
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub enum TrimBasis {
    Transparent,
    TopLeft,
    BottomRight,
}
pub fn trim_bounds(
    image: &Raster,
    basis: TrimBasis,
    tolerance: u8,
    sides: [bool; 4],
) -> Option<[f64; 4]> {
    if image.width == 0 || image.height == 0 {
        return None;
    }
    let reference = match basis {
        TrimBasis::BottomRight => image.get(image.width - 1, image.height - 1),
        _ => image.get(0, 0),
    };
    let mut b = [image.width, image.height, 0, 0];
    for y in 0..image.height {
        for x in 0..image.width {
            let p = image.get(x, y);
            let empty = match basis {
                TrimBasis::Transparent => p[3] == 0,
                _ => p
                    .iter()
                    .zip(reference)
                    .all(|(&a, b)| a.abs_diff(b) <= tolerance),
            };
            if !empty {
                b[0] = b[0].min(x);
                b[1] = b[1].min(y);
                b[2] = b[2].max(x + 1);
                b[3] = b[3].max(y + 1);
            }
        }
    }
    if b[2] == 0 {
        return None;
    }
    if !sides[0] {
        b[0] = 0;
    }
    if !sides[1] {
        b[1] = 0;
    }
    if !sides[2] {
        b[2] = image.width;
    }
    if !sides[3] {
        b[3] = image.height;
    }
    Some([
        b[0] as f64,
        b[1] as f64,
        (b[2] - b[0]) as f64,
        (b[3] - b[1]) as f64,
    ])
}
pub fn add_guide(doc: &mut Document, horizontal: bool, position: f64) -> Result<Uuid> {
    ensure!(position.is_finite(), "参考线位置无效");
    let id = Uuid::new_v4();
    doc.guides.push(Guide {
        id,
        axis: if horizontal { "horizontal" } else { "vertical" }.into(),
        position,
    });
    Ok(id)
}
pub fn snap_point(point: [f64; 2], xs: &[f64], ys: &[f64], tolerance: f64) -> [f64; 2] {
    fn nearest(v: f64, targets: &[f64], t: f64) -> f64 {
        targets
            .iter()
            .copied()
            .filter(|x| x.is_finite() && (*x - v).abs() <= t)
            .min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs()))
            .unwrap_or(v)
    }
    [
        nearest(point[0], xs, tolerance),
        nearest(point[1], ys, tolerance),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    Rectangle,
    Ellipse,
    Line,
}
/// Shape-tool drag geometry. Line endpoints remain in document space until normalized to its padded box.
pub fn shape_drag_geometry(
    kind: ShapeKind,
    start: [f64; 2],
    end: [f64; 2],
    shift: bool,
    alt: bool,
    line_width: f64,
) -> ([f64; 4], Option<[[f64; 2]; 2]>) {
    if start.iter().chain(&end).any(|v| !v.is_finite()) {
        return ([0.; 4], None);
    }
    let start = start.map(f64::round);
    if kind != ShapeKind::Line {
        return (crate::selection::drag_box(start, end, shift, alt), None);
    }
    let mut end = end;
    if shift {
        let delta = [end[0] - start[0], end[1] - start[1]];
        let eighth = std::f64::consts::FRAC_PI_4;
        let angle = (delta[1].atan2(delta[0]) / eighth).round() * eighth;
        let length = delta[0].hypot(delta[1]);
        end = [
            start[0] + angle.cos() * length,
            start[1] + angle.sin() * length,
        ];
    }
    let thickness = if line_width.is_finite() {
        line_width.clamp(1., 5000.)
    } else {
        1.
    };
    let rect = [
        start[0].min(end[0]) - thickness / 2.,
        start[1].min(end[1]) - thickness / 2.,
        (start[0] - end[0]).abs() + thickness,
        (start[1] - end[1]).abs() + thickness,
    ];
    let normalized = |point: [f64; 2]| {
        [
            (point[0] - rect[0]) / rect[2],
            (point[1] - rect[1]) / rect[3],
        ]
    };
    // The original line's committed box follows its endpoints; Alt affects filled shapes.
    (rect, Some([normalized(start), normalized(end)]))
}
pub fn add_shape(
    doc: &mut Document,
    kind: ShapeKind,
    rect: [f64; 4],
    color: [u8; 4],
    corner_radius: f64,
    line_width: f64,
    line_ends: Option<[[f64; 2]; 2]>,
) -> Result<Uuid> {
    ensure!(
        rect.iter().all(|x| x.is_finite()) && rect[2] >= 1. && rect[3] >= 1.,
        "形状尺寸必须是有效正数"
    );
    let w = rect[2].round() as u32;
    let h = rect[3].round() as u32;
    let mut style = json!({"kind":match kind{ShapeKind::Rectangle=>"Rectangle",ShapeKind::Ellipse=>"Ellipse",ShapeKind::Line=>"Line"},"red":color[0] as f64/255.,"green":color[1] as f64/255.,"blue":color[2] as f64/255.,"cornerRadius":corner_radius});
    if kind == ShapeKind::Line {
        let ends = line_ends.unwrap_or([[0., 0.], [1., 1.]]);
        style["lineWidth"] = json!(line_width);
        style["start"] = json!(ends[0]);
        style["end"] = json!(ends[1]);
    }
    let raster = shape_raster(&style, w, h)?;
    let name = crate::locale::text(style["kind"].as_str().unwrap()).into_owned();
    let id = add_layer(doc, name);
    let l = doc.layer_mut(id).unwrap();
    l.image = Some(Arc::new(raster));
    l.shape = Some(style);
    // v11 shape styles carry RGB only; layer opacity retains foreground alpha on resize/reopen.
    l.opacity = color[3] as f64 / 255.;
    l.transform = Transform::new(rect[0], rect[1], w as f64, h as f64);
    Ok(id)
}
pub fn shape_raster(style: &Value, width: u32, height: u32) -> Result<Raster> {
    ensure!(
        width > 0 && height > 0 && (width as u64 * height as u64) <= MAX_PIXELS,
        "形状超出图像大小上限"
    );
    let get = |key: &str, fallback: f64| style[key].as_f64().unwrap_or(fallback);
    let color = [
        (get("red", 0.).clamp(0., 1.) * 255.).round() as u8,
        (get("green", 0.).clamp(0., 1.) * 255.).round() as u8,
        (get("blue", 0.).clamp(0., 1.) * 255.).round() as u8,
        255,
    ];
    let kind = style["kind"].as_str().unwrap_or("Rectangle");
    let radius = get("cornerRadius", 0.).clamp(0., width.min(height) as f64 / 2.);
    let thickness = get("lineWidth", 4.).clamp(0.1, 5000.);
    let point = |key: &str, default: [f64; 2]| {
        let v = &style[key];
        if let Some(a) = v.as_array() {
            [
                a.first().and_then(Value::as_f64).unwrap_or(default[0]),
                a.get(1).and_then(Value::as_f64).unwrap_or(default[1]),
            ]
        } else {
            [
                v["x"].as_f64().unwrap_or(default[0]),
                v["y"].as_f64().unwrap_or(default[1]),
            ]
        }
    };
    let start = point("start", [0., 0.]);
    let end = point("end", [1., 1.]);
    let a = [start[0] * width as f64, start[1] * height as f64];
    let b = [end[0] * width as f64, end[1] * height as f64];
    let mut data = vec![0; width as usize * height as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let mut count = 0;
            for yy in 0..4 {
                for xx in 0..4 {
                    let px = x as f64 + (xx as f64 + 0.5) / 4.;
                    let py = y as f64 + (yy as f64 + 0.5) / 4.;
                    let inside = match kind {
                        "Ellipse" => {
                            ((px / width as f64) * 2. - 1.).powi(2)
                                + ((py / height as f64) * 2. - 1.).powi(2)
                                <= 1.
                        }
                        "Line" => {
                            let d = [b[0] - a[0], b[1] - a[1]];
                            let len = d[0] * d[0] + d[1] * d[1];
                            let t = if len == 0. {
                                0.
                            } else {
                                ((px - a[0]) * d[0] + (py - a[1]) * d[1]) / len
                            }
                            .clamp(0., 1.);
                            (px - a[0] - t * d[0]).hypot(py - a[1] - t * d[1]) <= thickness / 2.
                        }
                        _ => {
                            let cx = px.clamp(radius, width as f64 - radius);
                            let cy = py.clamp(radius, height as f64 - radius);
                            (px - cx).hypot(py - cy) <= radius
                        }
                    };
                    if inside {
                        count += 1;
                    }
                }
            }
            let i = ((y * width + x) * 4) as usize;
            data[i..i + 3].copy_from_slice(&color[..3]);
            data[i + 3] = (count * 255 / 16) as u8;
        }
    }
    Ok(Raster::from_rgba(width, height, data))
}

pub fn edit_pixels(
    doc: &mut Document,
    id: Uuid,
    mask: bool,
    operation: impl Fn([u8; 4]) -> [u8; 4],
) -> Result<()> {
    let layer = doc
        .layer(id)
        .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?
        .clone();
    let (mut raster, t) = if mask {
        let m = layer
            .mask
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("图层没有蒙版"))?;
        (
            m.raster.as_ref().clone(),
            m.placement.unwrap_or(layer.transform),
        )
    } else {
        (
            layer
                .image
                .as_deref()
                .cloned()
                .unwrap_or_else(|| Raster::blank(doc.width, doc.height)),
            layer.transform,
        )
    };
    for y in 0..raster.height {
        for x in 0..raster.width {
            let p = t.to_document([
                (x as f64 + 0.5) / raster.width as f64,
                (y as f64 + 0.5) / raster.height as f64,
            ]);
            let s = doc.selection.as_ref().map_or(1., |selection| {
                if p[0] < 0. || p[1] < 0. {
                    0.
                } else {
                    selection.get(p[0] as u32, p[1] as u32) as f64 / 255.
                }
            });
            if s > 0. {
                let a = raster.get(x, y);
                let mut b = crate::brush::interpolate(a, operation(a), s);
                if mask {
                    b = [b[0], b[0], b[0], 255];
                }
                if a != b {
                    raster.set(x, y, b);
                }
            }
        }
    }
    let l = doc.layer_mut(id).unwrap();
    if mask {
        l.mask.as_mut().unwrap().raster = Arc::new(raster);
    } else {
        l.image = Some(Arc::new(raster));
        l.text = None;
        l.shape = None;
    }
    Ok(())
}
pub fn fill(doc: &mut Document, id: Uuid, mask: bool, color: [u8; 4]) -> Result<()> {
    edit_pixels(doc, id, mask, |_| color)
}
pub fn clear(doc: &mut Document, id: Uuid, mask: bool) -> Result<()> {
    edit_pixels(doc, id, mask, |_| if mask { [255; 4] } else { [0; 4] })
}

/// Fills the current selection on the layer's native grid, padding it when the selection crosses its edges.
pub fn content_aware_fill_selected(doc: &mut Document, id: Uuid) -> Result<()> {
    let original = doc
        .layer(id)
        .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?
        .clone();
    ensure!(!original.is_group, "内容识别填充需要像素图层");
    let image = original
        .image
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("图层没有源像素"))?;
    let selection = doc
        .selection
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("内容识别填充需要选区"))?;
    let [x0, y0, x1, y1] = selection
        .outline()
        .bounds()
        .ok_or_else(|| anyhow::anyhow!("选区没有覆盖画布"))?;
    let t = original.transform;
    ensure!(
        t.size.iter().all(|v| v.is_finite() && *v > 0.)
            && t.origin.iter().all(|v| v.is_finite())
            && t.rotation.is_finite()
            && Document::valid_size(image.width, image.height),
        "图层变换参数无效"
    );
    let w = image.width as f64;
    let h = image.height as f64;
    let corners = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]].map(|p| {
        let uv = t.from_document([p[0] as f64, p[1] as f64]);
        [uv[0] * w, uv[1] * h]
    });
    let left = corners.iter().map(|p| p[0]).fold(0., f64::min).floor() as i64;
    let top = corners.iter().map(|p| p[1]).fold(0., f64::min).floor() as i64;
    let right = corners.iter().map(|p| p[0]).fold(w, f64::max).ceil() as i64;
    let bottom = corners.iter().map(|p| p[1]).fold(h, f64::max).ceil() as i64;
    let nw = right.saturating_sub(left);
    let nh = bottom.saturating_sub(top);
    ensure!(
        nw > 0
            && nh > 0
            && nw <= crate::model::MAX_SIDE as i64
            && nh <= crate::model::MAX_SIDE as i64
            && nw as u64 * nh as u64 <= MAX_PIXELS,
        "内容识别填充超出图像内存上限"
    );
    let mut grown = vec![0; nw as usize * nh as usize * 4];
    let source = image.to_rgba();
    for y in 0..image.height as usize {
        let start = ((y as i64 - top) as usize * nw as usize + (-left) as usize) * 4;
        let source_start = y * image.width as usize * 4;
        grown[start..start + image.width as usize * 4]
            .copy_from_slice(&source[source_start..source_start + image.width as usize * 4]);
    }
    let center = t.to_document([
        ((left + right) as f64 / 2.) / w,
        ((top + bottom) as f64 / 2.) / h,
    ]);
    let mut transform = t;
    transform.size = [nw as f64 * t.size[0] / w, nh as f64 * t.size[1] / h];
    transform.origin = [
        center[0] - transform.size[0] / 2.,
        center[1] - transform.size[1] / 2.,
    ];
    let mut mask = vec![0; nw as usize * nh as usize];
    for y in 0..nh as u32 {
        for x in 0..nw as u32 {
            let point =
                transform.to_document([(x as f64 + 0.5) / nw as f64, (y as f64 + 0.5) / nh as f64]);
            if point[0] >= 0.
                && point[1] >= 0.
                && point[0] < doc.width as f64
                && point[1] < doc.height as f64
            {
                mask[(y as u64 * nw as u64 + x as u64) as usize] =
                    selection.get(point[0].floor() as u32, point[1].floor() as u32);
            }
        }
    }
    let result = crate::brush::content_aware_fill(
        &Raster::from_rgba(nw as u32, nh as u32, grown),
        &Selection::from_coverage(nw as u32, nh as u32, mask),
    )?;
    let layer = doc.layer_mut(id).unwrap();
    if let Some(mask) = &mut layer.mask
        && mask.placement.is_none()
    {
        mask.placement = Some(t);
    }
    layer.image = Some(Arc::new(result));
    layer.transform = transform;
    layer.text = None;
    layer.shape = None;
    Ok(())
}
pub fn invert_pixels(doc: &mut Document, id: Uuid, mask: bool) -> Result<()> {
    edit_pixels(doc, id, mask, |p| {
        [255 - p[0], 255 - p[1], 255 - p[2], p[3]]
    })
}

pub fn layer_selection(doc: &Document, id: Uuid, mask: bool) -> Result<Selection> {
    let l = doc
        .layer(id)
        .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?;
    let (r, t) = if mask {
        let m = l
            .mask
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("图层没有蒙版"))?;
        (m.raster.as_ref(), m.placement.unwrap_or(l.transform))
    } else {
        (
            l.image
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("图层没有像素"))?,
            l.transform,
        )
    };
    let mut data = vec![0; doc.width as usize * doc.height as usize];
    for y in 0..doc.height {
        for x in 0..doc.width {
            let unit = t.from_document([x as f64 + 0.5, y as f64 + 0.5]);
            if unit.iter().all(|v| *v >= 0. && *v < 1.) {
                let p = crate::brush::sample(
                    r,
                    unit[0] * r.width as f64 - 0.5,
                    unit[1] * r.height as f64 - 0.5,
                );
                data[(y * doc.width + x) as usize] = if mask { 255 - p[0] } else { p[3] };
            }
        }
    }
    Ok(Selection::from_coverage(doc.width, doc.height, data))
}

pub fn apply_mask(doc: &mut Document, id: Uuid) -> Result<()> {
    let l = doc
        .layer(id)
        .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?
        .clone();
    let mask = l
        .mask
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("图层没有蒙版"))?;
    ensure!(!l.is_group, "应用组蒙版前请先合并该组");
    let mut image = l
        .image
        .as_deref()
        .cloned()
        .unwrap_or_else(|| Raster::blank(doc.width, doc.height));
    if mask.enabled {
        let t = mask.placement.unwrap_or(l.transform);
        let background = crate::render::mask_background(&mask.raster);
        for y in 0..image.height {
            for x in 0..image.width {
                let docp = l.transform.to_document([
                    (x as f64 + 0.5) / image.width as f64,
                    (y as f64 + 0.5) / image.height as f64,
                ]);
                let unit = t.from_document(docp);
                let coverage = (crate::render::sample_mask(
                    &mask.raster,
                    unit[0] * mask.raster.width as f64 - 0.5,
                    unit[1] * mask.raster.height as f64 - 0.5,
                    background,
                ) * 255.)
                    .round() as u8;
                let mut p = image.get(x, y);
                p[3] = ((p[3] as u16 * coverage as u16 + 127) / 255) as u8;
                image.set(x, y, p);
            }
        }
    }
    let l = doc.layer_mut(id).unwrap();
    l.image = Some(Arc::new(image));
    l.mask = None;
    l.text = None;
    l.shape = None;
    Ok(())
}

pub fn set_mask_linked(doc: &mut Document, id: Uuid, linked: bool) -> Result<()> {
    let l = doc
        .layer_mut(id)
        .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?;
    let m = l
        .mask
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("图层没有蒙版"))?;
    if !linked && m.placement.is_none() {
        m.placement = Some(l.transform);
    }
    m.linked = linked;
    Ok(())
}

pub fn trim(doc: &mut Document, basis: TrimBasis, tolerance: u8, sides: [bool; 4]) -> Result<()> {
    let image = crate::render::composite(doc);
    let b = trim_bounds(&image, basis, tolerance, sides)
        .ok_or_else(|| anyhow::anyhow!("修边后没有剩余内容"))?;
    crop(doc, b)
}

pub fn merge_layers(doc: &mut Document, ids: &[Uuid]) -> Result<Uuid> {
    ensure!(!ids.is_empty(), "请先选择图层");
    let mut selected = ids
        .iter()
        .flat_map(|&id| descendants(doc, id))
        .collect::<HashSet<_>>();
    if ids.len() == 1 {
        let active = doc
            .layer(ids[0])
            .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?;
        if !active.is_group {
            let index = doc.layers.iter().position(|l| l.id == active.id).unwrap();
            let below = doc.layers[..index]
                .iter()
                .rev()
                .find(|l| l.parent_id == active.parent_id && !l.is_group)
                .ok_or_else(|| anyhow::anyhow!("同组内没有下方图层"))?;
            selected.insert(below.id);
        }
    }
    let anchor = doc
        .layers
        .iter()
        .rposition(|l| selected.contains(&l.id))
        .ok_or_else(|| anyhow::anyhow!("选中的图层已不存在"))?;
    let name = doc.layers[anchor].name.clone();
    let parent = doc.layers[anchor]
        .parent_id
        .filter(|p| !selected.contains(p));
    let mut subset = doc.clone();
    subset.layers.retain(|l| selected.contains(&l.id));
    for l in &mut subset.layers {
        if l.parent_id.is_some_and(|id| !selected.contains(&id)) {
            l.parent_id = None;
        }
        if l.mask_source_id.is_some_and(|id| !selected.contains(&id)) {
            l.mask_source_id = None;
        }
    }
    let composite = crate::render::composite(&subset);
    let b =
        trim_bounds(&composite, TrimBasis::Transparent, 0, [true; 4]).unwrap_or([0., 0., 1., 1.]);
    let mut raster = Raster::blank(b[2] as u32, b[3] as u32);
    for y in 0..raster.height {
        for x in 0..raster.width {
            raster.set(x, y, composite.get(x + b[0] as u32, y + b[1] as u32));
        }
    }
    let mut merged = Layer::raster(name, raster);
    merged.parent_id = parent;
    merged.transform.origin = [b[0], b[1]];
    let id = merged.id;
    let insertion = doc.layers[..anchor]
        .iter()
        .filter(|l| !selected.contains(&l.id))
        .count();
    doc.layers.retain(|l| !selected.contains(&l.id));
    for l in &mut doc.layers {
        if l.mask_source_id.is_some_and(|id| selected.contains(&id)) {
            l.mask_source_id = Some(id);
        }
    }
    doc.layers.insert(insertion.min(doc.layers.len()), merged);
    doc.active_layer_id = Some(id);
    Ok(id)
}

/// Bake a projective corner transform into pixels while preserving independent masks.
pub fn free_distort(
    doc: &mut Document,
    id: Uuid,
    corners: [[f64; 2]; 4],
    mask_only: bool,
) -> Result<()> {
    ensure!(
        corners.iter().flatten().all(|v| v.is_finite()),
        "变形角点无效"
    );
    let l = doc
        .layer(id)
        .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?
        .clone();
    let (source, old) = if mask_only {
        let mask = l
            .mask
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("图层没有蒙版"))?;
        (mask.raster.as_ref(), mask.placement.unwrap_or(l.transform))
    } else {
        (
            l.image
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("图层没有像素"))?,
            l.transform,
        )
    };
    let minx = corners
        .iter()
        .map(|p| p[0])
        .fold(f64::INFINITY, f64::min)
        .floor();
    let miny = corners
        .iter()
        .map(|p| p[1])
        .fold(f64::INFINITY, f64::min)
        .floor();
    let w = (corners
        .iter()
        .map(|p| p[0])
        .fold(f64::NEG_INFINITY, f64::max)
        .ceil()
        - minx)
        .max(1.) as u32;
    let h = (corners
        .iter()
        .map(|p| p[1])
        .fold(f64::NEG_INFINITY, f64::max)
        .ceil()
        - miny)
        .max(1.) as u32;
    ensure!(
        w as u64 * h as u64 <= MAX_PIXELS,
        "变形后的图层超出图像大小上限"
    );
    // The kernel accepts a source and destination of equal size. Resample to that grid first.
    let mut data = vec![0; w as usize * h as usize * 4];
    for y in 0..h {
        for x in 0..w {
            let sx = (x as f64 + 0.5) * source.width as f64 / w as f64 - 0.5;
            let sy = (y as f64 + 0.5) * source.height as f64 / h as f64 - 0.5;
            let mut p = crate::brush::sample(
                source,
                if old.flip_x {
                    source.width as f64 - 1. - sx
                } else {
                    sx
                },
                if old.flip_y {
                    source.height as f64 - 1. - sy
                } else {
                    sy
                },
            );
            if mask_only && p[3] < 255 {
                p = [source.get(0, 0)[0]; 4];
                p[3] = 255;
            }
            let i = ((y * w + x) * 4) as usize;
            data[i..i + 4].copy_from_slice(&p);
        }
    }
    crate::pixels::premultiply(&mut data);
    let local = corners.map(|p| [p[0] - minx, p[1] - miny]);
    let mut data = crate::pixels::perspective_warp(&data, w as usize, h as usize, local)
        .map_err(anyhow::Error::msg)?;
    crate::pixels::unpremultiply(&mut data);
    if mask_only {
        for p in data.as_chunks_mut::<4>().0 {
            if p[3] == 0 {
                p.fill(255);
            } else {
                p[1] = p[0];
                p[2] = p[0];
                p[3] = 255;
            }
        }
    }
    let l = doc.layer_mut(id).unwrap();
    let t = Transform::new(minx, miny, w as f64, h as f64);
    if mask_only {
        let m = l.mask.as_mut().unwrap();
        m.raster = Arc::new(Raster::from_rgba(w, h, data));
        m.placement = Some(t);
    } else {
        if let Some(m) = &mut l.mask
            && m.placement.is_none()
        {
            m.placement = Some(l.transform);
        }
        l.image = Some(Arc::new(Raster::from_rgba(w, h, data)));
        l.transform = t;
        l.text = None;
        l.shape = None;
    }
    Ok(())
}

pub fn selected_pixels(doc: &Document, composite: &Raster) -> Option<(Raster, [f64; 2])> {
    let b = doc
        .selection
        .as_ref()
        .and_then(Selection::bounds)
        .unwrap_or([0, 0, doc.width, doc.height]);
    if doc.selection.as_ref().is_some_and(|s| s.bounds().is_none()) {
        return None;
    }
    let w = b[2] - b[0];
    let h = b[3] - b[1];
    let mut out = Raster::blank(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut p = composite.get(x + b[0], y + b[1]);
            let coverage = doc
                .selection
                .as_ref()
                .map_or(255, |s| s.get(x + b[0], y + b[1]));
            p[3] = ((p[3] as u16 * coverage as u16 + 127) / 255) as u8;
            out.set(x, y, p);
        }
    }
    Some((out, [b[0] as f64, b[1] as f64]))
}

/// Moves the selected pixels in their native grid, expanding only the necessary layer extent.
pub fn move_selected_pixels(
    doc: &mut Document,
    id: Uuid,
    mask: bool,
    offset: [f64; 2],
    duplicate: bool,
) -> Result<()> {
    ensure!(offset.iter().all(|v| v.is_finite()), "像素移动偏移无效");
    let selection = doc
        .selection
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("移动像素需要选区"))?
        .clone();
    let Some(bounds) = selection.bounds() else {
        return Ok(());
    };
    let layer = doc
        .layer(id)
        .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?
        .clone();
    let (source, old) = if mask {
        let m = layer
            .mask
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("图层没有蒙版"))?;
        (m.raster.clone(), m.placement.unwrap_or(layer.transform))
    } else {
        (
            layer
                .image
                .clone()
                .ok_or_else(|| anyhow::anyhow!("图层没有像素"))?,
            layer.transform,
        )
    };
    let corners = [
        [bounds[0] as f64 + offset[0], bounds[1] as f64 + offset[1]],
        [bounds[2] as f64 + offset[0], bounds[1] as f64 + offset[1]],
        [bounds[2] as f64 + offset[0], bounds[3] as f64 + offset[1]],
        [bounds[0] as f64 + offset[0], bounds[3] as f64 + offset[1]],
    ]
    .map(|p| old.from_document(p));
    let left = corners
        .iter()
        .map(|p| p[0] * source.width as f64)
        .fold(0., f64::min)
        .floor();
    let top = corners
        .iter()
        .map(|p| p[1] * source.height as f64)
        .fold(0., f64::min)
        .floor();
    let right = corners
        .iter()
        .map(|p| p[0] * source.width as f64)
        .fold(source.width as f64, f64::max)
        .ceil();
    let bottom = corners
        .iter()
        .map(|p| p[1] * source.height as f64)
        .fold(source.height as f64, f64::max)
        .ceil();
    let w = (right - left) as u32;
    let h = (bottom - top) as u32;
    ensure!(
        w > 0
            && h > 0
            && w <= crate::model::MAX_SIDE
            && h <= crate::model::MAX_SIDE
            && w as u64 * h as u64 <= MAX_PIXELS,
        "像素移动超出图层内存上限"
    );
    let center = old.to_document([
        (left + right) / 2. / source.width as f64,
        (top + bottom) / 2. / source.height as f64,
    ]);
    let mut t = old;
    t.size = [
        w as f64 * old.size[0] / source.width as f64,
        h as f64 * old.size[1] / source.height as f64,
    ];
    t.origin = [center[0] - t.size[0] / 2., center[1] - t.size[1] / 2.];
    let source_sample = |p: [f64; 2]| {
        let unit = old.from_document(p);
        if mask
            && (source.width == 1 && source.height == 1 || unit.iter().any(|v| *v < 0. || *v >= 1.))
        {
            if source.width == 1 && source.height == 1 {
                source.get(0, 0)
            } else {
                [255; 4]
            }
        } else {
            crate::brush::sample(
                &source,
                unit[0] * source.width as f64 - 0.5,
                unit[1] * source.height as f64 - 0.5,
            )
        }
    };
    let coverage = |p: [f64; 2]| {
        if p[0] < 0. || p[1] < 0. {
            0.
        } else {
            selection.get(p[0].floor() as u32, p[1].floor() as u32) as f64 / 255.
        }
    };
    let mut out = Raster::blank(w, h);
    for y in 0..h {
        for x in 0..w {
            let p = t.to_document([(x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64]);
            let mut base = source_sample(p);
            if !duplicate {
                let amount = coverage(p);
                if mask {
                    base = crate::brush::interpolate(base, [255; 4], amount);
                } else {
                    base[3] = (base[3] as f64 * (1. - amount)).round() as u8;
                }
            }
            let from = [p[0] - offset[0], p[1] - offset[1]];
            let color = source_sample(from);
            let amount = coverage(from);
            let result = if mask {
                crate::brush::interpolate(base, color, amount)
            } else {
                crate::brush::over(base, color, amount)
            };
            if result != [0; 4] {
                out.set(x, y, result);
            }
        }
    }
    let l = doc.layer_mut(id).unwrap();
    if mask {
        let m = l.mask.as_mut().unwrap();
        m.raster = Arc::new(out);
        m.placement = Some(t);
    } else {
        if let Some(m) = &mut l.mask
            && m.placement.is_none()
        {
            m.placement = Some(l.transform);
        }
        l.image = Some(Arc::new(out));
        l.transform = t;
        l.text = None;
        l.shape = None;
    }
    doc.selection = Some(crate::selection::translated(
        &selection,
        offset[0].round() as i32,
        offset[1].round() as i32,
        doc.width,
        doc.height,
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Mask;
    #[test]
    fn groups_duplicate_with_ids_and_reject_cycles() {
        let mut d = Document::new(10, 10);
        let child = d.active_layer_id.unwrap();
        let group = add_group(&mut d, "Group", &[child]).unwrap();
        assert_eq!(d.layer(child).unwrap().parent_id, Some(group));
        assert!(place_layer(&mut d, group, Some(group), None).is_err());
        let copies = duplicate_layers(&mut d, &[group]).unwrap();
        assert_eq!(copies.len(), 2);
        let copy = d
            .layers
            .iter()
            .find(|l| l.id != group && l.is_group)
            .unwrap();
        assert!(d.layers.iter().any(|l| l.parent_id == Some(copy.id)));
        delete_layers(&mut d, &[group]).unwrap();
        assert!(d.layer(child).is_none());
    }
    #[test]
    fn crop_moves_all_geometry_and_selection() {
        let mut d = Document::new(20, 20);
        add_guide(&mut d, false, 10.).unwrap();
        d.selection = Some(crate::selection::marquee(
            20,
            20,
            [5., 5., 5., 5.],
            false,
            false,
        ));
        crop(&mut d, [5., 5., 10., 10.]).unwrap();
        assert_eq!(d.width, 10);
        assert_eq!(d.active().unwrap().transform.origin, [-5., -5.]);
        assert_eq!(d.guides[0].position, 5.);
        assert_eq!(d.selection.as_ref().unwrap().get(0, 0), 255);
    }
    #[test]
    fn shapes_have_transparent_corners_and_redraw_radius() {
        let style = json!({"kind":"Ellipse","red":1.});
        let r = shape_raster(&style, 20, 20).unwrap();
        assert_eq!(r.get(0, 0)[3], 0);
        assert_eq!(r.get(10, 10), [255, 0, 0, 255]);
        let style = json!({"kind":"Rectangle","red":1.,"cornerRadius":5.});
        let r = shape_raster(&style, 20, 20).unwrap();
        assert_eq!(r.get(0, 0)[3], 0);
        assert_eq!(r.get(5, 0)[3], 255);
    }
    #[test]
    fn shape_move_rotation_sampling_and_flip_share_pixels_but_size_redraws() {
        let mut doc = Document::new(80, 80);
        let id = add_shape(
            &mut doc,
            ShapeKind::Rectangle,
            [10., 12., 20., 16.],
            [255, 0, 0, 255],
            5.,
            1.,
            None,
        )
        .unwrap();
        let image = doc.layer(id).unwrap().image.as_ref().unwrap().clone();
        let mut t = doc.layer(id).unwrap().transform;
        t.origin = [20., 25.];
        set_transform(&mut doc, id, t, false).unwrap();
        assert!(Arc::ptr_eq(
            doc.layer(id).unwrap().image.as_ref().unwrap(),
            &image
        ));
        t.rotation = 42.;
        t.flip_x = true;
        t.sampling = Sampling::Nearest;
        set_transform(&mut doc, id, t, false).unwrap();
        assert!(Arc::ptr_eq(
            doc.layer(id).unwrap().image.as_ref().unwrap(),
            &image
        ));
        t.size = [40., 32.];
        set_transform(&mut doc, id, t, false).unwrap();
        let resized = doc.layer(id).unwrap().image.as_ref().unwrap();
        assert!(!Arc::ptr_eq(resized, &image));
        assert_eq!([resized.width, resized.height], [40, 32]);
        assert_eq!(resized.get(0, 0)[3], 0);
        assert_eq!(resized.get(5, 0)[3], 255);
        assert_eq!(
            doc.layer(id).unwrap().shape.as_ref().unwrap()["cornerRadius"],
            5.
        );
    }
    #[test]
    fn foreground_alpha_is_kept_in_shape_layer_opacity_after_redraw() {
        for alpha in [0, 128, 255] {
            let mut doc = Document::new(40, 40);
            let id = add_shape(
                &mut doc,
                ShapeKind::Rectangle,
                [5., 5., 12., 12.],
                [255, 0, 0, alpha],
                0.,
                1.,
                None,
            )
            .unwrap();
            assert_eq!(doc.layer(id).unwrap().opacity, alpha as f64 / 255.);
            assert_eq!(crate::render::render_document(&doc).get(10, 10)[3], alpha);
            let mut t = doc.layer(id).unwrap().transform;
            t.size = [24., 24.];
            set_transform(&mut doc, id, t, false).unwrap();
            assert_eq!(crate::render::render_document(&doc).get(10, 10)[3], alpha);
            assert!(
                doc.layer(id)
                    .unwrap()
                    .shape
                    .as_ref()
                    .unwrap()
                    .get("alpha")
                    .is_none()
            );
        }
    }
    #[test]
    fn shape_drag_preserves_negative_slope_line_endpoints_and_stroke_width() {
        let (rect, ends) =
            shape_drag_geometry(ShapeKind::Line, [10., 30.], [30., 10.], false, false, 4.);
        assert_eq!(rect, [8., 8., 24., 24.]);
        let [a, b] = ends.unwrap();
        assert!(a[0] < b[0] && a[1] > b[1]);
        let mut doc = Document::new(64, 64);
        let id = add_shape(
            &mut doc,
            ShapeKind::Line,
            rect,
            [255, 0, 0, 255],
            0.,
            4.,
            Some([a, b]),
        )
        .unwrap();
        let image = doc.layer(id).unwrap().image.as_ref().unwrap();
        assert_eq!(image.get(12, 12), [255, 0, 0, 255]);
        assert_eq!(image.get(2, 2)[3], 0);
        for (end, point) in [a, b].into_iter().zip([[10., 30.], [30., 10.]]) {
            assert!((rect[0] + end[0] * rect[2] - point[0]).abs() < 1e-9);
            assert!((rect[1] + end[1] * rect[3] - point[1]).abs() < 1e-9);
        }
        let (horizontal, _) =
            shape_drag_geometry(ShapeKind::Line, [10., 10.], [30., 10.], false, false, 5.);
        assert_eq!(horizontal, [7.5, 7.5, 25., 5.]);
        let (vertical, _) =
            shape_drag_geometry(ShapeKind::Line, [10., 10.], [10., 30.], false, false, 5.);
        assert_eq!(vertical, [7.5, 7.5, 5., 25.]);
    }
    #[test]
    fn shape_drag_square_center_and_eighth_turn_snap_match_original() {
        assert_eq!(
            shape_drag_geometry(ShapeKind::Rectangle, [10., 10.], [6., 12.], true, true, 4.).0,
            [6., 6., 8., 8.]
        );
        let (rect, ends) =
            shape_drag_geometry(ShapeKind::Line, [10., 10.], [30., 25.], true, false, 4.);
        let [a, b] = ends.unwrap();
        let delta = [(b[0] - a[0]) * rect[2], (b[1] - a[1]) * rect[3]];
        assert!((delta[0] - delta[1]).abs() < 1e-9);
        assert!((delta[0].hypot(delta[1]) - 25.).abs() < 1e-9);
        let (flat, _) =
            shape_drag_geometry(ShapeKind::Line, [10., 10.], [30., 11.], true, false, 4.);
        assert!((flat[3] - 4.).abs() < 1e-9);
    }
    #[test]
    fn content_fill_extends_native_grid_into_selection_and_preserves_mask_placement() {
        let mut doc = Document::new(80, 64);
        let id = doc.active_layer_id.unwrap();
        let original = Arc::new(Raster::solid(24, 32, [51, 153, 204, 255]));
        let layer = doc.active_mut().unwrap();
        layer.image = Some(original.clone());
        layer.transform = Transform::new(20., 10., 24., 32.);
        layer.mask = Some(Mask::white());
        doc.selection = Some(crate::selection::marquee(
            80,
            64,
            [44., 10., 8., 32.],
            false,
            false,
        ));
        content_aware_fill_selected(&mut doc, id).unwrap();
        let layer = doc.layer(id).unwrap();
        assert_eq!(layer.transform, Transform::new(20., 10., 32., 32.));
        assert_eq!(
            layer.mask.as_ref().unwrap().placement,
            Some(Transform::new(20., 10., 24., 32.))
        );
        let image = layer.image.as_ref().unwrap();
        assert_eq!([image.width, image.height], [32, 32]);
        for y in 0..32 {
            for x in 0..32 {
                assert_eq!(image.get(x, y), [51, 153, 204, 255]);
            }
        }
        assert_eq!([original.width, original.height], [24, 32]);
        assert_eq!(
            doc.selection.as_ref().unwrap().bounds(),
            Some([44, 10, 52, 42])
        );
    }
    #[test]
    fn content_fill_rotation_flip_and_no_source_failures_leave_unselected_pixels_unchanged() {
        let mut doc = Document::new(80, 80);
        let id = doc.active_layer_id.unwrap();
        let original = Arc::new(Raster::solid(20, 20, [40, 120, 200, 255]));
        let mut t = Transform::new(25., 25., 30., 30.);
        t.rotation = 37.;
        t.flip_x = true;
        doc.active_mut().unwrap().image = Some(original.clone());
        doc.active_mut().unwrap().transform = t;
        let point = t.to_document([1.08, 0.5]);
        doc.selection = Some(crate::selection::marquee(
            80,
            80,
            [point[0] - 2., point[1] - 2., 4., 4.],
            false,
            false,
        ));
        content_aware_fill_selected(&mut doc, id).unwrap();
        let layer = doc.layer(id).unwrap();
        assert!(layer.transform.flip_x);
        assert_eq!(layer.transform.rotation, 37.);
        for y in 0..20 {
            for x in 0..20 {
                let p = t.to_document([(x as f64 + 0.5) / 20., (y as f64 + 0.5) / 20.]);
                let uv = layer.transform.from_document(p);
                let image = layer.image.as_ref().unwrap();
                assert_eq!(
                    image.get(
                        (uv[0] * image.width as f64).floor() as u32,
                        (uv[1] * image.height as f64).floor() as u32
                    ),
                    original.get(x, y)
                );
            }
        }
        doc.selection = Some(Selection::full(80, 80));
        let before = doc.clone();
        assert!(content_aware_fill_selected(&mut doc, id).is_err());
        assert_eq!(doc, before);
    }
    #[test]
    fn trim_finds_alpha_bounds_and_obeys_sides() {
        let mut r = Raster::blank(10, 10);
        r.set(3, 4, [255; 4]);
        assert_eq!(
            trim_bounds(&r, TrimBasis::Transparent, 0, [true; 4]),
            Some([3., 4., 1., 1.])
        );
        assert_eq!(
            trim_bounds(&r, TrimBasis::Transparent, 0, [false, true, true, true]),
            Some([0., 4., 4., 1.])
        );
    }
    #[test]
    fn resize_rasterizes_in_document_axes() {
        let mut d = Document::new(10, 10);
        let image = Arc::new(Raster::solid(10, 10, [255; 4]));
        d.active_mut().unwrap().image = Some(image);
        resize_image(&mut d, 5, 5, Sampling::High).unwrap();
        assert_eq!(d.active().unwrap().image.as_ref().unwrap().width, 5);
        assert_eq!(d.active().unwrap().transform.size, [5., 5.]);
    }
    #[test]
    fn applied_mask_bakes_alpha() {
        let mut d = Document::new(5, 5);
        let id = d.active_layer_id.unwrap();
        d.active_mut().unwrap().image = Some(Arc::new(Raster::solid(5, 5, [255, 0, 0, 255])));
        crate::brush::add_mask(&mut d, id, false).unwrap();
        apply_mask(&mut d, id).unwrap();
        assert!(d.active().unwrap().mask.is_none());
        assert_eq!(d.active().unwrap().image.as_ref().unwrap().get(2, 2)[3], 0);
    }
    #[test]
    fn selected_pixel_move_preserves_native_grid_and_cuts_once() {
        let mut d = Document::new(20, 20);
        let id = d.active_layer_id.unwrap();
        d.active_mut().unwrap().image = Some(Arc::new(Raster::solid(20, 20, [255, 0, 0, 255])));
        d.selection = Some(crate::selection::marquee(
            20,
            20,
            [4., 4., 3., 3.],
            false,
            false,
        ));
        move_selected_pixels(&mut d, id, false, [5., 0.], false).unwrap();
        let r = d.active().unwrap().image.as_ref().unwrap();
        assert_eq!(r.get(5, 5)[3], 0);
        assert_eq!(r.get(10, 5), [255, 0, 0, 255]);
        assert_eq!(d.selection.as_ref().unwrap().bounds(), Some([9, 4, 12, 7]));
    }
    #[test]
    fn group_transform_decomposes_rotated_nonuniform_axes() {
        let value = Transform {
            rotation: 90.,
            ..Transform::new(5., 5., 10., 4.)
        };
        let old = Transform::new(0., 0., 20., 20.);
        let new = Transform::new(0., 0., 40., 60.);
        let result = follow_transform(value, old, new);
        assert!((result.size[0] - 30.).abs() < 1e-6);
        assert!((result.size[1] - 8.).abs() < 1e-6);
        assert!((result.rotation - 90.).abs() < 1e-6);
    }
    #[test]
    fn unlinked_mask_keeps_document_placement() {
        let mut d = Document::new(10, 10);
        let id = d.active_layer_id.unwrap();
        crate::brush::add_mask(&mut d, id, true).unwrap();
        set_mask_linked(&mut d, id, false).unwrap();
        let old = d.active().unwrap().mask.as_ref().unwrap().placement;
        set_transform(&mut d, id, Transform::new(3., 3., 10., 10.), false).unwrap();
        assert_eq!(d.active().unwrap().mask.as_ref().unwrap().placement, old);
    }

    #[test]
    fn moving_layers_preserves_unlinked_mask_and_carries_linked_placement() {
        let mut d = Document::new(32, 32);
        let id = d.active_layer_id.unwrap();
        d.active_mut().unwrap().mask = Some(Mask {
            raster: Arc::new(Raster::solid(4, 4, [255; 4])),
            enabled: true,
            linked: false,
            placement: None,
        });
        let original = d.active().unwrap().transform;
        move_layers(&mut d, &[id], [10., -3.]).unwrap();
        assert_eq!(
            d.active().unwrap().mask.as_ref().unwrap().placement,
            Some(original)
        );
        set_mask_linked(&mut d, id, true).unwrap();
        move_layers(&mut d, &[id], [2., 4.]).unwrap();
        let placement = d
            .active()
            .unwrap()
            .mask
            .as_ref()
            .unwrap()
            .placement
            .unwrap();
        assert_eq!(placement.origin, [2., 4.]);
        let before = d.active().unwrap().mask.as_ref().unwrap().placement;
        crop(&mut d, [4., 4., 24., 24.]).unwrap();
        let mut expected = before.unwrap();
        expected.origin[0] -= 4.;
        expected.origin[1] -= 4.;
        assert_eq!(
            d.active().unwrap().mask.as_ref().unwrap().placement,
            Some(expected)
        );
    }
}
