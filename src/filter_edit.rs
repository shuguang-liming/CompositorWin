//! Destructive filters on a layer's native grid, including the transparent room a blur needs.
use crate::{
    filters,
    model::{Document, Raster, Selection, Transform},
    pixels,
};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

fn progressive_point(value: &Value) -> Option<[f64; 2]> {
    let coordinate = |i| {
        value
            .get(i)
            .or_else(|| value.get(["x", "y"][i]))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
    };
    Some([coordinate(0)?, coordinate(1)?])
}

/// Endpoints are document coordinates so previews, rotated layers and canvas gestures agree.
pub fn progressive_line(
    doc: &Document,
    id: Uuid,
    mask: bool,
    settings: &Value,
) -> Option<[[f64; 2]; 2]> {
    let layer = doc.layer(id)?;
    let placed = if mask {
        layer.mask.as_ref()?.placement.unwrap_or(layer.transform)
    } else {
        layer.transform
    };
    if !placed.is_valid() {
        return None;
    }
    let corners = [[0., 0.], [1., 0.], [1., 1.], [0., 1.]].map(|uv| placed.to_document(uv));
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for point in corners {
        bounds[0] = bounds[0].min(point[0]);
        bounds[1] = bounds[1].min(point[1]);
        bounds[2] = bounds[2].max(point[0]);
        bounds[3] = bounds[3].max(point[1]);
    }
    let visible = [
        bounds[0].max(0.),
        bounds[1].max(0.),
        bounds[2].min(doc.width as f64),
        bounds[3].min(doc.height as f64),
    ];
    if visible[2] > visible[0] && visible[3] > visible[1] {
        bounds = visible;
    }
    let x = (bounds[0] + bounds[2]) / 2.;
    Some([
        progressive_point(&settings["progressiveStart"])
            .unwrap_or([x, bounds[1] + (bounds[3] - bounds[1]) * 0.35]),
        progressive_point(&settings["progressiveEnd"]).unwrap_or([x, bounds[3]]),
    ])
}

/// Set a new dialog's line on the visible portion of its layer without moving an existing line.
pub fn initialize_progressive_line(
    doc: &Document,
    id: Uuid,
    mask: bool,
    settings: &mut Value,
) -> bool {
    let Some(points) = progressive_line(doc, id, mask, settings) else {
        return false;
    };
    let mut changed = false;
    for (key, point) in ["progressiveStart", "progressiveEnd"]
        .into_iter()
        .zip(points)
    {
        if progressive_point(&settings[key]).is_none() {
            if !settings.is_object() {
                *settings = serde_json::json!({});
            }
            settings[key] = serde_json::json!({"x":point[0],"y":point[1]});
            changed = true;
        }
    }
    changed
}

/// Applies one filter without recording history. Failed and unselected operations leave the document intact.
pub fn apply(doc: &mut Document, id: Uuid, mask: bool, kind: &str, settings: &Value) -> Result<()> {
    let original = doc
        .layer(id)
        .ok_or_else(|| anyhow::anyhow!("请先选择图层"))?
        .clone();
    if doc
        .selection
        .as_ref()
        .is_some_and(|selection| selection.bounds().is_none())
    {
        return Ok(());
    }
    ensure!(filters::FILTERS.contains(&kind), "未知滤镜：{kind}");
    let mut settings = if settings.is_object() {
        settings.clone()
    } else {
        Value::Object(Default::default())
    };
    let margin = blur_margin(kind, &mut settings);
    if (kind == "Vignette"
        && filters::number(
            &settings,
            "vignetteAmount",
            filters::number(&settings, "amount", 35.),
        ) <= 0.)
        || (kind == "Bloom / Glow"
            && filters::number(
                &settings,
                "bloomAmount",
                filters::number(&settings, "bloomIntensity", 40.),
            ) <= 0.)
        || (kind == "Gaussian Blur" && filters::number(&settings, "blurRadius", 10.) <= 0.01)
        || (kind == "Motion Blur" && filters::number(&settings, "motionDistance", 10.) <= 0.)
        || (kind == "Progressive Blur"
            && filters::number(&settings, "progressiveFrom", 0.) <= 0.01
            && filters::number(&settings, "progressiveTo", 20.) <= 0.01)
    {
        return Ok(());
    }
    let fills_clear = !mask && kind == "Vignette" && original.image.is_none();
    let source = if mask {
        original.mask.as_ref().map(|owned| owned.raster.clone())
    } else {
        ensure!(
            !original.is_group && original.adjustment.is_none(),
            "请选择像素图层"
        );
        original.image.clone()
    };
    let placement = if mask {
        original
            .mask
            .as_ref()
            .and_then(|owned| owned.placement)
            .unwrap_or(original.transform)
    } else {
        original.transform
    };
    ensure!(placement.is_valid(), "图层变换参数无效");
    let source = if let Some(source) = source {
        source
    } else if fills_clear {
        let width = placement.size[0].round().max(1.) as u32;
        let height = placement.size[1].round().max(1.) as u32;
        ensure!(
            Document::valid_size(width, height),
            "暗角滤镜超出图像内存上限"
        );
        Arc::new(Raster::blank(width, height))
    } else {
        anyhow::bail!("请选择像素图层");
    };
    ensure!(
        Document::valid_size(source.width, source.height),
        "源图像尺寸无效"
    );
    let (mut prepared, placed) = if !mask && margin > 0 {
        grow(
            &source,
            placement,
            [
                -(margin as i64),
                -(margin as i64),
                source.width as i64 + margin as i64,
                source.height as i64 + margin as i64,
            ],
        )?
    } else if fills_clear {
        grow(
            &source,
            placement,
            covering_canvas(doc, placement, source.width, source.height),
        )?
    } else {
        (source.as_ref().clone(), placement)
    };
    // Empty layers have no raster for PreviewCache to reduce. Grow the compact clear grid first,
    // then reduce all of it so even a tiny empty layer over a large canvas stays a small preview.
    if fills_clear {
        let scale = filters::number(&settings, "previewScale", 1.).clamp(0., 1.);
        if scale < 1. {
            prepared = Raster::blank(
                (prepared.width as f64 * scale).round().max(1.) as u32,
                (prepared.height as f64 * scale).round().max(1.) as u32,
            );
        }
    }
    if kind == "Progressive Blur" {
        let points = progressive_line(doc, id, mask, &settings)
            .ok_or_else(|| anyhow::anyhow!("渐进模糊图层坐标无效"))?;
        for (key, point) in ["_progressiveStartPixels", "_progressiveEndPixels"]
            .into_iter()
            .zip(points)
        {
            let uv = placed.from_document(point);
            settings[key] = serde_json::json!([
                uv[0] * prepared.width as f64,
                uv[1] * prepared.height as f64
            ]);
        }
    }
    let mut output = if fills_clear {
        empty_vignette(&prepared, placed, doc, &settings)
    } else if kind == "Bloom / Glow" {
        bloom(&prepared, &settings)
    } else {
        filters::apply_filter(&prepared, kind, &settings).map_err(anyhow::Error::msg)?
    };
    if let Some(selection) = &doc.selection
        && !blend_selected(
            &mut output,
            &prepared,
            placed,
            selection,
            doc.width,
            doc.height,
        )
    {
        return Ok(());
    }
    let (output, placed) = if !mask && margin > 0 {
        trim(output, placed)?
    } else {
        (output, placed)
    };
    let layer = doc.layer_mut(id).unwrap();
    if mask {
        layer.mask.as_mut().unwrap().raster = Arc::new(output);
    } else {
        if placed != original.transform
            && let Some(owned) = &mut layer.mask
            && owned.placement.is_none()
        {
            owned.placement = Some(original.transform);
        }
        layer.image = Some(Arc::new(output));
        layer.transform = placed;
        layer.text = None;
        layer.shape = None;
    }
    Ok(())
}

fn blur_margin(kind: &str, settings: &mut Value) -> u32 {
    let margin = match kind {
        "Gaussian Blur" => {
            let radius = filters::number(
                settings,
                "blurRadius",
                filters::number(settings, "radius", 10.),
            )
            .clamp(0., 250.);
            settings["blurRadius"] = radius.into();
            radius * 3. + 2.
        }
        "Motion Blur" => {
            let distance = filters::number(
                settings,
                "motionDistance",
                filters::number(settings, "distance", 10.),
            )
            .clamp(0., 2000.);
            settings["motionDistance"] = distance.into();
            distance / 2. + 2.
        }
        "Progressive Blur" => {
            let from = filters::number(settings, "progressiveFrom", 0.).clamp(0., 250.);
            let to = filters::number(settings, "progressiveTo", 20.).clamp(0., 250.);
            settings["progressiveFrom"] = from.into();
            settings["progressiveTo"] = to.into();
            from.max(to) * 3. + 2.
        }
        "Bloom / Glow" => {
            let radius = filters::number(settings, "bloomRadius", 24.).clamp(0., 150.);
            settings["bloomRadius"] = radius.into();
            radius * 3. + 2.
        }
        _ => 0.,
    };
    margin.ceil() as u32
}

fn covering_canvas(doc: &Document, placed: Transform, width: u32, height: u32) -> [i64; 4] {
    let corners = [
        [0., 0.],
        [doc.width as f64, 0.],
        [0., doc.height as f64],
        [doc.width as f64, doc.height as f64],
    ]
    .map(|point| placed.from_document(point))
    .map(|uv| [uv[0] * width as f64, uv[1] * height as f64]);
    [
        corners
            .iter()
            .map(|point| point[0])
            .fold(0., f64::min)
            .floor() as i64,
        corners
            .iter()
            .map(|point| point[1])
            .fold(0., f64::min)
            .floor() as i64,
        corners
            .iter()
            .map(|point| point[0])
            .fold(width as f64, f64::max)
            .ceil() as i64,
        corners
            .iter()
            .map(|point| point[1])
            .fold(height as f64, f64::max)
            .ceil() as i64,
    ]
}

fn grid_transform(
    placed: Transform,
    width: u32,
    height: u32,
    bounds: [i64; 4],
) -> Result<Transform> {
    let [left, top, right, bottom] = bounds;
    let center = placed.to_document([
        (left as f64 + right as f64) / 2. / width as f64,
        (top as f64 + bottom as f64) / 2. / height as f64,
    ]);
    let mut result = placed;
    result.size = [
        (right - left) as f64 * placed.size[0] / width as f64,
        (bottom - top) as f64 * placed.size[1] / height as f64,
    ];
    result.origin = [
        center[0] - result.size[0] / 2.,
        center[1] - result.size[1] / 2.,
    ];
    ensure!(result.is_valid(), "滤镜超出图层变换上限");
    Ok(result)
}

fn grow(source: &Raster, placed: Transform, bounds: [i64; 4]) -> Result<(Raster, Transform)> {
    let [left, top, right, bottom] = bounds;
    let width = right.saturating_sub(left);
    let height = bottom.saturating_sub(top);
    ensure!(
        width > 0
            && height > 0
            && width <= u32::MAX as i64
            && height <= u32::MAX as i64
            && Document::valid_size(width as u32, height as u32),
        "滤镜超出图像内存上限"
    );
    let result = grid_transform(placed, source.width, source.height, bounds)?;
    if bounds == [0, 0, source.width as i64, source.height as i64] {
        return Ok((source.clone(), placed));
    }
    if let Some(deep) = &source.deep {
        return Ok((
            Raster::from_deep(deep.padded(width as u32, height as u32, left, top)),
            result,
        ));
    }
    if source.pixels.is_empty() && source.tiles.is_empty() {
        return Ok((Raster::blank(width as u32, height as u32), result));
    }
    let mut padded = vec![0; width as usize * height as usize * 4];
    let original = source.to_rgba();
    for y in 0..source.height as usize {
        let row = ((y as i64 - top) as usize * width as usize + (-left) as usize) * 4;
        let source_row = y * source.width as usize * 4;
        padded[row..row + source.width as usize * 4]
            .copy_from_slice(&original[source_row..source_row + source.width as usize * 4]);
    }
    Ok((
        Raster::from_rgba(width as u32, height as u32, padded),
        result,
    ))
}

fn trim(image: Raster, placed: Transform) -> Result<(Raster, Transform)> {
    let width = image.width;
    let height = image.height;
    let deep = image.deep.clone();
    let data = image.to_rgba();
    let mut bounds = [width, height, 0, 0];
    for (i, pixel) in data.as_chunks::<4>().0.iter().enumerate() {
        if pixel[3] > 0
            || deep
                .as_ref()
                .is_some_and(|d| d.get(i as u32 % width, i as u32 / width)[3] > 0.)
        {
            let x = i as u32 % width;
            let y = i as u32 / width;
            bounds[0] = bounds[0].min(x);
            bounds[1] = bounds[1].min(y);
            bounds[2] = bounds[2].max(x + 1);
            bounds[3] = bounds[3].max(y + 1);
        }
    }
    if bounds[2] == 0 || bounds == [0, 0, width, height] {
        return Ok((image, placed));
    }
    let output_width = bounds[2] - bounds[0];
    let output_height = bounds[3] - bounds[1];
    if let Some(deep) = deep {
        return Ok((
            Raster::from_deep(deep.crop(bounds[0], bounds[1], output_width, output_height)),
            grid_transform(placed, width, height, bounds.map(i64::from))?,
        ));
    }
    let mut cropped = Vec::with_capacity(output_width as usize * output_height as usize * 4);
    for y in bounds[1]..bounds[3] {
        let row = (y as usize * width as usize + bounds[0] as usize) * 4;
        cropped.extend_from_slice(&data[row..row + output_width as usize * 4]);
    }
    Ok((
        Raster::from_rgba(output_width, output_height, cropped),
        grid_transform(placed, width, height, bounds.map(i64::from))?,
    ))
}

fn blend_selected(
    output: &mut Raster,
    original: &Raster,
    placed: Transform,
    selection: &Selection,
    canvas_width: u32,
    canvas_height: u32,
) -> bool {
    if let Some(deep) = output.deep.as_ref().or(original.deep.as_ref()) {
        let depth = deep.depth;
        let mut result = crate::deep_pixels::DeepRaster::blank(output.width, output.height, depth);
        let mut changed = false;
        for y in 0..output.height {
            for x in 0..output.width {
                let point = placed.to_document([
                    (x as f64 + 0.5) / output.width as f64,
                    (y as f64 + 0.5) / output.height as f64,
                ]);
                let amount = if point[0] >= 0.
                    && point[1] >= 0.
                    && point[0] < canvas_width as f64
                    && point[1] < canvas_height as f64
                {
                    selection.get(point[0].floor() as u32, point[1].floor() as u32) as f32 / 255.
                } else {
                    0.
                };
                let a = original.get_f32(x, y);
                let b = output.get_f32(x, y);
                if amount == 0. {
                    if let Some(d) = &original.deep {
                        result.set_bits(x, y, d.get_bits(x, y));
                    } else {
                        result.set(x, y, a);
                    }
                    continue;
                }
                let alpha = a[3] * (1. - amount) + b[3] * amount;
                let out = std::array::from_fn(|c| {
                    if c == 3 {
                        alpha
                    } else if alpha > 0. {
                        (a[c] * a[3] * (1. - amount) + b[c] * b[3] * amount) / alpha
                    } else {
                        0.
                    }
                });
                result.set(x, y, out);
                changed |= out != a;
            }
        }
        *output = Raster::from_deep(result);
        return changed;
    }
    let mut changed = false;
    let width = output.width;
    let height = output.height;
    for (i, pixel) in Arc::make_mut(&mut output.pixels)
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        let x = i as u32 % width;
        let y = i as u32 / width;
        let point = placed.to_document([
            (x as f64 + 0.5) / width as f64,
            (y as f64 + 0.5) / height as f64,
        ]);
        let amount = if point[0] >= 0.
            && point[1] >= 0.
            && point[0] < canvas_width as f64
            && point[1] < canvas_height as f64
        {
            selection.get(point[0].floor() as u32, point[1].floor() as u32) as u32
        } else {
            0
        };
        let base = original.get(x, y);
        if amount == 0 {
            pixel.copy_from_slice(&base);
            continue;
        }
        if amount < 255 {
            let alpha = base[3] as u32 * (255 - amount) + pixel[3] as u32 * amount;
            for channel in 0..3 {
                pixel[channel] = (base[channel] as u32 * base[3] as u32 * (255 - amount)
                    + pixel[channel] as u32 * pixel[3] as u32 * amount
                    + alpha / 2)
                    .checked_div(alpha)
                    .unwrap_or(0)
                    .min(255) as u8;
            }
            pixel[3] = ((alpha + 127) / 255) as u8;
        }
        changed |= *pixel != base;
    }
    changed
}

/// Screen a blurred premultiplied light field over the source, including its visible coverage.
fn bloom(source: &Raster, settings: &Value) -> Raster {
    if let Some(deep) = &source.deep {
        let result = deep
            .run_8bit(false, |mut p| {
                pixels::unpremultiply(&mut p);
                let out = bloom(&Raster::from_rgba(source.width, source.height, p), settings);
                let mut p = out.to_rgba();
                pixels::premultiply(&mut p);
                Ok(p)
            })
            .expect("bloom retains image dimensions");
        return Raster::from_deep(result);
    }
    let mut pixels = source.to_rgba();
    pixels::premultiply(&mut pixels);
    let mut light = pixels.clone();
    let threshold = (filters::number(settings, "bloomThreshold", 0.) / 100.).clamp(0., 1.);
    for pixel in light.as_chunks_mut::<4>().0 {
        let alpha = pixel[3] as f64;
        if alpha == 0. {
            continue;
        }
        let luminance =
            (0.2126 * pixel[0] as f64 + 0.7152 * pixel[1] as f64 + 0.0722 * pixel[2] as f64)
                / alpha;
        let weight = ((luminance - threshold) / (1. - threshold).max(0.01)).clamp(0., 1.);
        for channel in pixel {
            *channel = (*channel as f64 * weight).round() as u8;
        }
    }
    let light = pixels::gaussian_blur(
        &light,
        source.width as usize,
        source.height as usize,
        filters::number(settings, "bloomRadius", 24.),
    );
    let intensity = filters::number(
        settings,
        "bloomAmount",
        filters::number(settings, "bloomIntensity", 40.),
    )
    .clamp(0., 100.)
        / 50.;
    for (pixel, light) in pixels
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(light.as_chunks::<4>().0)
    {
        let glow_alpha = (light[3] as f64 * intensity).clamp(0., 255.);
        let alpha = pixel[3] as f64 + glow_alpha * (1. - pixel[3] as f64 / 255.);
        for channel in 0..3 {
            let glow = (light[channel] as f64 * intensity).clamp(0., glow_alpha);
            pixel[channel] = (pixel[channel] as f64 + glow * (1. - pixel[channel] as f64 / 255.))
                .round()
                .clamp(0., alpha) as u8;
        }
        pixel[3] = alpha.round() as u8;
    }
    pixels::unpremultiply(&mut pixels);
    Raster::from_rgba(source.width, source.height, pixels)
}

/// An empty layer's vignette is paint: transparent at the center and colored at the canvas edges.
fn empty_vignette(source: &Raster, placed: Transform, doc: &Document, settings: &Value) -> Raster {
    let mut pixels = source.to_rgba();
    let corners = [
        [0., 0.],
        [doc.width as f64, 0.],
        [0., doc.height as f64],
        [doc.width as f64, doc.height as f64],
    ]
    .map(|point| placed.from_document(point))
    .map(|uv| [uv[0] * source.width as f64, uv[1] * source.height as f64]);
    let left = corners
        .iter()
        .map(|point| point[0])
        .fold(f64::INFINITY, f64::min);
    let top = corners
        .iter()
        .map(|point| point[1])
        .fold(f64::INFINITY, f64::min);
    let width = corners
        .iter()
        .map(|point| point[0])
        .fold(f64::NEG_INFINITY, f64::max)
        - left;
    let height = corners
        .iter()
        .map(|point| point[1])
        .fold(f64::NEG_INFINITY, f64::max)
        - top;
    let strength = (filters::number(
        settings,
        "vignetteAmount",
        filters::number(settings, "amount", 35.),
    ) / 100.)
        .clamp(0., 1.);
    let midpoint = filters::number(settings, "vignetteMidpoint", 50.).clamp(0., 100.) / 100. * 0.85;
    let roundness = filters::number(settings, "vignetteRoundness", 100.).clamp(-100., 100.);
    let feather =
        (filters::number(settings, "vignetteFeather", 60.).clamp(0., 100.) / 100.).max(0.05);
    let color = filters::color(&settings["vignetteColor"], [0.; 3])
        .map(|value| (value * 255.).round() as u8);
    for (i, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let nx = (((i % source.width as usize) as f64 + 0.5 - left) / width) * 2. - 1.;
        let ny = (((i / source.width as usize) as f64 + 0.5 - top) / height) * 2. - 1.;
        let circle = nx.hypot(ny) / 2f64.sqrt();
        let distance = circle + (nx.abs().max(ny.abs()) - circle) * (1. - roundness / 100.) * 0.5;
        let t = ((distance - midpoint) / feather).clamp(0., 1.);
        let alpha = (strength * t * t * (3. - 2. * t) * 255.).round() as u8;
        if alpha > 0 {
            pixel.copy_from_slice(&[color[0], color[1], color[2], alpha]);
        }
    }
    Raster::from_rgba(source.width, source.height, pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        history::History,
        model::{Layer, Mask, Sampling},
        render,
    };
    use serde_json::json;
    #[test]
    fn deep_filter_selection_undo_and_grown_blur_preserve_authoritative_pixels() {
        let source = Raster::from_deep(
            crate::deep_pixels::DeepRaster::from_u16(
                3,
                1,
                vec![
                    30001, 20001, 10001, 65535, 30002, 20002, 10002, 65535, 30003, 20003, 10003,
                    65535,
                ],
            )
            .unwrap(),
        );
        let (mut doc, id) = document(source, [3, 1]);
        doc.selection = Some(crate::selection::marquee(
            3,
            1,
            [1., 0., 1., 1.],
            false,
            false,
        ));
        let before = doc.clone();
        apply(&mut doc, id, false, "Invert", &Value::Null).unwrap();
        let out = doc
            .layer(id)
            .unwrap()
            .image
            .as_ref()
            .unwrap()
            .deep
            .as_ref()
            .unwrap();
        assert_eq!(out.get_bits(0, 0), [30001, 20001, 10001, 65535]);
        assert_eq!(out.get_bits(1, 0), [35533, 45533, 55533, 65535]);
        assert_eq!(out.get_bits(2, 0), [30003, 20003, 10003, 65535]);
        let mut history = History::default();
        history.record("Invert", before.clone(), &doc);
        assert!(history.undo(&mut doc));
        assert_eq!(doc, before);
        doc.selection = None;
        apply(
            &mut doc,
            id,
            false,
            "Gaussian Blur",
            &json!({"blurRadius":1.}),
        )
        .unwrap();
        let out = doc.layer(id).unwrap().image.as_ref().unwrap();
        assert!(out.deep.is_some());
        assert!(out.width > 3 && out.height > 1);
        assert!(
            out.deep
                .as_ref()
                .unwrap()
                .to_bits()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[3] > 0 && p[3] < 65535)
        );
    }

    fn document(image: Raster, canvas: [u32; 2]) -> (Document, Uuid) {
        let layer = Layer::raster("Filter sample", image);
        let id = layer.id;
        let mut doc = Document::new(canvas[0], canvas[1]);
        doc.layers = vec![layer];
        doc.active_layer_id = Some(id);
        (doc, id)
    }

    fn at(doc: &Document, id: Uuid, point: [f64; 2]) -> [u8; 4] {
        let layer = doc.layer(id).unwrap();
        let image = layer.image.as_ref().unwrap();
        let uv = layer.transform.from_document(point);
        if uv[0] < 0. || uv[1] < 0. || uv[0] >= 1. || uv[1] >= 1. {
            return [0; 4];
        }
        image.get(
            (uv[0] * image.width as f64).floor() as u32,
            (uv[1] * image.height as f64).floor() as u32,
        )
    }

    #[test]
    fn progressive_default_line_and_document_mapping_survive_rotation_flip_and_growth() {
        let (mut doc, id) = document(Raster::solid(40, 20, [255; 4]), [120, 100]);
        let mut settings = filters::default_settings("Progressive Blur");
        assert!(initialize_progressive_line(&doc, id, false, &mut settings));
        assert_eq!(
            progressive_line(&doc, id, false, &settings),
            Some([[20., 7.], [20., 20.]])
        );
        assert!(!initialize_progressive_line(&doc, id, false, &mut settings));
        let mut placed = Transform::new(30., 25., 60., 30.);
        placed.rotation = 37.;
        placed.flip_x = true;
        doc.layer_mut(id).unwrap().transform = placed;
        let points = [[0.2, 0.25], [0.8, 0.9]].map(|uv| placed.to_document(uv));
        settings["progressiveStart"] = serde_json::json!(points[0]);
        settings["progressiveEnd"] = serde_json::json!({"x":points[1][0],"y":points[1][1]});
        let margin = blur_margin("Progressive Blur", &mut settings);
        assert_eq!(margin, 62);
        let (prepared, grown) = grow(
            doc.layer(id).unwrap().image.as_ref().unwrap(),
            placed,
            [-62, -62, 102, 82],
        )
        .unwrap();
        let line = progressive_line(&doc, id, false, &settings).unwrap();
        for (i, point) in line.into_iter().enumerate() {
            let uv = grown.from_document(point);
            let expected = [[8. + 62., 5. + 62.], [32. + 62., 18. + 62.]][i];
            assert!((uv[0] * prepared.width as f64 - expected[0]).abs() < 1e-8);
            assert!((uv[1] * prepared.height as f64 - expected[1]).abs() < 1e-8);
        }
    }

    #[test]
    fn progressive_apply_handles_selection_padding_and_history_as_one_change() {
        let mut image = Raster::blank(30, 20);
        for y in 0..20 {
            for x in 0..15 {
                image.set(x, y, [255; 4]);
            }
        }
        let (mut doc, id) = document(image, [100, 80]);
        doc.layer_mut(id).unwrap().transform = Transform::new(30., 25., 30., 20.);
        let before = doc.clone();
        let mut history = History::default();
        let settings = serde_json::json!({"progressiveTo":3,
            "progressiveStart":{"x":45,"y":25},"progressiveEnd":{"x":45,"y":45}});
        apply(&mut doc, id, false, "Progressive Blur", &settings).unwrap();
        assert!(at(&doc, id, [37.5, 46.5])[3] > 0);
        assert_eq!(at(&doc, id, [37.5, 25.5]), [255; 4]);
        assert!(history.record("Progressive Blur", before.clone(), &doc));
        assert!(history.undo(&mut doc));
        assert_eq!(doc, before);
        doc.selection = Some(crate::selection::marquee(
            100,
            80,
            [30., 35., 30., 10.],
            false,
            false,
        ));
        apply(&mut doc, id, false, "Progressive Blur", &settings).unwrap();
        assert_eq!(at(&doc, id, [37.5, 30.5]), [255; 4]);
        assert_eq!(at(&doc, id, [37.5, 46.5]), [0; 4]);
        assert!(at(&doc, id, [37.5, 44.5])[3] < 255);
    }

    #[test]
    fn progressive_zero_is_identity_and_mask_uses_its_independent_document_placement() {
        let (mut doc, id) = document(Raster::solid(20, 12, [100, 150, 200, 255]), [80, 80]);
        let before = doc.clone();
        apply(
            &mut doc,
            id,
            false,
            "Progressive Blur",
            &serde_json::json!({"progressiveFrom":0,"progressiveTo":0}),
        )
        .unwrap();
        assert_eq!(doc, before);
        let mut placed = Transform::new(25., 30., 12., 20.);
        placed.rotation = 90.;
        let image = Raster::from_rgba(
            12,
            20,
            (0..240)
                .flat_map(|i| {
                    let v = if i % 12 < 6 { 255 } else { 0 };
                    [v, v, v, 255]
                })
                .collect(),
        );
        doc.layer_mut(id).unwrap().mask = Some(Mask {
            raster: Arc::new(image),
            placement: Some(placed),
            enabled: true,
            linked: false,
        });
        let start = placed.to_document([0.5, 0.]);
        let end = placed.to_document([0.5, 1.]);
        apply(
            &mut doc,
            id,
            true,
            "Progressive Blur",
            &serde_json::json!({"progressiveTo":4,
            "progressiveStart":start,"progressiveEnd":end}),
        )
        .unwrap();
        let mask = doc.layer(id).unwrap().mask.as_ref().unwrap();
        assert_eq!(mask.placement, Some(placed));
        assert_eq!([mask.raster.width, mask.raster.height], [12, 20]);
        assert!(mask.raster.get(5, 19)[0] < mask.raster.get(5, 0)[0]);
        assert!(Arc::ptr_eq(
            doc.layer(id).unwrap().image.as_ref().unwrap(),
            before.layer(id).unwrap().image.as_ref().unwrap()
        ));
    }

    #[test]
    fn gaussian_spreads_past_the_layer_and_trims_empty_pixels_as_one_undo_step() {
        // FilterTests.swift: 40 × 20, white on the left half, transparent on the right.
        let mut image = Raster::blank(40, 20);
        for y in 0..20 {
            for x in 0..20 {
                image.set(x, y, [255; 4]);
            }
        }
        let (mut doc, id) = document(image, [40, 20]);
        doc.layer_mut(id).unwrap().effects = Some(json!({"stroke":{"size":3}}));
        let before = doc.clone();
        apply(
            &mut doc,
            id,
            false,
            "Gaussian Blur",
            &json!({"blurRadius":3}),
        )
        .unwrap();
        let layer = doc.layer(id).unwrap();
        let output = layer.image.as_ref().unwrap();
        assert!(output.height > 20);
        assert!(output.width < 40);
        assert!(layer.transform.origin[0] < 0. && layer.transform.origin[1] < 0.);
        let middle: Vec<_> = (0..output.width)
            .map(|x| output.get(x, output.height / 2)[3])
            .collect();
        assert!(*middle.iter().max().unwrap() >= 250);
        assert!(middle.iter().any(|&alpha| alpha > 20 && alpha < 235));
        assert!(*middle.last().unwrap() < 20);
        assert_eq!(layer.effects, before.layer(id).unwrap().effects);
        let after = doc.clone();
        let mut history = History::default();
        assert!(history.record("Gaussian Blur", before.clone(), &doc));
        assert!(history.undo(&mut doc));
        assert_eq!(doc, before);
        assert!(!history.undo(&mut doc));
        assert!(history.redo(&mut doc));
        assert_eq!(doc, after);
    }

    #[test]
    fn motion_streaks_right_up_and_left_down_without_clamping_layer_edges() {
        let (doc, id) = document(Raster::solid(1, 1, [255; 4]), [41, 41]);
        for angle in [0, 45, 90] {
            let mut made = doc.clone();
            made.layer_mut(id).unwrap().transform.origin = [20., 20.];
            apply(
                &mut made,
                id,
                false,
                "Motion Blur",
                &json!({"motionAngle":angle,"motionDistance":16}),
            )
            .unwrap();
            match angle {
                0 => {
                    assert!(at(&made, id, [24.5, 20.5])[3] > 0);
                    assert!(at(&made, id, [16.5, 20.5])[3] > 0);
                    assert_eq!(at(&made, id, [20.5, 24.5])[3], 0);
                }
                45 => {
                    assert!(at(&made, id, [23.5, 17.5])[3] > 0);
                    assert!(at(&made, id, [17.5, 23.5])[3] > 0);
                    assert_eq!(at(&made, id, [17.5, 17.5])[3], 0);
                }
                _ => {
                    assert!(at(&made, id, [20.5, 24.5])[3] > 0);
                    assert!(at(&made, id, [20.5, 16.5])[3] > 0);
                    assert_eq!(at(&made, id, [24.5, 20.5])[3], 0);
                }
            }
            assert!(
                made.layer(id).unwrap().image.as_ref().unwrap().width > 1
                    || made.layer(id).unwrap().image.as_ref().unwrap().height > 1
            );
        }
    }

    #[test]
    fn bloom_spreads_real_light_coverage_and_keeps_an_opaque_base_opaque() {
        // FinishingFilterTests.swift: a 5 × 5 white lamp on black, then on transparency.
        for opaque in [false, true] {
            let mut image = if opaque {
                Raster::solid(65, 65, [0, 0, 0, 255])
            } else {
                Raster::blank(65, 65)
            };
            for y in 30..35 {
                for x in 30..35 {
                    image.set(x, y, [255; 4]);
                }
            }
            let (mut doc, id) = document(image, [65, 65]);
            apply(
                &mut doc,
                id,
                false,
                "Bloom / Glow",
                &json!({"bloomAmount":100,"bloomRadius":12}),
            )
            .unwrap();
            let light = at(&doc, id, [40.5, 32.5]);
            let corner = at(&doc, id, [2.5, 2.5]);
            assert!(light[0] > corner[0]);
            assert!(light[0] > 0 && light[3] > 0);
            if opaque {
                for y in 0..65 {
                    for x in 0..65 {
                        assert_eq!(at(&doc, id, [x as f64 + 0.5, y as f64 + 0.5])[3], 255);
                    }
                }
            }
        }
        let (mut black, id) = document(Raster::solid(5, 5, [0, 0, 0, 255]), [25, 25]);
        apply(
            &mut black,
            id,
            false,
            "Bloom / Glow",
            &json!({"bloomAmount":100,"bloomRadius":4}),
        )
        .unwrap();
        assert_eq!(
            black.layer(id).unwrap().transform,
            Transform::new(0., 0., 5., 5.)
        );
        assert_eq!(
            black.layer(id).unwrap().image.as_ref().unwrap().get(0, 0),
            [0, 0, 0, 255]
        );
    }

    #[test]
    fn grown_rotated_flipped_grid_keeps_native_scale_and_the_masks_original_position() {
        let (mut doc, id) = document(Raster::solid(8, 6, [200, 100, 40, 255]), [80, 80]);
        let mut placed = Transform::new(25., 24., 16., 9.);
        placed.rotation = 37.;
        placed.flip_x = true;
        placed.flip_y = true;
        placed.sampling = Sampling::Nearest;
        let mut mask = Mask::white();
        mask.raster = Arc::new(Raster::solid(8, 6, [128, 128, 128, 255]));
        let shared_mask = mask.raster.clone();
        let layer = doc.layer_mut(id).unwrap();
        layer.transform = placed;
        layer.mask = Some(mask);
        let point = placed.to_document([2.5 / 8., 3.5 / 6.]);
        let coverage = render::raster_mask_coverage(doc.layer(id).unwrap(), point);
        apply(
            &mut doc,
            id,
            false,
            "Gaussian Blur",
            &json!({"blurRadius":2}),
        )
        .unwrap();
        let layer = doc.layer(id).unwrap();
        let output = layer.image.as_ref().unwrap();
        assert_eq!(layer.transform.rotation, placed.rotation);
        assert_eq!(layer.transform.flip_x, placed.flip_x);
        assert_eq!(layer.transform.flip_y, placed.flip_y);
        assert_eq!(layer.transform.sampling, placed.sampling);
        assert!(
            (layer.transform.size[0] / output.width as f64 - placed.size[0] / 8.).abs() < 1e-10
        );
        assert!(
            (layer.transform.size[1] / output.height as f64 - placed.size[1] / 6.).abs() < 1e-10
        );
        assert_eq!(layer.mask.as_ref().unwrap().placement, Some(placed));
        assert!(Arc::ptr_eq(
            &shared_mask,
            &layer.mask.as_ref().unwrap().raster
        ));
        assert_eq!(render::raster_mask_coverage(layer, point), coverage);
        // Regridding keeps the original pixel centers on integer positions in the new grid.
        let uv = layer.transform.from_document(point);
        let x = uv[0] * output.width as f64 - 0.5;
        let y = uv[1] * output.height as f64 - 0.5;
        assert!((x - x.round()).abs() < 1e-9);
        assert!((y - y.round()).abs() < 1e-9);
    }

    #[test]
    fn independently_placed_disabled_mask_is_preserved_by_growth() {
        let (mut doc, id) = document(Raster::solid(5, 7, [220, 80, 40, 255]), [80, 80]);
        let mut placement = Transform::new(14., 10., 9., 11.);
        placement.rotation = -28.;
        placement.flip_x = true;
        let mut mask = Mask::white();
        mask.placement = Some(placement);
        mask.enabled = false;
        mask.linked = false;
        let before = mask.clone();
        doc.layer_mut(id).unwrap().mask = Some(mask);
        apply(
            &mut doc,
            id,
            false,
            "Motion Blur",
            &json!({"motionDistance":8}),
        )
        .unwrap();
        assert_eq!(doc.layer(id).unwrap().mask, Some(before));
    }

    #[test]
    fn rotated_feathered_selection_blends_the_new_blur_alpha_once() {
        let (mut doc, id) = document(Raster::solid(8, 8, [255; 4]), [80, 80]);
        let mut placed = Transform::new(24., 24., 16., 16.);
        placed.rotation = 31.;
        placed.flip_y = true;
        doc.layer_mut(id).unwrap().transform = placed;
        let mut full = doc.clone();
        apply(
            &mut full,
            id,
            false,
            "Gaussian Blur",
            &json!({"blurRadius":3}),
        )
        .unwrap();
        doc.selection = Some(Selection::from_coverage(80, 80, vec![128; 80 * 80]));
        apply(
            &mut doc,
            id,
            false,
            "Gaussian Blur",
            &json!({"blurRadius":3}),
        )
        .unwrap();
        let point = placed.to_document([-1.5 / 8., 3.5 / 8.]);
        let before = at(&full, id, point);
        let selected = at(&doc, id, point);
        assert!(before[3] > 10 && before[3] < 230);
        assert_eq!(selected[3], ((before[3] as u32 * 128 + 127) / 255) as u8);
        assert_eq!(selected[..3], [255; 3]);
    }

    #[test]
    fn mask_filter_keeps_dimensions_placement_and_pixels_and_blends_selection_once() {
        let (mut doc, id) = document(Raster::solid(2, 2, [90, 150, 200, 255]), [32, 32]);
        let source = doc.layer(id).unwrap().image.clone().unwrap();
        let mut placement = Transform::new(8., 8., 8., 8.);
        placement.rotation = 45.;
        placement.flip_x = true;
        let mask = Mask {
            raster: Arc::new(Raster::solid(2, 2, [10, 10, 10, 255])),
            placement: Some(placement),
            enabled: false,
            linked: false,
        };
        doc.layer_mut(id).unwrap().mask = Some(mask);
        doc.selection = Some(Selection::from_coverage(32, 32, vec![64; 32 * 32]));
        apply(&mut doc, id, true, "Invert", &json!({})).unwrap();
        let layer = doc.layer(id).unwrap();
        let owned = layer.mask.as_ref().unwrap();
        assert_eq!([owned.raster.width, owned.raster.height], [2, 2]);
        assert_eq!(owned.placement, Some(placement));
        assert!(!owned.enabled && !owned.linked);
        assert_eq!(owned.raster.get(0, 0), [69, 69, 69, 255]);
        assert!(Arc::ptr_eq(&source, layer.image.as_ref().unwrap()));
        let original_dimensions = [owned.raster.width, owned.raster.height];
        apply(
            &mut doc,
            id,
            true,
            "Gaussian Blur",
            &json!({"blurRadius":3}),
        )
        .unwrap();
        let owned = doc.layer(id).unwrap().mask.as_ref().unwrap();
        assert_eq!(
            [owned.raster.width, owned.raster.height],
            original_dimensions
        );
        assert_eq!(owned.placement, Some(placement));
    }

    #[test]
    fn explicit_empty_and_off_canvas_selection_leave_every_field_and_arc_unchanged() {
        let (mut doc, id) = document(Raster::solid(5, 5, [255; 4]), [30, 30]);
        doc.layer_mut(id).unwrap().shape = Some(json!({"kind":"Rectangle"}));
        for selection in [
            Selection::empty(30, 30),
            crate::selection::translated(&Selection::full(30, 30), 50, 50, 30, 30),
        ] {
            doc.selection = Some(selection);
            let before = doc.clone();
            apply(
                &mut doc,
                id,
                false,
                "Gaussian Blur",
                &json!({"blurRadius":6}),
            )
            .unwrap();
            assert_eq!(doc, before);
            assert!(Arc::ptr_eq(
                doc.layer(id).unwrap().image.as_ref().unwrap(),
                before.layer(id).unwrap().image.as_ref().unwrap()
            ));
        }
        doc.selection = Some(crate::selection::marquee(
            30,
            30,
            [20., 20., 3., 3.],
            false,
            false,
        ));
        let before = doc.clone();
        apply(&mut doc, id, false, "Invert", &json!({})).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn empty_vignette_paints_colored_canvas_edges_and_leaves_the_middle_clear() {
        let mut doc = Document::new(41, 41);
        let layer = Layer::new("Empty", 41, 41);
        let id = layer.id;
        doc.layers = vec![layer];
        doc.active_layer_id = Some(id);
        apply(&mut doc,id,false,"Vignette",&json!({"vignetteAmount":100,"vignetteColor":{"red":1,"green":0.2,"blue":0},"vignetteHighlights":100})).unwrap();
        let source = doc.layer(id).unwrap().image.as_ref().unwrap();
        assert_eq!([source.width, source.height], [41, 41]);
        assert_eq!(source.get(20, 20), [0; 4]);
        let edge = source.get(0, 0);
        assert!(edge[3] > 0);
        assert_eq!(edge[..3], [255, 51, 0]);
    }

    #[test]
    fn empty_rotated_vignette_covers_canvas_and_preview_reduces_the_whole_grown_grid() {
        let mut doc = Document::new(1200, 800);
        let mut layer = Layer::new("Tiny empty", 2, 2);
        let id = layer.id;
        layer.transform = Transform::new(14., 18., 2., 2.);
        layer.transform.rotation = 28.;
        layer.transform.flip_x = true;
        doc.layers = vec![layer];
        doc.active_layer_id = Some(id);
        apply(
            &mut doc,
            id,
            false,
            "Vignette",
            &json!({"previewScale":0.05,"vignetteAmount":100}),
        )
        .unwrap();
        let layer = doc.layer(id).unwrap();
        let image = layer.image.as_ref().unwrap();
        assert!(image.width < 100 && image.height < 100);
        assert_eq!(layer.transform.rotation, 28.);
        assert!(layer.transform.flip_x);
        for point in [[0., 0.], [1200., 0.], [0., 800.], [1200., 800.]] {
            let uv = layer.transform.from_document(point);
            assert!(
                uv[0] >= -1e-10 && uv[1] >= -1e-10 && uv[0] <= 1. + 1e-10 && uv[1] <= 1. + 1e-10
            );
        }
    }

    #[test]
    fn empty_vignette_selection_changes_coverage_once_and_zero_amount_is_a_noop() {
        let mut doc = Document::new(41, 41);
        let layer = Layer::new("Empty", 41, 41);
        let id = layer.id;
        doc.layers = vec![layer];
        doc.active_layer_id = Some(id);
        let before = doc.clone();
        apply(
            &mut doc,
            id,
            false,
            "Vignette",
            &json!({"vignetteAmount":0}),
        )
        .unwrap();
        assert_eq!(doc, before);
        let mut full = doc.clone();
        apply(
            &mut full,
            id,
            false,
            "Vignette",
            &json!({"vignetteAmount":80}),
        )
        .unwrap();
        doc.selection = Some(Selection::from_coverage(41, 41, vec![128; 41 * 41]));
        apply(
            &mut doc,
            id,
            false,
            "Vignette",
            &json!({"vignetteAmount":80}),
        )
        .unwrap();
        let alpha = full.layer(id).unwrap().image.as_ref().unwrap().get(0, 0)[3];
        assert_eq!(
            doc.layer(id).unwrap().image.as_ref().unwrap().get(0, 0)[3],
            ((alpha as u32 * 128 + 127) / 255) as u8
        );
    }

    #[test]
    fn vignette_on_pixels_keeps_alpha_and_an_oversized_blur_fails_atomically() {
        let (mut doc, id) = document(Raster::solid(41, 41, [204, 204, 204, 128]), [41, 41]);
        apply(
            &mut doc,
            id,
            false,
            "Vignette",
            &json!({"vignetteAmount":80}),
        )
        .unwrap();
        let output = doc.layer(id).unwrap().image.as_ref().unwrap();
        assert!(output.get(20, 20)[0] > output.get(0, 0)[0] + 50);
        for pixel in output.to_rgba().as_chunks::<4>().0 {
            assert_eq!(pixel[3], 128);
        }
        let (mut huge, id) = document(Raster::solid(30_000, 1, [255; 4]), [30_000, 1]);
        let before = huge.clone();
        assert!(
            apply(
                &mut huge,
                id,
                false,
                "Gaussian Blur",
                &json!({"blurRadius":250})
            )
            .is_err()
        );
        assert_eq!(huge, before);
    }
}
