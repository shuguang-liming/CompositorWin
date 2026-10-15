//! Camera Raw kernels ported from AdjustPixels.c; values use original slider ranges.
use super::{boolean, linear_to_srgb, number, srgb_to_linear, string};
use crate::pixels;
use rayon::prelude::*;
use serde_json::Value;

fn map_colors(p: &mut [u8], f: impl Fn([f64; 3]) -> [f64; 3] + Sync) {
    if p.len() < 65_536 {
        super::map_colors(p, f);
    } else {
        p.par_chunks_mut(16_384)
            .for_each(|chunk| super::map_colors(chunk, &f));
    }
}

struct PreparedCurve {
    points: Vec<[f64; 2]>,
    slopes: Vec<f64>,
}

impl PreparedCurve {
    fn new(points: &Value) -> Self {
        Self::from_points(
            points
                .as_array()
                .map(|points| {
                    points
                        .iter()
                        .map(|point| [number(point, "x", 0.), number(point, "y", 0.)])
                        .collect()
                })
                .unwrap_or_default(),
        )
    }

    fn from_points(points: Vec<[f64; 2]>) -> Self {
        let mut slopes = Vec::new();
        if points.len() >= 2 {
            let delta = |i: usize| {
                (points[i + 1][1] - points[i][1]) / (points[i + 1][0] - points[i][0]).max(1e-10)
            };
            for i in 0..points.len() {
                slopes.push(if i == 0 {
                    delta(0)
                } else if i + 1 == points.len() {
                    delta(i - 1)
                } else {
                    let a = delta(i - 1);
                    let b = delta(i);
                    if a * b <= 0.0 {
                        0.0
                    } else {
                        2.0 / (1.0 / a + 1.0 / b)
                    }
                });
            }
        }
        Self { points, slopes }
    }

    fn value(&self, x: f64) -> f64 {
        if self.points.len() < 2 {
            return x;
        }
        let i = self
            .points
            .iter()
            .rposition(|point| point[0] <= x)
            .unwrap_or(0)
            .min(self.points.len() - 2);
        let [a, b] = [self.points[i], self.points[i + 1]];
        let h = b[0] - a[0];
        if h <= 0.0 {
            return x;
        }
        let t = ((x - a[0]) / h).clamp(0.0, 1.0);
        ((2.0 * t * t * t - 3.0 * t * t + 1.0) * a[1]
            + (t * t * t - 2.0 * t * t + t) * h * self.slopes[i]
            + (-2.0 * t * t * t + 3.0 * t * t) * b[1]
            + (t * t * t - t * t) * h * self.slopes[i + 1])
            .clamp(0.0, 1.0)
    }
}

fn lum(c: [f64; 3]) -> f64 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}
fn read(p: &[u8]) -> [f64; 3] {
    if p[3] == 0 {
        [0.0; 3]
    } else {
        std::array::from_fn(|i| (p[i] as f64 / p[3] as f64).min(1.0))
    }
}
fn write(p: &mut [u8], c: [f64; 3]) {
    let a = p[3] as f64;
    for i in 0..3 {
        p[i] = (c[i] * a).round().clamp(0.0, a) as u8;
    }
}
fn scale_lum(c: &mut [f64; 3], target: f64) {
    let target = target.clamp(0.0, 1.0);
    let y = lum(*c);
    if (target - y).abs() < 1e-8 {
        return;
    }
    if y < 1e-8 {
        if target > y {
            *c = [target; 3];
        }
        return;
    }
    for v in c {
        *v = (*v * target / y).clamp(0.0, 1.0);
    }
}
fn hsl(c: [f64; 3]) -> [f64; 3] {
    let [r, g, b] = c;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    let d = max - min;
    if d < 1e-6 {
        return [0.0, 0.0, l];
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [h / 6.0, s, l]
}
fn rgb(h: f64, s: f64, l: f64) -> [f64; 3] {
    if s <= 1e-6 {
        return [l; 3];
    }
    let h = h.rem_euclid(1.0);
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue = |mut t: f64| {
        t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0)]
}
fn circular(a: f64, b: f64) -> f64 {
    let d = (a - b).abs().rem_euclid(1.0);
    if d > 0.5 { 1.0 - d } else { d }
}
fn blur_plane(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0.0; src.len()];
    let mut out = vec![0.0; src.len()];
    let n = (r * 2 + 1) as f64;
    for y in 0..h {
        let mut sum = 0.0f64;
        for k in 0..=r * 2 {
            sum += src[y * w + k.saturating_sub(r).min(w - 1)] as f64;
        }
        for x in 0..w {
            tmp[y * w + x] = (sum / n) as f32;
            sum += src[y * w + (x + r + 1).min(w - 1)] as f64;
            sum -= src[y * w + x.saturating_sub(r)] as f64;
        }
    }
    for x in 0..w {
        let mut sum = 0.0f64;
        for k in 0..=r * 2 {
            sum += tmp[k.saturating_sub(r).min(h - 1) * w + x] as f64;
        }
        for y in 0..h {
            out[y * w + x] = (sum / n) as f32;
            sum += tmp[(y + r + 1).min(h - 1) * w + x] as f64;
            sum -= tmp[y.saturating_sub(r) * w + x] as f64;
        }
    }
    out
}
fn luma(p: &[u8]) -> Vec<f32> {
    p.as_chunks::<4>()
        .0
        .iter()
        .map(|p| lum(read(p)) as f32)
        .collect()
}
fn radius(base: f64, scale: f64) -> usize {
    (base * scale).round().clamp(1.0, 64.0) as usize
}

pub fn apply(p: &mut [u8], w: usize, h: usize, s: &Value) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Ok(());
    }
    let scale = number(s, "previewScale", 1.0).max(0.01);
    if matches!(number(s, "clipping", 0.) as i32, 1 | 2) {
        basic_grade(p, s);
        return Ok(());
    }
    if boolean(&s["detail"], "showSharpenMask", false) {
        basic_grade(p, s);
        detail(p, w, h, &s["detail"], scale);
        return Ok(());
    }
    geometry(p, w, h, &s["geometry"])?;
    calibration(p, &s["calibration"]);
    basic_grade(p, s);
    curve_color(p, s);
    effects(p, w, h, s, scale);
    pixels::grain(
        p,
        w,
        pixels::Grain {
            amount: number(s, "grainAmount", 0.0),
            size: 0.5 + number(s, "grainSize", 25.0) / 100.0 * 19.5,
            roughness: number(s, "grainRoughness", 50.0),
            seed: number(s, "seed", 0.0) as u32,
            origin: [0.0; 2],
            units_per_pixel: 1.0 / scale,
        },
    );
    optics(p, w, h, &s["optics"]);
    detail(p, w, h, &s["detail"], scale);
    clip_overlay(
        p,
        boolean(s, "showShadowClipping", false),
        boolean(s, "showHighlightClipping", false),
    );
    Ok(())
}
pub fn basic_grade(p: &mut [u8], s: &Value) {
    let warm = number(s, "temperature", 0.0) / 100.0;
    let magenta = number(s, "tint", 0.0) / 100.0;
    let gains = [
        1.0 + 0.35 * warm + 0.15 * magenta,
        1.0 - 0.3 * magenta,
        1.0 - 0.35 * warm + 0.15 * magenta,
    ];
    let exposure = 2f64.powf(number(s, "exposure", 0.0));
    let contrast = 1.0 + number(s, "contrast", 0.0) / 100.0;
    let highlight = number(s, "highlights", 0.0) / 100.0;
    let shadow = number(s, "shadows", 0.0) / 100.0;
    let white = number(s, "whites", 0.0) / 100.0;
    let black = number(s, "blacks", 0.0) / 100.0;
    let vibrance = number(s, "vibrance", 0.0) / 100.0;
    let saturation = number(s, "saturation", 0.0) / 100.0;
    let clipping = number(s, "clipping", 0.0) as i32;
    map_colors(p, |c| {
        let mut c = std::array::from_fn(|i| {
            (0.5 + (linear_to_srgb((srgb_to_linear(c[i]) * gains[i] * exposure).clamp(0.0, 1.0))
                - 0.5)
                * contrast)
                .clamp(0.0, 1.0)
        });
        let y = lum(c);
        let t = ((y - 0.5) / 0.5).clamp(0.0, 1.0);
        let target = y + highlight * t * t * if highlight >= 0.0 { 1.0 - y } else { y - 0.5 };
        scale_lum(&mut c, target);
        let y = lum(c);
        let t = ((0.5 - y) / 0.5).clamp(0.0, 1.0);
        let target = y + shadow * t * t * if shadow >= 0.0 { 0.5 - y } else { y };
        scale_lum(&mut c, target);
        let y = lum(c);
        scale_lum(
            &mut c,
            if y <= 0.75 {
                y
            } else {
                0.75 + (y - 0.75) * (1.0 + white)
            },
        );
        let y = lum(c);
        scale_lum(
            &mut c,
            if y >= 0.25 {
                y
            } else {
                0.25 + (y - 0.25) * (1.0 - black)
            },
        );
        let max = c[0].max(c[1]).max(c[2]);
        let min = c[0].min(c[1]).min(c[2]);
        let chroma = max - min;
        let sat = if max <= 1e-8 { 0.0 } else { chroma / max };
        let hue = hsl(c)[0] * 360.0;
        let skin = if (10.0..=50.0).contains(&hue) {
            (if hue <= 30.0 {
                (hue - 10.0) / 20.0
            } else {
                (50.0 - hue) / 20.0
            }) * ((sat - 0.15) / 0.35).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let amount = vibrance
            * (1.0 - sat)
            * if vibrance > 0.0 {
                1.0 - 0.7 * skin
            } else {
                1.0
            };
        let y = lum(c);
        for v in &mut c {
            *v = (y + (*v - y) * (1.0 + amount)).clamp(0.0, 1.0);
        }
        let y = lum(c);
        for v in &mut c {
            *v = (y + (*v - y) * (1.0 + saturation)).clamp(0.0, 1.0);
        }
        if clipping == 1 {
            c = c.map(|v| if v >= 254.5 / 255.0 { 1.0 } else { 0.0 });
        } else if clipping == 2 {
            c = c.map(|v| if v <= 0.5 / 255.0 { 0.0 } else { 1.0 });
        }
        c
    });
}
fn calibration(p: &mut [u8], s: &Value) {
    let scale = match string(s, "process", "Version 6") {
        "Version 1" => 0.55,
        "Version 2" => 0.65,
        "Version 3" => 0.75,
        "Version 4" => 0.85,
        "Version 5" => 0.92,
        _ => 1.0,
    };
    let tint = number(s, "shadowTint", 0.0) / 100.0 * scale;
    let hue = ["redHue", "greenHue", "blueHue"]
        .map(|key| number(s, key, 0.0) / 100.0 * 15.0 / 360.0 * scale);
    let saturation = ["redSaturation", "greenSaturation", "blueSaturation"]
        .map(|key| number(s, key, 0.0) / 100.0 * 0.45 * scale);
    map_colors(p, |c| {
        let [mut h, mut sat, l] = hsl(c);
        if l < 0.35 {
            h += tint * 0.06;
        }
        let max = c[0].max(c[1]).max(c[2]);
        let min = c[0].min(c[1]).min(c[2]);
        if max - min > 1e-5 {
            let index = if c[0] >= c[1] && c[0] >= c[2] {
                0
            } else if c[1] >= c[0] && c[1] >= c[2] {
                1
            } else {
                2
            };
            h += hue[index];
            sat = (sat * (1.0 + saturation[index])).clamp(0.0, 1.0);
        }
        rgb(h, sat, l)
    });
}
fn curve_color(p: &mut [u8], settings: &Value) {
    let s = &settings["curve"];
    let mixer = &settings["mixer"];
    let grading = &settings["grading"];
    let refine = number(s, "refineSaturation", 0.0) / 100.0;
    let bend = |t: f64, lower: f64, low: f64, upper: f64, high: f64| {
        if t < lower && lower > 0.0 {
            lower * (t / lower).powf(2f64.powf(-low / 100.0 * 1.66))
        } else if t > upper && upper < 1.0 {
            1.0 - (1.0 - upper) * ((1.0 - t) / (1.0 - upper)).powf(2f64.powf(high / 100.0 * 1.66))
        } else {
            t
        }
    };
    let anchors = PreparedCurve::from_points(
        (0..=32)
            .map(|i| {
                let x = i as f64 / 32.0;
                let y = bend(
                    bend(
                        x,
                        number(s, "shadowSplit", 25.0) / 100.0,
                        number(s, "shadows", 0.0),
                        number(s, "lightSplit", 75.0) / 100.0,
                        number(s, "highlights", 0.0),
                    ),
                    number(s, "darkSplit", 50.0) / 100.0,
                    number(s, "darks", 0.0),
                    number(s, "darkSplit", 50.0) / 100.0,
                    number(s, "lights", 0.0),
                );
                [x, y]
            })
            .collect(),
    );
    let master = PreparedCurve::new(&s["rgb"]);
    let channels = ["red", "green", "blue"].map(|key| PreparedCurve::new(&s[key]));
    let tone = |v: f64| master.value(anchors.value(v));
    let centers = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0];
    let bands: [[f64; 3]; 8] = std::array::from_fn(|i| {
        ["hue", "saturation", "luminance"].map(|key| mixer[key][i].as_f64().unwrap_or(0.0) / 100.0)
    });
    let split = 0.5 - number(grading, "balance", 0.0) / 100.0 * 0.2;
    let reach = 0.12 + number(grading, "blending", 50.0) / 100.0 * 0.38;
    let wheels = ["shadows", "midtones", "highlights", "global"].map(|key| {
        let wheel = &grading[key];
        (
            number(wheel, "saturation", 0.0) / 100.0,
            number(wheel, "luminance", 0.0) / 100.0,
            rgb(number(wheel, "hue", 0.0) / 360.0, 1.0, 0.5),
        )
    });
    map_colors(p, |c| {
        let mut curved = c.map(tone);
        if refine < 0.0 {
            let mut brightness = c;
            scale_lum(&mut brightness, tone(lum(c)));
            for i in 0..3 {
                curved[i] += (brightness[i] - curved[i]) * (-refine);
            }
        } else if refine > 0.0 {
            let y = lum(curved);
            curved = curved.map(|v| (y + (v - y) * (1.0 + refine)).clamp(0.0, 1.0));
        }
        let mut c = std::array::from_fn(|i| channels[i].value(curved[i]));
        let [mut h, mut sat, mut l] = hsl(c);
        let original = [h, sat, l];
        let mut delta = [0.0; 3];
        let mut weight = 0.0;
        for (i, center) in centers.iter().enumerate() {
            let w = 1.0 - circular(h, center / 360.0) / (40.0 / 360.0);
            if w <= 0.0 {
                continue;
            }
            delta[0] += bands[i][0] * w * 30.0 / 360.0;
            delta[1] += bands[i][1] * w;
            delta[2] += bands[i][2] * w * 0.25;
            weight += w;
        }
        if weight > 1.0 {
            for d in &mut delta {
                *d /= weight;
            }
        }
        h = (h + delta[0]).rem_euclid(1.0);
        sat = (sat * (1.0 + delta[1])).clamp(0.0, 1.0);
        l = (l + delta[2]).clamp(0.0, 1.0);
        let point_weight = |h: f64, sat: f64, l: f64, point: &Value| {
            let hue = 1.0
                - circular(h, number(point, "hue", 0.0) / 360.0)
                    / (number(point, "hueRange", 30.0) / 360.0).max(0.01);
            let sat = 1.0
                - (sat - number(point, "saturation", 0.0)).abs()
                    / number(point, "saturationRange", 0.4).max(0.01);
            let l = 1.0
                - (l - number(point, "luminance", 0.0)).abs()
                    / number(point, "luminanceRange", 0.4).max(0.01);
            if hue < 0.0 || sat < 0.0 || l < 0.0 {
                0.0
            } else {
                hue * sat * l
            }
        };
        if let Some(points) = mixer["points"].as_array() {
            for point in points {
                let w = point_weight(h, sat, l, point);
                h += number(point, "hueShift", 0.0) / 100.0 * w * 30.0 / 360.0;
                sat = (sat * (1.0 + number(point, "saturationShift", 0.0) / 100.0 * w))
                    .clamp(0.0, 1.0);
                l = (l + number(point, "luminanceShift", 0.0) / 100.0 * w * 0.25).clamp(0.0, 1.0);
            }
        }
        c = rgb(h, sat, l);
        let y = lum(c);
        let mut weights = [
            ((split + reach - y) / (reach * 2.0).max(0.05)).clamp(0.0, 1.0),
            (1.0 - (y - split).abs() / (0.35 + reach)).clamp(0.0, 1.0),
            ((y - (split - reach)) / (reach * 2.0).max(0.05)).clamp(0.0, 1.0),
            1.0,
        ];
        let sum = weights[..3].iter().sum::<f64>();
        if sum > 1e-4 {
            for w in &mut weights[..3] {
                *w /= sum;
            }
        }
        for (index, &(ws, wl, tint)) in wheels.iter().enumerate() {
            for i in 0..3 {
                c[i] = (c[i] + (tint[i] - 0.5) * ws * weights[index] * 0.85).clamp(0.0, 1.0);
            }
            let y = lum(c);
            scale_lum(&mut c, y + wl * 0.25 * weights[index]);
        }
        if let Some(points) = mixer["points"].as_array()
            && let Some(point) = points.iter().find(|p| boolean(p, "visualize", false))
            && point_weight(original[0], original[1], original[2], point) <= 0.05
        {
            c = c.map(|v| v * 0.35);
        }
        c
    });
}
fn vignette_mask(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    midpoint: f64,
    roundness: f64,
    feather: f64,
) -> f64 {
    let nx = x / w * 2.0 - 1.0;
    let ny = y / h * 2.0 - 1.0;
    let square = nx.abs().max(ny.abs());
    let circle = nx.hypot(ny) / 2f64.sqrt();
    let dist = circle + (square - circle) * (1.0 - roundness / 100.0) * 0.5;
    let t = ((dist - midpoint / 100.0 * 0.85) / (feather / 100.0).max(0.05)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
#[allow(clippy::too_many_arguments)]
pub fn colored_vignette(
    p: &mut [u8],
    w: usize,
    h: usize,
    amount: f64,
    midpoint: f64,
    roundness: f64,
    feather: f64,
    highlights: f64,
    color: [f64; 3],
) {
    for (i, p) in p.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        if p[3] == 0 {
            continue;
        }
        let mut c = read(p);
        let bright = ((lum(c) - 0.45) / 0.55).clamp(0.0, 1.0);
        let effect = (amount / 100.0).clamp(0.0, 1.0)
            * vignette_mask(
                (i % w) as f64 + 0.5,
                (i / w) as f64 + 0.5,
                w as f64,
                h as f64,
                midpoint,
                roundness,
                feather,
            )
            * (1.0 - highlights / 100.0 * bright);
        for j in 0..3 {
            c[j] += (color[j] - c[j]) * effect;
        }
        write(p, c);
    }
}
fn effects(p: &mut [u8], w: usize, h: usize, s: &Value, scale: f64) {
    let texture = number(s, "texture", 0.0);
    let clarity = number(s, "clarity", 0.0);
    let dehaze = number(s, "dehaze", 0.0);
    let glow = number(s, "glow", 0.0);
    let vignette = number(s, "vignetteAmount", 0.0);
    if texture == 0.0 && clarity == 0.0 && dehaze == 0.0 && glow <= 0.0 && vignette == 0.0 {
        return;
    }
    let luma = luma(p);
    let fine = if texture != 0.0 {
        Some(blur_plane(&luma, w, h, radius(1.0, scale)))
    } else {
        None
    };
    let coarse = if clarity != 0.0 {
        Some(blur_plane(&luma, w, h, radius(4.0, scale)))
    } else {
        None
    };
    let style = string(s, "glowStyle", "Diffusion");
    let glowplane = if glow > 0.0 {
        let threshold = (0.55 + 0.4 * number(s, "glowRange", 0.0) / 100.0) as f32;
        let source: Vec<_> = luma
            .iter()
            .map(|&v| ((v - threshold) / (1.0 - threshold).max(0.05)).clamp(0.0, 1.0))
            .collect();
        Some(blur_plane(
            &source,
            w,
            h,
            radius(
                (if style == "Bloom" { 2.0 } else { 5.0 })
                    * (1.0 + number(s, "glowSpread", 0.0) / 100.0),
                scale,
            ),
        ))
    } else {
        None
    };
    let warmth = number(s, "glowWarmth", 0.0) / 100.0;
    let (glowcolor, gain) = if style == "Halation" {
        ([1.0, 0.35 - 0.3 * warmth, 0.2 - 0.2 * warmth], 1.0)
    } else {
        (
            [
                0.75 + 0.25 * warmth,
                0.6 + 0.2 * warmth,
                0.75 - 0.6 * warmth,
            ],
            if style == "Bloom" { 1.4 } else { 1.0 },
        )
    };
    for (i, p) in p.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        if p[3] == 0 {
            continue;
        }
        let mut c = read(p);
        let y = lum(c);
        let mut detail = 0.0;
        if let Some(f) = &fine {
            detail += texture / 100.0 * (y - f[i] as f64);
        }
        if let Some(f) = &coarse {
            detail += clarity / 100.0 * (y - f[i] as f64);
        }
        scale_lum(&mut c, y + detail);
        if dehaze != 0.0 {
            let d = dehaze / 100.0;
            let y = lum(c);
            let pivot = 0.45 - 0.1 * d.max(0.0);
            let mut target = (pivot + (y - 0.45) * (1.0 + 0.8 * d)).clamp(0.0, 1.0);
            target = if d < 0.0 {
                (target + (-d) * (1.0 - target) * 0.45).clamp(0.0, 1.0)
            } else {
                (target - d * (0.4 - target).max(0.0)).clamp(0.0, 1.0)
            };
            scale_lum(&mut c, target);
            let y = lum(c);
            c = c.map(|v| (y + (v - y) * (1.0 + 0.7 * d)).clamp(0.0, 1.0));
        }
        if let Some(g) = &glowplane {
            let add = g[i] as f64 * glow / 100.0 * gain;
            for j in 0..3 {
                c[j] = (c[j] + add * glowcolor[j]).clamp(0.0, 1.0);
            }
        }
        if vignette != 0.0 {
            let mask = vignette_mask(
                (i % w) as f64 + 0.5,
                (i / w) as f64 + 0.5,
                w as f64,
                h as f64,
                number(s, "vignetteMidpoint", 50.0),
                number(s, "vignetteRoundness", 0.0),
                number(s, "vignetteFeather", 50.0),
            );
            let mut effect = vignette / 100.0 * mask;
            let style = string(s, "vignetteStyle", "Highlight Priority");
            if effect < 0.0 && style == "Highlight Priority" {
                effect *= 1.0
                    - number(s, "vignetteHighlights", 0.0) / 100.0
                        * ((lum(c) - 0.45) / 0.55).clamp(0.0, 1.0);
            }
            c = c.map(|v| {
                if effect < 0.0 {
                    v * (1.0 + effect)
                } else {
                    v + (1.0 - v) * effect
                }
            });
            if style == "Color Priority" {
                let y = lum(c);
                let sat = 1.0 - 0.75 * mask * (vignette / 100.0).abs();
                c = c.map(|v| (y + (v - y) * sat).clamp(0.0, 1.0));
            }
        }
        write(p, c);
    }
}
fn edge(luma: &[f32], w: usize, h: usize, x: usize, y: usize, r: usize) -> f32 {
    let mut sum = 0.0;
    let mut n = 0;
    for dy in [-1i64, 0, 1] {
        for dx in [-1i64, 0, 1] {
            if dx == 0 && dy == 0 {
                continue;
            }
            let sx = x as i64 + dx * r as i64;
            let sy = y as i64 + dy * r as i64;
            if sx >= 0 && sy >= 0 && sx < w as i64 && sy < h as i64 {
                sum += (luma[sy as usize * w + sx as usize] - luma[y * w + x]).abs();
                n += 1;
            }
        }
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
}
fn detail(p: &mut [u8], w: usize, h: usize, s: &Value, scale: f64) {
    let sharpen = number(s, "sharpenAmount", 0.0);
    let noise_lum = number(s, "noiseLuminance", 0.0);
    let noise_color = number(s, "noiseColor", 0.0);
    let mask_view = boolean(s, "showSharpenMask", false);
    if sharpen == 0.0 && noise_lum == 0.0 && noise_color == 0.0 && !mask_view {
        return;
    }
    let mut luma = luma(p);
    if noise_lum > 0.0 && !mask_view {
        let work = blur_plane(&luma, w, h, radius(1.0 + noise_lum / 50.0, scale));
        let strength = noise_lum / 100.0;
        let preserve = number(s, "noiseLuminanceDetail", 50.0) / 100.0;
        let contrast = number(s, "noiseLuminanceContrast", 0.0) / 100.0;
        for (i, p) in p.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            if p[3] == 0 {
                continue;
            }
            let edge = edge(&luma, w, h, i % w, i / w, 1);
            let local = strength * (1.0 - preserve * (edge as f64 * 6.0).min(1.0));
            let target = (luma[i] as f64 * (1.0 - local)
                + work[i] as f64 * local
                + contrast * 0.25 * (luma[i] - work[i]) as f64) as f32;
            luma[i] = target;
            let mut c = read(p);
            scale_lum(&mut c, target as f64);
            write(p, c);
        }
    }
    if noise_color > 0.0 && !mask_view {
        let chroma: Vec<_> = p
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| hsl(read(p))[1] as f32)
            .collect();
        let work = blur_plane(
            &chroma,
            w,
            h,
            radius(1.0 + number(s, "noiseColorSmoothness", 50.0) / 40.0, scale),
        );
        let strength = noise_color / 100.0;
        let preserve = number(s, "noiseColorDetail", 50.0) / 100.0;
        for (i, p) in p.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            if p[3] == 0 {
                continue;
            }
            let edge = (chroma[i] - work[i]).abs();
            let local = strength * (1.0 - preserve * (edge as f64 * 4.0).min(1.0));
            let sat = chroma[i] * (1.0 - local) as f32 + work[i] * local as f32;
            let [h, _, l] = hsl(read(p));
            write(p, rgb(h, sat as f64, l));
        }
    }
    if sharpen > 0.0 || mask_view {
        luma = crate::filters::camera_raw::luma(p);
        let r = radius(
            (0.5 + number(s, "sharpenRadius", 10.0) / 100.0 * 2.5) * scale,
            1.0,
        );
        let work = blur_plane(&luma, w, h, r);
        let amount = sharpen / 100.0;
        let mix = number(s, "sharpenDetail", 25.0) / 100.0;
        let threshold = number(s, "sharpenMasking", 0.0) / 100.0 * 0.35;
        for (i, p) in p.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            if p[3] == 0 {
                continue;
            }
            let edge = edge(&luma, w, h, i % w, i / w, r) as f64;
            let mask = ((edge * (0.5 + mix) - threshold) / (0.35 - threshold * 0.5).max(0.04))
                .clamp(0.0, 1.0);
            if mask_view {
                write(p, [mask; 3]);
            } else {
                let target =
                    luma[i] as f64 + (luma[i] - work[i]) as f64 * amount * mask * (0.5 + mix);
                let mut c = read(p);
                scale_lum(&mut c, target);
                write(p, c);
            }
        }
    }
}
fn optics(p: &mut [u8], w: usize, h: usize, s: &Value) {
    let profile = boolean(s, "enableLensProfile", false);
    let distortion = number(s, "distortion", 0.0)
        + if profile {
            number(s, "profileDistortion", 100.0) * 0.12
        } else {
            0.0
        };
    if distortion != 0.0 {
        let out = pixels::lens_distort(p, w, h, distortion / 100.0 * 0.35);
        p.copy_from_slice(&out);
    }
    if boolean(s, "removeChromaticAberration", false) {
        let copy = p.to_vec();
        let cx = w as f64 * 0.5;
        let cy = h as f64 * 0.5;
        let maxr = cx.hypot(cy);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let q = &mut p[i..i + 4];
                if q[3] == 0 {
                    continue;
                }
                let radial = (x as f64 + 0.5 - cx).hypot(y as f64 + 0.5 - cy) / maxr;
                let shift = 0.45 * radial * radial * 2.5;
                let rx = (x as f64 - shift).round().clamp(0.0, (w - 1) as f64) as usize;
                let bx = (x as f64 + shift).round().clamp(0.0, (w - 1) as f64) as usize;
                let r = read(&copy[(y * w + rx) * 4..])[0];
                let b = read(&copy[(y * w + bx) * 4..])[2];
                let g = read(&copy[i..])[1];
                write(q, [r, g, b]);
            }
        }
    }
    let vignette = number(s, "vignetteAmount", 0.0)
        + if profile {
            number(s, "profileVignetting", 100.0) / 100.0 * 35.0
        } else {
            0.0
        };
    for (i, p) in p.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        if p[3] == 0 {
            continue;
        }
        let mut c = read(p);
        let hue = hsl(c)[0] * 360.0;
        let max = c[0].max(c[1]).max(c[2]);
        let min = c[0].min(c[1]).min(c[2]);
        let sat = if max <= 0.0 { 0.0 } else { (max - min) / max };
        let mut reduce = 0.0f64;
        for (prefix, lo, hi) in [("purple", 270.0, 310.0), ("green", 60.0, 120.0)] {
            let low = number(s, &format!("{prefix}HueLow"), lo);
            let high = number(s, &format!("{prefix}HueHigh"), hi);
            if if low <= high {
                hue >= low && hue <= high
            } else {
                hue >= low || hue <= high
            } {
                reduce = reduce.max(number(s, &format!("{prefix}Amount"), 0.0) / 100.0);
            }
        }
        let y = lum(c);
        c = c.map(|v| (y + (v - y) * (1.0 - reduce * sat)).clamp(0.0, 1.0));
        let nx = ((i % w) as f64 + 0.5) / w as f64 * 2.0 - 1.0;
        let ny = ((i / w) as f64 + 0.5) / h as f64 * 2.0 - 1.0;
        let t = ((nx.hypot(ny) / 2f64.sqrt() - number(s, "vignetteMidpoint", 50.0) / 100.0 * 0.85)
            / 0.35)
            .clamp(0.0, 1.0);
        let lift = vignette / 100.0 * t * t * (3.0 - 2.0 * t);
        c = c.map(|v| {
            if lift > 0.0 {
                (v + (1.0 - v) * lift).clamp(0.0, 1.0)
            } else {
                v * (1.0 + lift)
            }
        });
        write(p, c);
    }
}
fn clip_overlay(p: &mut [u8], shadows: bool, highlights: bool) {
    if !shadows && !highlights {
        return;
    }
    map_colors(p, |mut c| {
        if shadows && c.iter().any(|&v| v <= 0.5 / 255.0) {
            c = [c[0] * 0.35, c[1] * 0.35, c[2] * 0.35 + 0.65];
        }
        if highlights && c.iter().any(|&v| v >= 254.5 / 255.0) {
            c = [c[0] * 0.35 + 0.65, c[1] * 0.35, c[2] * 0.35];
        }
        c
    });
}
fn geometry(p: &mut [u8], w: usize, h: usize, s: &Value) -> Result<(), String> {
    let mut vertical = number(s, "vertical", 0.0);
    let mut horizontal = number(s, "horizontal", 0.0);
    let mut rotation = number(s, "rotate", 0.0);
    if string(s, "upright", "Off") == "Guided"
        && let Some(guides) = s["guides"].as_array()
        && let Some(first) = guides.first()
    {
        let dx = number(first, "endX", 0.0) - number(first, "startX", 0.0);
        let dy = number(first, "endY", 0.0) - number(first, "startY", 0.0);
        if dx.hypot(dy) > 1e-4 {
            let mut angle = -dy.atan2(dx).to_degrees();
            if angle > 45.0 {
                angle -= 90.0;
            } else if angle < -45.0 {
                angle += 90.0;
            }
            rotation += angle;
            if let Some(second) = guides.get(1) {
                let dx = number(second, "endX", 0.0) - number(second, "startX", 0.0);
                let dy = number(second, "endY", 0.0) - number(second, "startY", 0.0);
                if dx.hypot(dy) > 1e-4 {
                    let angle = dy.atan2(dx).to_degrees();
                    if angle.abs() > 45.0 {
                        vertical += if angle > 0.0 { 25.0 } else { -25.0 };
                    } else {
                        horizontal += if angle > 0.0 { 25.0 } else { -25.0 };
                    }
                }
            }
        }
    }
    let aspect = 1.0 + number(s, "aspect", 0.0) / 200.0;
    let zoom = 1.0 + number(s, "scale", 0.0) / 100.0;
    let ox = number(s, "offsetX", 0.0) / 100.0 * w as f64 * 0.15;
    let oy = number(s, "offsetY", 0.0) / 100.0 * h as f64 * 0.15;
    if vertical == 0.0
        && horizontal == 0.0
        && rotation == 0.0
        && aspect == 1.0
        && zoom == 1.0
        && ox == 0.0
        && oy == 0.0
    {
        return Ok(());
    }
    let strength = if string(s, "projection", "Perspective") == "Perspective" {
        1.0
    } else {
        0.55
    };
    let v = vertical / 100.0 * w as f64 * 0.18 * strength;
    let hz = horizontal / 100.0 * h as f64 * 0.18 * strength;
    let center = [w as f64 * 0.5 + ox, h as f64 * 0.5 + oy];
    let angle = (-rotation).to_radians();
    let corners = [
        [-v + ox, -oy],
        [w as f64 + v + ox, -oy],
        [w as f64 + hz + ox, h as f64 + oy],
        [-hz + ox, h as f64 + oy],
    ]
    .map(|q| {
        let dx = q[0] - center[0];
        let dy = q[1] - center[1];
        [
            center[0] + (dx * angle.cos() - dy * angle.sin()) * aspect * zoom,
            center[1] + (dx * angle.sin() + dy * angle.cos()) / aspect * zoom,
        ]
    });
    let out = pixels::perspective_warp(p, w, h, corners)?;
    p.copy_from_slice(&out);
    if boolean(s, "constrainCrop", false) {
        let [x0, y0, x1, y1] = pixels::alpha_bounds(p, w, h);
        if x1 > x0 && y1 > y0 && (x1 - x0 < w || y1 - y0 < h) {
            let scale = (w as f64 / (x1 - x0) as f64).min(h as f64 / (y1 - y0) as f64);
            let dw = (x1 - x0) as f64 * scale;
            let dh = (y1 - y0) as f64 * scale;
            let dx = (w as f64 - dw) * 0.5;
            let dy = (h as f64 - dh) * 0.5;
            let source = p.to_vec();
            for y in 0..h {
                for x in 0..w {
                    let sx = (x as f64 + 0.5 - dx) / scale - 0.5 + x0 as f64;
                    let sy = (y as f64 + 0.5 - dy) / scale - 0.5 + y0 as f64;
                    p[(y * w + x) * 4..(y * w + x + 1) * 4]
                        .copy_from_slice(&pixels::bilinear(&source, w, h, sx, sy, false));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn prepared_curves_match_original_interpolation_bit_for_bit() {
        for points in [
            Value::Null,
            json!([{"x":0.,"y":0.},{"x":1.,"y":1.}]),
            json!([{"x":0.,"y":0.1},{"x":0.2,"y":0.6},{"x":0.7,"y":0.3},{"x":1.,"y":0.9}]),
            json!([{"x":0.,"y":0.},{"x":0.2,"y":0.1},{"x":0.2,"y":0.6},{"x":1.,"y":1.}]),
        ] {
            let prepared = PreparedCurve::new(&points);
            for i in -100..=1100 {
                let x = i as f64 / 1000.;
                assert_eq!(
                    prepared.value(x).to_bits(),
                    super::super::curve(x, &points, 1.).to_bits(),
                    "x={x} points={points}"
                );
            }
        }
    }

    #[test]
    fn parallel_color_stages_match_serial_pixels_and_alpha() {
        let source: Vec<_> = (0..20_000)
            .flat_map(|i| {
                let alpha = (i % 256) as u8;
                [
                    ((i * 17) % (usize::from(alpha) + 1)) as u8,
                    ((i * 31) % (usize::from(alpha) + 1)) as u8,
                    ((i * 47) % (usize::from(alpha) + 1)) as u8,
                    alpha,
                ]
            })
            .collect();
        let transform = |color: [f64; 3]| {
            rgb(
                hsl(color)[0] + 0.1,
                hsl(color)[1] * 0.7,
                hsl(color)[2] * 0.8,
            )
        };
        let mut expected = source.clone();
        super::super::map_colors(&mut expected, transform);
        let mut actual = source;
        map_colors(&mut actual, transform);
        assert_eq!(actual, expected);
    }

    #[test]
    fn basic_grade_identity() {
        let mut p = [100, 50, 25, 128, 255, 0, 100, 255];
        let original = p;
        basic_grade(&mut p, &Value::Null);
        assert_eq!(p, original);
    }
    #[test]
    fn temporary_clipping_uses_only_basic_grade_and_preserves_alpha() {
        let source = vec![
            255, 0, 40, 255, 80, 30, 60, 128, 0, 0, 0, 0, 100, 120, 140, 255,
        ];
        for clipping in [1, 2] {
            let settings = json!({
                "clipping":clipping,"exposure":0.5,"contrast":20,
                "curve":{"lights":80},"grading":{"global":{"hue":20,"saturation":90}},
                "clarity":90,"glow":90,"grainAmount":80,
                "geometry":{"rotate":23},"calibration":{"redHue":70},
                "detail":{"sharpenAmount":120,"showSharpenMask":true},
                "optics":{"distortion":80,"vignetteAmount":100},
                "showShadowClipping":true,"showHighlightClipping":true
            });
            let mut expected = source.clone();
            basic_grade(&mut expected, &settings);
            let mut actual = source.clone();
            apply(&mut actual, 2, 2, &settings).unwrap();
            assert_eq!(actual, expected);
            for (before, after) in source
                .as_chunks::<4>()
                .0
                .iter()
                .zip(actual.as_chunks::<4>().0)
            {
                assert_eq!(before[3], after[3]);
                assert!(
                    after[..3]
                        .iter()
                        .all(|&channel| channel == 0 || channel == after[3])
                );
            }
        }
    }
    #[test]
    fn sharpening_mask_ignores_other_stages_and_noise_reduction() {
        let source: Vec<_> = (0..64)
            .flat_map(|i| {
                [
                    (i * 47 % 256) as u8,
                    (i * 19 % 256) as u8,
                    (i * 83 % 256) as u8,
                    255,
                ]
            })
            .collect();
        let settings = json!({
            "exposure":0.25,"curve":{"lights":80},"clarity":90,"grainAmount":80,
            "geometry":{"rotate":23},"calibration":{"redHue":70},
            "detail":{"showSharpenMask":true,"sharpenRadius":40,"sharpenDetail":75,
                      "sharpenMasking":40,"noiseLuminance":100,"noiseColor":100},
            "optics":{"distortion":80,"vignetteAmount":100},
            "showShadowClipping":true,"showHighlightClipping":true
        });
        let mut expected = source.clone();
        basic_grade(&mut expected, &settings);
        let mut mask_settings = settings["detail"].clone();
        mask_settings["noiseLuminance"] = json!(0);
        mask_settings["noiseColor"] = json!(0);
        detail(&mut expected, 8, 8, &mask_settings, 1.);
        let mut actual = source;
        apply(&mut actual, 8, 8, &settings).unwrap();
        assert_eq!(actual, expected);
        assert!(
            actual
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[0] == p[1] && p[1] == p[2] && p[3] == 255)
        );
    }
    #[test]
    fn combined_optics_precedes_detail_as_in_original() {
        let source: Vec<_> = (0..64)
            .flat_map(|i| {
                [
                    (i * 47 % 256) as u8,
                    (i * 19 % 256) as u8,
                    (i * 83 % 256) as u8,
                    255,
                ]
            })
            .collect();
        let settings = json!({"detail":{"sharpenAmount":110,"noiseColor":30},"optics":{"distortion":55,"purpleAmount":60}});
        let mut expected = source.clone();
        optics(&mut expected, 8, 8, &settings["optics"]);
        detail(&mut expected, 8, 8, &settings["detail"], 1.);
        let mut reversed = source.clone();
        detail(&mut reversed, 8, 8, &settings["detail"], 1.);
        optics(&mut reversed, 8, 8, &settings["optics"]);
        assert_ne!(expected, reversed);
        let mut actual = source;
        apply(&mut actual, 8, 8, &settings).unwrap();
        assert_eq!(actual, expected);
    }
    #[test]
    fn raw_all_groups_change_real_pixels() {
        let base: Vec<_> = (0..64).flat_map(|i| [i * 3, i * 2, i, 255]).collect();
        for s in [
            json!({"calibration":{"redHue":100}}),
            json!({"contrast":80}),
            json!({"curve":{"lights":75}}),
            json!({"mixer":{"hue":[100,0,0,0,0,0,0,0]}}),
            json!({"grading":{"global":{"hue":200,"saturation":50}}}),
            json!({"clarity":70}),
            json!({"detail":{"sharpenAmount":100}}),
            json!({"optics":{"vignetteAmount":70}}),
            json!({"geometry":{"rotate":20}}),
        ] {
            let mut p = base.clone();
            apply(&mut p, 8, 8, &s).unwrap();
            assert_ne!(p, base, "{s}");
        }
    }
}
