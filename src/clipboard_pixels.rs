use crate::{
    model::{Document, Raster, Sampling, Transform},
    render,
};
use anyhow::{Result, ensure};
use rayon::prelude::*;

/// Copies the active layer's source pixels or mask in document coordinates.
/// Copy Merged includes the document's appearance; ordinary Copy excludes it.
pub fn copy_pixels(
    document: &Document,
    merged: bool,
    mask_target: bool,
) -> Result<Option<(Raster, [f64; 2])>> {
    ensure!(
        Document::valid_size(document.width, document.height),
        "剪贴板画布超出图像大小上限"
    );
    if let Some(selection) = &document.selection {
        ensure!(
            selection.width == document.width && selection.height == document.height,
            "选区尺寸与画布不一致"
        );
    }
    let Some([x0, y0, x1, y1]) = copy_region(document) else {
        return Ok(None);
    };
    let width = x1 - x0;
    let height = y1 - y0;
    let origin = [x0 as f64, y0 as f64];
    let mut pixels = if merged {
        if document.layers.iter().all(|layer| layer.image.is_none()) {
            return Ok(None);
        }
        if render::CompositeSampler::needs_full_surface(document) {
            let sampler = render::CompositeSampler::new(document);
            let mut output = vec![0; width as usize * height as usize * 4];
            output
                .par_chunks_mut(width as usize * 4)
                .enumerate()
                .for_each(|(y, row)| {
                    for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                        let point = [x0 as f64 + x as f64, y0 as f64 + y as f64];
                        pixel.copy_from_slice(&sampler.sample(point).unwrap_or([0; 4]));
                    }
                });
            output
        } else {
            render::render_region(
                document,
                [origin[0], origin[1], width as f64, height as f64],
                width,
                height,
            )
            .to_rgba()
        }
    } else {
        let Some(layer) = document.active() else {
            return Ok(None);
        };
        let (source, transform, mask_background, placed_mask) = if mask_target {
            let Some(mask) = &layer.mask else {
                return Ok(None);
            };
            (
                mask.raster.as_ref(),
                mask.placement.unwrap_or(layer.transform),
                render::mask_background(&mask.raster),
                mask.placement.is_some(),
            )
        } else {
            if layer.is_group {
                return Ok(None);
            }
            let Some(image) = &layer.image else {
                return Ok(None);
            };
            (image.as_ref(), layer.transform, 0, false)
        };
        ensure!(transform.is_valid(), "剪贴板图层变换无效");
        ensure!(
            source.width > 0 && source.height > 0,
            "剪贴板源图像尺寸无效"
        );
        let sampler = (!mask_target
            && transform.sampling == Sampling::High
            && (source.width as f64 > transform.size[0]
                || source.height as f64 > transform.size[1]))
            .then(|| render::LayerSampler::new(source, transform.sampling, transform.size));
        let mut output = vec![0; width as usize * height as usize * 4];
        output
            .par_chunks_mut(width as usize * 4)
            .enumerate()
            .for_each(|(y, row)| {
                for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let point = [x0 as f64 + x as f64 + 0.5, y0 as f64 + y as f64 + 0.5];
                    let unit = transform.from_document(point);
                    let sampled = if mask_target {
                        let gray =
                            mask_pixel(source, transform, unit, mask_background, placed_mask);
                        [gray, gray, gray, 255]
                    } else {
                        image_pixel(source, transform, unit, sampler.as_ref())
                    };
                    pixel.copy_from_slice(&sampled);
                }
            });
        output
    };
    if let Some(selection) = &document.selection {
        pixels
            .par_chunks_mut(width as usize * 4)
            .enumerate()
            .for_each(|(y, row)| {
                for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let coverage = selection.get(x0 + x as u32, y0 + y as u32);
                    pixel[3] = ((pixel[3] as u16 * coverage as u16 + 127) / 255) as u8;
                    if pixel[3] == 0 {
                        pixel.fill(0);
                    }
                }
            });
    }
    Ok(Some((Raster::from_rgba(width, height, pixels), origin)))
}

fn image_pixel(
    source: &Raster,
    transform: Transform,
    unit: [f64; 2],
    sampler: Option<&render::LayerSampler>,
) -> [u8; 4] {
    if let Some(sampler) = sampler {
        let mut pixel = sampler
            .sample(unit)
            .map(|v| (v * 255.).round().clamp(0., 255.) as u8);
        crate::pixels::unpremultiply(&mut pixel);
        return pixel;
    }
    let x = unit[0] * source.width as f64;
    let y = unit[1] * source.height as f64;
    if transform.sampling == Sampling::Nearest {
        if x < 0. || y < 0. || x >= source.width as f64 || y >= source.height as f64 {
            return [0; 4];
        }
        return source.get(x.floor() as u32, y.floor() as u32);
    }
    let sx = x - 0.5;
    let sy = y - 0.5;
    // Preserve source channels exactly for an upright pixel-for-pixel copy.
    if (sx - sx.round()).abs() < 1e-9 && (sy - sy.round()).abs() < 1e-9 {
        if sx < 0. || sy < 0. || sx >= source.width as f64 || sy >= source.height as f64 {
            return [0; 4];
        }
        return source.get(sx.round() as u32, sy.round() as u32);
    }
    render::sample_with_sampling(source, sx, sy, transform.sampling)
}

fn mask_pixel(
    source: &Raster,
    transform: Transform,
    unit: [f64; 2],
    background: u8,
    placed: bool,
) -> u8 {
    if !placed && (unit[0] < 0. || unit[1] < 0. || unit[0] >= 1. || unit[1] >= 1.) {
        return 0;
    }
    let x = unit[0] * source.width as f64;
    let y = unit[1] * source.height as f64;
    if transform.sampling == Sampling::Nearest {
        if x < 0. || y < 0. || x >= source.width as f64 || y >= source.height as f64 {
            return background;
        }
        let pixel = source.get(x.floor() as u32, y.floor() as u32);
        return ((pixel[0] as u16 * pixel[3] as u16 + 127) / 255) as u8;
    }
    (render::sample_mask_with_sampling(source, x - 0.5, y - 0.5, background, transform.sampling)
        * 255.)
        .round()
        .clamp(0., 255.) as u8
}

/// Scans retained coverage regions rather than the empty area of a large canvas.
fn copy_region(document: &Document) -> Option<[u32; 4]> {
    let Some(selection) = &document.selection else {
        return Some([0, 0, document.width, document.height]);
    };
    let mut bounds = [document.width, document.height, 0, 0];
    for (origin, size, coverage) in selection.regions() {
        let clip = |axis: usize, canvas: u32| {
            let start = (-i128::from(origin[axis])).clamp(0, i128::from(size[axis]));
            let end =
                (i128::from(canvas) - i128::from(origin[axis])).clamp(0, i128::from(size[axis]));
            [start as u32, end as u32]
        };
        let [sx0, sx1] = clip(0, document.width);
        let [sy0, sy1] = clip(1, document.height);
        for sy in sy0..sy1 {
            for sx in sx0..sx1 {
                if coverage[(u64::from(sy) * u64::from(size[0]) + u64::from(sx)) as usize] == 0 {
                    continue;
                }
                let x = (i128::from(origin[0]) + i128::from(sx)) as u32;
                let y = (i128::from(origin[1]) + i128::from(sy)) as u32;
                bounds[0] = bounds[0].min(x);
                bounds[1] = bounds[1].min(y);
                bounds[2] = bounds[2].max(x + 1);
                bounds[3] = bounds[3].max(y + 1);
            }
        }
    }
    (bounds[0] < bounds[2] && bounds[1] < bounds[3]).then_some(bounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BlendMode, Layer, Mask, Selection, SelectionRegion};
    use serde_json::json;
    use std::sync::Arc;

    fn layer_document(width: u32, height: u32, color: [u8; 4]) -> Document {
        let mut document = Document::new(width, height);
        document.layers[0].image = Some(Arc::new(Raster::solid(width, height, color)));
        document
    }

    #[test]
    fn ordinary_copy_preserves_source_channels_and_excludes_layer_appearance() {
        let color = [220, 45, 100, 180];
        let mut document = layer_document(3, 2, color);
        let layer = &mut document.layers[0];
        layer.visible = false;
        layer.opacity = 0.2;
        layer.blend_mode = BlendMode::Multiply;
        layer.effects = Some(
            json!({"colorOverlay":{"isEnabled":true,"red":0.,"green":1.,"blue":0.,"opacity":1.}}),
        );
        layer.mask = Some(Mask {
            raster: Arc::new(Raster::solid(1, 1, [0, 0, 0, 255])),
            ..Mask::white()
        });
        let (copied, origin) = copy_pixels(&document, false, false).unwrap().unwrap();
        assert_eq!(origin, [0., 0.]);
        for y in 0..2 {
            for x in 0..3 {
                assert_eq!(copied.get(x, y), color);
            }
        }
    }

    #[test]
    fn copy_merged_keeps_blend_and_opacity_and_ignores_mask_edit_target() {
        let mut document = layer_document(3, 2, [200, 100, 50, 255]);
        let mut top = Layer::raster("Top", Raster::solid(3, 2, [100, 200, 250, 255]));
        top.blend_mode = BlendMode::Multiply;
        top.opacity = 0.5;
        top.mask = Some(Mask::white());
        document.active_layer_id = Some(top.id);
        document.layers.push(top);
        let mut selection = Selection::empty(3, 2);
        selection.set(1, 1, 255);
        document.selection = Some(selection);
        let (copied, origin) = copy_pixels(&document, true, true).unwrap().unwrap();
        assert_eq!([copied.width, copied.height], [1, 1]);
        assert_eq!(origin, [1., 1.]);
        assert_eq!(copied.get(0, 0), [139, 89, 50, 255]);
        let (raw, _) = copy_pixels(&document, false, false).unwrap().unwrap();
        assert_eq!(raw.get(0, 0), [100, 200, 250, 255]);
    }

    #[test]
    fn placed_mask_copy_preserves_gray_and_white_exterior() {
        let mut document = layer_document(5, 5, [255, 0, 0, 255]);
        let mut mask = Raster::solid(3, 3, [255; 4]);
        mask.set(1, 1, [0, 0, 0, 255]);
        let mut placement = Transform::new(1., 1., 3., 3.);
        placement.sampling = Sampling::Nearest;
        document.layers[0].mask = Some(Mask {
            raster: Arc::new(mask),
            enabled: false,
            linked: false,
            placement: Some(placement),
        });
        let (copied, _) = copy_pixels(&document, false, true).unwrap().unwrap();
        assert_eq!(copied.get(2, 2), [0, 0, 0, 255]);
        assert_eq!(copied.get(1, 1), [255; 4]);
        assert_eq!(copied.get(0, 0), [255; 4]);
        assert_eq!(copied.get(4, 4), [255; 4]);
    }

    #[test]
    fn unplaced_mask_copy_is_black_outside_layer_and_opaque_gray_inside() {
        let mut document = layer_document(5, 5, [255, 0, 0, 255]);
        document.layers[0].transform = Transform::new(1., 1., 3., 3.);
        document.layers[0].mask = Some(Mask::white());
        let (copied, _) = copy_pixels(&document, false, true).unwrap().unwrap();
        assert_eq!(copied.get(0, 0), [0, 0, 0, 255]);
        assert_eq!(copied.get(4, 4), [0, 0, 0, 255]);
        assert_eq!(copied.get(1, 1), [255; 4]);
        assert_eq!(copied.get(2, 2), [255; 4]);
    }

    #[test]
    fn soft_off_canvas_selection_crops_to_canvas_and_modulates_only_alpha() {
        let mut document = layer_document(3, 1, [80, 90, 100, 200]);
        document.selection = Some(Selection::from_regions(
            3,
            1,
            vec![SelectionRegion {
                origin: [-2, 0],
                size: [6, 1],
                coverage: Arc::new(vec![255, 255, 64, 128, 0, 255]),
            }],
        ));
        let (copied, origin) = copy_pixels(&document, false, false).unwrap().unwrap();
        assert_eq!(origin, [0., 0.]);
        assert_eq!([copied.width, copied.height], [2, 1]);
        assert_eq!(copied.get(0, 0), [80, 90, 100, 50]);
        assert_eq!(copied.get(1, 0), [80, 90, 100, 100]);
    }

    #[test]
    fn nearest_copy_respects_rotation_flip_and_document_origin() {
        let mut document = Document::new(6, 6);
        document.layers[0].image = Some(Arc::new(Raster::from_rgba(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
        )));
        let mut transform = Transform::new(2., 2., 2., 2.);
        transform.rotation = 90.;
        transform.flip_x = true;
        transform.sampling = Sampling::Nearest;
        document.layers[0].transform = transform;
        let mut selection = Selection::empty(6, 6);
        for y in 2..4 {
            for x in 2..4 {
                selection.set(x, y, 255);
            }
        }
        document.selection = Some(selection);
        let (copied, origin) = copy_pixels(&document, false, false).unwrap().unwrap();
        assert_eq!(origin, [2., 2.]);
        assert_eq!(copied.get(0, 0), [255; 4]);
        assert_eq!(copied.get(1, 0), [0, 255, 0, 255]);
        assert_eq!(copied.get(0, 1), [0, 0, 255, 255]);
        assert_eq!(copied.get(1, 1), [255, 0, 0, 255]);
    }

    #[test]
    fn high_minified_copy_averages_four_source_pixels_and_transparent_color() {
        for (data, expected) in [
            (
                vec![
                    0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255,
                ],
                [128, 128, 128, 255],
            ),
            (
                vec![255, 0, 0, 0, 255, 0, 0, 0, 0, 0, 255, 255, 0, 0, 255, 255],
                [0, 0, 255, 128],
            ),
        ] {
            let mut document = Document::new(1, 1);
            document.layers[0].image = Some(Arc::new(Raster::from_rgba(4, 1, data)));
            let (copied, _) = copy_pixels(&document, false, false).unwrap().unwrap();
            assert_eq!(copied.get(0, 0), expected);
            assert_eq!(
                copied.to_rgba(),
                render::render_document(&document).to_rgba()
            );
        }
    }

    #[test]
    fn high_minified_copy_prefilters_periodic_detail_like_native_composite() {
        let mut document = Document::new(8, 4);
        document.layers[0].image = Some(Arc::new(Raster::from_rgba(
            32,
            16,
            (0..32 * 16)
                .flat_map(|i| {
                    let v = if matches!(i % 4, 1 | 2) { 255 } else { 0 };
                    [v, v, v, 255]
                })
                .collect(),
        )));
        let (copied, _) = copy_pixels(&document, false, false).unwrap().unwrap();
        assert!(
            copied
                .to_rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [128, 128, 128, 255])
        );
        assert_eq!(
            copied.to_rgba(),
            render::render_document(&document).to_rgba()
        );
    }

    #[test]
    fn merged_selection_keeps_gaussian_neighborhoods_and_noise_document_seed() {
        for settings in [
            json!({"kind":"Gaussian Blur","blurRadius":3}),
            json!({"kind":"Add Noise","noiseAmount":25,"noiseSeed":745}),
        ] {
            let mut document = Document::new(17, 13);
            document.layers[0].image = Some(Arc::new(Raster::from_rgba(
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
            )));
            let mut adjustment = Layer::new("Spatial", 17, 13);
            adjustment.adjustment = Some(settings.clone());
            document.layers.push(adjustment);
            let mut selection = Selection::empty(17, 13);
            for y in 4..9 {
                for x in 5..11 {
                    selection.set(x, y, 255);
                }
            }
            document.selection = Some(selection);
            let full = render::render_document(&document);
            let legacy_region = render::render_region(&document, [5., 4., 6., 5.], 6, 5);
            let (copied, origin) = copy_pixels(&document, true, false).unwrap().unwrap();
            assert_eq!(origin, [5., 4.]);
            assert_eq!([copied.width, copied.height], [6, 5]);
            let mut legacy_differs = false;
            for y in 0..5 {
                for x in 0..6 {
                    let expected = full.get(x + 5, y + 4);
                    assert_eq!(copied.get(x, y), expected, "{settings}, {x},{y}");
                    legacy_differs |= legacy_region.get(x, y) != expected;
                }
            }
            assert!(
                legacy_differs,
                "Fixture must expose selection-local filtering: {settings}"
            );
        }
    }

    #[test]
    fn copy_then_cut_matches_active_source_and_preserves_other_layer() {
        let mut document = layer_document(3, 1, [255, 0, 0, 255]);
        let background = document.layers[0].clone();
        let top = Layer::raster("Active", Raster::solid(3, 1, [0, 0, 255, 255]));
        let active = top.id;
        document.active_layer_id = Some(active);
        document.layers.push(top);
        let mut selection = Selection::empty(3, 1);
        selection.set(1, 0, 255);
        document.selection = Some(selection);
        let (copied, origin) = copy_pixels(&document, false, false).unwrap().unwrap();
        assert_eq!(origin, [1., 0.]);
        assert_eq!(copied.get(0, 0), [0, 0, 255, 255]);
        crate::edit::clear(&mut document, active, false).unwrap();
        assert_eq!(document.layers[0], background);
        let image = document.active().unwrap().image.as_ref().unwrap();
        assert_eq!(image.get(0, 0), [0, 0, 255, 255]);
        assert_eq!(image.get(1, 0)[3], 0);
        assert_eq!(image.get(2, 0), [0, 0, 255, 255]);
    }

    #[test]
    fn missing_source_group_or_entirely_off_canvas_selection_returns_none() {
        let mut document = Document::new(4, 4);
        assert!(copy_pixels(&document, false, false).unwrap().is_none());
        assert!(copy_pixels(&document, false, true).unwrap().is_none());
        assert!(copy_pixels(&document, true, false).unwrap().is_none());
        document.layers[0].is_group = true;
        assert!(copy_pixels(&document, false, false).unwrap().is_none());
        document.layers[0].mask = Some(Mask::white());
        assert!(copy_pixels(&document, false, true).unwrap().is_some());
        document.selection = Some(Selection::from_regions(
            4,
            4,
            vec![SelectionRegion {
                origin: [i64::MIN, i64::MAX],
                size: [1, 1],
                coverage: Arc::new(vec![255]),
            }],
        ));
        assert!(copy_pixels(&document, false, true).unwrap().is_none());
    }

    #[test]
    fn sparse_large_canvas_copy_allocates_only_selected_output() {
        let mut document = Document::new(30_000, 6_000);
        let mut raster = Raster::blank(30_000, 6_000);
        raster.set(29_998, 5_998, [12, 34, 56, 255]);
        document.layers[0].image = Some(Arc::new(raster));
        document.selection = Some(Selection::from_regions(
            30_000,
            6_000,
            vec![SelectionRegion {
                origin: [29_998, 5_998],
                size: [1, 1],
                coverage: Arc::new(vec![255]),
            }],
        ));
        let (copied, origin) = copy_pixels(&document, false, false).unwrap().unwrap();
        assert_eq!(origin, [29_998., 5_998.]);
        assert_eq!([copied.width, copied.height], [1, 1]);
        assert_eq!(copied.byte_len(), 4);
        assert_eq!(copied.get(0, 0), [12, 34, 56, 255]);
    }
}
