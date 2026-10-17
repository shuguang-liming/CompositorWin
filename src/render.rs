//! CPU reference compositor. Model rasters use straight alpha; intermediate surfaces are premultiplied.
use crate::model::{BlendMode, Document, Layer, Raster, Sampling};
use crate::{filters, pixels};
use rayon::prelude::*;
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, Weak},
};
use uuid::Uuid;
#[path = "render_deep.rs"]
mod deep;

pub fn has_deep_layers(doc: &Document) -> bool {
    deep::has_deep(doc)
}

pub fn sample(image: &Raster, x: f64, y: f64) -> [u8; 4] {
    sample_with_sampling(image, x, y, Sampling::Smooth)
}
/// Pixel-center coordinates; High uses Catmull–Rom bicubic interpolation in premultiplied sRGB.
pub fn sample_with_sampling(image: &Raster, x: f64, y: f64, sampling: Sampling) -> [u8; 4] {
    let v = sample_premultiplied(image, x, y, sampling);
    let a = v[3];
    if a <= 0.0 {
        [0; 4]
    } else {
        [
            (v[0] / a * 255.0).round().clamp(0.0, 255.0) as u8,
            (v[1] / a * 255.0).round().clamp(0.0, 255.0) as u8,
            (v[2] / a * 255.0).round().clamp(0.0, 255.0) as u8,
            (a * 255.0).round().clamp(0.0, 255.0) as u8,
        ]
    }
}
fn premultiplied_pixel(image: &Raster, x: i64, y: i64) -> [f64; 4] {
    if x < 0 || y < 0 || x >= image.width as i64 || y >= image.height as i64 {
        return [0.0; 4];
    }
    let p = image.get(x as u32, y as u32);
    let a = p[3] as f64 / 255.0;
    [
        ((p[0] as u32 * p[3] as u32 + 127) / 255) as f64 / 255.0,
        ((p[1] as u32 * p[3] as u32 + 127) / 255) as f64 / 255.0,
        ((p[2] as u32 * p[3] as u32 + 127) / 255) as f64 / 255.0,
        a,
    ]
}
fn cubic_weight(distance: f64) -> f64 {
    let t = distance.abs();
    if t <= 1. {
        ((1.5 * t - 2.5) * t) * t + 1.
    } else if t < 2. {
        ((-0.5 * t + 2.5) * t - 4.) * t + 2.
    } else {
        0.
    }
}
fn sample_kernel(
    x: f64,
    y: f64,
    sampling: Sampling,
    pixel: impl Fn(i64, i64) -> [f64; 4],
) -> [f64; 4] {
    if !x.is_finite() || !y.is_finite() {
        return [0.; 4];
    }
    if sampling == Sampling::Nearest {
        return pixel((x + 0.5).floor() as i64, (y + 0.5).floor() as i64);
    }
    let ix = x.floor() as i64;
    let iy = y.floor() as i64;
    let fx = x - x.floor();
    let fy = y - y.floor();
    let mut out = [0.0; 4];
    if sampling == Sampling::High {
        let wx: [f64; 4] = std::array::from_fn(|i| cubic_weight(i as f64 - 1. - fx));
        let wy: [f64; 4] = std::array::from_fn(|j| cubic_weight(j as f64 - 1. - fy));
        for (j, yw) in wy.into_iter().enumerate() {
            for (i, xw) in wx.into_iter().enumerate() {
                if xw == 0. || yw == 0. {
                    continue;
                }
                let p = pixel(ix + i as i64 - 1, iy + j as i64 - 1);
                for c in 0..4 {
                    out[c] += p[c] * xw * yw;
                }
            }
        }
        let alpha = out[3].clamp(0., 1.);
        let rescale = if out[3] > 1. { alpha / out[3] } else { 1. };
        out[3] = alpha;
        for channel in &mut out[..3] {
            *channel = (*channel * rescale).clamp(0., alpha);
        }
        return out;
    }
    for j in 0..2 {
        for i in 0..2 {
            let p = pixel(ix + i, iy + j);
            let w = if i == 0 { 1.0 - fx } else { fx } * if j == 0 { 1.0 - fy } else { fy };
            for c in 0..4 {
                out[c] += p[c] * w;
            }
        }
    }
    out
}
pub(crate) fn sample_premultiplied(image: &Raster, x: f64, y: f64, sampling: Sampling) -> [f64; 4] {
    sample_kernel(x, y, sampling, |x, y| premultiplied_pixel(image, x, y))
}

/// CPU minification uses the same floor-sized, area-weighted RGBA8 pyramid as WGSL mipmaps.
/// Levels are prepared once per rendered layer; the interactive PreviewCache already retains reductions.
struct MipLevel {
    width: u32,
    height: u32,
    bounds: [u32; 4],
    pixels: Vec<u8>,
}
impl MipLevel {
    fn pixel(&self, x: i64, y: i64) -> [f64; 4] {
        if x < self.bounds[0] as i64
            || y < self.bounds[1] as i64
            || x >= self.bounds[2] as i64
            || y >= self.bounds[3] as i64
        {
            return [0.; 4];
        }
        let i = if self.pixels.len() == 4 {
            0
        } else {
            (((y as u32 - self.bounds[1]) * (self.bounds[2] - self.bounds[0]) + x as u32
                - self.bounds[0])
                * 4) as usize
        };
        std::array::from_fn(|c| self.pixels[i + c] as f64 / 255.)
    }
}

/// Only transparent bases permit skipping untouched tiles. Keep coordinates on the full mip grid.
fn raster_support(image: &Raster) -> [u32; 4] {
    if !image.pixels.is_empty() && !(image.pixels.len() == 4 && image.pixels[3] == 0) {
        return [0, 0, image.width, image.height];
    }
    let mut bounds = [image.width, image.height, 0, 0];
    for &(tx, ty) in image.tiles.keys() {
        let x = tx.saturating_mul(crate::model::TILE_SIZE);
        let y = ty.saturating_mul(crate::model::TILE_SIZE);
        if x >= image.width || y >= image.height {
            continue;
        }
        bounds[0] = bounds[0].min(x);
        bounds[1] = bounds[1].min(y);
        bounds[2] = bounds[2].max(x.saturating_add(crate::model::TILE_SIZE).min(image.width));
        bounds[3] = bounds[3].max(y.saturating_add(crate::model::TILE_SIZE).min(image.height));
    }
    if bounds[2] == 0 { [0; 4] } else { bounds }
}

fn reduced_support(bounds: [u32; 4], previous: [u32; 2], next: [u32; 2]) -> [u32; 4] {
    if bounds[2] == 0 || bounds[3] == 0 {
        return [0; 4];
    }
    std::array::from_fn(|i| {
        let axis = i % 2;
        let numerator = bounds[i] as u64 * next[axis] as u64;
        if i < 2 {
            (numerator / previous[axis] as u64) as u32
        } else {
            numerator.div_ceil(previous[axis] as u64) as u32
        }
    })
}

pub(crate) struct LayerSampler {
    source: Raster,
    sampling: Sampling,
    levels: Vec<MipLevel>,
    first_level: usize,
    lod: f64,
    support: Option<[f64; 4]>,
}
impl LayerSampler {
    pub(crate) fn new(source: &Raster, sampling: Sampling, display: [f64; 2]) -> Self {
        let max_level = 31 - source.width.max(source.height).max(1).leading_zeros();
        let ratio = (source.width as f64 / display[0].abs().max(1e-6))
            .max(source.height as f64 / display[1].abs().max(1e-6));
        let lod = if sampling == Sampling::High {
            ratio.log2().clamp(0., max_level as f64)
        } else {
            0.
        };
        let count = lod.ceil() as usize;
        let source_bounds = raster_support(source);
        let mut sampler = Self {
            source: source.clone(),
            sampling,
            levels: Vec::with_capacity(count),
            first_level: 1,
            lod,
            support: None,
        };
        for level in 0..count {
            let (pw, ph) = if level == 0 {
                (source.width, source.height)
            } else {
                (
                    sampler.levels[level - 1].width,
                    sampler.levels[level - 1].height,
                )
            };
            let (width, height) = ((pw / 2).max(1), (ph / 2).max(1));
            let previous_bounds = if level == 0 {
                source_bounds
            } else {
                sampler.levels[level - 1].bounds
            };
            let bounds = reduced_support(previous_bounds, [pw, ph], [width, height]);
            if bounds[2] == 0 || bounds[3] == 0 {
                sampler.levels.push(MipLevel {
                    width,
                    height,
                    bounds,
                    pixels: Vec::new(),
                });
                continue;
            }
            if source.tiles.is_empty() && source.pixels.len() <= 4 {
                let pixels = premultiplied_pixel(source, 0, 0)
                    .map(|v| (v * 255.).round() as u8)
                    .to_vec();
                sampler.levels.push(MipLevel {
                    width,
                    height,
                    bounds,
                    pixels,
                });
                continue;
            }
            let stride = (bounds[2] - bounds[0]) as usize * 4;
            let mut pixels = vec![0; stride * (bounds[3] - bounds[1]) as usize];
            let ratio = [pw as f64 / width as f64, ph as f64 / height as f64];
            let fill_row = |(row_index, row): (usize, &mut [u8])| {
                let y = bounds[1] + row_index as u32;
                for (column, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let x = bounds[0] + column as u32;
                    let begin = [x as f64 * ratio[0], y as f64 * ratio[1]];
                    let end = [(x + 1) as f64 * ratio[0], (y + 1) as f64 * ratio[1]];
                    let start = [begin[0].floor() as i64, begin[1].floor() as i64];
                    let mut total = [0.; 4];
                    let mut weight = 0.;
                    for yy in 0..3 {
                        for xx in 0..3 {
                            let at = [start[0] + xx, start[1] + yy];
                            if at[0] >= pw as i64 || at[1] >= ph as i64 {
                                continue;
                            }
                            let w = (end[0].min(at[0] as f64 + 1.) - begin[0].max(at[0] as f64))
                                .max(0.)
                                * (end[1].min(at[1] as f64 + 1.) - begin[1].max(at[1] as f64))
                                    .max(0.);
                            if w == 0. {
                                continue;
                            }
                            let p = if level == 0 {
                                premultiplied_pixel(source, at[0], at[1])
                            } else {
                                sampler.levels[level - 1].pixel(at[0], at[1])
                            };
                            for c in 0..4 {
                                total[c] += p[c] * w;
                            }
                            weight += w;
                        }
                    }
                    for c in 0..4 {
                        pixel[c] = (total[c] / weight.max(1.) * 255.).round().clamp(0., 255.) as u8;
                    }
                }
            };
            if pixels.len() >= 256 * 256 * 4 {
                pixels
                    .par_chunks_exact_mut(stride)
                    .enumerate()
                    .for_each(fill_row);
            } else {
                pixels
                    .chunks_exact_mut(stride)
                    .enumerate()
                    .for_each(fill_row);
            }
            sampler.levels.push(MipLevel {
                width,
                height,
                bounds,
                pixels,
            });
        }
        // Intermediate reductions are needed only while constructing the selected level(s).
        // Retain the integer LOD, plus its neighbor for fractional LOD blending.
        sampler.first_level = (lod.floor() as usize).max(1);
        if sampler.first_level > 1 {
            sampler.levels.drain(..sampler.first_level - 1);
        }
        let radius = match sampling {
            Sampling::Nearest => 0.5,
            Sampling::Smooth => 1.,
            Sampling::High => 2.,
        };
        let mut include = |bounds: [u32; 4], width: u32, height: u32| {
            if bounds[2] == 0 || bounds[3] == 0 {
                return;
            }
            let uv = [
                (bounds[0] as f64 - radius) / width.max(1) as f64,
                (bounds[1] as f64 - radius) / height.max(1) as f64,
                (bounds[2] as f64 + radius) / width.max(1) as f64,
                (bounds[3] as f64 + radius) / height.max(1) as f64,
            ];
            sampler.support = Some(if let Some(previous) = sampler.support {
                [
                    previous[0].min(uv[0]),
                    previous[1].min(uv[1]),
                    previous[2].max(uv[2]),
                    previous[3].max(uv[3]),
                ]
            } else {
                uv
            });
        };
        if lod < 1. {
            include(source_bounds, source.width, source.height);
        }
        for mip in &sampler.levels {
            include(mip.bounds, mip.width, mip.height);
        }
        sampler
    }
    pub(crate) fn sample(&self, uv: [f64; 2]) -> [f64; 4] {
        let Some(bounds) = self.support else {
            return [0.; 4];
        };
        if !(bounds[0]..=bounds[2]).contains(&uv[0]) || !(bounds[1]..=bounds[3]).contains(&uv[1]) {
            return [0.; 4];
        }
        let level_sample = |level: usize| {
            if level == 0 {
                sample_premultiplied(
                    &self.source,
                    uv[0] * self.source.width as f64 - 0.5,
                    uv[1] * self.source.height as f64 - 0.5,
                    self.sampling,
                )
            } else {
                let m = &self.levels[level - self.first_level];
                sample_kernel(
                    uv[0] * m.width as f64 - 0.5,
                    uv[1] * m.height as f64 - 0.5,
                    self.sampling,
                    |x, y| m.pixel(x, y),
                )
            }
        };
        let low = self.lod.floor() as usize;
        let first = level_sample(low);
        let fraction = self.lod.fract();
        if fraction == 0. {
            return first;
        }
        let second = level_sample(low + 1);
        std::array::from_fn(|c| first[c] * (1. - fraction) + second[c] * fraction)
    }
}
fn luminosity(c: [f64; 3]) -> f64 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}
fn saturation(c: [f64; 3]) -> f64 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}
fn set_luminosity(mut c: [f64; 3], l: f64) -> [f64; 3] {
    let d = l - luminosity(c);
    for v in &mut c {
        *v += d;
    }
    let lum = luminosity(c);
    let min = c[0].min(c[1]).min(c[2]);
    let max = c[0].max(c[1]).max(c[2]);
    if min < 0.0 {
        for v in &mut c {
            *v = lum + (*v - lum) * lum / (lum - min).max(1e-10);
        }
    }
    if max > 1.0 {
        for v in &mut c {
            *v = lum + (*v - lum) * (1.0 - lum) / (max - lum).max(1e-10);
        }
    }
    c
}
fn set_saturation(mut c: [f64; 3], s: f64) -> [f64; 3] {
    let mut order = [0, 1, 2];
    order.sort_by(|&a, &b| c[a].total_cmp(&c[b]));
    let [lo, mid, hi] = order;
    if c[hi] > c[lo] {
        c[mid] = (c[mid] - c[lo]) * s / (c[hi] - c[lo]);
        c[hi] = s;
    } else {
        c[mid] = 0.0;
        c[hi] = 0.0;
    }
    c[lo] = 0.0;
    c
}
fn burn(b: f64, s: f64) -> f64 {
    if b >= 1.0 {
        1.0
    } else if s <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - b) / s).min(1.0)
    }
}
fn dodge(b: f64, s: f64) -> f64 {
    if b <= 0.0 {
        0.0
    } else if s >= 1.0 {
        1.0
    } else {
        (b / (1.0 - s)).min(1.0)
    }
}
fn blend_channel(b: f64, s: f64, m: BlendMode) -> f64 {
    use BlendMode::*;
    match m {
        Normal => s,
        Darken => b.min(s),
        Multiply => b * s,
        ColorBurn => burn(b, s),
        LinearBurn => (b + s - 1.0).max(0.0),
        Lighten => b.max(s),
        Screen => b + s - b * s,
        ColorDodge => dodge(b, s),
        LinearDodge => (b + s).min(1.0),
        Overlay => {
            if b <= 0.5 {
                2.0 * b * s
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - s)
            }
        }
        HardLight => {
            if s <= 0.5 {
                2.0 * b * s
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - s)
            }
        }
        SoftLight => {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 {
                    ((16.0 * b - 12.0) * b + 4.0) * b
                } else {
                    b.sqrt()
                };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }
        VividLight => {
            if s <= 0.5 {
                burn(b, 2.0 * s)
            } else {
                dodge(b, 2.0 * s - 1.0)
            }
        }
        LinearLight => (b + 2.0 * s - 1.0).clamp(0.0, 1.0),
        PinLight => {
            if s <= 0.5 {
                b.min(2.0 * s)
            } else {
                b.max(2.0 * s - 1.0)
            }
        }
        HardMix => {
            if blend_channel(b, s, VividLight) < 0.5 {
                0.0
            } else {
                1.0
            }
        }
        Difference => (b - s).abs(),
        Exclusion => b + s - 2.0 * b * s,
        Subtract => (b - s).max(0.0),
        Divide => {
            if s <= 0.0 {
                1.0
            } else {
                (b / s).min(1.0)
            }
        }
        Hue | Saturation | Color | Luminosity => s,
    }
}
/// Source-over composition in encoded sRGB, retaining alpha-correct soft burn/dodge edges.
pub fn blend_pixel(bottom: [u8; 4], top: [u8; 4], mode: BlendMode, opacity: f64) -> [u8; 4] {
    let premul = |p: [u8; 4]| {
        let a = p[3] as f64 / 255.0;
        [
            p[0] as f64 / 255.0 * a,
            p[1] as f64 / 255.0 * a,
            p[2] as f64 / 255.0 * a,
            a,
        ]
    };
    let mut t = premul(top);
    for v in &mut t {
        *v *= opacity.clamp(0.0, 1.0);
    }
    let out = blend_premultiplied(premul(bottom), t, mode);
    if out[3] <= 0.0 {
        [0; 4]
    } else {
        [
            (out[0] / out[3] * 255.0).round().clamp(0.0, 255.0) as u8,
            (out[1] / out[3] * 255.0).round().clamp(0.0, 255.0) as u8,
            (out[2] / out[3] * 255.0).round().clamp(0.0, 255.0) as u8,
            (out[3] * 255.0).round().clamp(0.0, 255.0) as u8,
        ]
    }
}
fn blend_premultiplied(b: [f64; 4], s: [f64; 4], mode: BlendMode) -> [f64; 4] {
    let ab = b[3];
    let asrc = s[3];
    if asrc <= 0.0 {
        return b;
    }
    if mode == BlendMode::Normal {
        let alpha = asrc + ab * (1.0 - asrc);
        // Preserve the reference evaluation order at half-byte rounding boundaries.
        return std::array::from_fn(|i| {
            if i == 3 {
                alpha
            } else {
                ((1.0 - asrc) * b[i] + (1.0 - ab) * s[i] + asrc * ab * (s[i] / asrc))
                    .clamp(0.0, alpha)
            }
        });
    }
    let bc = std::array::from_fn(|i| if ab <= 0.0 { 0.0 } else { b[i] / ab });
    let sc = std::array::from_fn(|i| s[i] / asrc);
    let mixed = match mode {
        BlendMode::Hue => set_luminosity(set_saturation(sc, saturation(bc)), luminosity(bc)),
        BlendMode::Saturation => set_luminosity(set_saturation(bc, saturation(sc)), luminosity(bc)),
        BlendMode::Color => set_luminosity(sc, luminosity(bc)),
        BlendMode::Luminosity => set_luminosity(bc, luminosity(sc)),
        _ => std::array::from_fn(|i| blend_channel(bc[i], sc[i], mode)),
    };
    let mut out = [0.0; 4];
    out[3] = asrc + ab * (1.0 - asrc);
    for i in 0..3 {
        out[i] =
            ((1.0 - asrc) * b[i] + (1.0 - ab) * s[i] + asrc * ab * mixed[i]).clamp(0.0, out[3]);
    }
    out
}
pub fn render_document(doc: &Document) -> Raster {
    render_region(
        doc,
        [0.0, 0.0, doc.width as f64, doc.height as f64],
        doc.width,
        doc.height,
    )
}
pub fn composite(doc: &Document) -> Raster {
    render_document(doc)
}
pub fn render_preview(doc: &Document, max_side: u32) -> Raster {
    let scale = (max_side.max(1) as f64 / doc.width.max(doc.height).max(1) as f64).min(1.0);
    render_region(
        doc,
        [0.0, 0.0, doc.width as f64, doc.height as f64],
        ((doc.width as f64 * scale).round() as u32).max(1),
        ((doc.height as f64 * scale).round() as u32).max(1),
    )
}
/// Native document-pixel picking. Build on a worker and retain while `matches` is true.
/// Local color adjustments use one-pixel composition; spatial/global adjustments retain
/// one native composite so their neighborhoods, coordinate seeds and geometry stay exact.
pub struct CompositeSampler {
    document: Document,
    effects: HashMap<Uuid, (Raster, f64)>,
    samplers: HashMap<Uuid, LayerSampler>,
    composite: Option<Raster>,
}
impl CompositeSampler {
    /// Determine whether a requested region needs native full-canvas filter context.
    /// This inspects only visible traversal and settings; no pixels/effects/mips are prepared.
    pub fn needs_full_surface(doc: &Document) -> bool {
        if has_deep_layers(doc) {
            return true;
        }
        let mut ordered = Vec::new();
        ordered_visible(doc, None, 0, &mut ordered);
        Self::layers_need_full_surface(&ordered)
    }
    fn layers_need_full_surface(ordered: &[&Layer]) -> bool {
        ordered.iter().any(|layer| {
            layer.adjustment.as_ref().is_some_and(|s| {
                !matches!(
                    filters::string(s, "kind", "Invert"),
                    "Invert"
                        | "Levels"
                        | "Curves"
                        | "Exposure"
                        | "Gradient Map"
                        | "Black & White"
                        | "Color Balance"
                        | "Hue/Saturation"
                )
            })
        })
    }
    pub fn new(doc: &Document) -> Self {
        let mut document = doc.clone();
        document.selection = None;
        let mut ordered = Vec::new();
        ordered_visible(&document, None, 0, &mut ordered);
        let global = has_deep_layers(&document) || Self::layers_need_full_surface(&ordered);
        if global {
            let composite = Some(render_document(&document));
            return Self {
                document,
                effects: HashMap::new(),
                samplers: HashMap::new(),
                composite,
            };
        }
        let mut effects = HashMap::new();
        let mut samplers = HashMap::new();
        for layer in ordered {
            if let Some(effect) = prepare_layer_effect(&document, layer) {
                effects.insert(layer.id, effect);
            }
            let (image, transform) = if let Some((image, inset)) = effects.get(&layer.id) {
                (image, layer_effect_transform(layer, image, *inset))
            } else if let Some(image) = &layer.image {
                (image.as_ref(), layer.transform)
            } else {
                continue;
            };
            samplers.insert(
                layer.id,
                LayerSampler::new(image, transform.sampling, transform.size),
            );
        }
        Self {
            document,
            effects,
            samplers,
            composite: None,
        }
    }
    /// Selection, active layer, guides and metadata do not change the sampled composite.
    pub fn matches(&self, doc: &Document) -> bool {
        same_composite(&self.document, doc)
    }
    pub fn uses_full_surface(&self) -> bool {
        self.composite.is_some()
    }
    /// Extra pixel storage owned by this cache, excluding shared document source/mask pixels.
    /// Metadata and temporary preparation workspaces are not included.
    pub fn allocated_bytes(&self) -> usize {
        self.composite.as_ref().map_or(0, Raster::byte_len)
            + self
                .effects
                .values()
                .map(|(r, _)| r.byte_len())
                .sum::<usize>()
            + self
                .samplers
                .values()
                .flat_map(|s| &s.levels)
                .map(|m| m.pixels.len())
                .sum::<usize>()
    }
    pub fn sample(&self, point: [f64; 2]) -> Option<[u8; 4]> {
        if !point.iter().all(|v| v.is_finite())
            || point[0] < 0.
            || point[1] < 0.
            || point[0] >= self.document.width as f64
            || point[1] >= self.document.height as f64
        {
            return None;
        }
        let x = point[0].floor() as u32;
        let y = point[1].floor() as u32;
        if let Some(composite) = &self.composite {
            return Some(composite.get(x, y));
        }
        let mut p = render_group(
            &self.document,
            None,
            [x as f64, y as f64, 1., 1.],
            1,
            1,
            0,
            Some(&self.effects),
            Some(&self.samplers),
        );
        pixels::unpremultiply(&mut p);
        Some(p[..4].try_into().unwrap())
    }
}
/// Compare immutable raster identities instead of walking large pixel buffers.
pub fn same_composite(a: &Document, b: &Document) -> bool {
    fn same_image(a: &Arc<Raster>, b: &Arc<Raster>) -> bool {
        Arc::ptr_eq(a, b)
            || (a.width == b.width
                && a.height == b.height
                && Arc::ptr_eq(&a.pixels, &b.pixels)
                && same_deep_pixels(a, b)
                && a.tiles.len() == b.tiles.len()
                && a.tiles
                    .iter()
                    .all(|(key, data)| b.tiles.get(key).is_some_and(|v| Arc::ptr_eq(data, v))))
    }
    fn same_mask(a: Option<&crate::model::Mask>, b: Option<&crate::model::Mask>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                a.enabled == b.enabled
                    && a.placement == b.placement
                    && same_image(&a.raster, &b.raster)
            }
            _ => false,
        }
    }
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
                    (Some(a), Some(b)) => same_image(a, b),
                    _ => false,
                }
                && same_mask(a.mask.as_ref(), b.mask.as_ref())
                && a.mask_source_id == b.mask_source_id
                && a.adjustment == b.adjustment
                && a.effects == b.effects
        })
}
pub(crate) fn same_deep_pixels(a: &Raster, b: &Raster) -> bool {
    match (&a.deep, &b.deep) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a.shares(b),
        _ => false,
    }
}
/// Render a document rectangle into a requested resolution. Preview/dirty tiles never allocate a full canvas.
pub fn render_region(doc: &Document, region: [f64; 4], width: u32, height: u32) -> Raster {
    if width == 0 || height == 0 {
        return Raster::blank(width, height);
    }
    if deep::has_deep(doc) {
        return deep::render(doc, region, width, height);
    }
    let data = render_group(doc, None, region, width, height, 0, None, None);
    to_straight_raster(data, width, height)
}
/// Preview callers may retain masked layer effects across transform/opacity-only edits.
/// Each cached value is `(effect raster, source-pixel inset)` after the layer's own raster mask.
pub fn render_region_cached(
    doc: &Document,
    region: [f64; 4],
    width: u32,
    height: u32,
    effects: &HashMap<Uuid, (Raster, f64)>,
) -> Raster {
    if width == 0 || height == 0 {
        return Raster::blank(width, height);
    }
    if deep::has_deep(doc) {
        return deep::render(doc, region, width, height);
    }
    to_straight_raster(
        render_group(doc, None, region, width, height, 0, Some(effects), None),
        width,
        height,
    )
}
fn to_straight_raster(mut p: Vec<u8>, w: u32, h: u32) -> Raster {
    pixels::unpremultiply(&mut p);
    Raster::from_rgba(w, h, p)
}
fn layer_mask(doc: &Document, layer: &Layer, point: [f64; 2], depth: usize) -> f64 {
    let mut coverage = raster_mask_coverage(layer, point);
    if depth < 256
        && let Some(id) = layer.mask_source_id
        && let Some(source) = doc.layer(id)
        && source.id != layer.id
        && !source.is_group
    {
        coverage *= source_alpha(doc, source, point, depth + 1);
    }
    coverage
}
/// Coverage of the layer's own bitmap mask, excluding live source and ancestor masks.
pub fn raster_mask_coverage(layer: &Layer, point: [f64; 2]) -> f64 {
    let mut coverage = 1.0;
    if let Some(m) = &layer.mask
        && m.enabled
    {
        let t = m.placement.as_ref().unwrap_or(&layer.transform);
        let uv = t.from_document(point);
        coverage *= sample_mask_with_sampling(
            &m.raster,
            uv[0] * m.raster.width as f64 - 0.5,
            uv[1] * m.raster.height as f64 - 0.5,
            cached_mask_background(&m.raster),
            t.sampling,
        );
    }
    coverage
}
/// Exterior mask value follows the average perimeter tone; 1×1 masks are uniform everywhere.
pub fn mask_background(image: &Raster) -> u8 {
    let gray = |x, y| {
        let p = image.get(x, y);
        ((p[0] as u32 * p[3] as u32 + 127) / 255) as u8
    };
    if image.width == 0 || image.height == 0 {
        return 255;
    }
    if image.width == 1 && image.height == 1 {
        return gray(0, 0);
    }
    let mut sum = 0u64;
    let mut count = 0u64;
    for x in 0..image.width {
        sum += gray(x, 0) as u64;
        count += 1;
        if image.height > 1 {
            sum += gray(x, image.height - 1) as u64;
            count += 1;
        }
    }
    for y in 1..image.height.saturating_sub(1) {
        sum += gray(0, y) as u64;
        count += 1;
        if image.width > 1 {
            sum += gray(image.width - 1, y) as u64;
            count += 1;
        }
    }
    if sum * 2 >= count * 255 { 255 } else { 0 }
}
thread_local! {
    static MASK_BACKGROUNDS: RefCell<HashMap<usize,(Weak<Raster>,u8)>> = RefCell::new(HashMap::new());
}
pub(crate) fn cached_mask_background(image: &Arc<Raster>) -> u8 {
    let key = Arc::as_ptr(image) as usize;
    MASK_BACKGROUNDS.with_borrow_mut(|cache| {
        if let Some((old, value)) = cache.get(&key)
            && old.upgrade().is_some_and(|old| Arc::ptr_eq(&old, image))
        {
            return *value;
        }
        if cache.len() > 128 {
            cache.retain(|_, (old, _)| old.strong_count() > 0);
        }
        let value = mask_background(image);
        cache.insert(key, (Arc::downgrade(image), value));
        value
    })
}
/// Bilinear grayscale mask sampling with an explicit, precomputed exterior value.
pub fn sample_mask(image: &Raster, x: f64, y: f64, background: u8) -> f64 {
    sample_mask_with_sampling(image, x, y, background, Sampling::Smooth)
}
pub fn sample_mask_with_sampling(
    image: &Raster,
    x: f64,
    y: f64,
    background: u8,
    sampling: Sampling,
) -> f64 {
    let gray = |x: i64, y: i64| {
        if x < 0 || y < 0 || x >= image.width as i64 || y >= image.height as i64 {
            return background as f64 / 255.;
        }
        let p = image.get(x as u32, y as u32);
        ((p[0] as u32 * p[3] as u32 + 127) / 255) as f64 / 255.
    };
    if image.width == 1 && image.height == 1 {
        return gray(0, 0);
    }
    sample_kernel(x, y, sampling, |x, y| {
        let v = gray(x, y);
        [v, v, v, 1.]
    })[0]
}
fn source_alpha(doc: &Document, l: &Layer, point: [f64; 2], depth: usize) -> f64 {
    let uv = l.transform.from_document(point);
    let a = if let Some(r) = &l.image {
        if r.deep.is_some() {
            return r.sample_f32(
                uv[0] * r.width as f64 - 0.5,
                uv[1] * r.height as f64 - 0.5,
                l.transform.sampling,
            )[3] as f64
                * effective_opacity(doc, l)
                * layer_mask(doc, l, point, depth);
        }
        sample_premultiplied(
            r,
            uv[0] * r.width as f64 - 0.5,
            uv[1] * r.height as f64 - 0.5,
            l.transform.sampling,
        )[3]
    } else {
        0.0
    };
    a * effective_opacity(doc, l) * layer_mask(doc, l, point, depth)
}
fn effective_opacity(doc: &Document, l: &Layer) -> f64 {
    let mut opacity = l.opacity;
    let mut parent = l.parent_id;
    for _ in 0..64 {
        let Some(id) = parent else {
            break;
        };
        let Some(folder) = doc.layer(id) else {
            break;
        };
        opacity *= folder.opacity;
        parent = folder.parent_id;
    }
    opacity
}
fn folder_coverage(doc: &Document, l: &Layer, point: [f64; 2]) -> f64 {
    let mut mask = 1.0;
    let mut parent = l.parent_id;
    for _ in 0..64 {
        let Some(id) = parent else {
            break;
        };
        let Some(folder) = doc.layer(id) else {
            break;
        };
        mask *= layer_mask(doc, folder, point, 0);
        parent = folder.parent_id;
    }
    mask
}
fn ordered_visible<'a>(
    doc: &'a Document,
    parent: Option<Uuid>,
    depth: usize,
    out: &mut Vec<&'a Layer>,
) {
    if depth > 64 {
        return;
    }
    for l in doc
        .layers
        .iter()
        .filter(|l| l.parent_id == parent && l.visible && l.opacity > 0.0)
    {
        if l.is_group {
            ordered_visible(doc, Some(l.id), depth + 1, out);
        } else {
            out.push(l);
        }
    }
}

fn prepare_layer_effect(doc: &Document, layer: &Layer) -> Option<(Raster, f64)> {
    layer.image.as_ref().and_then(|image| {
        layer.effects.as_ref().map(|effects| {
            if image.deep.is_some() {
                let mut shown = image.as_ref().clone();
                if layer.mask.as_ref().is_some_and(|m| m.enabled) {
                    let mut own = layer.clone();
                    own.mask_source_id = None;
                    for y in 0..image.height {
                        for x in 0..image.width {
                            let point = layer.transform.to_document([
                                (x as f64 + 0.5) / image.width as f64,
                                (y as f64 + 0.5) / image.height as f64,
                            ]);
                            let mut p = image.get_f32(x, y);
                            p[3] *= layer_mask(doc, &own, point, 0) as f32;
                            shown.set_f32(x, y, p);
                        }
                    }
                }
                return filters::apply_effects(&shown, effects);
            }
            // Effects follow the raster-masked shape, before opacity or live clipping.
            let mut shown = image.to_rgba();
            if layer.mask.as_ref().is_some_and(|m| m.enabled) {
                let mut own = layer.clone();
                own.mask_source_id = None;
                for y in 0..image.height {
                    for x in 0..image.width {
                        let point = layer.transform.to_document([
                            (x as f64 + 0.5) / image.width as f64,
                            (y as f64 + 0.5) / image.height as f64,
                        ]);
                        let coverage = layer_mask(doc, &own, point, 0);
                        let i = ((y * image.width + x) * 4) as usize;
                        shown[i + 3] =
                            (shown[i + 3] as f64 * coverage).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
            filters::apply_effects(
                &Raster::from_rgba(image.width, image.height, shown),
                effects,
            )
        })
    })
}
fn layer_effect_transform(layer: &Layer, r: &Raster, inset: f64) -> crate::model::Transform {
    let mut t = layer.transform;
    let ow = r.width as f64 - inset * 2.0;
    let oh = r.height as f64 - inset * 2.0;
    t.size = [
        t.size[0] * r.width as f64 / ow,
        t.size[1] * r.height as f64 / oh,
    ];
    t.origin = [
        layer.transform.origin[0] + (layer.transform.size[0] - t.size[0]) / 2.0,
        layer.transform.origin[1] + (layer.transform.size[1] - t.size[1]) / 2.0,
    ];
    t
}
#[allow(clippy::too_many_arguments)] // Caches are borrowed by every clipping-stack traversal.
fn render_group(
    doc: &Document,
    parent: Option<Uuid>,
    region: [f64; 4],
    width: u32,
    height: u32,
    depth: usize,
    effects: Option<&HashMap<Uuid, (Raster, f64)>>,
    samplers: Option<&HashMap<Uuid, LayerSampler>>,
) -> Vec<u8> {
    let mut bottom = vec![0u8; width as usize * height as usize * 4];
    if depth > 256 {
        return bottom;
    }
    let mut ordered = Vec::new();
    ordered_visible(doc, parent, depth, &mut ordered);
    render_layers(
        doc,
        &ordered,
        &mut bottom,
        region,
        width,
        height,
        false,
        None,
        effects,
        samplers,
    );
    bottom
}
#[allow(clippy::too_many_arguments)] // Shared render target and stack state avoid copying a document or surface.
fn render_layers(
    doc: &Document,
    ordered: &[&Layer],
    bottom: &mut [u8],
    region: [f64; 4],
    width: u32,
    height: u32,
    stack_member: bool,
    blend_override: Option<BlendMode>,
    effect_cache: Option<&HashMap<Uuid, (Raster, f64)>>,
    sampler_cache: Option<&HashMap<Uuid, LayerSampler>>,
) {
    let mut index = 0;
    while index < ordered.len() {
        let layer = ordered[index];
        index += 1;
        if !stack_member && layer.mask_source_id.is_none() && layer.adjustment.is_none() {
            let first_child = index;
            while index < ordered.len()
                && ordered[index].mask_source_id == Some(layer.id)
                && ordered[index].parent_id == layer.parent_id
            {
                index += 1;
            }
            if first_child < index {
                let mut stack = vec![0; bottom.len()];
                render_layers(
                    doc,
                    &[layer],
                    &mut stack,
                    region,
                    width,
                    height,
                    true,
                    Some(BlendMode::Normal),
                    effect_cache,
                    sampler_cache,
                );
                let alpha = pixels::extract_alpha(&stack);
                pixels::unpremultiply(&mut stack);
                for pixel in stack.as_chunks_mut::<4>().0 {
                    pixel[3] = 255;
                }
                render_layers(
                    doc,
                    &ordered[first_child..index],
                    &mut stack,
                    region,
                    width,
                    height,
                    true,
                    None,
                    effect_cache,
                    sampler_cache,
                );
                pixels::restore_alpha(&mut stack, &alpha);
                for (b, s) in bottom
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(stack.as_chunks::<4>().0.iter())
                {
                    let out = blend_premultiplied(
                        std::array::from_fn(|c| b[c] as f64 / 255.0),
                        std::array::from_fn(|c| s[c] as f64 / 255.0),
                        layer.blend_mode,
                    );
                    for c in 0..4 {
                        b[c] = (out[c] * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
                continue;
            }
        }
        let mut own = layer.clone();
        if stack_member {
            own.mask_source_id = None;
        }
        if let Some(adjustment) = &layer.adjustment {
            let raster = to_straight_raster(bottom.to_vec(), width, height);
            let adjusted = filters::apply_adjustment(&raster, adjustment);
            for y in 0..height {
                for x in 0..width {
                    let point = [
                        region[0] + (x as f64 + 0.5) * region[2] / width as f64,
                        region[1] + (y as f64 + 0.5) * region[3] / height as f64,
                    ];
                    let uv = layer.transform.from_document(point);
                    if !(0.0..=1.0).contains(&uv[0]) || !(0.0..=1.0).contains(&uv[1]) {
                        continue;
                    }
                    let i = ((y * width + x) * 4) as usize;
                    let weight = (effective_opacity(doc, layer)
                        * layer_mask(doc, &own, point, 0)
                        * folder_coverage(doc, layer, point))
                    .clamp(0.0, 1.0);
                    let p = adjusted.get(x, y);
                    let mut pm = p;
                    pixels::premultiply(&mut pm);
                    if layer.blend_mode != BlendMode::Normal {
                        let alpha = bottom[i + 3] as f64 / 255.0;
                        if alpha > 0.0 {
                            let b = std::array::from_fn(|c| {
                                if c == 3 {
                                    1.0
                                } else {
                                    bottom[i + c] as f64 / 255.0 / alpha
                                }
                            });
                            let s = std::array::from_fn(|c| {
                                if c == 3 {
                                    1.0
                                } else {
                                    pm[c] as f64 / 255.0 / alpha
                                }
                            });
                            let blended = blend_premultiplied(b, s, layer.blend_mode);
                            for c in 0..3 {
                                pm[c] =
                                    (blended[c] * alpha * 255.0).round().clamp(0.0, 255.0) as u8;
                            }
                        }
                    }
                    // Adjustment interpolates the full lower composite, preserving its original alpha.
                    for c in 0..3 {
                        bottom[i + c] = (bottom[i + c] as f64 * (1.0 - weight)
                            + pm[c] as f64 * weight)
                            .round() as u8;
                    }
                }
            }
            continue;
        }
        let cached_effect = effect_cache.and_then(|cache| cache.get(&layer.id));
        let calculated_effect = if cached_effect.is_none() {
            prepare_layer_effect(doc, layer)
        } else {
            None
        };
        let effect = cached_effect.or(calculated_effect.as_ref());
        let (image, transform) = if let Some((r, inset)) = &effect {
            (r, layer_effect_transform(layer, r, *inset))
        } else if let Some(image) = &layer.image {
            (image.as_ref(), layer.transform)
        } else {
            continue;
        };
        let corners = transform.corners();
        let cached_sampler = sampler_cache.and_then(|cache| cache.get(&layer.id));
        let calculated_sampler = cached_sampler.is_none().then(|| {
            LayerSampler::new(
                image,
                transform.sampling,
                [
                    transform.size[0] * width as f64 / region[2],
                    transform.size[1] * height as f64 / region[3],
                ],
            )
        });
        let sampler = cached_sampler.or(calculated_sampler.as_ref()).unwrap();
        let radius = if transform.sampling == Sampling::High {
            2.
        } else {
            1.
        };
        let padding = (radius
            * (transform.size[0].abs() / image.width.max(1) as f64
                + transform.size[1].abs() / image.height.max(1) as f64)
            * (width as f64 / region[2]).max(height as f64 / region[3]))
        .max(1.);
        let minx = corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
        let maxx = corners
            .iter()
            .map(|p| p[0])
            .fold(f64::NEG_INFINITY, f64::max);
        let miny = corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
        let maxy = corners
            .iter()
            .map(|p| p[1])
            .fold(f64::NEG_INFINITY, f64::max);
        let mut x0 = (((minx - region[0]) * width as f64 / region[2]).floor() - padding)
            .clamp(0.0, width as f64) as u32;
        let mut x1 = (((maxx - region[0]) * width as f64 / region[2]).ceil() + padding)
            .clamp(0.0, width as f64) as u32;
        let mut y0 = (((miny - region[1]) * height as f64 / region[3]).floor() - padding)
            .clamp(0.0, height as f64) as u32;
        let mut y1 = (((maxy - region[1]) * height as f64 / region[3]).ceil() + padding)
            .clamp(0.0, height as f64) as u32;
        let Some(support) = sampler.support else {
            continue;
        };
        // A single-pixel target cannot benefit from narrowing its bounding box again.
        // The sampler still rejects coordinates outside the occupied support.
        if width != 1 || height != 1 {
            let support_corners = [
                [support[0], support[1]],
                [support[2], support[1]],
                [support[2], support[3]],
                [support[0], support[3]],
            ]
            .map(|uv| transform.to_document(uv));
            let sparse_minx = support_corners
                .iter()
                .map(|p| p[0])
                .fold(f64::INFINITY, f64::min);
            let sparse_maxx = support_corners
                .iter()
                .map(|p| p[0])
                .fold(f64::NEG_INFINITY, f64::max);
            let sparse_miny = support_corners
                .iter()
                .map(|p| p[1])
                .fold(f64::INFINITY, f64::min);
            let sparse_maxy = support_corners
                .iter()
                .map(|p| p[1])
                .fold(f64::NEG_INFINITY, f64::max);
            x0 = x0.max(
                (((sparse_minx - region[0]) * width as f64 / region[2]).floor() - 1.)
                    .clamp(0., width as f64) as u32,
            );
            x1 = x1.min(
                (((sparse_maxx - region[0]) * width as f64 / region[2]).ceil() + 1.)
                    .clamp(0., width as f64) as u32,
            );
            y0 = y0.max(
                (((sparse_miny - region[1]) * height as f64 / region[3]).floor() - 1.)
                    .clamp(0., height as f64) as u32,
            );
            y1 = y1.min(
                (((sparse_maxy - region[1]) * height as f64 / region[3]).ceil() + 1.)
                    .clamp(0., height as f64) as u32,
            );
        }
        if effect.is_some() {
            own.mask = None;
        }
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let layer_opacity = effective_opacity(doc, layer);
        let stride = width as usize * 4;
        let blend_mode = blend_override.unwrap_or(layer.blend_mode);
        let compose_row = |(row_index, row): (usize, &mut [u8])| {
            let y = y0 + row_index as u32;
            for (column, bottom_pixel) in row[x0 as usize * 4..x1 as usize * 4]
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .enumerate()
            {
                let x = x0 + column as u32;
                let point = [
                    region[0] + (x as f64 + 0.5) * region[2] / width as f64,
                    region[1] + (y as f64 + 0.5) * region[3] / height as f64,
                ];
                let uv = transform.from_document(point);
                let mut top = sampler.sample(uv);
                if top[3] <= 0.0 {
                    continue;
                }
                let opacity = layer_opacity
                    * layer_mask(doc, &own, point, 0)
                    * folder_coverage(doc, layer, point);
                for c in &mut top {
                    *c *= opacity;
                }
                if top[3] <= 0.0 {
                    continue;
                }
                let b = std::array::from_fn(|c| bottom_pixel[c] as f64 / 255.0);
                let p = blend_premultiplied(b, top, blend_mode);
                for c in 0..4 {
                    bottom_pixel[c] = (p[c] * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        };
        let rows = &mut bottom[y0 as usize * stride..y1 as usize * stride];
        if (x1 - x0) as u64 * (y1 - y0) as u64 >= 64 * 1024
            && y1 - y0 >= 16
            && rayon::current_num_threads() > 1
        {
            rows.par_chunks_exact_mut(stride)
                .enumerate()
                .for_each(compose_row);
        } else {
            rows.chunks_exact_mut(stride)
                .enumerate()
                .for_each(compose_row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn row_parallel_composition_matches_one_thread_for_dense_sparse_and_masked_stacks() {
        let serial = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let parallel = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let mut source = Raster::blank(529, 397);
        source.set(0, 0, [250, 23, 171, 2]);
        source.set(528, 396, [15, 225, 141, 255]);
        for y in 128..257 {
            for x in 127..259 {
                source.set(x, y, [171, 59, 211, ((x + y * 3) % 221 + 21) as u8]);
            }
        }
        let mut doc = Document::new(387, 263);
        let background = Raster::from_rgba(
            doc.width,
            doc.height,
            (0..doc.width * doc.height)
                .flat_map(|i| [i as u8, (i / 7) as u8, (i / 13) as u8, (i % 191 + 41) as u8])
                .collect(),
        );
        let mut folder = Layer::new("Folder", doc.width, doc.height);
        folder.is_group = true;
        folder.opacity = 0.71;
        folder.mask = Some(crate::model::Mask {
            raster: Arc::new(Raster::from_rgba(
                23,
                17,
                (0..23 * 17)
                    .flat_map(|i| {
                        let gray = (i % 251) as u8;
                        [gray, gray, gray, 255]
                    })
                    .collect(),
            )),
            enabled: true,
            linked: false,
            placement: Some(crate::model::Transform::new(20., -7., 327., 211.)),
        });
        let mut layer = Layer::raster("Sparse", source);
        layer.parent_id = Some(folder.id);
        layer.transform.size = [doc.width as f64, doc.height as f64];
        layer.transform.rotation = 7.;
        layer.transform.flip_x = true;
        layer.opacity = 0.83;
        layer.mask = Some(crate::model::Mask {
            raster: Arc::new(Raster::solid(1, 1, [217, 217, 217, 255])),
            enabled: true,
            linked: true,
            placement: None,
        });
        let mut clip = Layer::raster("Clip", Raster::solid(529, 397, [21, 241, 131, 117]));
        clip.parent_id = Some(folder.id);
        clip.mask_source_id = Some(layer.id);
        clip.transform = layer.transform;
        doc.layers = vec![
            Layer::raster("Dense background", background),
            folder,
            layer,
            clip,
        ];
        for sampling in [Sampling::Nearest, Sampling::Smooth, Sampling::High] {
            doc.layers[2].transform.sampling = sampling;
            doc.layers[3].transform.sampling = sampling;
            for mode in [
                BlendMode::Normal,
                BlendMode::ColorDodge,
                BlendMode::Luminosity,
            ] {
                doc.layers[2].blend_mode = mode;
                let expected = serial.install(|| render_document(&doc));
                assert_eq!(
                    parallel.install(|| render_document(&doc)),
                    expected,
                    "{sampling:?}, {mode:?}"
                );
                let mut dense = doc.clone();
                dense.layers[2].image = dense.layers[2]
                    .image
                    .as_ref()
                    .map(|image| Arc::new(image.materialized()));
                assert_eq!(
                    parallel.install(|| render_document(&dense)),
                    expected,
                    "dense {sampling:?}, {mode:?}"
                );
            }
        }
    }
    #[test]
    fn sparse_mips_match_dense_grid_at_odd_dimensions_and_fractional_lod() {
        for (width, height) in [(523, 301), (519, 1), (1, 301)] {
            let mut source = Raster::blank(width, height);
            for i in 0..31 {
                source.set(
                    (width / 2 + i * 3).min(width - 1),
                    (height / 2 + i * 2).min(height - 1),
                    [
                        i as u8 * 7,
                        220 - i as u8 * 5,
                        i as u8 * 3,
                        17 + i as u8 * 7,
                    ],
                );
            }
            let mut sources = vec![source];
            if width > 1 && height > 1 {
                let mut edges = sources[0].clone();
                for (x, y, alpha) in [
                    (0, 0, 1),
                    (width - 1, height - 1, 2),
                    (127, 127, 1),
                    (128, 127, 2),
                    (127, 128, 255),
                    (128, 128, 255),
                ] {
                    edges.set(x, y, [251, 13, 173, alpha]);
                }
                sources.push(edges);
            }
            for source in sources {
                let dense = source.materialized();
                for display in [
                    [width as f64, height as f64],
                    [79., 45.],
                    [37., 21.],
                    [7., 3.],
                ] {
                    for sampling in [Sampling::Nearest, Sampling::Smooth, Sampling::High] {
                        let sparse = LayerSampler::new(&source, sampling, display);
                        let reference = LayerSampler::new(&dense, sampling, display);
                        assert_eq!(sparse.first_level, reference.first_level);
                        for (actual, expected) in sparse.levels.iter().zip(&reference.levels) {
                            for y in 0..expected.height {
                                for x in 0..expected.width {
                                    assert_eq!(
                                        actual.pixel(x as i64, y as i64),
                                        expected.pixel(x as i64, y as i64)
                                    );
                                }
                            }
                        }
                        for y in -2..35 {
                            for x in -2..35 {
                                let uv = [x as f64 / 32., y as f64 / 32.];
                                assert_eq!(
                                    sparse.sample(uv),
                                    reference.sample(uv),
                                    "{width}x{height} {sampling:?} {display:?}, {uv:?}"
                                );
                            }
                        }
                        let bounds = raster_support(&source);
                        let mut positions = vec![
                            [0.5 / width as f64, 0.5 / height as f64],
                            [
                                (width as f64 - 0.5) / width as f64,
                                (height as f64 - 0.5) / height as f64,
                            ],
                        ];
                        for y in [127., 128., bounds[1] as f64, bounds[3] as f64] {
                            for x in [127., 128., bounds[0] as f64, bounds[2] as f64] {
                                positions
                                    .push([(x + 0.5) / width as f64, (y + 0.5) / height as f64]);
                            }
                        }
                        let center = [
                            (bounds[0] as f64 + bounds[2] as f64) / (2. * width as f64),
                            (bounds[1] as f64 + bounds[3] as f64) / (2. * height as f64),
                        ];
                        for offset in [-1e-12, 0., 1e-12] {
                            for edge in [bounds[0], bounds[2]] {
                                positions.push([edge as f64 / width as f64 + offset, center[1]]);
                            }
                            for edge in [bounds[1], bounds[3]] {
                                positions.push([center[0], edge as f64 / height as f64 + offset]);
                            }
                        }
                        if let Some(support) = sparse.support {
                            for offset in [-1e-12, 0., 1e-12] {
                                for edge in [support[0], support[2]] {
                                    positions.push([edge + offset, center[1]]);
                                }
                                for edge in [support[1], support[3]] {
                                    positions.push([center[0], edge + offset]);
                                }
                            }
                            for y in [support[1], support[3]] {
                                for x in [support[0], support[2]] {
                                    for dy in [-1e-12, 0., 1e-12] {
                                        for dx in [-1e-12, 0., 1e-12] {
                                            positions.push([x + dx, y + dy]);
                                        }
                                    }
                                }
                            }
                        }
                        for uv in positions {
                            assert_eq!(
                                sparse.sample(uv),
                                reference.sample(uv),
                                "edge {width}x{height} {sampling:?} {display:?}, {uv:?}"
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn sparse_render_matches_dense_with_rotation_flip_masks_blends_and_clipping() {
        let mut source = Raster::blank(387, 263);
        for y in 140..181 {
            for x in 181..226 {
                source.set(x, y, [180, 51, 209, ((x + y) % 193 + 32) as u8]);
            }
        }
        let mut doc = Document::new(193, 137);
        let mut folder = Layer::new("Folder", doc.width, doc.height);
        folder.is_group = true;
        folder.opacity = 0.71;
        folder.mask = Some(crate::model::Mask {
            raster: Arc::new(Raster::solid(1, 1, [231, 231, 231, 255])),
            enabled: true,
            linked: true,
            placement: None,
        });
        let mut layer = Layer::raster("Sparse", source);
        layer.parent_id = Some(folder.id);
        layer.transform.origin = [-19., 12.];
        layer.transform.size = [171., 116.];
        layer.transform.rotation = 37.;
        layer.transform.flip_x = true;
        layer.transform.flip_y = true;
        layer.opacity = 0.83;
        layer.mask = folder.mask.clone();
        let mut clip = Layer::raster("Clipping", Raster::solid(387, 263, [13, 211, 117, 127]));
        clip.parent_id = Some(folder.id);
        clip.mask_source_id = Some(layer.id);
        clip.transform = layer.transform;
        doc.layers = vec![
            Layer::raster(
                "Background",
                Raster::solid(doc.width, doc.height, [50, 90, 130, 171]),
            ),
            folder,
            layer,
            clip,
        ];
        for (flip_x, flip_y) in [(true, true), (true, false), (false, true)] {
            doc.layers[2].transform.flip_x = flip_x;
            doc.layers[2].transform.flip_y = flip_y;
            doc.layers[3].transform = doc.layers[2].transform;
            for mode in BlendMode::ALL {
                doc.layers[2].blend_mode = mode;
                let mut dense = doc.clone();
                for layer in &mut dense.layers {
                    layer.image = layer
                        .image
                        .as_ref()
                        .map(|image| Arc::new(image.materialized()));
                }
                for (region, width, height) in [
                    ([0., 0., 193., 137.], 71, 51),
                    ([53., 29., 111., 91.], 83, 69),
                    ([66., 81., 1., 1.], 1, 1),
                    ([0., 0., 1., 1.], 1, 1),
                ] {
                    assert_eq!(
                        render_region(&doc, region, width, height),
                        render_region(&dense, region, width, height),
                        "{mode:?}, flip={flip_x},{flip_y}, {region:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn sparse_minification_retains_only_occupied_mip_pixels() {
        let mut source = Raster::blank(4096, 4096);
        source.set(2048, 2048, [255, 70, 21, 255]);
        let sampler = LayerSampler::new(&source, Sampling::High, [1024., 1024.]);
        assert_eq!(sampler.levels.len(), 1);
        assert_eq!(sampler.levels[0].pixels.len(), 32 * 32 * 4);
        assert_eq!(sampler.sample([0.1, 0.1]), [0.; 4]);
        assert!(
            LayerSampler::new(&Raster::blank(4096, 4096), Sampling::High, [1024., 1024.])
                .levels
                .iter()
                .all(|mip| mip.pixels.is_empty())
        );
    }
    #[test]
    fn native_picker_preserves_single_pixel_details_and_invalidates_changed_tiles() {
        let mut d = Document::new(4096, 1);
        let mut image = Raster::solid(4096, 1, [0, 0, 255, 255]);
        image.set(3001, 0, [255, 0, 0, 255]);
        d.layers = vec![Layer::raster("Fine detail", image)];
        let sampler = CompositeSampler::new(&d);
        assert!(!sampler.uses_full_surface());
        assert_eq!(sampler.sample([3001.8, 0.1]), Some([255, 0, 0, 255]));
        assert_ne!(render_preview(&d, 2048).get(1500, 0), [255, 0, 0, 255]);
        assert_eq!(sampler.sample([-0.1, 0.]), None);
        assert_eq!(sampler.sample([4096., 0.]), None);
        assert_eq!(sampler.sample([f64::NAN, 0.]), None);
        d.selection = Some(crate::model::Selection::full(d.width, d.height));
        d.active_layer_id = None;
        d.layers[0].name = "Renamed".into();
        assert!(sampler.matches(&d));
        let mut image = d.layers[0].image.as_ref().unwrap().as_ref().clone();
        image.set(3001, 0, [0, 255, 0, 255]);
        d.layers[0].image = Some(Arc::new(image));
        assert!(!sampler.matches(&d));
        assert_eq!(
            CompositeSampler::new(&d).sample([3001., 0.]),
            Some([0, 255, 0, 255])
        );
    }
    #[test]
    fn native_picker_matches_groups_clipping_effects_masks_and_color_adjustments() {
        use serde_json::json;
        let mut d = Document::new(31, 23);
        let mut folder = Layer::new("Folder", 31, 23);
        folder.is_group = true;
        folder.opacity = 0.73;
        folder.mask = Some(crate::model::Mask {
            raster: Arc::new(Raster::solid(1, 1, [173, 173, 173, 255])),
            enabled: true,
            linked: false,
            placement: None,
        });
        let mut base = Layer::raster(
            "Base",
            Raster::from_rgba(
                47,
                41,
                (0..47 * 41)
                    .flat_map(|i| {
                        [
                            (i * 13 % 256) as u8,
                            (i * 19 % 256) as u8,
                            (i * 29 % 256) as u8,
                            (20 + i * 7 % 236) as u8,
                        ]
                    })
                    .collect(),
            ),
        );
        base.parent_id = Some(folder.id);
        base.transform.origin = [2., 3.];
        base.transform.size = [19., 15.];
        base.transform.rotation = 17.;
        base.mask = Some(crate::model::Mask {
            raster: Arc::new(Raster::solid(1, 1, [204, 204, 204, 255])),
            enabled: true,
            linked: true,
            placement: None,
        });
        base.effects = Some(
            json!({"shadow":{"enabled":true,"distance":2,"blur":2,"opacity":0.6,"color":{"red":0.1,"green":0.8,"blue":0.2}}}),
        );
        let mut clipped = Layer::raster("Clip", Raster::solid(31, 23, [200, 40, 100, 128]));
        clipped.mask_source_id = Some(base.id);
        clipped.parent_id = Some(folder.id);
        let mut adjustment = Layer::new("Exposure", 31, 23);
        adjustment.adjustment = Some(
            json!({"kind":"Exposure","exposureSettings":{"exposure":0.7,"offset":0.04,"gamma":1.2}}),
        );
        adjustment.opacity = 0.62;
        adjustment.blend_mode = BlendMode::SoftLight;
        d.layers = vec![
            Layer::raster("Background", Raster::solid(31, 23, [60, 90, 120, 170])),
            folder,
            base,
            clipped,
            adjustment,
        ];
        for mode in BlendMode::ALL {
            d.layers[2].blend_mode = mode;
            let expected = render_document(&d);
            let sampler = CompositeSampler::new(&d);
            assert!(!sampler.uses_full_surface());
            for y in [0, 4, 10, 15, 22] {
                for x in [0, 3, 12, 20, 30] {
                    assert_eq!(
                        sampler.sample([x as f64 + 0.8, y as f64 + 0.2]),
                        Some(expected.get(x, y)),
                        "{mode:?}, {x},{y}"
                    );
                }
            }
        }
    }
    #[test]
    fn native_picker_retains_spatial_neighborhoods_and_global_coordinate_seeds() {
        use serde_json::json;
        for settings in [
            json!({"kind":"Gaussian Blur","blurRadius":3}),
            json!({"kind":"Add Noise","noiseAmount":25,"noiseSeed":745}),
            json!({"kind":"Lens Correction","distortion":70}),
            json!({"kind":"Camera Raw Filter","geometry":{"rotate":14},"clarity":50}),
        ] {
            let mut d = Document::new(17, 13);
            d.layers = vec![Layer::raster(
                "Image",
                Raster::from_rgba(
                    17,
                    13,
                    (0..17 * 13)
                        .flat_map(|i| {
                            [
                                (i * 17 % 256) as u8,
                                (i * 23 % 256) as u8,
                                (i * 47 % 256) as u8,
                                255,
                            ]
                        })
                        .collect(),
                ),
            )];
            let mut a = Layer::new("Spatial", 17, 13);
            a.adjustment = Some(settings);
            d.layers.push(a);
            let expected = render_document(&d);
            let sampler = CompositeSampler::new(&d);
            assert!(sampler.uses_full_surface());
            for y in 0..13 {
                for x in 0..17 {
                    assert_eq!(
                        sampler.sample([x as f64, y as f64]),
                        Some(expected.get(x, y))
                    );
                }
            }
        }
    }
    #[test]
    fn soft_dodge_keeps_alpha_edge() {
        assert_eq!(
            blend_pixel(
                [102, 102, 102, 255],
                [204, 204, 204, 128],
                BlendMode::ColorDodge,
                1.0
            ),
            [179, 179, 179, 255]
        );
    }
    #[test]
    fn transparent_sample_has_no_halo() {
        let r = Raster::from_rgba(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 0]);
        assert_eq!(sample(&r, 0.5, 0.0), [255, 0, 0, 128]);
    }
    #[test]
    fn mask_placement_is_independent_of_linkage_and_uniform_masks_are_global() {
        let mut layer = Layer::raster("Source", Raster::solid(3, 1, [255, 0, 0, 255]));
        layer.mask = Some(crate::model::Mask {
            raster: Arc::new(Raster::solid(1, 1, [128, 128, 128, 255])),
            enabled: true,
            linked: true,
            placement: Some(crate::model::Transform::new(100., 100., 1., 1.)),
        });
        assert_eq!(raster_mask_coverage(&layer, [0.5, 0.5]), 128. / 255.);
        layer.mask = Some(crate::model::Mask {
            raster: Arc::new(Raster::from_rgba(
                3,
                1,
                vec![255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255],
            )),
            enabled: true,
            linked: true,
            placement: Some(crate::model::Transform::new(1., 0., 3., 1.)),
        });
        assert!((raster_mask_coverage(&layer, [0.5, 0.5]) - 1.).abs() < 1e-12);
        assert_eq!(raster_mask_coverage(&layer, [2.5, 0.5]), 0.);
        assert!((raster_mask_coverage(&layer, [4.5, 0.5]) - 1.).abs() < 1e-12);
    }
    #[test]
    fn high_sampling_resolves_edges_without_invisible_color_or_overshoot() {
        let source = Raster::from_rgba(
            5,
            1,
            vec![
                0, 255, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0,
            ],
        );
        let smooth = sample_with_sampling(&source, 2.25, 0., Sampling::Smooth);
        let high = sample_with_sampling(&source, 2.25, 0., Sampling::High);
        assert_ne!(
            high[3], smooth[3],
            "High has a distinct reconstruction kernel"
        );
        assert_eq!(
            &high[..3],
            &[255, 0, 0],
            "Transparent blue/green never enters visible red"
        );
        for x in -16..64 {
            let p = sample_premultiplied(&source, x as f64 / 16., 0., Sampling::High);
            assert!((0. ..=1.).contains(&p[3]));
            assert!(p[..3].iter().all(|v| (0. ..=p[3]).contains(v)));
        }
        for x in 0..5 {
            assert_eq!(
                sample_with_sampling(&source, x as f64, 0., Sampling::High),
                if source.get(x, 0)[3] == 0 {
                    [0; 4]
                } else {
                    source.get(x, 0)
                }
            );
        }
        let mask = Raster::from_rgba(
            4,
            1,
            vec![
                0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ],
        );
        assert!(
            (sample_mask_with_sampling(&mask, 1.25, 0., 0, Sampling::High)
                - sample_mask_with_sampling(&mask, 1.25, 0., 0, Sampling::Smooth))
            .abs()
                > 0.01
        );
    }
    #[test]
    fn high_minification_prefilters_fine_detail_and_odd_dimensions() {
        let data = (0..256 * 256)
            .flat_map(|i| {
                let v = if (i % 256 + i / 256) % 2 == 0 { 0 } else { 255 };
                [v, v, v, 255]
            })
            .collect();
        let mut doc = Document::new(256, 256);
        doc.layers = vec![Layer::raster("Checker", Raster::from_rgba(256, 256, data))];
        assert_eq!(render_preview(&doc, 32).get(16, 16), [128, 128, 128, 255]);
        let source = Raster::solid(9, 7, [200, 80, 20, 255]);
        let sampler = LayerSampler::new(&source, Sampling::High, [2.25, 1.75]);
        assert_eq!(sampler.first_level, 2);
        assert_eq!(sampler.levels.len(), 1);
        assert_eq!((sampler.levels[0].width, sampler.levels[0].height), (2, 1));
        assert!(sampler.levels.iter().all(|m| m.pixels.len() == 4));
        let p = sampler.sample([0.5, 0.5]);
        assert!(
            (p[0] - 200. / 255.).abs() < 1e-12 && (p[3] - 1.).abs() < 1e-12,
            "{p:?}"
        );
    }
    #[test]
    fn picker_retains_only_used_mip_levels_and_reports_extra_pixel_bytes() {
        let image = Raster::from_rgba(64, 32, vec![127; 64 * 32 * 4]);
        let mut layer = Layer::raster("Scaled", image);
        layer.transform.size = [12., 6.];
        let mut d = Document::new(16, 8);
        d.layers = vec![layer];
        let sampler = CompositeSampler::new(&d);
        let cached = &sampler.samplers[&d.layers[0].id];
        assert_eq!(cached.first_level, 2);
        assert_eq!(cached.levels.len(), 2);
        assert_eq!(sampler.allocated_bytes(), (16 * 8 + 8 * 4) * 4);
        let full = render_document(&d);
        for y in 0..8 {
            for x in 0..16 {
                assert_eq!(sampler.sample([x as f64, y as f64]), Some(full.get(x, y)));
            }
        }
    }
    #[test]
    fn folder_opacity_passes_through_per_child() {
        let mut d = Document::new(2, 1);
        d.layers.clear();
        let mut g = Layer::new("Group", 2, 1);
        g.is_group = true;
        g.opacity = 0.5;
        let id = g.id;
        let mut a = Layer::raster("A", Raster::solid(2, 1, [255, 0, 0, 255]));
        a.parent_id = Some(id);
        let mut b = Layer::raster("B", Raster::solid(2, 1, [0, 0, 255, 255]));
        b.parent_id = Some(id);
        d.layers = vec![g, a, b];
        assert_eq!(render_document(&d).get(0, 0), [85, 0, 170, 192]);
    }
    #[test]
    fn live_clipping_ignores_source_visibility() {
        let mut d = Document::new(2, 1);
        let mut a = Layer::raster(
            "Base",
            Raster::from_rgba(2, 1, vec![0, 0, 0, 255, 0, 0, 0, 0]),
        );
        a.visible = false;
        let mut b = Layer::raster("Clip", Raster::solid(2, 1, [255, 0, 0, 255]));
        b.mask_source_id = Some(a.id);
        d.layers = vec![a, b];
        let r = render_document(&d);
        assert_eq!(r.get(0, 0), [255, 0, 0, 255]);
        assert_eq!(r.get(1, 0), [0; 4]);
    }
    #[test]
    fn clipping_stack_shares_soft_base_alpha_and_base_blend() {
        let mut d = Document::new(1, 1);
        let base = Layer::raster("Base", Raster::solid(1, 1, [255, 0, 0, 128]));
        let mut clip = Layer::raster("Clip", Raster::solid(1, 1, [0, 0, 255, 128]));
        clip.mask_source_id = Some(base.id);
        d.layers = vec![base, clip];
        assert_eq!(render_document(&d).get(0, 0), [128, 0, 128, 128]);
        d.layers[0].blend_mode = BlendMode::Multiply;
        d.layers.insert(
            0,
            Layer::raster("Background", Raster::solid(1, 1, [128, 128, 128, 255])),
        );
        assert_eq!(render_document(&d).get(0, 0)[3], 255);
        let p = render_document(&d).get(0, 0);
        assert!(p[0] <= 97 && p[2] <= 97, "Stack uses base Multiply: {p:?}");
    }
    #[test]
    fn mask_and_flip() {
        let mut d = Document::new(2, 1);
        let mut l = Layer::raster("Layer", Raster::solid(2, 1, [255, 0, 0, 255]));
        l.mask = Some(crate::model::Mask {
            raster: Arc::new(Raster::from_rgba(
                2,
                1,
                vec![255; 4].into_iter().chain([0, 0, 0, 255]).collect(),
            )),
            enabled: true,
            linked: true,
            placement: None,
        });
        l.transform.flip_x = true;
        d.layers = vec![l];
        let r = render_document(&d);
        assert_eq!(r.get(0, 0), [0; 4]);
        assert_eq!(r.get(1, 0), [255, 0, 0, 255]);
    }
}
