//! Original-source viewport previews. Source pixels and layer effects survive view-only edits.
use crate::model::{Document, Mask, Raster, Transform};
use crate::{filters, render};
use rayon::prelude::*;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;

/// An integer document rectangle and the physical pixels needed to show it.
/// At 100% and above the output retains one texel per document pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewRequest {
    pub region: [u32; 4],
    pub width: u32,
    pub height: u32,
}
impl ViewRequest {
    pub fn new(canvas: [u32; 2], visible: [f64; 4], scale: f64) -> Option<Self> {
        if !scale.is_finite() || scale <= 0.0 || visible.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let x = visible[0].floor().clamp(0.0, canvas[0] as f64) as u32;
        let y = visible[1].floor().clamp(0.0, canvas[1] as f64) as u32;
        let right = (visible[0] + visible[2])
            .ceil()
            .clamp(0.0, canvas[0] as f64) as u32;
        let bottom = (visible[1] + visible[3])
            .ceil()
            .clamp(0.0, canvas[1] as f64) as u32;
        if right <= x || bottom <= y {
            return None;
        }
        let region = [x, y, right - x, bottom - y];
        Some(Self {
            region,
            width: (region[2] as f64 * scale.min(1.0)).ceil().max(1.0) as u32,
            height: (region[3] as f64 * scale.min(1.0)).ceil().max(1.0) as u32,
        })
    }
    pub fn native(self) -> bool {
        self.width == self.region[2] && self.height == self.region[3]
    }
    pub fn document_region(self) -> [f64; 4] {
        self.region.map(f64::from)
    }
    /// Crop a full-resolution filter result without uploading the entire image.
    pub fn sample(self, raster: &Raster) -> Raster {
        let sampler = (!self.native()).then(|| {
            render::LayerSampler::new(
                raster,
                crate::model::Sampling::High,
                [
                    raster.width as f64 * self.width as f64 / self.region[2] as f64,
                    raster.height as f64 * self.height as f64 / self.region[3] as f64,
                ],
            )
        });
        let mut pixels = vec![0; self.width as usize * self.height as usize * 4];
        pixels
            .par_chunks_exact_mut(self.width as usize * 4)
            .enumerate()
            .for_each(|(y, row)| {
                for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let source = [
                        self.region[0] as f64
                            + (x as f64 + 0.5) * self.region[2] as f64 / self.width as f64,
                        self.region[1] as f64
                            + (y as f64 + 0.5) * self.region[3] as f64 / self.height as f64,
                    ];
                    if let Some(sampler) = &sampler {
                        let sampled = sampler.sample([
                            source[0] / raster.width as f64,
                            source[1] / raster.height as f64,
                        ]);
                        let alpha = sampled[3].clamp(0.0, 1.0);
                        if alpha > 0.0 {
                            for channel in 0..3 {
                                pixel[channel] =
                                    (sampled[channel] / alpha * 255.0).round().clamp(0.0, 255.0)
                                        as u8;
                            }
                        }
                        pixel[3] = (alpha * 255.0).round() as u8;
                    } else {
                        *pixel = raster.get(source[0] as u32, source[1] as u32);
                    }
                }
            });
        Raster::from_rgba(self.width, self.height, pixels)
    }
}

#[derive(Default)]
pub struct PreviewCache {
    sources: HashMap<(Uuid, bool), SourceEntry>,
    effects: HashMap<Uuid, EffectEntry>,
    output: Option<(Document, u32, Arc<Raster>)>,
    view_output: Option<(Document, ViewRequest, Arc<Raster>)>,
    full_output: Option<(Document, Arc<Raster>)>,
}
struct SourceEntry {
    original: Arc<Raster>,
    width: u32,
    height: u32,
    image: Arc<Raster>,
}
struct EffectEntry {
    image: Arc<Raster>,
    mask: Option<Mask>,
    independent_transform: Option<Transform>,
    settings: Value,
    result: (Raster, f64),
}

/// Prepare sources at their projected pixel density, retaining document-space transforms.
pub fn prepare(doc: &Document, max_side: u32) -> Document {
    PreviewCache::default().prepare(doc, max_side)
}
impl PreviewCache {
    pub fn clear(&mut self) {
        self.sources.clear();
        self.effects.clear();
        self.output = None;
        self.view_output = None;
        self.full_output = None;
    }
    pub fn prepare(&mut self, doc: &Document, max_side: u32) -> Document {
        let scale = preview_scale(doc, max_side);
        self.sources.retain(|(id, mask), _| {
            doc.layer(*id).is_some_and(|layer| {
                if *mask {
                    layer.mask.is_some()
                } else {
                    layer.image.is_some()
                }
            })
        });
        self.effects.retain(|id, _| {
            doc.layer(*id)
                .is_some_and(|l| l.effects.is_some() && l.image.is_some())
        });
        let mut prepared = doc.clone();
        prepared.selection = None;
        for layer in &mut prepared.layers {
            if let Some(image) = &layer.image {
                let shrink = source_scale(image, &layer.transform, scale);
                let (width, height) = scaled_size(image, shrink);
                layer.image = Some(self.source((layer.id, false), image, width, height));
                if let Some(effects) = &mut layer.effects {
                    scale_effects(effects, shrink);
                }
            }
            if let Some(mask) = &mut layer.mask {
                let placement = mask.placement.as_ref().unwrap_or(&layer.transform);
                let shrink = source_scale(&mask.raster, placement, scale);
                let (width, height) = scaled_size(&mask.raster, shrink);
                mask.raster = self.source((layer.id, true), &mask.raster, width, height);
            }
            if let Some(settings) = &mut layer.adjustment {
                scale_adjustment(settings, scale);
            }
        }
        prepared
    }
    /// Unchanged compositing state returns the same immutable result without a pixel copy.
    pub fn render(&mut self, doc: &Document, max_side: u32) -> Arc<Raster> {
        if let Some((previous, previous_max, raster)) = &self.output
            && *previous_max == max_side
            && same_composite(previous, doc)
        {
            return raster.clone();
        }
        let prepared = if render::has_deep_layers(doc) {
            doc.clone()
        } else {
            self.prepare(doc, max_side)
        };
        let effects = if render::has_deep_layers(doc) {
            HashMap::new()
        } else {
            self.prepared_effects(&prepared)
        };
        let scale = preview_scale(doc, max_side);
        let raster = Arc::new(render::render_region_cached(
            &prepared,
            [0.0, 0.0, doc.width as f64, doc.height as f64],
            (doc.width as f64 * scale).round().max(1.0) as u32,
            (doc.height as f64 * scale).round().max(1.0) as u32,
            &effects,
        ));
        let mut state = doc.clone();
        state.selection = None;
        self.output = Some((state, max_side, raster.clone()));
        raster
    }
    /// Composite visible original pixels; a large document never becomes a fixed-size thumbnail.
    pub fn render_view(&mut self, doc: &Document, view: ViewRequest) -> Arc<Raster> {
        if let Some((previous, previous_view, raster)) = &self.view_output
            && *previous_view == view
            && same_composite(previous, doc)
        {
            return raster.clone();
        }
        self.sources.clear();
        let effects = if render::has_deep_layers(doc) {
            HashMap::new()
        } else {
            self.prepared_effects(doc)
        };
        let raster = if render::CompositeSampler::needs_full_surface(doc) {
            if !self
                .full_output
                .as_ref()
                .is_some_and(|(previous, _)| same_composite(previous, doc))
            {
                let full = Arc::new(render::render_region_cached(
                    doc,
                    [0.0, 0.0, doc.width as f64, doc.height as f64],
                    doc.width,
                    doc.height,
                    &effects,
                ));
                let mut state = doc.clone();
                state.selection = None;
                self.full_output = Some((state, full));
            }
            Arc::new(view.sample(&self.full_output.as_ref().unwrap().1))
        } else {
            self.full_output = None;
            Arc::new(render::render_region_cached(
                doc,
                view.document_region(),
                view.width,
                view.height,
                &effects,
            ))
        };
        let mut state = doc.clone();
        state.selection = None;
        self.view_output = Some((state, view, raster.clone()));
        raster
    }
    fn prepared_effects(&mut self, doc: &Document) -> HashMap<Uuid, (Raster, f64)> {
        self.effects.retain(|id, _| {
            doc.layer(*id)
                .is_some_and(|layer| layer.effects.is_some() && layer.image.is_some())
        });
        let mut effects = HashMap::new();
        for layer in &doc.layers {
            if let (Some(image), Some(settings)) = (&layer.image, &layer.effects) {
                let independent_transform = layer
                    .mask
                    .as_ref()
                    .filter(|m| m.enabled && m.placement.is_some())
                    .map(|_| layer.transform);
                let reusable = self.effects.get(&layer.id).is_some_and(|e| {
                    same_pixels(&e.image, image)
                        && same_mask(e.mask.as_ref(), layer.mask.as_ref())
                        && e.independent_transform == independent_transform
                        && e.settings == *settings
                });
                if !reusable {
                    let mut shown = image.to_rgba();
                    if layer.mask.as_ref().is_some_and(|m| m.enabled) {
                        shown
                            .par_chunks_exact_mut(image.width as usize * 4)
                            .enumerate()
                            .for_each(|(y, row)| {
                                for (x, p) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                                    let point = layer.transform.to_document([
                                        (x as f64 + 0.5) / image.width as f64,
                                        (y as f64 + 0.5) / image.height as f64,
                                    ]);
                                    let coverage = render::raster_mask_coverage(layer, point);
                                    p[3] = (p[3] as f64 * coverage).round().clamp(0.0, 255.0) as u8;
                                }
                            });
                    }
                    let result = filters::apply_effects(
                        &Raster::from_rgba(image.width, image.height, shown),
                        settings,
                    );
                    self.effects.insert(
                        layer.id,
                        EffectEntry {
                            image: image.clone(),
                            mask: layer.mask.clone(),
                            independent_transform,
                            settings: settings.clone(),
                            result,
                        },
                    );
                }
                effects.insert(layer.id, self.effects[&layer.id].result.clone());
            }
        }
        effects
    }
    fn source(
        &mut self,
        key: (Uuid, bool),
        image: &Arc<Raster>,
        width: u32,
        height: u32,
    ) -> Arc<Raster> {
        if width == image.width && height == image.height {
            self.sources.remove(&key);
            return image.clone();
        }
        if let Some(e) = self.sources.get(&key)
            && e.width == width
            && e.height == height
            && same_pixels(&e.original, image)
        {
            return e.image.clone();
        }
        let scaled = Arc::new(area_resize(image, width, height));
        self.sources.insert(
            key,
            SourceEntry {
                original: image.clone(),
                width,
                height,
                image: scaled.clone(),
            },
        );
        scaled
    }
}

fn preview_scale(doc: &Document, max_side: u32) -> f64 {
    (max_side.max(1) as f64 / doc.width.max(doc.height).max(1) as f64).min(1.0)
}
fn source_scale(image: &Raster, transform: &Transform, scale: f64) -> f64 {
    ((transform.size[0] * scale / image.width.max(1) as f64)
        .max(transform.size[1] * scale / image.height.max(1) as f64))
    .clamp(0.0, 1.0)
}
fn scaled_size(image: &Raster, scale: f64) -> (u32, u32) {
    (
        (image.width as f64 * scale).round().max(1.0) as u32,
        (image.height as f64 * scale).round().max(1.0) as u32,
    )
}
fn same_pixels(a: &Arc<Raster>, b: &Arc<Raster>) -> bool {
    Arc::ptr_eq(a, b)
        || (a.width == b.width
            && a.height == b.height
            && Arc::ptr_eq(&a.pixels, &b.pixels)
            && render::same_deep_pixels(a, b)
            && a.tiles.len() == b.tiles.len()
            && a.tiles.iter().all(|(key, data)| {
                b.tiles
                    .get(key)
                    .is_some_and(|other| Arc::ptr_eq(data, other))
            }))
}
fn same_mask(a: Option<&Mask>, b: Option<&Mask>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.enabled == b.enabled
                && a.linked == b.linked
                && a.placement == b.placement
                && same_pixels(&a.raster, &b.raster)
        }
        _ => false,
    }
}
pub(crate) fn same_composite(a: &Document, b: &Document) -> bool {
    a.width == b.width
        && a.height == b.height
        && a.layers.len() == b.layers.len()
        && a.layers.iter().zip(&b.layers).all(|(a, b)| {
            a.id == b.id
                && a.visible == b.visible
                && a.parent_id == b.parent_id
                && a.is_group == b.is_group
                && a.opacity == b.opacity
                && a.blend_mode == b.blend_mode
                && a.transform == b.transform
                && match (&a.image, &b.image) {
                    (None, None) => true,
                    (Some(a), Some(b)) => same_pixels(a, b),
                    _ => false,
                }
                && same_mask(a.mask.as_ref(), b.mask.as_ref())
                && a.mask_source_id == b.mask_source_id
                && a.adjustment == b.adjustment
                && a.effects == b.effects
        })
}

/// Area coverage averages premultiplied colors, so invisible RGB never creates fringes.
pub(crate) fn area_resize(image: &Raster, width: u32, height: u32) -> Raster {
    if let Some(deep) = &image.deep {
        return Raster::from_deep(deep.resized(width, height));
    }
    if image.tiles.is_empty() && image.pixels.len() <= 4 {
        return Raster::solid(width, height, image.get(0, 0));
    }
    let mut result = vec![0; width as usize * height as usize * 4];
    let sx = image.width as f64 / width as f64;
    let sy = image.height as f64 / height as f64;
    result
        .par_chunks_exact_mut(width as usize * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let y0 = y as f64 * sy;
            let y1 = (y + 1) as f64 * sy;
            for (x, out) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let x0 = x as f64 * sx;
                let x1 = (x + 1) as f64 * sx;
                let mut sum = [0.0; 4];
                for iy in y0.floor() as u32..(y1.ceil() as u32).min(image.height) {
                    let dy = y1.min(iy as f64 + 1.0) - y0.max(iy as f64);
                    for ix in x0.floor() as u32..(x1.ceil() as u32).min(image.width) {
                        let dx = x1.min(ix as f64 + 1.0) - x0.max(ix as f64);
                        let p = image.get(ix, iy);
                        let coverage = p[3] as f64 * dx * dy;
                        for c in 0..3 {
                            sum[c] += p[c] as f64 * coverage;
                        }
                        sum[3] += coverage;
                    }
                }
                if sum[3] > 0.0 {
                    for c in 0..3 {
                        out[c] = (sum[c] / sum[3]).round().clamp(0.0, 255.0) as u8;
                    }
                    out[3] = (sum[3] / (sx * sy)).round().clamp(0.0, 255.0) as u8;
                }
            }
        });
    Raster::from_rgba(width, height, result)
}
fn scaled_number(value: &mut Value, key: &str, default: f64, scale: f64) {
    let number = filters::number(value, key, default);
    value[key] = Value::from(number * scale);
}
fn scale_effects(effects: &mut Value, scale: f64) {
    if scale >= 1.0 {
        return;
    }
    for (key, fields) in [
        ("stroke", &[("size", 4.0)][..]),
        ("shadow", &[("distance", 20.0), ("blur", 20.0)][..]),
        ("outerGlow", &[("size", 20.0)][..]),
        ("innerGlow", &[("size", 10.0)][..]),
        ("innerShadow", &[("distance", 10.0), ("blur", 10.0)][..]),
    ] {
        if let Some(settings) = effects.get_mut(key).filter(|s| s.is_object()) {
            for &(field, default) in fields {
                scaled_number(settings, field, default, scale);
            }
        }
    }
}
/// Scale pixel-based filter parameters for a display sample without changing tonal controls.
pub fn scale_filter(settings: &mut Value, kind: &str, scale: f64) {
    let original_kind = settings.get("kind").cloned();
    settings["kind"] = Value::from(kind);
    scale_adjustment(settings, scale);
    match original_kind {
        Some(value) => settings["kind"] = value,
        None => {
            settings.as_object_mut().unwrap().remove("kind");
        }
    }
}
pub fn scale_adjustment(settings: &mut Value, scale: f64) {
    if scale >= 1.0 {
        return;
    }
    match filters::string(settings, "kind", "Invert") {
        "Gaussian Blur" => {
            let radius = filters::number(settings, "radius", 10.0);
            scaled_number(settings, "blurRadius", radius, scale);
        }
        "Motion Blur" => {
            let distance = filters::number(settings, "distance", 10.0);
            scaled_number(settings, "motionDistance", distance, scale);
        }
        "Progressive Blur" => {
            scaled_number(settings, "progressiveFrom", 0., scale);
            scaled_number(settings, "progressiveTo", 20., scale);
        }
        "Bloom / Glow" => scaled_number(settings, "bloomRadius", 24.0, scale),
        "Tonal Contrast" => scaled_number(settings, "tonalRadius", 20.0, scale),
        "Camera Raw Filter" => {
            let section = if settings.get("cameraRaw").is_some() {
                &mut settings["cameraRaw"]
            } else {
                settings
            };
            scaled_number(section, "previewScale", 1.0, scale);
        }
        "Grain" => {
            let section = if settings.get("grainSettings").is_some() {
                &mut settings["grainSettings"]
            } else {
                settings
            };
            scaled_number(section, "size", 1.5, scale);
        }
        "Dither" => {
            let section = if settings.get("dither").is_some() {
                &mut settings["dither"]
            } else {
                settings
            };
            for (field, default) in [
                ("pixelSize", 2.0),
                ("cellSize", 8.0),
                ("textSize", 14.0),
                ("lineSpacing", 4.0),
            ] {
                scaled_number(section, field, default, scale);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BlendMode, Layer, Sampling};
    use serde_json::json;

    #[test]
    fn native_view_retains_one_pixel_detail_beyond_old_thumbnail_limit() {
        let mut doc = Document::new(8192, 8);
        let source = Raster::from_rgba(
            doc.width,
            doc.height,
            (0..doc.width * doc.height)
                .flat_map(|index| {
                    let value = if index % 2 == 0 { 0 } else { 255 };
                    [value, value, value, 255]
                })
                .collect(),
        );
        doc.layers = vec![Layer::raster("Original detail", source.clone())];
        let view = ViewRequest::new([8192, 8], [3501.25, 1.0, 255.5, 4.0], 1.0).unwrap();
        assert_eq!(view.region, [3501, 1, 256, 4]);
        assert_eq!([view.width, view.height], [256, 4]);
        let mut cache = PreviewCache::default();
        let actual = cache.render_view(&doc, view);
        for y in 0..view.height {
            for x in 0..view.width {
                assert_eq!(
                    actual.get(x, y),
                    source.get(view.region[0] + x, view.region[1] + y)
                );
            }
        }
        assert!(Arc::ptr_eq(&actual, &cache.render_view(&doc, view)));
        let moved = ViewRequest {
            region: [3502, 1, 256, 4],
            ..view
        };
        let moved_pixels = cache.render_view(&doc, moved);
        assert!(!Arc::ptr_eq(&actual, &moved_pixels));
        assert_ne!(actual.get(0, 0), moved_pixels.get(0, 0));
        Arc::make_mut(doc.layers[0].image.as_mut().unwrap()).set(3502, 1, [10, 20, 30, 255]);
        assert_eq!(cache.render_view(&doc, moved).get(0, 0), [10, 20, 30, 255]);
    }
    #[test]
    fn view_requests_align_to_original_pixels_and_bound_output_to_the_viewport() {
        let native =
            ViewRequest::new([100_000, 100_000], [50_000.25, 40_000.5, 640.0, 400.0], 2.0).unwrap();
        assert_eq!(native.region, [50_000, 40_000, 641, 401]);
        assert_eq!([native.width, native.height], [641, 401]);
        let fit =
            ViewRequest::new([100_000, 100_000], [0.0, 0.0, 100_000.0, 100_000.0], 0.01).unwrap();
        assert_eq!([fit.width, fit.height], [1000, 1000]);
        assert!(!fit.native());
        assert!(ViewRequest::new([100, 100], [101.0, 0.0, 10.0, 10.0], 1.0).is_none());
        assert!(ViewRequest::new([100, 100], [0.0, 0.0, 10.0, 10.0], f64::NAN).is_none());
    }
    #[test]
    fn native_view_keeps_full_canvas_context_for_spatial_adjustments() {
        let mut doc = Document::new(48, 24);
        let source = Raster::from_rgba(
            48,
            24,
            (0..48 * 24)
                .flat_map(|index| {
                    let value = if index % 48 < 24 { 255 } else { 0 };
                    [value, value, value, 255]
                })
                .collect(),
        );
        doc.layers = vec![Layer::raster("Edge", source)];
        let mut blur = Layer::new("Blur", 48, 24);
        blur.adjustment = Some(json!({"kind":"Gaussian Blur", "blurRadius":4.0}));
        doc.layers.push(blur);
        let view = ViewRequest::new([48, 24], [24.0, 8.0, 8.0, 8.0], 1.0).unwrap();
        let full = render::render_document(&doc);
        let expected = view.sample(&full);
        assert!(
            expected.get(0, 0)[0] > 0,
            "Blur crosses the viewport's left edge"
        );
        let mut cache = PreviewCache::default();
        assert_eq!(*cache.render_view(&doc, view), expected);
        let full_identity = cache.full_output.as_ref().unwrap().1.clone();
        let moved = ViewRequest {
            region: [20, 8, 8, 8],
            ..view
        };
        assert_eq!(*cache.render_view(&doc, moved), moved.sample(&full));
        assert!(Arc::ptr_eq(
            &full_identity,
            &cache.full_output.as_ref().unwrap().1
        ));
    }
    #[test]
    fn area_resize_preserves_soft_alpha_without_transparent_color_fringe() {
        let input = Raster::from_rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 0]);
        assert_eq!(area_resize(&input, 1, 1).get(0, 0), [255, 0, 0, 128]);
    }
    #[test]
    fn sources_are_shared_and_only_changed_tiles_invalidate() {
        let mut doc = Document::new(4000, 4000);
        let mut source = Raster::blank(4000, 4000);
        source.set(100, 100, [255; 4]);
        doc.active_mut().unwrap().image = Some(Arc::new(source));
        let mut cache = PreviewCache::default();
        let first = cache.prepare(&doc, 1000);
        doc.active_mut().unwrap().transform.origin[0] = 40.0;
        let moved = cache.prepare(&doc, 1000);
        assert!(Arc::ptr_eq(
            first.active().unwrap().image.as_ref().unwrap(),
            moved.active().unwrap().image.as_ref().unwrap()
        ));
        let changed = Arc::make_mut(doc.active_mut().unwrap().image.as_mut().unwrap());
        changed.set(200, 200, [40, 100, 220, 255]);
        let edited = cache.prepare(&doc, 1000);
        assert!(!Arc::ptr_eq(
            moved.active().unwrap().image.as_ref().unwrap(),
            edited.active().unwrap().image.as_ref().unwrap()
        ));
    }
    #[test]
    fn removed_mask_and_image_release_their_original_and_scaled_sources() {
        let mut doc = Document::new(256, 256);
        let layer = doc.active_mut().unwrap();
        layer.image = Some(Arc::new(Raster::solid(256, 256, [100, 150, 200, 255])));
        let image = Arc::downgrade(layer.image.as_ref().unwrap());
        layer.mask = Some(Mask {
            raster: Arc::new(Raster::solid(256, 256, [128, 128, 128, 255])),
            ..Mask::white()
        });
        let mask = Arc::downgrade(&layer.mask.as_ref().unwrap().raster);
        let mut cache = PreviewCache::default();
        let prepared = cache.prepare(&doc, 64);
        let scaled_mask = Arc::downgrade(&prepared.active().unwrap().mask.as_ref().unwrap().raster);
        let scaled_image = Arc::downgrade(prepared.active().unwrap().image.as_ref().unwrap());
        drop(prepared);
        doc.active_mut().unwrap().mask.as_mut().unwrap().enabled = false;
        cache.prepare(&doc, 64);
        assert!(mask.upgrade().is_some() && scaled_mask.upgrade().is_some());
        doc.active_mut().unwrap().mask = None;
        cache.prepare(&doc, 64);
        assert!(mask.upgrade().is_none() && scaled_mask.upgrade().is_none());
        assert!(image.upgrade().is_some() && scaled_image.upgrade().is_some());
        doc.active_mut().unwrap().image = None;
        cache.prepare(&doc, 64);
        assert!(image.upgrade().is_none() && scaled_image.upgrade().is_none());
    }
    #[test]
    fn default_radii_scale_in_their_original_coordinate_space() {
        let mut doc = Document::new(1024, 1024);
        doc.active_mut().unwrap().image = Some(Arc::new(Raster::solid(512, 512, [255; 4])));
        doc.active_mut().unwrap().effects = Some(json!({"shadow":{}}));
        let mut adjustment = Layer::new("Blur", 1024, 1024);
        adjustment.adjustment = Some(json!({"kind":"Gaussian Blur"}));
        doc.layers.push(adjustment);
        let prepared = prepare(&doc, 256);
        assert_eq!(prepared.layers[0].image.as_ref().unwrap().width, 256);
        assert_eq!(
            prepared.layers[0].effects.as_ref().unwrap()["shadow"]["blur"],
            10.0
        );
        assert_eq!(
            prepared.layers[1].adjustment.as_ref().unwrap()["blurRadius"],
            2.5
        );
        assert_eq!(prepared.layers[0].transform, doc.layers[0].transform);
        assert!(
            doc.layers[0].effects.as_ref().unwrap()["shadow"]
                .get("blur")
                .is_none()
        );
    }
    #[test]
    fn full_resolution_cached_effects_match_masks_clipping_and_rotation() {
        let mut doc = Document::new(64, 64);
        let base = doc.active_mut().unwrap();
        base.image = Some(Arc::new(Raster::solid(16, 16, [220, 80, 30, 200])));
        base.transform = Transform::new(18.0, 18.0, 24.0, 24.0);
        base.transform.rotation = 32.0;
        base.effects =
            Some(json!({"shadow":{"distance":4,"blur":3},"stroke":{"size":1,"green":1}}));
        let mut mask = Mask::white();
        mask.linked = false;
        mask.placement = Some(Transform::new(22.0, 8.0, 16.0, 40.0));
        base.mask = Some(mask);
        let id = base.id;
        let mut clip = Layer::raster("Clip", Raster::solid(64, 64, [20, 130, 230, 255]));
        clip.mask_source_id = Some(id);
        clip.blend_mode = BlendMode::Screen;
        doc.layers.push(clip);
        let expected = render::render_document(&doc);
        let mut cache = PreviewCache::default();
        let actual = cache.render(&doc, 64);
        assert_eq!(*actual, expected);
        let repeated = cache.render(&doc, 64);
        assert!(Arc::ptr_eq(&actual, &repeated));
        doc.layers[0].opacity = 0.5;
        let changed = cache.render(&doc, 64);
        assert!(!Arc::ptr_eq(&actual, &changed));
        assert_eq!(*changed, render::render_document(&doc));
    }
    #[test]
    fn downsampled_effect_bounds_and_mask_match_full_resolution_preview() {
        let mut doc = Document::new(256, 256);
        let base = doc.active_mut().unwrap();
        base.image = Some(Arc::new(Raster::solid(64, 64, [200, 90, 20, 255])));
        base.transform = Transform::new(80.0, 80.0, 64.0, 64.0);
        base.transform.sampling = Sampling::Nearest;
        base.effects = Some(
            json!({"shadow":{"distance":16,"angle":90,"blur":0,"red":0.0},"stroke":{"size":8,"green":1,"red":0}}),
        );
        let expected = render::render_preview(&doc, 64);
        let actual = PreviewCache::default().render(&doc, 64);
        assert_eq!(actual.width, 64);
        let mean_error = actual
            .to_rgba()
            .iter()
            .zip(expected.to_rgba())
            .map(|(&a, b)| (a as f64 - b as f64).abs())
            .sum::<f64>()
            / (64.0 * 64.0 * 4.0);
        assert!(mean_error < 2.0, "mean byte error={mean_error}");
        assert_eq!(actual.get(27, 27), [200, 90, 20, 255]);
        assert_eq!(actual.get(0, 0), [0; 4]);
    }
}
