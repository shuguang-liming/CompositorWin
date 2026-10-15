//! Subject-mask refinement and non-destructive application, ported from GuidedMatte and SubjectRemoval.
use crate::{
    model::{Document, Mask, Raster},
    render,
};
use anyhow::{Result, ensure};
use rayon::prelude::*;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundQuality {
    Basic,
    Advanced,
}
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub background_quality: BackgroundQuality,
    pub refine_edges: f64,
    pub shift_edge: f64,
    pub matte_contrast: f64,
    pub preview_scale: f64,
}
impl Settings {
    pub fn from_value(value: &Value) -> Self {
        let number = |key: &str, fallback: f64, min: f64, max: f64| {
            value[key]
                .as_f64()
                .filter(|v| v.is_finite())
                .unwrap_or(fallback)
                .clamp(min, max)
        };
        Self {
            background_quality: if value["backgroundQuality"].as_str() == Some("Advanced") {
                BackgroundQuality::Advanced
            } else {
                BackgroundQuality::Basic
            },
            refine_edges: number("refineEdges", 12., 0., 40.),
            shift_edge: number("shiftEdge", 0., -10., 10.),
            matte_contrast: number("matteContrast", 25., 0., 100.),
            preview_scale: number("previewScale", 1., 0.001, 1.),
        }
    }
}

/// Radius controls remain in native pixels while a reduced preview scales their reach.
pub fn preview_settings(settings: &Value, scale: f64) -> Value {
    let mut value = settings.clone();
    value["previewScale"] = Value::from(if scale.is_finite() {
        scale.clamp(0.001, 1.)
    } else {
        1.
    });
    value
}

pub fn refine(alpha: &[u8], source: &Raster, settings: &Value) -> Result<Vec<u8>> {
    refine_tiled(alpha, source, settings, 768)
}

// Two box-filter stages reach 2r beyond a pixel. Keeping that halo preserves the full-size result
// while only one tile's float buffers are alive; even 200 MP images do not allocate full-size floats.
fn refine_tiled(
    alpha: &[u8],
    source: &Raster,
    settings: &Value,
    tile_side: usize,
) -> Result<Vec<u8>> {
    let width = source.width as usize;
    let height = source.height as usize;
    ensure!(
        Document::valid_size(source.width, source.height),
        "Invalid matte dimensions"
    );
    ensure!(
        alpha.len() == width * height,
        "Subject mask dimensions differ from source"
    );
    let settings = Settings::from_value(settings);
    if settings.background_quality == BackgroundQuality::Basic {
        return Ok(alpha.to_vec());
    }
    let mut mask = alpha.to_vec();
    if settings.refine_edges > 0. {
        let radius = (settings.refine_edges * settings.preview_scale)
            .round()
            .max(1.) as usize;
        for_tiles(width, height, tile_side, radius * 2, |area, inner| {
            let [left, top, right, bottom] = area;
            let tw = right - left;
            let th = bottom - top;
            let guide = (0..tw * th)
                .into_par_iter()
                .map(|i| {
                    let p = source.get((left + i % tw) as u32, (top + i / tw) as u32);
                    // Drawn against black, matching a gray context without alpha.
                    ((0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32)
                        * p[3] as f32
                        / 255.)
                        .round()
                        / 255.
                })
                .collect::<Vec<_>>();
            let input = (top..bottom)
                .flat_map(|y| alpha[y * width + left..y * width + right].iter())
                .map(|&v| v as f32 / 255.)
                .collect::<Vec<_>>();
            let refined = guided(&input, &guide, tw, th, radius, 1e-4);
            for y in inner[1]..inner[3] {
                for x in inner[0]..inner[2] {
                    mask[y * width + x] = byte(refined[(y - top) * tw + x - left]);
                }
            }
        });
    }
    let slope = 1. / (1. - settings.matte_contrast as f32 / 100. * 0.98).max(0.02);
    let contrast = |v: f32| byte(v * slope + (1. - slope) / 2.);
    if settings.shift_edge != 0. {
        let sigma = settings.shift_edge.abs() * settings.preview_scale / 2.;
        let halo = (sigma * 3.).ceil() as usize;
        let level = if settings.shift_edge < 0. { 0.75 } else { 0.25 };
        let mut shifted = vec![0; mask.len()];
        for_tiles(width, height, tile_side, halo, |area, inner| {
            let [left, top, right, bottom] = area;
            let tw = right - left;
            let input = (top..bottom)
                .flat_map(|y| mask[y * width + left..y * width + right].iter())
                .map(|&v| v as f32 / 255.)
                .collect::<Vec<_>>();
            let blurred = gaussian(&input, tw, bottom - top, sigma);
            for y in inner[1]..inner[3] {
                for x in inner[0]..inner[2] {
                    let value =
                        ((blurred[(y - top) * tw + x - left] - level) / 0.001).clamp(0., 1.);
                    shifted[y * width + x] = contrast(value);
                }
            }
        });
        return Ok(shifted);
    }
    for value in &mut mask {
        *value = contrast(*value as f32 / 255.);
    }
    Ok(mask)
}
fn for_tiles(
    width: usize,
    height: usize,
    side: usize,
    halo: usize,
    mut work: impl FnMut([usize; 4], [usize; 4]),
) {
    for top in (0..height).step_by(side.max(1)) {
        for left in (0..width).step_by(side.max(1)) {
            let right = (left + side).min(width);
            let bottom = (top + side).min(height);
            work(
                [
                    left.saturating_sub(halo),
                    top.saturating_sub(halo),
                    (right + halo).min(width),
                    (bottom + halo).min(height),
                ],
                [left, top, right, bottom],
            );
        }
    }
}
fn byte(value: f32) -> u8 {
    (value.clamp(0., 1.) * 255. + 0.5) as u8
}

/// Clamped (2r+1)^2 mean. Scratch has one float per pixel and runtime is independent of radius.
fn box_mean(source: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    let span = (radius * 2 + 1) as f32;
    let mut pass = vec![0.; source.len()];
    pass.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let input = &source[y * width..(y + 1) * width];
        let mut sum = 0.;
        for i in 0..=radius * 2 {
            sum += input[i.saturating_sub(radius).min(width - 1)];
        }
        for x in 0..width {
            row[x] = sum / span;
            sum -= input[x.saturating_sub(radius)];
            sum += input[(x + radius + 1).min(width - 1)];
        }
    });
    let mut sums = vec![0.; width];
    for i in 0..=radius * 2 {
        let row = i.saturating_sub(radius).min(height - 1) * width;
        for x in 0..width {
            sums[x] += pass[row + x];
        }
    }
    let mut result = vec![0.; source.len()];
    for y in 0..height {
        let previous = y.saturating_sub(radius) * width;
        let next = (y + radius + 1).min(height - 1) * width;
        for x in 0..width {
            result[y * width + x] = sums[x] / span;
            sums[x] -= pass[previous + x];
            sums[x] += pass[next + x];
        }
    }
    result
}

fn guided(
    mask: &[f32],
    guide: &[f32],
    width: usize,
    height: usize,
    radius: usize,
    epsilon: f32,
) -> Vec<f32> {
    let mean_guide = box_mean(guide, width, height, radius);
    let mut mean_mask = box_mean(mask, width, height, radius);
    let squares = guide.iter().map(|v| v * v).collect::<Vec<_>>();
    let mut variance = box_mean(&squares, width, height, radius);
    drop(squares);
    let products = guide
        .iter()
        .zip(mask)
        .map(|(a, b)| a * b)
        .collect::<Vec<_>>();
    let mut slope = box_mean(&products, width, height, radius);
    drop(products);
    for i in 0..guide.len() {
        variance[i] = (variance[i] - mean_guide[i] * mean_guide[i]).max(0.);
        slope[i] = (slope[i] - mean_guide[i] * mean_mask[i]) / (variance[i] + epsilon);
        mean_mask[i] -= slope[i] * mean_guide[i];
    }
    drop(variance);
    drop(mean_guide);
    let mean_slope = box_mean(&slope, width, height, radius);
    drop(slope);
    let mean_offset = box_mean(&mean_mask, width, height, radius);
    drop(mean_mask);
    mean_slope
        .into_iter()
        .zip(guide)
        .zip(mean_offset)
        .map(|((a, &g), b)| (a * g + b).clamp(0., 1.))
        .collect()
}

fn gaussian(source: &[f32], width: usize, height: usize, sigma: f64) -> Vec<f32> {
    if sigma <= 0.01 {
        return source.to_vec();
    }
    let radius = (sigma * 3.).ceil() as i32;
    let mut kernel = (-radius..=radius)
        .map(|i| (-(i as f64).powi(2) / (2. * sigma * sigma)).exp() as f32)
        .collect::<Vec<_>>();
    let sum = kernel.iter().sum::<f32>();
    for value in &mut kernel {
        *value /= sum;
    }
    let mut pass = vec![0.; source.len()];
    pass.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        for (x, out) in row.iter_mut().enumerate() {
            *out = kernel
                .iter()
                .enumerate()
                .map(|(i, &k)| {
                    let sx = (x as i32 + i as i32 - radius).clamp(0, width as i32 - 1) as usize;
                    source[y * width + sx] * k
                })
                .sum();
        }
    });
    let mut result = vec![0.; source.len()];
    result
        .par_chunks_mut(width)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, out) in row.iter_mut().enumerate() {
                *out = kernel
                    .iter()
                    .enumerate()
                    .map(|(i, &k)| {
                        let sy =
                            (y as i32 + i as i32 - radius).clamp(0, height as i32 - 1) as usize;
                        pass[sy * width + x] * k
                    })
                    .sum();
            }
        });
    result
}

/// Replaces only the layer mask. Original pixels, editable text/shape and effects remain shared.
pub fn apply_subject_mask(doc: &mut Document, id: Uuid, alpha: &[u8]) -> Result<()> {
    let layer = doc
        .layer(id)
        .ok_or_else(|| anyhow::anyhow!("Layer does not exist"))?;
    let source = layer
        .image
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Layer has no source image"))?;
    ensure!(
        !layer.is_group && Document::valid_size(source.width, source.height),
        "Invalid source layer"
    );
    ensure!(
        alpha.len() == source.width as usize * source.height as usize,
        "Subject mask dimensions differ from source"
    );
    if doc
        .selection
        .as_ref()
        .is_some_and(|selection| selection.bounds().is_none())
    {
        return Ok(());
    }
    let width = source.width;
    let height = source.height;
    let transform = layer.transform;
    ensure!(
        transform.origin.iter().all(|v| v.is_finite())
            && transform.size.iter().all(|v| v.is_finite() && *v > 0.)
            && transform.rotation.is_finite(),
        "Invalid layer transform"
    );
    let linked = layer.mask.as_ref().is_none_or(|mask| mask.linked);
    let existing = layer
        .mask
        .as_ref()
        .map(|mask| (mask, render::mask_background(&mask.raster)));
    let mut selected = false;
    let mut pixels = Vec::with_capacity(alpha.len() * 4);
    for y in 0..height {
        for x in 0..width {
            let point = transform.to_document([
                (x as f64 + 0.5) / width as f64,
                (y as f64 + 0.5) / height as f64,
            ]);
            let base = existing.map_or(1., |(mask, background)| {
                let placement = mask.placement.unwrap_or(transform);
                let uv = placement.from_document(point);
                render::sample_mask(
                    &mask.raster,
                    uv[0] * mask.raster.width as f64 - 0.5,
                    uv[1] * mask.raster.height as f64 - 0.5,
                    background,
                )
            });
            let selection = doc.selection.as_ref().map_or(1., |selection| {
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
            selected |= selection > 0.;
            let subject = alpha[(y * width + x) as usize] as f64 / 255.;
            let value = (base * (1. - selection + selection * subject) * 255.)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{Selection, Transform},
        selection,
    };
    use serde_json::json;
    #[test]
    fn basic_returns_model_mask_and_advanced_contrast_and_shift_change_real_edges() {
        let source = Raster::solid(32, 8, [255; 4]);
        let mask = (0..32 * 8)
            .map(|i| if i % 32 >= 16 { 255 } else { 0 })
            .collect::<Vec<_>>();
        assert_eq!(
            refine(
                &mask,
                &source,
                &json!({"refineEdges":40,"shiftEdge":10,"matteContrast":100})
            )
            .unwrap(),
            mask
        );
        let shrink = refine(&mask,&source,&json!({"backgroundQuality":"Advanced","refineEdges":0,"shiftEdge":-6,"matteContrast":0})).unwrap();
        let grow = refine(&mask,&source,&json!({"backgroundQuality":"Advanced","refineEdges":0,"shiftEdge":6,"matteContrast":0})).unwrap();
        assert_eq!(shrink[4 * 32 + 16], 0);
        assert_eq!(grow[4 * 32 + 15], 255);
        let values = [0, 64, 128, 192, 255];
        let source = Raster::solid(5, 1, [255; 4]);
        let contrasted = refine(
            &values,
            &source,
            &json!({"backgroundQuality":"Advanced","refineEdges":0,"matteContrast":100}),
        )
        .unwrap();
        assert_eq!(contrasted[0], 0);
        assert_eq!(contrasted[1], 0);
        assert_eq!(contrasted[3], 255);
        assert_eq!(contrasted[4], 255);
    }
    #[test]
    fn guided_filter_follows_image_edge_and_box_mean_matches_clamped_reference() {
        let w = 64;
        let h = 12;
        let source = Raster::from_rgba(
            w,
            h,
            (0..w * h)
                .flat_map(|i| if i % w < 32 { [0, 0, 0, 255] } else { [255; 4] })
                .collect(),
        );
        let mask = (0..w * h)
            .map(|i| (((i % w) as f64 - 20.) / 24.).clamp(0., 1.) * 255.)
            .map(|v| v.round() as u8)
            .collect::<Vec<_>>();
        let settings = json!({"backgroundQuality":"Advanced","refineEdges":8,"matteContrast":0});
        let refined = refine(&mask, &source, &settings).unwrap();
        let flat = refine(&mask, &Raster::solid(w, h, [128, 128, 128, 255]), &settings).unwrap();
        let i = (6 * w + 31) as usize;
        assert!(
            refined[i + 1] as i16 - refined[i] as i16 > flat[i + 1] as i16 - flat[i] as i16 + 50
        );
        let data = (0..35).map(|i| (i % 9) as f32 / 9.).collect::<Vec<_>>();
        let actual = box_mean(&data, 7, 5, 2);
        for y in 0..5 {
            for x in 0..7 {
                let mut sum = 0.;
                for dy in -2..=2 {
                    for dx in -2..=2 {
                        sum += data[((y as i32 + dy).clamp(0, 4) * 7 + (x as i32 + dx).clamp(0, 6))
                            as usize];
                    }
                }
                assert!((actual[y * 7 + x] - sum / 25.).abs() < 1e-6);
            }
        }
    }
    #[test]
    fn subject_mask_multiplies_placed_existing_mask_through_selection_and_preserves_source() {
        let mut doc = Document::new(12, 8);
        let id = doc.active_layer_id.unwrap();
        let image = Arc::new(Raster::solid(8, 4, [30, 100, 180, 255]));
        let t = Transform::new(2., 2., 8., 4.);
        let layer = doc.active_mut().unwrap();
        layer.image = Some(image.clone());
        layer.transform = t;
        layer.text = Some(json!({"text":"Keep metadata"}));
        layer.effects = Some(json!({"stroke":{"size":1}}));
        layer.mask = Some(Mask {
            raster: Arc::new(Raster::solid(2, 4, [128, 128, 128, 255])),
            enabled: false,
            linked: false,
            placement: Some(Transform::new(4., 2., 2., 4.)),
        });
        doc.selection = Some(selection::marquee(12, 8, [4., 2., 3., 4.], false, false));
        let before = doc.clone();
        apply_subject_mask(&mut doc, id, &[128; 32]).unwrap();
        let layer = doc.layer(id).unwrap();
        assert!(Arc::ptr_eq(layer.image.as_ref().unwrap(), &image));
        assert_eq!(layer.text, before.layer(id).unwrap().text);
        assert_eq!(layer.effects, before.layer(id).unwrap().effects);
        let mask = layer.mask.as_ref().unwrap();
        assert!(mask.enabled);
        assert!(!mask.linked);
        assert_eq!(mask.placement, Some(t));
        assert_eq!(mask.raster.get(1, 1)[0], 255);
        assert_eq!(mask.raster.get(3, 1)[0], 64);
        assert_eq!(mask.raster.get(4, 1)[0], 128);
        assert_eq!(mask.raster.get(6, 1)[0], 255);
        doc.selection = Some(Selection::empty(12, 8));
        let before = doc.clone();
        apply_subject_mask(&mut doc, id, &[0; 32]).unwrap();
        assert_eq!(doc, before);
    }
    #[test]
    fn white_mask_exterior_and_soft_selection_are_applied_once() {
        let mut doc = Document::new(8, 4);
        let id = doc.active_layer_id.unwrap();
        doc.active_mut().unwrap().image = Some(Arc::new(Raster::solid(8, 4, [255; 4])));
        doc.active_mut().unwrap().mask = Some(Mask {
            raster: Arc::new(Raster::solid(2, 2, [255; 4])),
            enabled: true,
            linked: true,
            placement: Some(Transform::new(2., 1., 2., 2.)),
        });
        doc.selection = Some(Selection::from_coverage(8, 4, vec![128; 32]));
        apply_subject_mask(&mut doc, id, &[0; 32]).unwrap();
        let mask = doc.active().unwrap().mask.as_ref().unwrap();
        assert_eq!(mask.raster.get(0, 0), [127, 127, 127, 255]);
        assert_eq!(mask.raster.get(3, 2), [127, 127, 127, 255]);
        let settings = preview_settings(
            &json!({"backgroundQuality":"Advanced","refineEdges":20}),
            0.25,
        );
        assert_eq!(Settings::from_value(&settings).preview_scale, 0.25);
        assert_eq!(Settings::from_value(&settings).refine_edges, 20.);
    }
    #[test]
    fn tiled_refinement_preserves_full_filter_at_seams_and_bounds() {
        let (w, h) = (91, 57);
        let source = Raster::from_rgba(
            w,
            h,
            (0..w * h)
                .flat_map(|i| {
                    let x = i % w;
                    let y = i / w;
                    [
                        (x * 17 % 256) as u8,
                        (y * 31 % 256) as u8,
                        ((x + y) * 13 % 256) as u8,
                        (80 + (x * y % 176)) as u8,
                    ]
                })
                .collect(),
        );
        let alpha = (0..w * h)
            .map(|i| ((i % w * 3 + i / w * 7) % 256) as u8)
            .collect::<Vec<_>>();
        for shift in [-3., 0., 3.] {
            let settings = json!({"backgroundQuality":"Advanced","refineEdges":5,"shiftEdge":shift,"matteContrast":25});
            let full = refine_tiled(&alpha, &source, &settings, 200).unwrap();
            let tiled = refine_tiled(&alpha, &source, &settings, 17).unwrap();
            assert!(full.iter().zip(&tiled).all(|(&a, &b)| a.abs_diff(b) <= 1));
        }
        let mut largest = 0;
        for_tiles(16_000, 12_500, 768, 80, |area, _| {
            largest = largest.max((area[2] - area[0]) * (area[3] - area[1]));
        });
        assert_eq!(largest, 928 * 928);
    }
    #[test]
    fn transformed_mask_and_soft_selection_match_document_pixel_sampling() {
        let mut doc = Document::new(32, 24);
        let id = doc.active_layer_id.unwrap();
        let mut t = Transform::new(9., 7., 12., 8.);
        t.rotation = 31.;
        t.flip_x = true;
        t.flip_y = true;
        let mut placement = Transform::new(8., 5., 14., 11.);
        placement.rotation = -19.;
        placement.flip_y = true;
        let old = Arc::new(Raster::from_rgba(
            7,
            5,
            (0..35)
                .flat_map(|i| {
                    let v = (30 + i * 5) as u8;
                    [v, v, v, 255]
                })
                .collect(),
        ));
        doc.active_mut().unwrap().image = Some(Arc::new(Raster::solid(8, 6, [200, 100, 20, 255])));
        doc.active_mut().unwrap().transform = t;
        doc.active_mut().unwrap().mask = Some(Mask {
            raster: old.clone(),
            enabled: true,
            linked: false,
            placement: Some(placement),
        });
        let coverage = (0..doc.width * doc.height)
            .map(|i| if i % doc.width >= 15 { 128 } else { 0 })
            .collect::<Vec<_>>();
        doc.selection = Some(Selection::from_coverage(doc.width, doc.height, coverage));
        let alpha = (0..48).map(|i| (i * 5) as u8).collect::<Vec<_>>();
        let before = doc.clone();
        apply_subject_mask(&mut doc, id, &alpha).unwrap();
        let output = &doc.active().unwrap().mask.as_ref().unwrap().raster;
        let bg = render::mask_background(&old);
        let mut selected = 0;
        for y in 0..6 {
            for x in 0..8 {
                let p = t.to_document([(x as f64 + 0.5) / 8., (y as f64 + 0.5) / 6.]);
                let uv = placement.from_document(p);
                let base = render::sample_mask(&old, uv[0] * 7. - 0.5, uv[1] * 5. - 0.5, bg);
                let s = before
                    .selection
                    .as_ref()
                    .unwrap()
                    .get(p[0].floor() as u32, p[1].floor() as u32) as f64
                    / 255.;
                selected += usize::from(s > 0.);
                let expected = (255.
                    * base
                    * (1. - s + s * alpha[(y * 8 + x) as usize] as f64 / 255.))
                    .round() as u8;
                assert_eq!(output.get(x, y), [expected, expected, expected, 255]);
            }
        }
        assert!(selected > 0 && selected < 48);
        doc.active_mut().unwrap().transform.rotation = f64::NAN;
        assert!(apply_subject_mask(&mut doc, id, &alpha).is_err());
    }
}
