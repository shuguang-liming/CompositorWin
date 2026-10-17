//! Floating-point composition for documents with authoritative high-precision layers.
use super::*;
use crate::deep_pixels::{DeepRaster, Depth};

pub(super) fn has_deep(doc: &Document) -> bool {
    doc.layers
        .iter()
        .any(|layer| layer.image.as_ref().is_some_and(|r| r.deep.is_some()))
}

pub(super) fn render(doc: &Document, region: [f64; 4], width: u32, height: u32) -> Raster {
    let depth = if doc.layers.iter().any(|layer| {
        layer
            .image
            .as_ref()
            .and_then(|r| r.deep.as_ref())
            .is_some_and(|r| r.depth == Depth::Hdr)
    }) {
        Depth::Hdr
    } else {
        Depth::U16
    };
    let mut ordered = Vec::new();
    ordered_visible(doc, None, 0, &mut ordered);
    let native_region = [0., 0., doc.width as f64, doc.height as f64];
    if CompositeSampler::layers_need_full_surface(&ordered)
        && (region != native_region || width != doc.width || height != doc.height)
    {
        let full = render(doc, native_region, doc.width, doc.height);
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend(full.sample_f32(
                    region[0] + (x as f64 + 0.5) * region[2] / width as f64 - 0.5,
                    region[1] + (y as f64 + 0.5) * region[3] / height as f64 - 0.5,
                    Sampling::High,
                ));
            }
        }
        return raster(width, height, depth, pixels);
    }
    let mut surface = vec![[0_f32; 4]; width as usize * height as usize];
    compose(
        doc,
        &ordered,
        &mut surface,
        region,
        [width, height],
        depth,
        false,
        None,
    );
    let pixels: Vec<_> = surface
        .into_iter()
        .flat_map(|p| {
            if p[3] > 0. {
                [p[0] / p[3], p[1] / p[3], p[2] / p[3], p[3]]
            } else {
                [0.; 4]
            }
        })
        .collect();
    raster(width, height, depth, pixels)
}

fn raster(width: u32, height: u32, depth: Depth, mut pixels: Vec<f32>) -> Raster {
    let (low, high) = if depth == Depth::Hdr {
        (-65504., 65504.)
    } else {
        (0., 1.)
    };
    // Cubic interpolation and blend math can exceed the storage range even
    // for valid source pixels. Saturate only when writing the final surface.
    for pixel in pixels.as_chunks_mut::<4>().0 {
        for channel in &mut pixel[..3] {
            *channel = if channel.is_nan() {
                0.
            } else {
                channel.clamp(low, high)
            };
        }
        pixel[3] = if pixel[3].is_nan() {
            0.
        } else {
            pixel[3].clamp(0., 1.)
        };
    }
    Raster::from_deep(
        DeepRaster::from_f32(width, height, depth, &pixels).expect("bounded composited pixels"),
    )
}

fn blend(bottom: [f32; 4], top: [f32; 4], mode: BlendMode, hdr: bool) -> [f32; 4] {
    let ab = bottom[3];
    let at = top[3];
    if at <= 0. {
        return bottom;
    }
    let alpha = at + ab * (1. - at);
    if mode == BlendMode::Normal {
        return std::array::from_fn(|c| {
            if c == 3 {
                alpha
            } else {
                top[c] + bottom[c] * (1. - at)
            }
        });
    }
    let b = std::array::from_fn(|c| {
        if ab > 0. {
            bottom[c] as f64 / ab as f64
        } else {
            0.
        }
    });
    let s = std::array::from_fn(|c| top[c] as f64 / at as f64);
    let mixed = match mode {
        BlendMode::Hue => set_luminosity(set_saturation(s, saturation(b)), luminosity(b)),
        BlendMode::Saturation => set_luminosity(set_saturation(b, saturation(s)), luminosity(b)),
        BlendMode::Color => set_luminosity(s, luminosity(b)),
        BlendMode::Luminosity => set_luminosity(b, luminosity(s)),
        _ => std::array::from_fn(|c| blend_channel(b[c], s[c], mode)),
    };
    std::array::from_fn(|c| {
        if c == 3 {
            alpha
        } else {
            let v = (1. - at) * bottom[c] + (1. - ab) * top[c] + at * ab * mixed[c] as f32;
            if hdr {
                v.clamp(-65504. * alpha, 65504. * alpha)
            } else {
                v.clamp(0., alpha)
            }
        }
    })
}

fn straight(surface: &[[f32; 4]], size: [u32; 2], depth: Depth) -> Raster {
    let values: Vec<_> = surface
        .iter()
        .flat_map(|p| {
            if p[3] > 0. {
                [p[0] / p[3], p[1] / p[3], p[2] / p[3], p[3]]
            } else {
                [0.; 4]
            }
        })
        .collect();
    raster(size[0], size[1], depth, values)
}

#[allow(clippy::too_many_arguments)]
fn compose(
    doc: &Document,
    ordered: &[&Layer],
    bottom: &mut [[f32; 4]],
    region: [f64; 4],
    size: [u32; 2],
    depth: Depth,
    member: bool,
    override_blend: Option<BlendMode>,
) {
    let [width, height] = size;
    let mut index = 0;
    while index < ordered.len() {
        let layer = ordered[index];
        index += 1;
        if !member && layer.mask_source_id.is_none() && layer.adjustment.is_none() {
            let first = index;
            while index < ordered.len()
                && ordered[index].mask_source_id == Some(layer.id)
                && ordered[index].parent_id == layer.parent_id
            {
                index += 1;
            }
            if first < index {
                let mut stack = vec![[0_f32; 4]; bottom.len()];
                compose(
                    doc,
                    &[layer],
                    &mut stack,
                    region,
                    size,
                    depth,
                    true,
                    Some(BlendMode::Normal),
                );
                let alpha: Vec<_> = stack.iter().map(|p| p[3]).collect();
                for p in &mut stack {
                    if p[3] > 0. {
                        for c in 0..3 {
                            p[c] /= p[3];
                        }
                    }
                    p[3] = 1.;
                }
                compose(
                    doc,
                    &ordered[first..index],
                    &mut stack,
                    region,
                    size,
                    depth,
                    true,
                    None,
                );
                for ((b, mut p), a) in bottom.iter_mut().zip(stack).zip(alpha) {
                    for channel in &mut p {
                        *channel *= a;
                    }
                    *b = blend(*b, p, layer.blend_mode, depth == Depth::Hdr);
                }
                continue;
            }
        }
        let mut own = layer.clone();
        if member {
            own.mask_source_id = None;
        }
        if let Some(settings) = &layer.adjustment {
            let adjusted = filters::apply_adjustment(&straight(bottom, size, depth), settings);
            for (at, p) in bottom.iter_mut().enumerate() {
                let (x, y) = (at as u32 % width, at as u32 / width);
                let point = [
                    region[0] + (x as f64 + 0.5) * region[2] / width as f64,
                    region[1] + (y as f64 + 0.5) * region[3] / height as f64,
                ];
                let uv = layer.transform.from_document(point);
                if uv.iter().any(|v| !(0. ..=1.).contains(v)) {
                    continue;
                }
                let weight = (effective_opacity(doc, layer)
                    * layer_mask(doc, &own, point, 0)
                    * folder_coverage(doc, layer, point))
                .clamp(0., 1.) as f32;
                let a = p[3];
                let source = adjusted.get_f32(x, y);
                let color = if layer.blend_mode == BlendMode::Normal {
                    source
                } else {
                    let opaque = if a > 0. {
                        [p[0] / a, p[1] / a, p[2] / a, 1.]
                    } else {
                        [0., 0., 0., 1.]
                    };
                    blend(
                        opaque,
                        [source[0], source[1], source[2], 1.],
                        layer.blend_mode,
                        depth == Depth::Hdr,
                    )
                };
                for c in 0..3 {
                    p[c] = p[c] * (1. - weight) + color[c] * a * weight;
                }
            }
            continue;
        }
        let effect = prepare_layer_effect(doc, layer);
        let (image, transform) = if let Some((r, inset)) = &effect {
            (r, layer_effect_transform(layer, r, *inset))
        } else if let Some(image) = &layer.image {
            (image.as_ref(), layer.transform)
        } else {
            continue;
        };
        if effect.is_some() {
            own.mask = None;
        }
        bottom
            .par_chunks_mut(width as usize)
            .enumerate()
            .for_each(|(y, row)| {
                for (x, pixel) in row.iter_mut().enumerate() {
                    let point = [
                        region[0] + (x as f64 + 0.5) * region[2] / width as f64,
                        region[1] + (y as f64 + 0.5) * region[3] / height as f64,
                    ];
                    let uv = transform.from_document(point);
                    let mut p = image.sample_f32(
                        uv[0] * image.width as f64 - 0.5,
                        uv[1] * image.height as f64 - 0.5,
                        transform.sampling,
                    );
                    let opacity = effective_opacity(doc, layer)
                        * layer_mask(doc, &own, point, 0)
                        * folder_coverage(doc, layer, point);
                    p[3] *= opacity as f32;
                    for c in 0..3 {
                        p[c] *= p[3];
                    }
                    *pixel = blend(
                        *pixel,
                        p,
                        override_blend.unwrap_or(layer.blend_mode),
                        depth == Depth::Hdr,
                    );
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr_plateau() -> Document {
        let mut doc = Document::new(4, 1);
        let values: Vec<_> = [0., 65504., 65504., 0.]
            .into_iter()
            .flat_map(|v| [v, -v, 0., 1.])
            .collect();
        doc.layers[0].image = Some(Arc::new(Raster::from_deep(
            DeepRaster::from_f32(4, 1, Depth::Hdr, &values).unwrap(),
        )));
        doc
    }

    fn assert_finite_half(image: &Raster) {
        for y in 0..image.height {
            for x in 0..image.width {
                let p = image.get_f32(x, y);
                assert!(p[..3].iter().all(|v| v.is_finite() && v.abs() <= 65504.));
                assert!((0. ..=1.).contains(&p[3]));
            }
        }
        image.deep.as_ref().unwrap().validate().unwrap();
    }

    #[test]
    fn hdr_cubic_resampling_saturates_at_storage_boundary() {
        let mut doc = hdr_plateau();
        let source = doc.layers[0].image.as_ref().unwrap();
        assert!(source.sample_f32(1.5, 0., Sampling::High)[0] > 65504.);
        // A spatial adjustment takes the full-surface resampling route; both
        // routes must accept interpolation overshoot from valid half pixels.
        for full_surface in [false, true] {
            if full_surface {
                let mut adjustment = Layer::new("Blur", 4, 1);
                adjustment.adjustment =
                    Some(serde_json::json!({"kind":"Gaussian Blur","blurRadius":0.}));
                doc.layers.push(adjustment);
            }
            let out = render(&doc, [0., 0., 4., 1.], 8, 1);
            assert_finite_half(&out);
            assert_eq!(out.get_f32(3, 0), [65504., -65504., 0., 1.]);
        }
        // File/input validation remains strict; only internal output saturates.
        assert!(DeepRaster::from_f32(1, 1, Depth::Hdr, &[65505., 0., 0., 1.]).is_err());
        assert!(DeepRaster::from_f32(1, 1, Depth::Hdr, &[f32::NAN, 0., 0., 1.]).is_err());
    }

    #[test]
    fn hdr_subpixel_transform_and_adjustment_surface_keep_finite_pixels() {
        let mut doc = hdr_plateau();
        doc.layers[0].transform.origin[0] = 0.5;
        let mut adjustment = Layer::new("Blur", 4, 1);
        adjustment.adjustment = Some(serde_json::json!({"kind":"Gaussian Blur","blurRadius":0.}));
        doc.layers.push(adjustment);
        let out = render(&doc, [0., 0., 4., 1.], 4, 1);
        assert_finite_half(&out);
        assert_eq!(out.get_f32(2, 0), [65504., -65504., 0., 1.]);
    }

    #[test]
    fn extreme_hdr_normal_blending_preserves_half_limits_and_partial_alpha() {
        let mut doc = Document::new(1, 1);
        let image = Arc::new(Raster::from_deep(DeepRaster::solid(
            1,
            1,
            Depth::Hdr,
            [65504., -65504., 32752., 0.5],
        )));
        doc.layers[0].image = Some(Arc::clone(&image));
        let mut top = Layer::new("Bright", 1, 1);
        top.image = Some(image);
        doc.layers.push(top);
        let out = render(&doc, [0., 0., 1., 1.], 1, 1);
        assert_finite_half(&out);
        assert_eq!(out.get_f32(0, 0), [65504., -65504., 32752., 0.75]);
    }

    #[test]
    fn hidden_deep_layer_keeps_visible_byte_layer_cubic_sampling() {
        let mut doc = Document::new(2, 2);
        doc.layers[0].image = Some(Arc::new(Raster::solid(1, 1, [255, 0, 0, 255])));
        let expected = render_document(&doc).to_rgba();
        assert!((190..=193).contains(&expected[3]));
        let mut hidden = Layer::new("Hidden 16-bit", 2, 2);
        hidden.visible = false;
        hidden.image = Some(Arc::new(Raster::from_deep(DeepRaster::solid(
            2,
            2,
            Depth::U16,
            [0.; 4],
        ))));
        doc.layers.push(hidden);
        let actual = render_document(&doc).to_rgba();
        assert!(expected.iter().zip(actual).all(|(a, b)| a.abs_diff(b) <= 1));

        doc.width = 9;
        doc.height = 9;
        doc.layers[0].image = Some(Arc::new(Raster::from_rgba(
            3,
            3,
            vec![
                255, 30, 0, 255, 0, 220, 40, 128, 0, 0, 255, 0, 10, 100, 200, 64, 255, 255, 255,
                255, 100, 0, 255, 190, 255, 0, 0, 0, 0, 255, 20, 255, 0, 60, 255, 220,
            ],
        )));
        doc.layers[0].transform.origin = [0.2, 0.3];
        doc.layers[0].transform.size = [7., 7.];
        doc.layers[0].transform.rotation = 12.;
        let actual = render_document(&doc).to_rgba();
        doc.layers.pop();
        let expected = render_document(&doc).to_rgba();
        for (a, b) in expected
            .as_chunks::<4>()
            .0
            .iter()
            .zip(actual.as_chunks::<4>().0)
        {
            assert!(a[3].abs_diff(b[3]) <= 1, "alpha: {a:?} != {b:?}");
            // The byte renderer rounds premultiplied colors before converting
            // to straight RGB. Compare visible coverage, including thin edges.
            for c in 0..3 {
                let first = (a[c] as u32 * a[3] as u32 + 127) / 255;
                let second = (b[c] as u32 * b[3] as u32 + 127) / 255;
                assert!(first.abs_diff(second) <= 1, "color: {a:?} != {b:?}");
            }
        }
    }

    #[test]
    fn fine_ramp_survives_composition_and_adjustment_layers() {
        let bits: Vec<u16> = (0..256)
            .flat_map(|x| [25000 + x, 25000 + x, 25000 + x, 65535])
            .collect();
        let mut doc = Document::new(256, 1);
        doc.layers[0].image = Some(Arc::new(Raster::from_deep(
            DeepRaster::from_u16(256, 1, bits.clone()).unwrap(),
        )));
        let out = render(&doc, [0., 0., 256., 1.], 256, 1);
        assert_eq!(out.deep.unwrap().to_bits(), bits);
        let mut adjustment = Layer::new("Exposure", 256, 1);
        adjustment.adjustment =
            Some(serde_json::json!({"kind":"Exposure","exposure":-1.,"offset":0.,"gamma":1.}));
        doc.layers.push(adjustment);
        let out = render(&doc, [0., 0., 256., 1.], 256, 1);
        let distinct: std::collections::BTreeSet<_> = (0..256)
            .map(|x| out.deep.as_ref().unwrap().get_bits(x, 0)[0])
            .collect();
        assert!(distinct.len() > 100);
    }
    #[test]
    fn hdr_highlights_reappear_after_exposure_and_clip_stack_keeps_alpha() {
        let mut doc = Document::new(1, 1);
        doc.layers[0].image = Some(Arc::new(Raster::from_deep(DeepRaster::solid(
            1,
            1,
            Depth::Hdr,
            [4., 2., 1., 0.5],
        ))));
        let mut adjustment = Layer::new("Exposure", 1, 1);
        adjustment.adjustment =
            Some(serde_json::json!({"kind":"Exposure","exposure":-6.,"offset":0.,"gamma":1.}));
        doc.layers.push(adjustment);
        let out = render(&doc, [0., 0., 1., 1.], 1, 1).get_f32(0, 0);
        assert!(
            (out[0] - filters::linear_to_srgb(filters::srgb_to_linear(4.) / 64.) as f32).abs()
                < 0.002
                && out[0] < 1.
                && out[0] > out[1]
                && out[1] > out[2],
            "{out:?}"
        );
        assert!((out[3] - 0.5).abs() < 0.002);
    }
}
