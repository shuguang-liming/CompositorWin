//! CPU image kernels. Buffers are tightly packed, top-down premultiplied RGBA8 in sRGB.
use std::collections::VecDeque;

pub fn premultiply(pixels: &mut [u8]) {
    for p in pixels.as_chunks_mut::<4>().0 {
        for c in 0..3 {
            p[c] = ((p[c] as u32 * p[3] as u32 + 127) / 255) as u8;
        }
    }
}
pub fn unpremultiply(pixels: &mut [u8]) {
    for p in pixels.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        for v in &mut p[..3] {
            *v = (*v as u32 * 255 + a / 2)
                .checked_div(a)
                .unwrap_or(0)
                .min(255) as u8;
        }
    }
}
pub fn clamp_premultiplied(pixels: &mut [u8]) {
    for p in pixels.as_chunks_mut::<4>().0 {
        for c in 0..3 {
            p[c] = p[c].min(p[3]);
        }
    }
}
pub fn alpha_bounds(pixels: &[u8], width: usize, height: usize) -> [usize; 4] {
    assert_eq!(pixels.len(), width * height * 4);
    let mut bounds = [width, height, 0, 0];
    for (y, row) in pixels.chunks_exact(width.max(1) * 4).enumerate() {
        for (x, pixel) in row.as_chunks::<4>().0.iter().enumerate() {
            if pixel[3] != 0 {
                bounds[0] = bounds[0].min(x);
                bounds[1] = bounds[1].min(y);
                bounds[2] = bounds[2].max(x + 1);
                bounds[3] = y + 1;
            }
        }
    }
    if bounds[2] == 0 { [0; 4] } else { bounds }
}
pub fn coverage_bounds(mask: &[u8], width: usize, height: usize) -> [usize; 4] {
    assert_eq!(mask.len(), width * height);
    let mut b = [width, height, 0, 0];
    for y in 0..height {
        for x in 0..width {
            if mask[y * width + x] != 0 {
                b[0] = b[0].min(x);
                b[1] = b[1].min(y);
                b[2] = b[2].max(x + 1);
                b[3] = b[3].max(y + 1);
            }
        }
    }
    if b[2] == 0 { [0; 4] } else { b }
}
pub fn extract_alpha(pixels: &[u8]) -> Vec<u8> {
    pixels.as_chunks::<4>().0.iter().map(|p| p[3]).collect()
}
pub fn restore_alpha(pixels: &mut [u8], alpha: &[u8]) {
    assert_eq!(pixels.len() / 4, alpha.len());
    for (p, &a) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(alpha) {
        for v in &mut p[..3] {
            *v = ((*v as u32 * a as u32 + 127) / 255) as u8;
        }
        p[3] = a;
    }
}
pub fn levels_apply(pixels: &mut [u8], tables: &[[f32; 256]; 3]) {
    for p in pixels.as_chunks_mut::<4>().0 {
        let a = p[3] as f32;
        if a == 0.0 {
            continue;
        }
        for c in 0..3 {
            let x = (p[c] as f32 * 255.0 / a).min(255.0);
            let lo = x as usize;
            let hi = (lo + 1).min(255);
            let v = tables[c][lo] + (tables[c][hi] - tables[c][lo]) * (x - lo as f32);
            p[c] = (v * a).round().clamp(0.0, a) as u8;
        }
    }
}
pub fn histogram(pixels: &[u8], coverage: Option<&[u8]>) -> [[f64; 256]; 4] {
    if let Some(m) = coverage {
        assert_eq!(m.len(), pixels.len() / 4);
    }
    let mut bins = [[0.0; 256]; 4];
    for (i, p) in pixels.as_chunks::<4>().0.iter().enumerate() {
        if p[3] == 0 {
            continue;
        }
        let weight = p[3] as f64 / 255.0 * coverage.map_or(1.0, |m| m[i] as f64 / 255.0);
        for c in 0..3 {
            let v = (p[c] as f64 * 255.0 / p[3] as f64).round().min(255.0) as usize;
            bins[c + 1][v] += weight;
            bins[0][v] += weight / 3.0;
        }
    }
    bins
}
pub fn cube_apply(pixels: &mut [u8], cube: &[[f32; 4]], n: usize) {
    assert!(n >= 2);
    assert_eq!(cube.len(), n * n * n);
    for p in pixels.as_chunks_mut::<4>().0 {
        let a = p[3] as f32;
        if a == 0.0 {
            continue;
        }
        let mut lo = [0; 3];
        let mut f = [0.0; 3];
        for c in 0..3 {
            let v = (p[c] as f32 * 255.0 / a).min(255.0) * (n - 1) as f32 / 255.0;
            lo[c] = (v as usize).min(n - 2);
            f[c] = v - lo[c] as f32;
        }
        for c in 0..3 {
            let mut v = 0.0;
            for z in 0..2 {
                for y in 0..2 {
                    for x in 0..2 {
                        let w = if x == 0 { 1.0 - f[0] } else { f[0] }
                            * if y == 0 { 1.0 - f[1] } else { f[1] }
                            * if z == 0 { 1.0 - f[2] } else { f[2] };
                        v += cube[lo[0] + x + (lo[1] + y) * n + (lo[2] + z) * n * n][c] * w;
                    }
                }
            }
            p[c] = (v * a).round().clamp(0.0, a) as u8;
        }
    }
}
pub fn hash32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846ca68b);
    x ^ (x >> 16)
}
fn noise_unit(x: u32) -> f32 {
    (hash32(x) >> 8) as f32 / 16_777_216.0
}
#[derive(Clone, Copy, Debug)]
pub struct Noise {
    pub amount: f32,
    pub gaussian: bool,
    pub monochromatic: bool,
    pub seed: u32,
    pub origin: [i64; 2],
}
pub fn add_noise(pixels: &mut [u8], width: usize, settings: Noise) {
    let spread = settings.amount / 100.0 * 127.5;
    for (i, p) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        if p[3] == 0 {
            continue;
        }
        let x = (settings.origin[0] + (i % width) as i64) as u32;
        let y = (settings.origin[1] + (i / width) as i64) as u32;
        let base = hash32(
            settings.seed ^ hash32(x.wrapping_mul(0x9e3779b9) ^ hash32(y.wrapping_mul(0x85ebca6b))),
        );
        for c in 0..3 {
            let key = if settings.monochromatic {
                base
            } else {
                base.wrapping_add((c as u32).wrapping_mul(0x9e3779b9))
            };
            let n = if settings.gaussian {
                (-2.0 * (1.0 - noise_unit(key)).ln()).sqrt()
                    * (std::f32::consts::TAU * noise_unit(key ^ 0x68e31da4)).cos()
                    * spread
                    * (2.0 / 3.0)
            } else {
                (noise_unit(key) * 2.0 - 1.0) * spread
            };
            p[c] = ((p[c] as f32 * 255.0 / p[3] as f32 + n).clamp(0.0, 255.0) * p[3] as f32 / 255.0)
                .round() as u8;
        }
    }
}
pub fn bilinear(
    pixels: &[u8],
    width: usize,
    height: usize,
    x: f64,
    y: f64,
    clamp: bool,
) -> [u8; 4] {
    if width == 0 || height == 0 {
        return [0; 4];
    }
    let (x, y) = if clamp {
        (
            x.clamp(0.0, (width - 1) as f64),
            y.clamp(0.0, (height - 1) as f64),
        )
    } else {
        (x, y)
    };
    let (ix, iy) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let mut out = [0.0; 4];
    for j in 0..2 {
        for i in 0..2 {
            let (sx, sy) = (ix + i, iy + j);
            if sx < 0 || sy < 0 || sx >= width as i64 || sy >= height as i64 {
                continue;
            }
            let w = if i == 0 { 1.0 - fx } else { fx } * if j == 0 { 1.0 - fy } else { fy };
            let p = &pixels[(sy as usize * width + sx as usize) * 4..];
            for c in 0..4 {
                out[c] += p[c] as f64 * w;
            }
        }
    }
    out.map(|v| v.round().clamp(0.0, 255.0) as u8)
}
pub fn lens_distort(pixels: &[u8], width: usize, height: usize, k: f64) -> Vec<u8> {
    assert_eq!(pixels.len(), width * height * 4);
    if k == 0.0 {
        return pixels.to_vec();
    }
    let (cx, cy) = (width as f64 * 0.5, height as f64 * 0.5);
    let diag = cx * cx + cy * cy;
    let mut out = vec![0; pixels.len()];
    for y in 0..height {
        for x in 0..width {
            let (dx, dy) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
            let s = 1.0 - k * (dx * dx + dy * dy) / diag;
            out[(y * width + x) * 4..(y * width + x + 1) * 4].copy_from_slice(&bilinear(
                pixels,
                width,
                height,
                cx + dx * s - 0.5,
                cy + dy * s - 0.5,
                false,
            ));
        }
    }
    out
}
/// Projective warp, with TL/TR/BR/BL target corners in top-down pixel-edge coordinates.
pub fn perspective_warp(
    pixels: &[u8],
    width: usize,
    height: usize,
    corners: [[f64; 2]; 4],
) -> Result<Vec<u8>, String> {
    let source = [
        [0.0, 0.0],
        [width as f64, 0.0],
        [width as f64, height as f64],
        [0.0, height as f64],
    ];
    let mut m = [[0.0; 9]; 8];
    for i in 0..4 {
        let [x, y] = corners[i];
        let [u, v] = source[i];
        m[i * 2] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        m[i * 2 + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    for column in 0..8 {
        let pivot = (column..8)
            .max_by(|&a, &b| m[a][column].abs().total_cmp(&m[b][column].abs()))
            .unwrap();
        m.swap(column, pivot);
        let divisor = m[column][column];
        if divisor.abs() < 1e-10 {
            return Err("Degenerate perspective quadrilateral".into());
        }
        for v in &mut m[column][column..] {
            *v /= divisor;
        }
        let pivot_row = m[column];
        for (r, row) in m.iter_mut().enumerate() {
            if r == column {
                continue;
            }
            let f = row[column];
            for (v, &p) in row[column..].iter_mut().zip(&pivot_row[column..]) {
                *v -= p * f;
            }
        }
    }
    let a = std::array::from_fn::<_, 8, _>(|i| m[i][8]);
    let mut out = vec![0; pixels.len()];
    for y in 0..height {
        for x in 0..width {
            let px = x as f64 + 0.5;
            let py = y as f64 + 0.5;
            let d = a[6] * px + a[7] * py + 1.0;
            if d.abs() < 1e-10 {
                continue;
            }
            let sx = (a[0] * px + a[1] * py + a[2]) / d - 0.5;
            let sy = (a[3] * px + a[4] * py + a[5]) / d - 0.5;
            out[(y * width + x) * 4..(y * width + x + 1) * 4]
                .copy_from_slice(&bilinear(pixels, width, height, sx, sy, false));
        }
    }
    Ok(out)
}
pub fn wand_mask(
    pixels: &[u8],
    width: usize,
    height: usize,
    seed: [usize; 2],
    radius: usize,
    tolerance: u8,
    contiguous: bool,
) -> Vec<u8> {
    assert_eq!(pixels.len(), width * height * 4);
    let mut mask = vec![0; width * height];
    if seed[0] >= width || seed[1] >= height {
        return mask;
    }
    let mut sums = [0u64; 4];
    let mut n = 0u64;
    for y in seed[1].saturating_sub(radius)..=seed[1].saturating_add(radius).min(height - 1) {
        for x in seed[0].saturating_sub(radius)..=seed[0].saturating_add(radius).min(width - 1) {
            for c in 0..4 {
                sums[c] += pixels[(y * width + x) * 4 + c] as u64;
            }
            n += 1;
        }
    }
    let reference = sums.map(|s| ((s + n / 2) / n) as i32);
    let matches = |i: usize| {
        (0..4).all(|c| (pixels[i * 4 + c] as i32 - reference[c]).abs() <= tolerance as i32)
    };
    if !contiguous {
        for (i, v) in mask.iter_mut().enumerate() {
            *v = if matches(i) { 255 } else { 0 };
        }
        return mask;
    }
    let mut stack = vec![(seed[0], seed[1])];
    while let Some((x, y)) = stack.pop() {
        let at = y * width + x;
        if mask[at] != 0 || !matches(at) {
            continue;
        }
        let (mut left, mut right) = (x, x);
        while left > 0 && mask[y * width + left - 1] == 0 && matches(y * width + left - 1) {
            left -= 1;
        }
        while right + 1 < width
            && mask[y * width + right + 1] == 0
            && matches(y * width + right + 1)
        {
            right += 1;
        }
        mask[y * width + left..=y * width + right].fill(255);
        for ny in [y.checked_sub(1), y.checked_add(1).filter(|&n| n < height)]
            .into_iter()
            .flatten()
        {
            let mut in_run = false;
            for nx in left..=right {
                let candidate = mask[ny * width + nx] == 0 && matches(ny * width + nx);
                if candidate && !in_run {
                    stack.push((nx, ny));
                }
                in_run = candidate;
            }
        }
    }
    mask
}
pub fn color_range(
    pixels: &[u8],
    include: &[[u8; 3]],
    exclude: &[[u8; 3]],
    fuzziness: u8,
    invert: bool,
) -> Vec<u8> {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let rgb = std::array::from_fn::<_, 3, _>(|c| {
                if p[3] == 0 {
                    0
                } else {
                    ((p[c] as u32 * 255 + p[3] as u32 / 2) / p[3] as u32).min(255) as u8
                }
            });
            let near = |cs: &[[u8; 3]]| {
                cs.iter()
                    .any(|c| (0..3).all(|i| rgb[i].abs_diff(c[i]) <= fuzziness))
            };
            if (p[3] != 0 && near(include) && !near(exclude)) ^ invert {
                255
            } else {
                0
            }
        })
        .collect()
}
/// Closed, clockwise outer boundaries and counterclockwise holes in top-left coordinates.
pub fn trace_mask(mask: &[u8], width: usize, height: usize) -> Result<Vec<Vec<[i32; 2]>>, String> {
    assert_eq!(mask.len(), width * height);
    let stride = width + 1;
    let mut edges = vec![0u8; stride * (height + 1)];
    let mut n = 0usize;
    for y in 0..height {
        for x in 0..width {
            if mask[y * width + x] == 0 {
                continue;
            }
            if y == 0 || mask[(y - 1) * width + x] == 0 {
                edges[y * stride + x] |= 1;
                n += 1;
            }
            if x + 1 == width || mask[y * width + x + 1] == 0 {
                edges[y * stride + x + 1] |= 2;
                n += 1;
            }
            if y + 1 == height || mask[(y + 1) * width + x] == 0 {
                edges[(y + 1) * stride + x + 1] |= 4;
                n += 1;
            }
            if x == 0 || mask[y * width + x - 1] == 0 {
                edges[(y + 1) * stride + x] |= 8;
                n += 1;
            }
        }
        if n > 8_000_000 {
            return Err("Selection outline exceeds 8 million edges".into());
        }
    }
    let mut loops = Vec::new();
    for start in 0..edges.len() {
        while edges[start] != 0 {
            let (mut v, mut heading, mut initial) = (start, 0u8, 0u8);
            let mut points = Vec::new();
            loop {
                let bits = edges[v];
                let right = if heading == 8 { 1 } else { heading << 1 };
                let left = if heading == 1 { 8 } else { heading >> 1 };
                let d = if heading != 0 && bits & right != 0 {
                    right
                } else if heading != 0 && bits & heading != 0 {
                    heading
                } else if heading != 0 && bits & left != 0 {
                    left
                } else {
                    bits.isolate_lowest_one()
                };
                if d == 0 {
                    break;
                }
                edges[v] &= !d;
                if d != heading {
                    points.push([(v % stride) as i32, (v / stride) as i32]);
                }
                if heading == 0 {
                    initial = d;
                }
                heading = d;
                v = match d {
                    1 => v + 1,
                    2 => v + stride,
                    4 => v - 1,
                    _ => v - stride,
                };
                if v == start {
                    break;
                }
            }
            if heading == initial && !points.is_empty() {
                points.remove(0);
            }
            if !points.is_empty() {
                loops.push(points);
            }
        }
    }
    Ok(loops)
}
pub fn gradient_map(pixels: &mut [u8], table: &[[u8; 3]; 256]) {
    for p in pixels.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        if a == 0 {
            continue;
        }
        let c: [u32; 3] = std::array::from_fn(|i| ((p[i] as u32 * 255 + a / 2) / a).min(255));
        let level = ((2126 * c[0] + 7152 * c[1] + 722 * c[2] + 5000) / 10000).min(255) as usize;
        for i in 0..3 {
            p[i] = ((table[level][i] as u32 * a + 127) / 255) as u8;
        }
    }
}
pub fn black_white(pixels: &mut [u8], weights: [f32; 6], tint: Option<[f32; 2]>) {
    for p in pixels.as_chunks_mut::<4>().0 {
        if p[3] == 0 {
            continue;
        }
        let a = p[3] as f32;
        let [r, g, b] = std::array::from_fn(|i| (p[i] as f32 / a).min(1.0));
        let mx = r.max(g).max(b);
        let mn = r.min(g).min(b);
        let md = r + g + b - mx - mn;
        let (primary, secondary) = if mx == r {
            (0, if g >= b { 1 } else { 5 })
        } else if mx == g {
            (2, if r >= b { 1 } else { 3 })
        } else {
            (4, if g >= r { 3 } else { 5 })
        };
        let gray =
            (mn + (md - mn) * weights[secondary] + (mx - md) * weights[primary]).clamp(0.0, 1.0);
        let c = tint.map_or([gray; 3], |[h, s]| hsl_to_rgb(h, s, gray));
        for i in 0..3 {
            p[i] = (c[i] * a).round().clamp(0.0, a) as u8;
        }
    }
}
pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let v = match hp as u32 {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    v.map(|v| (v + l - c / 2.0).clamp(0.0, 1.0))
}
pub fn rgb_to_hsl(c: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = c;
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    let l = (mx + mn) * 0.5;
    if d < 1e-7 {
        return [0.0, 0.0, l];
    }
    let h = if mx == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [h * 60.0, d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-7), l]
}
pub fn color_balance(
    pixels: &mut [u8],
    shadows: [f32; 3],
    midtones: [f32; 3],
    highlights: [f32; 3],
    preserve: bool,
) {
    for p in pixels.as_chunks_mut::<4>().0 {
        if p[3] == 0 {
            continue;
        }
        let a = p[3] as f32;
        let mut c: [f32; 3] = std::array::from_fn(|i| (p[i] as f32 / a).min(1.0));
        let before = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
        for i in 0..3 {
            let v = c[i];
            let s = ((v - 0.333) / -0.25 + 0.5).clamp(0.0, 1.0) * 0.7;
            let h = ((v + 0.333 - 1.0) / 0.25 + 0.5).clamp(0.0, 1.0) * 0.7;
            let m = ((v - 0.333) / 0.25 + 0.5).clamp(0.0, 1.0)
                * ((v + 0.333 - 1.0) / -0.25 + 0.5).clamp(0.0, 1.0)
                * 0.7;
            c[i] = (v + shadows[i] * s + midtones[i] * m + highlights[i] * h).clamp(0.0, 1.0);
        }
        let after = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
        if preserve && after > 0.0001 {
            for v in &mut c {
                *v = (*v * before / after).clamp(0.0, 1.0);
            }
        }
        for i in 0..3 {
            p[i] = (c[i] * a).round() as u8;
        }
    }
}
fn lattice(x: i64, y: i64, seed: u32) -> f32 {
    let h = hash32(
        (x as u32).wrapping_mul(0x9e3779b1) ^ hash32((y as u32).wrapping_mul(0x85ebca77) ^ seed),
    );
    (h & 65535) as f32 / 65535.0 + (h >> 16) as f32 / 65535.0 - 1.0
}
fn grain_field(u: f64, v: f64, scale: f64, seed: u32) -> f32 {
    let (x, y) = ((u / scale).floor(), (v / scale).floor());
    let (mut tx, mut ty) = ((u / scale - x) as f32, (v / scale - y) as f32);
    tx = tx * tx * (3.0 - 2.0 * tx);
    ty = ty * ty * (3.0 - 2.0 * ty);
    let a = lattice(x as i64, y as i64, seed);
    let b = lattice(x as i64 + 1, y as i64, seed);
    let c = lattice(x as i64, y as i64 + 1, seed);
    let d = lattice(x as i64 + 1, y as i64 + 1, seed);
    ((a + (b - a) * tx) + (c + (d - c) * tx - a - (b - a) * tx) * ty) * 1.6
}
#[derive(Clone, Copy, Debug)]
pub struct Grain {
    pub amount: f64,
    pub size: f64,
    pub roughness: f64,
    pub seed: u32,
    pub origin: [f64; 2],
    pub units_per_pixel: f64,
}
pub fn grain(pixels: &mut [u8], width: usize, s: Grain) {
    if s.amount <= 0.0 || s.units_per_pixel <= 0.0 {
        return;
    }
    let size = s.size.max(0.01);
    let strength = s.amount.min(100.0) as f32 / 100.0 * 0.35 * 255.0;
    let rough = s.roughness.clamp(0.0, 100.0) as f32 / 100.0;
    for (i, p) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        if p[3] == 0 {
            continue;
        }
        let u = s.origin[0] + (i % width) as f64 * s.units_per_pixel + 0.5 * s.units_per_pixel;
        let v = s.origin[1] + (i / width) as f64 * s.units_per_pixel + 0.5 * s.units_per_pixel;
        let smooth = grain_field(u, v, size, s.seed);
        let fine = grain_field(u, v, (size * 0.35).max(0.5), hash32(s.seed ^ 0xa511e9b3));
        let n = smooth + (fine - smooth) * rough;
        let c: [f32; 3] = std::array::from_fn(|i| p[i] as f32 * 255.0 / p[3] as f32);
        let level = ((0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]) / 255.0).min(1.0);
        let d = n * strength * (0.4 + 2.4 * level * (1.0 - level));
        for i in 0..3 {
            p[i] = ((c[i] + d).clamp(0.0, 255.0) * p[3] as f32 / 255.0 + 0.5) as u8;
        }
    }
}
/// Separable Gaussian blur in premultiplied space, with clamped image edges.
pub fn gaussian_blur(pixels: &[u8], width: usize, height: usize, sigma: f64) -> Vec<u8> {
    assert_eq!(pixels.len(), width * height * 4);
    if sigma <= 0.01 || width == 0 || height == 0 {
        return pixels.to_vec();
    }
    // Three box passes approximate a Gaussian while keeping runtime independent of radius.
    let ideal = (4.0 * sigma * sigma + 1.0).sqrt();
    let mut lo = ideal.floor() as usize;
    if lo.is_multiple_of(2) {
        lo = lo.saturating_sub(1);
    }
    lo = lo.max(1);
    let hi = lo + 2;
    let m = ((12.0 * sigma * sigma - 3.0 * (lo * lo) as f64 - 12.0 * lo as f64 - 9.0)
        / (-4.0 * lo as f64 - 4.0))
        .round()
        .clamp(0.0, 3.0) as usize;
    let radii: [usize; 3] = std::array::from_fn(|pass| (if pass < m { lo } else { hi }) / 2);
    let max_radius = radii.into_iter().max().unwrap();
    let mut current = if max_radius == 0 {
        pixels.to_vec()
    } else {
        vec![0; pixels.len()]
    };
    if max_radius != 0 {
        let mut workspace = BlurWorkspace::new(width, height, max_radius);
        let mut passes = radii.into_iter().filter(|&r| r != 0);
        box_blur_into(
            pixels,
            &mut current,
            width,
            height,
            passes.next().unwrap(),
            &mut workspace,
        );
        if let Some(r) = passes.next() {
            let mut next = vec![0; pixels.len()];
            box_blur_into(&current, &mut next, width, height, r, &mut workspace);
            std::mem::swap(&mut current, &mut next);
            for r in passes {
                box_blur_into(&current, &mut next, width, height, r, &mut workspace);
                std::mem::swap(&mut current, &mut next);
            }
        }
    }
    clamp_premultiplied(&mut current);
    current
}
struct BlurWorkspace {
    rows: Vec<f64>,
    sums: Vec<f64>,
    row_slots: usize,
}
impl BlurWorkspace {
    fn new(width: usize, height: usize, max_radius: usize) -> Self {
        // Include the entering row before releasing the outgoing row. Both must remain
        // available for the original `sum += entering - outgoing` operation.
        let row_slots = height.min(max_radius * 2 + 2);
        Self {
            rows: vec![0.0; row_slots * width * 4],
            sums: vec![0.0; width * 4],
            row_slots,
        }
    }
}
fn box_blur_row(pixels: &[u8], out: &mut [f64], r: usize, n: f64) {
    let input = pixels.as_chunks::<4>().0;
    let mut sum = [0.0; 4];
    for i in 0..=r * 2 {
        let p = input[i.saturating_sub(r).min(input.len() - 1)];
        for c in 0..4 {
            sum[c] += p[c] as f64;
        }
    }
    for (x, p) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let entering = input[(x + r + 1).min(input.len() - 1)];
        let outgoing = input[x.saturating_sub(r)];
        for c in 0..4 {
            p[c] = sum[c] / n;
            sum[c] += entering[c] as f64 - outgoing[c] as f64;
        }
    }
}
fn box_blur_into(
    pixels: &[u8],
    out: &mut [u8],
    width: usize,
    height: usize,
    r: usize,
    workspace: &mut BlurWorkspace,
) {
    debug_assert!(r != 0 && width != 0 && height != 0);
    let stride = width * 4;
    let n = (r * 2 + 1) as f64;
    let mut next_source_row = (r + 1).min(height);
    for y in 0..next_source_row {
        box_blur_row(
            &pixels[y * stride..(y + 1) * stride],
            &mut workspace.rows[y * stride..(y + 1) * stride],
            r,
            n,
        );
    }
    workspace.sums.fill(0.0);
    // Interleave columns, but retain each column's exact floating-point addition order.
    for i in 0..=r * 2 {
        let row = i.saturating_sub(r).min(height - 1);
        for (sum, &v) in workspace
            .sums
            .iter_mut()
            .zip(&workspace.rows[row * stride..(row + 1) * stride])
        {
            *sum += v;
        }
    }
    for (y, row) in out.chunks_exact_mut(stride).enumerate() {
        let entering = (y + r + 1).min(height - 1);
        let entering_slot = entering % workspace.row_slots;
        if entering == next_source_row {
            box_blur_row(
                &pixels[entering * stride..(entering + 1) * stride],
                &mut workspace.rows[entering_slot * stride..(entering_slot + 1) * stride],
                r,
                n,
            );
            next_source_row += 1;
        }
        let outgoing_slot = y.saturating_sub(r) % workspace.row_slots;
        let entering_row = &workspace.rows[entering_slot * stride..(entering_slot + 1) * stride];
        let outgoing_row = &workspace.rows[outgoing_slot * stride..(outgoing_slot + 1) * stride];
        for (((pixel, sum), &entering), &outgoing) in row
            .iter_mut()
            .zip(&mut workspace.sums)
            .zip(entering_row)
            .zip(outgoing_row)
        {
            *pixel = (*sum / n).round().clamp(0.0, 255.0) as u8;
            *sum += entering - outgoing;
        }
    }
}
/// Square morphology using monotone queues; image exterior is zero for erosion.
pub fn morphology(
    source: &[u8],
    width: usize,
    height: usize,
    radius: usize,
    erode: bool,
) -> Vec<u8> {
    assert_eq!(source.len(), width * height);
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut temp = vec![0; source.len()];
    let mut out = vec![0; source.len()];
    {
        let (input, output, lines, count, line_step, step) =
            (source, &mut temp[..], height, width, width, 1);
        morph_pass(input, output, lines, count, line_step, step, radius, erode);
    }
    morph_pass(&temp, &mut out, width, height, 1, width, radius, erode);
    out
}
#[allow(clippy::too_many_arguments)]
fn morph_pass(
    input: &[u8],
    output: &mut [u8],
    lines: usize,
    count: usize,
    line_step: usize,
    step: usize,
    r: usize,
    erode: bool,
) {
    for line in 0..lines {
        let base = line * line_step;
        let mut q = VecDeque::new();
        let mut next = 0;
        for center in 0..count {
            while next <= (center + r).min(count - 1) {
                let v = input[base + next * step];
                while let Some(&back) = q.back() {
                    let previous = input[base + back * step];
                    if if erode { previous < v } else { previous > v } {
                        break;
                    }
                    q.pop_back();
                }
                q.push_back(next);
                next += 1;
            }
            while q.front().is_some_and(|&f| f < center.saturating_sub(r)) {
                q.pop_front();
            }
            output[base + center * step] = if erode && (center < r || center + r >= count) {
                0
            } else {
                input[base + q[0] * step]
            };
        }
    }
}
pub fn feather_mask(mask: &[u8], width: usize, height: usize, sigma: f64) -> Vec<u8> {
    let rgba: Vec<u8> = mask.iter().flat_map(|&v| [v, v, v, v]).collect();
    extract_alpha(&gaussian_blur(&rgba, width, height, sigma))
}
/// Continuous soft brush coverage, matching the Metal eight-point quadrature kernel.
pub fn brush_coverage(
    point: [f64; 2],
    a: [f64; 2],
    b: [f64; 2],
    radius: f64,
    hardness: f64,
) -> f64 {
    if radius <= 0.0 {
        return 0.0;
    }
    let tip = |d2: f64| {
        let d = d2.sqrt();
        if hardness >= 1.0 {
            (radius - d + 0.5).clamp(0.0, 1.0)
        } else {
            let t = ((d / radius - hardness) / (1.0 - hardness)).clamp(0.0, 1.0);
            ((-2.5 * t * t).exp() - (-2.5f64).exp()) / (1.0 - (-2.5f64).exp())
        }
    };
    let v = [b[0] - a[0], b[1] - a[1]];
    let len = v[0].hypot(v[1]);
    let p = [point[0] - a[0], point[1] - a[1]];
    if len < 1e-6 {
        return tip(p[0] * p[0] + p[1] * p[1]);
    }
    let projection = (p[0] * v[0] + p[1] * v[1]) / len;
    let perpendicular = (p[0] * p[0] + p[1] * p[1] - projection * projection).max(0.0);
    if hardness >= 1.0 {
        let t = projection.clamp(0.0, len) - projection;
        return tip(perpendicular + t * t);
    }
    if perpendicular >= radius * radius {
        return 0.0;
    }
    let reach = (radius * radius - perpendicular).sqrt();
    let lo = (projection - reach).max(0.0);
    let hi = (projection + reach).min(len);
    if hi <= lo {
        return 0.0;
    }
    let mid = (lo + hi) * 0.5;
    let half = (hi - lo) * 0.5;
    let nodes = [0.1834346425, 0.5255324099, 0.7966664774, 0.9602898565];
    let weights = [0.3626837834, 0.3137066459, 0.2223810345, 0.1012285363];
    let mut integral = 0.0;
    for i in 0..4 {
        for sign in [-1.0, 1.0] {
            let d = mid + sign * half * nodes[i] - projection;
            integral += weights[i] * (-(1.0 - tip(perpendicular + d * d)).max(0.001).ln());
        }
    }
    let density = integral * half / (radius * 0.2).max(0.5);
    1.0 - (-density.min(20.0)).exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn alpha_roundtrip_and_bounds() {
        let mut p = vec![255, 100, 0, 128, 0, 0, 0, 0];
        premultiply(&mut p);
        assert_eq!(p, vec![128, 50, 0, 128, 0, 0, 0, 0]);
        assert_eq!(alpha_bounds(&p, 2, 1), [0, 0, 1, 1]);
        unpremultiply(&mut p);
        assert_eq!(p, vec![255, 100, 0, 128, 0, 0, 0, 0]);
    }
    #[test]
    fn direct_alpha_bounds_keeps_empty_and_last_row_edges() {
        assert_eq!(alpha_bounds(&[], 0, 13), [0; 4]);
        assert_eq!(alpha_bounds(&[255, 0, 0, 0].repeat(17 * 9), 17, 9), [0; 4]);
        let mut pixels = vec![0; 17 * 9 * 4];
        pixels[(2 * 17 + 15) * 4 + 3] = 1;
        pixels[(8 * 17 + 3) * 4 + 3] = 255;
        assert_eq!(alpha_bounds(&pixels, 17, 9), [3, 2, 16, 9]);
    }
    #[test]
    fn lens_identity_and_transparent_exterior() {
        let p = [100, 50, 0, 128].repeat(16);
        assert_eq!(lens_distort(&p, 4, 4, 0.0), p);
        let changed = lens_distort(&p, 4, 4, -1.0);
        assert_eq!(changed[3], 3);
        assert_eq!(changed[4 * 5 + 3], 128);
    }
    #[test]
    fn wand_connectivity_and_reference() {
        let p: Vec<_> = [[0, 0, 0, 255], [255, 0, 0, 255], [0, 0, 0, 255]]
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(wand_mask(&p, 3, 1, [0, 0], 0, 0, true), vec![255, 0, 0]);
        assert_eq!(wand_mask(&p, 3, 1, [0, 0], 0, 0, false), vec![255, 0, 255]);
    }
    #[test]
    fn histogram_weights_alpha_and_selection() {
        let b = histogram(&[128, 0, 0, 128], Some(&[128]));
        let w = (128.0 / 255.0f64).powi(2);
        assert!((b[1][255] - w).abs() < 1e-10);
        assert!((b[0].iter().sum::<f64>() - w).abs() < 1e-10);
    }
    #[test]
    fn seeded_noise_crop_consistency() {
        let mut whole = vec![128; 4 * 4 * 3];
        for p in whole.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
        let mut crop = whole[4 * 4..4 * 8].to_vec();
        let n = Noise {
            amount: 30.0,
            gaussian: true,
            monochromatic: false,
            seed: 123,
            origin: [0, 0],
        };
        add_noise(&mut whole, 4, n);
        add_noise(
            &mut crop,
            4,
            Noise {
                origin: [0, 1],
                ..n
            },
        );
        assert_eq!(crop, whole[16..32]);
    }
    #[test]
    fn trace_diagonal_and_hole() {
        let loops = trace_mask(&[255, 0, 0, 255], 2, 2).unwrap();
        assert_eq!(loops.len(), 2);
        assert!(loops.iter().all(|p| p.len() == 4));
        let loops = trace_mask(&[255, 255, 255, 255, 0, 255, 255, 255, 255], 3, 3).unwrap();
        assert_eq!(loops.len(), 2);
    }
    #[test]
    fn default_black_white_primary() {
        let mut p = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255];
        black_white(&mut p, [0.4, 0.6, 0.4, 0.6, 0.2, 0.8], None);
        assert_eq!(p, [102, 102, 102, 255, 102, 102, 102, 255, 51, 51, 51, 255]);
    }
    #[test]
    fn morphology_square_and_exterior() {
        let mask = [0, 0, 0, 0, 255, 0, 0, 0, 0];
        assert_eq!(morphology(&mask, 3, 3, 1, false), vec![255; 9]);
        assert_eq!(morphology(&[255; 9], 3, 3, 1, true), mask);
    }
    #[test]
    fn blur_keeps_constant_and_premultiplied() {
        let p = [64, 32, 16, 128].repeat(63);
        assert_eq!(gaussian_blur(&p, 9, 7, 10.0), p);
    }

    // The original full-image algorithm is retained only for exact regression checks.
    fn reference_box_blur(pixels: &[u8], width: usize, height: usize, r: usize) -> Vec<u8> {
        if r == 0 {
            return pixels.to_vec();
        }
        let mut temp = vec![0.0f64; pixels.len()];
        let mut out = vec![0; pixels.len()];
        let n = (r * 2 + 1) as f64;
        for y in 0..height {
            for c in 0..4 {
                let mut sum = 0.0;
                for i in 0..=r * 2 {
                    sum += pixels[(y * width + i.saturating_sub(r).min(width - 1)) * 4 + c] as f64;
                }
                for x in 0..width {
                    temp[(y * width + x) * 4 + c] = sum / n;
                    sum += pixels[(y * width + (x + r + 1).min(width - 1)) * 4 + c] as f64
                        - pixels[(y * width + x.saturating_sub(r)) * 4 + c] as f64;
                }
            }
        }
        for x in 0..width {
            for c in 0..4 {
                let mut sum = 0.0;
                for i in 0..=r * 2 {
                    sum += temp[(i.saturating_sub(r).min(height - 1) * width + x) * 4 + c];
                }
                for y in 0..height {
                    out[(y * width + x) * 4 + c] = (sum / n).round().clamp(0.0, 255.0) as u8;
                    sum += temp[((y + r + 1).min(height - 1) * width + x) * 4 + c]
                        - temp[(y.saturating_sub(r) * width + x) * 4 + c];
                }
            }
        }
        out
    }

    fn reference_gaussian_blur(pixels: &[u8], width: usize, height: usize, sigma: f64) -> Vec<u8> {
        if sigma <= 0.01 || width == 0 || height == 0 {
            return pixels.to_vec();
        }
        let ideal = (4.0 * sigma * sigma + 1.0).sqrt();
        let mut lo = ideal.floor() as usize;
        if lo.is_multiple_of(2) {
            lo = lo.saturating_sub(1);
        }
        lo = lo.max(1);
        let hi = lo + 2;
        let m = ((12.0 * sigma * sigma - 3.0 * (lo * lo) as f64 - 12.0 * lo as f64 - 9.0)
            / (-4.0 * lo as f64 - 4.0))
            .round()
            .clamp(0.0, 3.0) as usize;
        let mut current = pixels.to_vec();
        for pass in 0..3 {
            let r = (if pass < m { lo } else { hi }) / 2;
            current = reference_box_blur(&current, width, height, r);
        }
        clamp_premultiplied(&mut current);
        current
    }

    fn blur_test_pixels(width: usize, height: usize) -> Vec<u8> {
        let mut seed = 0x485b_03b7u32;
        let mut out = vec![0; width * height * 4];
        for p in out.as_chunks_mut::<4>().0 {
            seed = hash32(seed.wrapping_add(0x9e37_79b9));
            let bytes = seed.to_le_bytes();
            p[3] = bytes[3];
            for c in 0..3 {
                p[c] = bytes[c].min(p[3]);
            }
        }
        out
    }

    #[test]
    fn streaming_box_blur_matches_reference_across_ring_and_image_edges() {
        for (width, height) in [(1, 1), (1, 67), (71, 1), (2, 3), (17, 129), (65, 37)] {
            let pixels = blur_test_pixels(width, height);
            let mut workspace = BlurWorkspace::new(width, height, 50);
            let mut out = vec![0; pixels.len()];
            for r in [1, 2, 3, 10, 50, 10, 1] {
                // Reuse the buffers in both directions of radius change.
                box_blur_into(&pixels, &mut out, width, height, r, &mut workspace);
                assert_eq!(
                    out,
                    reference_box_blur(&pixels, width, height, r),
                    "{width}x{height}, radius {r}"
                );
            }
            let mut small_workspace = BlurWorkspace::new(width, height, 1);
            box_blur_into(&pixels, &mut out, width, height, 1, &mut small_workspace);
            assert_eq!(out, reference_box_blur(&pixels, width, height, 1));
        }
    }

    #[test]
    fn streaming_gaussian_blur_matches_reference_for_sigma_and_alpha_edges() {
        for (width, height) in [(0, 7), (7, 0), (1, 1), (1, 19), (19, 1), (3, 5), (37, 65)] {
            let pixels = blur_test_pixels(width, height);
            for sigma in [0.0, 0.001, 0.01, 0.02, 0.1, 0.5, 0.7, 1.0, 2.0, 10.0, 50.0] {
                assert_eq!(
                    gaussian_blur(&pixels, width, height, sigma),
                    reference_gaussian_blur(&pixels, width, height, sigma),
                    "{width}x{height}, sigma {sigma}"
                );
            }
        }
        let pixels = [
            255, 17, 64, 0, 140, 64, 33, 127, 2, 0, 0, 1, 255, 64, 17, 255,
        ];
        for sigma in [0.0, 0.02, 0.7, 1.0, 10.0] {
            assert_eq!(
                gaussian_blur(&pixels, 2, 2, sigma),
                reference_gaussian_blur(&pixels, 2, 2, sigma)
            );
        }
        let arbitrary: Vec<u8> = (0..37 * 65 * 4).map(|i| hash32(i as u32) as u8).collect();
        for sigma in [0.0, 0.02, 0.1, 0.7, 1.0, 10.0, 50.0] {
            assert_eq!(
                gaussian_blur(&arbitrary, 37, 65, sigma),
                reference_gaussian_blur(&arbitrary, 37, 65, sigma),
                "Unclamped RGBA, sigma {sigma}"
            );
        }
    }
}
