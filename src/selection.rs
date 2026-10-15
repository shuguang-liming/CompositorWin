use std::{collections::VecDeque, sync::Arc};

use crate::model::{Raster, Selection, SelectionRegion};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMode {
    Replace,
    Add,
    Subtract,
    Intersect,
}

pub fn drag_box(anchor: [f64; 2], point: [f64; 2], square: bool, centered: bool) -> [f64; 4] {
    let mut d = [point[0].round() - anchor[0], point[1].round() - anchor[1]];
    if square {
        let s = d[0].abs().max(d[1].abs());
        for v in &mut d {
            *v = if *v < 0. { -s } else { s };
        }
    }
    if centered {
        [
            anchor[0] - d[0].abs(),
            anchor[1] - d[1].abs(),
            d[0].abs() * 2.,
            d[1].abs() * 2.,
        ]
    } else {
        [
            anchor[0].min(anchor[0] + d[0]),
            anchor[1].min(anchor[1] + d[1]),
            d[0].abs(),
            d[1].abs(),
        ]
    }
}

pub fn marquee(
    width: u32,
    height: u32,
    rect: [f64; 4],
    ellipse: bool,
    antialiased: bool,
) -> Selection {
    rasterize(width, height, rect, antialiased, |x, y| {
        if rect[2] <= 0. || rect[3] <= 0. {
            return false;
        }
        if ellipse {
            ((x - rect[0]) / rect[2] * 2. - 1.).powi(2)
                + ((y - rect[1]) / rect[3] * 2. - 1.).powi(2)
                <= 1.
        } else {
            x >= rect[0] && y >= rect[1] && x < rect[0] + rect[2] && y < rect[1] + rect[3]
        }
    })
}

pub fn polygon(width: u32, height: u32, points: &[[f64; 2]], antialiased: bool) -> Selection {
    if points.len() < 3 || points.iter().flatten().any(|v| !v.is_finite()) {
        return Selection::empty(width, height);
    }
    let minx = points.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
    let miny = points.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
    let maxx = points
        .iter()
        .map(|p| p[0])
        .fold(f64::NEG_INFINITY, f64::max);
    let maxy = points
        .iter()
        .map(|p| p[1])
        .fold(f64::NEG_INFINITY, f64::max);
    rasterize(
        width,
        height,
        [minx, miny, maxx - minx, maxy - miny],
        antialiased,
        |x, y| {
            let mut winding = 0;
            for i in 0..points.len() {
                let a = points[i];
                let b = points[(i + 1) % points.len()];
                let cross = (b[0] - a[0]) * (y - a[1]) - (x - a[0]) * (b[1] - a[1]);
                if a[1] <= y && b[1] > y && cross > 0. {
                    winding += 1;
                }
                if a[1] > y && b[1] <= y && cross < 0. {
                    winding -= 1;
                }
            }
            winding != 0
        },
    )
}

fn rasterize(
    width: u32,
    height: u32,
    b: [f64; 4],
    aa: bool,
    inside: impl Fn(f64, f64) -> bool,
) -> Selection {
    let mut data = vec![0; width as usize * height as usize];
    if b.iter().any(|x| !x.is_finite()) {
        return Selection::from_coverage(width, height, data);
    }
    let x0 = b[0].floor().max(0.) as u32;
    let y0 = b[1].floor().max(0.) as u32;
    let x1 = (b[0] + b[2]).ceil().clamp(0., width as f64) as u32;
    let y1 = (b[1] + b[3]).ceil().clamp(0., height as f64) as u32;
    let samples = if aa { 4 } else { 1 };
    for y in y0..y1 {
        for x in x0..x1 {
            let mut hits = 0;
            for sy in 0..samples {
                for sx in 0..samples {
                    if inside(
                        x as f64 + (sx as f64 + 0.5) / samples as f64,
                        y as f64 + (sy as f64 + 0.5) / samples as f64,
                    ) {
                        hits += 1;
                    }
                }
            }
            data[(y * width + x) as usize] = (hits * 255 / (samples * samples)) as u8;
        }
    }
    Selection::from_coverage(width, height, data)
}

pub fn combine(
    current: Option<&Selection>,
    incoming: &Selection,
    mode: SelectionMode,
) -> Selection {
    if mode == SelectionMode::Replace {
        return incoming.clone();
    }
    let current = current.map(|s| s.feather_base.as_deref().unwrap_or(s));
    let incoming = incoming.feather_base.as_deref().unwrap_or(incoming);
    let Some(current) = current else {
        return if mode == SelectionMode::Add {
            Selection::from_coverage(
                incoming.width,
                incoming.height,
                incoming.canvas_coverage().to_vec(),
            )
        } else {
            Selection::empty(incoming.width, incoming.height)
        };
    };
    if current.origin == [0, 0]
        && current.coverage_size == [incoming.width, incoming.height]
        && current.additional.is_empty()
    {
        let data = (0..incoming.height)
            .flat_map(|y| {
                (0..incoming.width).map(move |x| {
                    let a = current.get(x, y);
                    let b = incoming.get(x, y);
                    match mode {
                        SelectionMode::Add => a.max(b),
                        SelectionMode::Subtract => a.saturating_sub(b),
                        SelectionMode::Intersect => a.min(b),
                        SelectionMode::Replace => b,
                    }
                })
            })
            .collect::<Vec<_>>();
        return Selection::from_coverage(incoming.width, incoming.height, data);
    }
    let mut result = current.clone();
    result.width = incoming.width;
    result.height = incoming.height;
    if mode == SelectionMode::Add {
        if let Some([x0, y0, x1, y1]) = incoming.bounds() {
            let data = (y0..y1)
                .flat_map(|y| (x0..x1).map(move |x| incoming.get(x, y)))
                .collect();
            result.additional.push(SelectionRegion {
                origin: [x0 as i64, y0 as i64],
                size: [x1 - x0, y1 - y0],
                coverage: Arc::new(data),
            });
        }
    } else {
        let modify = |origin: [i64; 2], size: [u32; 2], coverage: &mut Arc<Vec<u8>>| {
            for (i, value) in Arc::make_mut(coverage).iter_mut().enumerate() {
                if *value == 0 {
                    continue;
                }
                let x = origin[0].saturating_add(i as i64 % size[0] as i64);
                let y = origin[1].saturating_add(i as i64 / size[0] as i64);
                let b = if x >= 0
                    && y >= 0
                    && x < incoming.width as i64
                    && y < incoming.height as i64
                {
                    incoming.get(x as u32, y as u32)
                } else {
                    0
                };
                *value = if mode == SelectionMode::Subtract {
                    value.saturating_sub(b)
                } else {
                    (*value).min(b)
                };
            }
        };
        modify(result.origin, result.coverage_size, &mut result.coverage);
        for region in &mut result.additional {
            modify(region.origin, region.size, &mut region.coverage);
        }
    }
    result
}

pub fn inverse(selection: &Selection) -> Option<Selection> {
    let amount = selection.feather;
    let selection = selection.feather_base.as_deref().unwrap_or(selection);
    let result = Selection::from_coverage(
        selection.width,
        selection.height,
        selection
            .canvas_coverage()
            .iter()
            .map(|v| 255 - v)
            .collect::<Vec<_>>(),
    );
    result.bounds().map(|_| soften(&result, amount))
}

/// Offset immutable coverage grids without clipping, so separate committed moves remain reversible.
pub fn translated(selection: &Selection, dx: i32, dy: i32, width: u32, height: u32) -> Selection {
    let mut result = selection.clone();
    result.width = width;
    result.height = height;
    result.origin[0] = result.origin[0].saturating_add(dx as i64);
    result.origin[1] = result.origin[1].saturating_add(dy as i64);
    for region in &mut result.additional {
        region.origin[0] = region.origin[0].saturating_add(dx as i64);
        region.origin[1] = region.origin[1].saturating_add(dy as i64);
    }
    if let Some(base) = &selection.feather_base {
        result.feather_base = Some(Arc::new(translated(base, dx, dy, width, height)));
    }
    result
}

pub fn flipped(selection: &Selection, horizontal: bool) -> Selection {
    if let Some(base) = &selection.feather_base {
        return soften(&flipped(base, horizontal), selection.feather);
    }
    let regions = selection
        .regions()
        .map(|(origin, size, coverage)| {
            let mut data = vec![0; coverage.len()];
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let sx = if horizontal { size[0] - 1 - x } else { x };
                    let sy = if horizontal { y } else { size[1] - 1 - y };
                    data[(y as u64 * size[0] as u64 + x as u64) as usize] =
                        coverage[(sy as u64 * size[0] as u64 + sx as u64) as usize];
                }
            }
            let origin = if horizontal {
                [
                    selection.width as i64 - origin[0] - size[0] as i64,
                    origin[1],
                ]
            } else {
                [
                    origin[0],
                    selection.height as i64 - origin[1] - size[1] as i64,
                ]
            };
            SelectionRegion {
                origin,
                size,
                coverage: Arc::new(data),
            }
        })
        .collect();
    Selection::from_regions(selection.width, selection.height, regions)
}

pub fn scaled(selection: &Selection, width: u32, height: u32) -> Selection {
    if let Some(base) = &selection.feather_base {
        return soften(&scaled(base, width, height), selection.feather);
    }
    if selection.width == 0 || selection.height == 0 {
        return Selection::empty(width, height);
    }
    let sx = width as f64 / selection.width as f64;
    let sy = height as f64 / selection.height as f64;
    let regions = selection
        .regions()
        .map(|(origin, size, coverage)| {
            let x0 = (origin[0] as f64 * sx).floor() as i64;
            let y0 = (origin[1] as f64 * sy).floor() as i64;
            let x1 = ((origin[0] as f64 + size[0] as f64) * sx).ceil() as i64;
            let y1 = ((origin[1] as f64 + size[1] as f64) * sy).ceil() as i64;
            let w = x1.saturating_sub(x0) as u32;
            let h = y1.saturating_sub(y0) as u32;
            let data = (0..h)
                .flat_map(|y| {
                    (0..w).map(move |x| {
                        let ix = ((x0 as f64 + x as f64 + 0.5) / sx).floor() as i64 - origin[0];
                        let iy = ((y0 as f64 + y as f64 + 0.5) / sy).floor() as i64 - origin[1];
                        if ix >= 0 && iy >= 0 && ix < size[0] as i64 && iy < size[1] as i64 {
                            coverage[(iy as u64 * size[0] as u64 + ix as u64) as usize]
                        } else {
                            0
                        }
                    })
                })
                .collect();
            SelectionRegion {
                origin: [x0, y0],
                size: [w, h],
                coverage: Arc::new(data),
            }
        })
        .collect();
    Selection::from_regions(width, height, regions)
}

/// Exact squared Euclidean distance transform, in two linear-time sweeps.
fn distance_to(coverage: &[u8], w: usize, h: usize, selected: bool) -> Vec<f64> {
    let mut data = coverage
        .iter()
        .map(|&v| if (v >= 128) == selected { 0. } else { 1e20 })
        .collect::<Vec<_>>();
    fn line(f: &[f64]) -> Vec<f64> {
        let n = f.len();
        if n == 0 {
            return vec![];
        }
        let mut v = vec![0usize; n];
        let mut z = vec![0.; n + 1];
        let mut k = 0;
        z[0] = f64::NEG_INFINITY;
        z[1] = f64::INFINITY;
        for q in 1..n {
            let mut s = ((f[q] + (q * q) as f64) - (f[v[k]] + (v[k] * v[k]) as f64))
                / (2. * (q - v[k]) as f64);
            while s <= z[k] && k > 0 {
                k -= 1;
                s = ((f[q] + (q * q) as f64) - (f[v[k]] + (v[k] * v[k]) as f64))
                    / (2. * (q - v[k]) as f64);
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f64::INFINITY;
        }
        k = 0;
        (0..n)
            .map(|q| {
                while z[k + 1] < (q as f64) {
                    k += 1;
                }
                let d = q as f64 - v[k] as f64;
                d * d + f[v[k]]
            })
            .collect()
    }
    for y in 0..h {
        let values = line(&data[y * w..(y + 1) * w]);
        data[y * w..(y + 1) * w].copy_from_slice(&values);
    }
    for x in 0..w {
        let values = line(&(0..h).map(|y| data[y * w + x]).collect::<Vec<_>>());
        for y in 0..h {
            data[y * w + x] = values[y];
        }
    }
    if !selected {
        for y in 0..h {
            for x in 0..w {
                let edge = (x + 1).min(y + 1).min(w - x).min(h - y) as f64;
                data[y * w + x] = data[y * w + x].min(edge * edge);
            }
        }
    }
    data
}

pub fn expand(selection: &Selection, amount: u32) -> Selection {
    morphology(selection, amount.min(500), true)
}
pub fn contract(selection: &Selection, amount: u32) -> Selection {
    morphology(selection, amount.min(500), false)
}
fn morphology(s: &Selection, amount: u32, grow: bool) -> Selection {
    if amount == 0 {
        return s.clone();
    }
    if let Some(base) = &s.feather_base {
        return soften(&morphology(base, amount, grow), s.feather);
    }
    let radius_squared = (amount as f64).powi(2);
    let padding = amount as usize + 1;
    let regions = region_bounds(s, if grow { amount } else { 0 }, grow)
        .into_iter()
        .map(|[x0, y0, x1, y1]| {
            let width = (x1 - x0) as usize;
            let height = (y1 - y0) as usize;
            let mut data = vec![0; width * height];
            // Bound distance-transform scratch memory even for a large document.
            for oy in (0..height).step_by(512) {
                for ox in (0..width).step_by(512) {
                    let tw = (width - ox).min(512);
                    let th = (height - oy).min(512);
                    let iw = tw + padding * 2;
                    let ih = th + padding * 2;
                    let input = (0..ih)
                        .flat_map(|y| {
                            (0..iw).map(move |x| {
                                s.get_unclipped(
                                    x0 + ox as i64 + x as i64 - padding as i64,
                                    y0 + oy as i64 + y as i64 - padding as i64,
                                )
                            })
                        })
                        .collect::<Vec<_>>();
                    let distance = distance_to(&input, iw, ih, grow);
                    for y in 0..th {
                        for x in 0..tw {
                            let i = (y + padding) * iw + x + padding;
                            data[(oy + y) * width + ox + x] = if distance[i] <= radius_squared {
                                if grow { 255 } else { 0 }
                            } else {
                                input[i]
                            };
                        }
                    }
                }
            }
            SelectionRegion {
                origin: [x0, y0],
                size: [width as u32, height as u32],
                coverage: Arc::new(data),
            }
        })
        .collect();
    Selection::from_regions(s.width, s.height, regions)
}

/// Tight, disjoint processing rectangles. Far-away parts stay separate rather than filling gaps.
pub(crate) fn region_bounds(s: &Selection, padding: u32, clip_canvas: bool) -> Vec<[i64; 4]> {
    let mut bounds = Vec::<[i64; 4]>::new();
    for (origin, size, coverage) in s.regions() {
        let mut b = [size[0], size[1], 0, 0];
        for (i, &value) in coverage.iter().enumerate() {
            if value != 0 {
                let x = i as u32 % size[0];
                let y = i as u32 / size[0];
                b[0] = b[0].min(x);
                b[1] = b[1].min(y);
                b[2] = b[2].max(x + 1);
                b[3] = b[3].max(y + 1);
            }
        }
        if b[0] >= b[2] || b[1] >= b[3] {
            continue;
        }
        let p = padding as i64;
        let mut rect = [
            origin[0].saturating_add(b[0] as i64).saturating_sub(p),
            origin[1].saturating_add(b[1] as i64).saturating_sub(p),
            origin[0].saturating_add(b[2] as i64).saturating_add(p),
            origin[1].saturating_add(b[3] as i64).saturating_add(p),
        ];
        if clip_canvas {
            rect[0] = rect[0].max(0);
            rect[1] = rect[1].max(0);
            rect[2] = rect[2].min(s.width as i64);
            rect[3] = rect[3].min(s.height as i64);
        }
        if rect[0] >= rect[2] || rect[1] >= rect[3] {
            continue;
        }
        let mut i = 0;
        while i < bounds.len() {
            let old = bounds[i];
            let union = [
                rect[0].min(old[0]),
                rect[1].min(old[1]),
                rect[2].max(old[2]),
                rect[3].max(old[3]),
            ];
            let overlaps =
                rect[0] <= old[2] && old[0] <= rect[2] && rect[1] <= old[3] && old[1] <= rect[3];
            let area = (union[2] as i128 - union[0] as i128) as u128
                * (union[3] as i128 - union[1] as i128) as u128;
            if overlaps && area <= crate::model::MAX_PIXELS as u128 {
                rect = union;
                bounds.swap_remove(i);
                i = 0;
            } else {
                i += 1;
            }
        }
        bounds.push(rect);
    }
    bounds
}

pub fn feather(s: &Selection, amount: f64) -> Selection {
    if !amount.is_finite() || amount <= 0. {
        return s.clone();
    }
    let total = s.feather.hypot(amount).min(250.);
    if total == s.feather {
        return s.clone();
    }
    soften(s.feather_base.as_deref().unwrap_or(s), total)
}

fn soften(s: &Selection, amount: f64) -> Selection {
    if amount <= 0. {
        return s.clone();
    }
    let sigma = amount.min(250.) / 2.;
    let r = (sigma * 3.).ceil() as i32;
    let mut kernel = (-r..=r)
        .map(|x| (-(x as f64).powi(2) / (2. * sigma * sigma)).exp())
        .collect::<Vec<_>>();
    let sum: f64 = kernel.iter().sum();
    for v in &mut kernel {
        *v /= sum;
    }
    let regions = region_bounds(s, r as u32, false)
        .into_iter()
        .map(|[x0, y0, x1, y1]| {
            let w = (x1 - x0) as usize;
            let h = (y1 - y0) as usize;
            let mut result = vec![0; w * h];
            for oy in (0..h).step_by(128) {
                let th = (h - oy).min(128);
                let ih = th + r as usize * 2;
                let mut tmp = vec![0.; w * ih];
                for y in 0..ih {
                    for x in 0..w {
                        tmp[y * w + x] = kernel
                            .iter()
                            .enumerate()
                            .map(|(i, k)| {
                                s.get_unclipped(
                                    x0 + x as i64 + i as i64 - r as i64,
                                    y0 + oy as i64 + y as i64 - r as i64,
                                ) as f64
                                    * k
                            })
                            .sum();
                    }
                }
                for y in 0..th {
                    for x in 0..w {
                        result[(oy + y) * w + x] = kernel
                            .iter()
                            .enumerate()
                            .map(|(i, k)| tmp[(y + i) * w + x] * k)
                            .sum::<f64>()
                            .round() as u8;
                    }
                }
            }
            SelectionRegion {
                origin: [x0, y0],
                size: [w as u32, h as u32],
                coverage: Arc::new(result),
            }
        })
        .collect();
    let mut result = Selection::from_regions(s.width, s.height, regions);
    result.feather = amount.min(250.);
    result.feather_base = Some(Arc::new(s.clone()));
    result
}

#[derive(Clone, Copy, Debug)]
pub struct WandSettings {
    pub tolerance: u8,
    pub sample_radius: u32,
    pub contiguous: bool,
}
impl Default for WandSettings {
    fn default() -> Self {
        Self {
            tolerance: 32,
            sample_radius: 0,
            contiguous: true,
        }
    }
}

/// Matches the original C algorithm's RGBA sample average and four-channel tolerance.
pub fn wand(image: &Raster, point: [f64; 2], settings: WandSettings) -> Selection {
    let w = image.width;
    let h = image.height;
    let mut mask = vec![0; w as usize * h as usize];
    if point.iter().any(|x| !x.is_finite())
        || point[0] < 0.
        || point[1] < 0.
        || point[0] >= w as f64
        || point[1] >= h as f64
    {
        return Selection::from_coverage(w, h, mask);
    }
    let x = point[0].floor() as u32;
    let y = point[1].floor() as u32;
    let r = settings.sample_radius.min(2);
    let mut sum = [0u64; 4];
    let mut n = 0;
    for yy in y.saturating_sub(r)..=(y + r).min(h - 1) {
        for xx in x.saturating_sub(r)..=(x + r).min(w - 1) {
            let p = image.get(xx, yy);
            for c in 0..4 {
                sum[c] += p[c] as u64;
            }
            n += 1;
        }
    }
    let reference = sum.map(|v| ((v + n / 2) / n) as u8);
    let matches = |xx, yy| {
        image
            .get(xx, yy)
            .iter()
            .zip(reference)
            .all(|(&a, b)| a.abs_diff(b) <= settings.tolerance)
    };
    if settings.contiguous {
        let mut queue = VecDeque::from([(x, y)]);
        while let Some((xx, yy)) = queue.pop_front() {
            if mask[(yy * w + xx) as usize] != 0 || !matches(xx, yy) {
                continue;
            }
            let mut left = xx;
            let mut right = xx;
            while left > 0 && mask[(yy * w + left - 1) as usize] == 0 && matches(left - 1, yy) {
                left -= 1;
            }
            while right + 1 < w
                && mask[(yy * w + right + 1) as usize] == 0
                && matches(right + 1, yy)
            {
                right += 1;
            }
            mask[(yy * w + left) as usize..=(yy * w + right) as usize].fill(255);
            for ny in [yy.checked_sub(1), (yy + 1 < h).then_some(yy + 1)]
                .into_iter()
                .flatten()
            {
                let mut run = false;
                for nx in left..=right {
                    let candidate = mask[(ny * w + nx) as usize] == 0 && matches(nx, ny);
                    if candidate && !run {
                        queue.push_back((nx, ny));
                    }
                    run = candidate;
                }
            }
        }
    } else {
        for yy in 0..h {
            for xx in 0..w {
                if matches(xx, yy) {
                    mask[(yy * w + xx) as usize] = 255;
                }
            }
        }
    }
    Selection::from_coverage(w, h, mask)
}

pub fn color_range(image: &Raster, colors: &[[u8; 4]], fuzziness: u8) -> Selection {
    let mut data = vec![0; image.width as usize * image.height as usize];
    for y in 0..image.height {
        for x in 0..image.width {
            let p = image.get(x, y);
            let d = colors
                .iter()
                .map(|c| (0..3).map(|i| p[i].abs_diff(c[i])).max().unwrap())
                .min()
                .unwrap_or(255);
            data[(y * image.width + x) as usize] = if fuzziness == 0 {
                if d == 0 { 255 } else { 0 }
            } else {
                ((1. - d as f64 / (fuzziness as f64 * 2.)).clamp(0., 1.) * 255.).round() as u8
            };
        }
    }
    Selection::from_coverage(image.width, image.height, data)
}

pub fn alpha(image: &Raster, invert: bool) -> Selection {
    let mut data = vec![0; image.width as usize * image.height as usize];
    for y in 0..image.height {
        for x in 0..image.width {
            let p = image.get(x, y);
            data[(y * image.width + x) as usize] = if invert { 255 - p[0] } else { p[3] };
        }
    }
    Selection::from_coverage(image.width, image.height, data)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_is_not_absent_and_modes_preserve_it() {
        let a = marquee(8, 8, [1., 1., 4., 4.], false, false);
        let b = combine(Some(&a), &a, SelectionMode::Subtract);
        assert!(b.bounds().is_none());
        assert!(inverse(&Selection::full(8, 8)).is_none());
        assert!(
            combine(None, &a, SelectionMode::Subtract)
                .bounds()
                .is_none()
        );
    }
    #[test]
    fn committed_moves_preserve_off_canvas_outline_and_share_coverage() {
        let original = marquee(12, 10, [2., 3., 5., 4.], false, false);
        let outside = translated(&original, -20, 14, 12, 10);
        assert!(outside.bounds().is_none());
        assert!(!outside.is_empty());
        assert_eq!(outside.get_unclipped(-18, 17), 255);
        assert_eq!(outside.get(2, 3), 0);
        assert!(Arc::ptr_eq(&original.coverage, &outside.coverage));
        let back = translated(&outside, 20, -14, 12, 10);
        assert_eq!(back, original);
        assert!(Arc::ptr_eq(&original.coverage, &back.coverage));
        let mut left = translated(&original, -5, 0, 12, 10);
        assert_eq!(left.bounds(), Some([0, 3, 2, 7]));
        left.set(0, 3, 0);
        let back = translated(&left, 5, 0, 12, 10);
        assert_eq!(back.get(5, 3), 0);
        assert_eq!(back.get(2, 3), 255);
        assert_eq!(original.get(5, 3), 255);
    }
    #[test]
    fn combine_keeps_retained_parts_without_allocating_far_away_gaps() {
        let original = marquee(12, 10, [2., 3., 5., 4.], false, false);
        let outside = translated(&original, -1_000_000, 0, 12, 10);
        let incoming = marquee(12, 10, [8., 1., 2., 2.], false, false);
        let added = combine(Some(&outside), &incoming, SelectionMode::Add);
        assert_eq!(added.get(8, 1), 255);
        assert_eq!(added.get_unclipped(-999_998, 3), 255);
        assert_eq!(
            added
                .regions()
                .map(|(_, _, data)| data.len())
                .sum::<usize>(),
            124
        );
        let removed = combine(Some(&added), &incoming, SelectionMode::Subtract);
        assert_eq!(removed.get(8, 1), 0);
        assert_eq!(removed.get_unclipped(-999_998, 3), 255);
        let intersection = combine(Some(&added), &incoming, SelectionMode::Intersect);
        assert_eq!(intersection.get(8, 1), 255);
        assert_eq!(intersection.get_unclipped(-999_998, 3), 0);
        assert_eq!(inverse(&outside).unwrap(), Selection::full(12, 10));
    }
    #[test]
    fn modifiers_flip_scale_and_canvas_changes_use_document_coordinates() {
        let original = marquee(20, 16, [6., 5., 7., 6.], false, false);
        let outside = translated(&original, -30, 0, 20, 16);
        let contracted = contract(&outside, 1);
        let back = translated(&contracted, 30, 0, 20, 16);
        assert_eq!(
            back.canvas_coverage(),
            contract(&original, 1).canvas_coverage()
        );
        let softened = feather(&outside, 4.);
        let back = translated(&softened, 30, 0, 20, 16);
        assert_eq!(
            back.canvas_coverage(),
            feather(&original, 4.).canvas_coverage()
        );
        assert_eq!(flipped(&flipped(&outside, true), true), outside);
        let resized = scaled(&outside, 40, 32);
        let back = translated(&resized, 60, 0, 40, 32);
        assert_eq!(
            back.canvas_coverage(),
            scaled(&original, 40, 32).canvas_coverage()
        );
        let larger_canvas = translated(&outside, 30, 0, 40, 32);
        assert_eq!(larger_canvas.bounds(), original.bounds());
        assert_eq!(larger_canvas.coverage_size, [20, 16]);
    }
    #[test]
    fn circle_and_polygon_clip_to_canvas() {
        let s = marquee(10, 10, [-2., -2., 8., 8.], true, true);
        assert_eq!(s.get(5, 5), 0);
        assert_eq!(s.get(1, 1), 255);
        let p = polygon(8, 8, &[[1., 1.], [6., 1.], [1., 6.]], false);
        assert_eq!(p.get(1, 1), 255);
        assert_eq!(p.get(5, 5), 0);
    }
    #[test]
    fn disk_morphology_and_contract_to_empty() {
        let s = marquee(9, 9, [3., 3., 3., 3.], false, false);
        let e = expand(&s, 2);
        assert_eq!(e.get(1, 3), 255);
        assert_eq!(e.get(1, 1), 0);
        assert!(contract(&s, 2).bounds().is_none());
    }
    #[test]
    fn feather_fades_both_sides() {
        let s = marquee(20, 12, [5., 0., 10., 12.], false, false);
        let f = feather(&s, 4.);
        assert!(f.get(4, 5) > 0);
        assert!(f.get(5, 5) < 255);
        assert!(f.get(0, 5) < f.get(4, 5));
    }
    #[test]
    fn repeated_feather_adds_variance_from_the_original_outline_and_caps_at_250() {
        let original = marquee(30, 24, [10., 6., 10., 12.], false, false);
        let once = feather(&original, 3.);
        let twice = feather(&once, 4.);
        assert_eq!(twice.feather, 5.);
        assert_eq!(twice, feather(&original, 5.));
        assert!(Arc::ptr_eq(
            &twice.feather_base.as_ref().unwrap().coverage,
            &original.coverage
        ));
        let moved = translated(&twice, -50, 0, 30, 24);
        let removed = combine(Some(&moved), &original, SelectionMode::Add);
        assert_eq!(removed.feather, 0.);
        assert_eq!(removed.get_unclipped(-40, 6), 255);
        assert_eq!(removed.get_unclipped(-41, 6), 0);
        let expanded = expand(&twice, 2);
        assert_eq!(expanded.feather, 5.);
        assert_eq!(expanded, feather(&expand(&original, 2), 5.));
        let capped = feather(&Selection::empty(8, 8), 250.);
        let repeated = feather(&capped, 200.);
        assert_eq!(repeated.feather, 250.);
        assert!(Arc::ptr_eq(&capped.coverage, &repeated.coverage));
    }
    #[test]
    fn wand_contiguous_and_rgba_tolerance() {
        let mut r = Raster::solid(5, 1, [255, 0, 0, 255]);
        r.set(2, 0, [0, 0, 0, 255]);
        let s = wand(
            &r,
            [0., 0.],
            WandSettings {
                tolerance: 0,
                ..Default::default()
            },
        );
        assert_eq!(s.get(1, 0), 255);
        assert_eq!(s.get(3, 0), 0);
        let s = wand(
            &r,
            [0., 0.],
            WandSettings {
                tolerance: 0,
                contiguous: false,
                ..Default::default()
            },
        );
        assert_eq!(s.get(3, 0), 255);
    }
    #[test]
    fn centered_square_tracks_drag_direction() {
        assert_eq!(
            drag_box([10., 10.], [6., 12.], true, true),
            [6., 6., 8., 8.]
        );
    }
}
