//! Camera Raw 1.5.3 measured color tables and adaptive shadows.
//! Ported from Compositor, Copyright (c) 2026 Wonder Assembly LLC (MIT).
//! The original license and attribution are retained in LICENSES.md.
use super::{PreparedCurve, linear_to_srgb, number, srgb_to_linear};
use flate2::read::ZlibDecoder;
use rayon::prelude::*;
use serde_json::Value;
use std::{
    io::Read,
    sync::{Arc, Mutex, OnceLock},
};

const SIZE: usize = 17;
pub(super) const GRID: usize = 33;
const CELLS: usize = SIZE * SIZE * SIZE;
const ENTRIES: usize = CELLS * 3;
const TABLE_COUNT: usize = 763;
const OUTPUT_ENTRIES: usize = GRID * GRID * GRID * 3;

fn tables() -> &'static [u8] {
    static TABLES: OnceLock<Vec<u8>> = OnceLock::new();
    TABLES.get_or_init(|| {
        // Original CRT3 delta planes, losslessly repacked from LZMA to zlib at
        // development time so the application can reuse its existing decoder.
        let source = include_bytes!("../assets/camera_raw_tables.bin");
        assert_eq!(&source[..4], b"CRZ3");
        let field = |at| u16::from_le_bytes([source[at], source[at + 1]]) as usize;
        assert_eq!((field(4), field(6)), (SIZE, TABLE_COUNT));
        let step = field(8) as i32;
        let mut planes = Vec::with_capacity(TABLE_COUNT * ENTRIES);
        ZlibDecoder::new(&source[10..])
            .read_to_end(&mut planes)
            .expect("embedded Camera Raw tables");
        assert_eq!(planes.len(), TABLE_COUNT * ENTRIES);
        let levels: [i32; SIZE] =
            std::array::from_fn(|i| (i as f64 * 255. / (SIZE - 1) as f64).round() as i32);
        let mut tables = vec![0; planes.len()];
        tables
            .par_chunks_mut(ENTRIES)
            .enumerate()
            .for_each(|(table, out)| {
                for channel in 0..3 {
                    let base = (table * 3 + channel) * CELLS;
                    let mut running = 0u8;
                    for cell in 0..CELLS {
                        running = running.wrapping_add(planes[base + cell]);
                        let axis = match channel {
                            0 => cell / (SIZE * SIZE),
                            1 => (cell / SIZE) % SIZE,
                            _ => cell % SIZE,
                        };
                        out[cell * 3 + channel] =
                            (levels[axis] + running as i8 as i32 * step).clamp(0, 255) as u8;
                    }
                }
            });
        tables
    })
}

#[derive(Clone, Copy)]
struct Stage {
    parts: [(Option<usize>, f32); 8],
}
fn blend(parts: &[(Option<usize>, f64)]) -> Stage {
    let mut stage = Stage {
        parts: [(None, 0.); 8],
    };
    for (out, &(table, weight)) in stage.parts.iter_mut().zip(parts) {
        *out = (table, weight as f32);
    }
    stage
}
fn between(value: f64, points: &[f64]) -> (usize, f64) {
    let value = value.clamp(points[0], points[points.len() - 1]);
    let mut index = 0;
    while index < points.len() - 2 && value > points[index + 1] {
        index += 1;
    }
    (
        index,
        (value - points[index]) / (points[index + 1] - points[index]),
    )
}
fn spaced(value: f64, low: f64, step: f64, count: usize) -> (usize, f64) {
    let position = ((value - low) / step).clamp(0., (count - 1) as f64);
    let index = (position as usize).min(count - 2);
    (index, position - index as f64)
}
fn stage(start: usize, value: f64, low: f64, step: f64, count: usize) -> Stage {
    let (index, fraction) = spaced(value, low, step, count);
    blend(&[
        (Some(start + index), 1. - fraction),
        (Some(start + index + 1), fraction),
    ])
}
fn fifty(start: usize, value: f64) -> Stage {
    stage(start, value, -100., 50., 5)
}
fn adaptive(start: usize, value: f64, brightness: f64) -> Stage {
    let (row, down) = between(brightness, &[53., 80., 104., 130., 160., 190., 224.]);
    let (column, across) = between(value, &[-100., -50., -25., 0., 25., 50., 100.]);
    let base = start + row * 7 + column;
    blend(&[
        (Some(base), (1. - down) * (1. - across)),
        (Some(base + 1), (1. - down) * across),
        (Some(base + 7), down * (1. - across)),
        (Some(base + 8), down * across),
    ])
}
fn white_balance(temperature: f64, tint: f64) -> Stage {
    let (row, down) = spaced(temperature, -100., 25., 9);
    let (column, across) = spaced(tint, -100., 25., 9);
    let base = row * 9 + column;
    blend(&[
        (Some(base), (1. - down) * (1. - across)),
        (Some(base + 1), (1. - down) * across),
        (Some(base + 9), down * (1. - across)),
        (Some(base + 10), down * across),
    ])
}
fn indices(input: [f64; 3], size: usize) -> ([usize; 3], [f64; 3]) {
    let values = input.map(|v| v.clamp(0., 1.) * (size - 1) as f64);
    let indices = values.map(|v| (v as usize).min(size - 2));
    (
        indices,
        std::array::from_fn(|i| values[i] - indices[i] as f64),
    )
}
fn sample_bytes(table: &[u8], color: [f64; 3]) -> [f64; 3] {
    let (i, f) = indices(color, SIZE);
    let mut out = [0.; 3];
    for corner in 0..8 {
        let delta = [corner >> 2, (corner >> 1) & 1, corner & 1];
        let weights: [f64; 3] =
            std::array::from_fn(|c| if delta[c] != 0 { f[c] } else { 1. - f[c] });
        let weight = weights[0] * weights[1] * weights[2];
        if weight == 0. {
            continue;
        }
        let at = (((i[0] + delta[0]) * SIZE + i[1] + delta[1]) * SIZE + i[2] + delta[2]) * 3;
        for c in 0..3 {
            out[c] += weight * table[at + c] as f64;
        }
    }
    out.map(|v| v / 255.)
}
fn stage_color(stage: &Stage, color: [f64; 3]) -> [f64; 3] {
    let mut next = [0.; 3];
    for &(table, weight) in &stage.parts {
        if weight == 0. {
            continue;
        }
        let looked = table.map_or(color, |table| {
            sample_bytes(&tables()[table * ENTRIES..(table + 1) * ENTRIES], color)
        });
        for c in 0..3 {
            next[c] += weight as f64 * looked[c];
        }
    }
    next.map(|v| v.clamp(0., 1.))
}

/// Invert the same measured white-balance stage used by the renderer. The
/// bounded coarse-to-fine search matches CameraRawTables.neutralize in 1.5.3.
pub(super) fn neutralize(color: [f64; 3]) -> Option<[f64; 2]> {
    if color.iter().any(|v| !v.is_finite() || *v <= 1e-4) {
        return None;
    }
    let cast = |temperature, tint| {
        let color = stage_color(&white_balance(temperature, tint), color);
        color.into_iter().fold(f64::NEG_INFINITY, f64::max)
            - color.into_iter().fold(f64::INFINITY, f64::min)
    };
    let mut best = ([0., 0.], cast(0., 0.));
    for (step, reach) in [(10., 100.), (2., 10.), (0.5, 2.)] {
        let center = best.0;
        let count = (2. * reach / step) as usize;
        for row in 0..=count {
            let temperature = center[0] - reach + row as f64 * step;
            for column in 0..=count {
                let tint = center[1] - reach + column as f64 * step;
                if !(-100. ..=100.).contains(&temperature) || !(-100. ..=100.).contains(&tint) {
                    continue;
                }
                let found = cast(temperature, tint);
                if found < best.1 {
                    best = ([temperature, tint], found);
                }
            }
        }
    }
    Some(best.0)
}

pub(super) fn sample(table: &[f32], color: [f64; 3]) -> [f64; 3] {
    let (i, f) = indices(color, GRID);
    let mut out = [0.; 3];
    for corner in 0..8 {
        let delta = [corner >> 2, (corner >> 1) & 1, corner & 1];
        let weights: [f64; 3] =
            std::array::from_fn(|c| if delta[c] != 0 { f[c] } else { 1. - f[c] });
        let weight = weights[0] * weights[1] * weights[2];
        if weight == 0. {
            continue;
        }
        let at = (((i[0] + delta[0]) * GRID + i[1] + delta[1]) * GRID + i[2] + delta[2]) * 3;
        for c in 0..3 {
            out[c] += weight * table[at + c] as f64;
        }
    }
    out.map(|v| v.clamp(0., 1.))
}
fn compose(stages: &[Stage]) -> Option<Vec<f32>> {
    if stages.is_empty() {
        return None;
    }
    let mut out = vec![0.; OUTPUT_ENTRIES];
    out.par_chunks_mut(3).enumerate().for_each(|(cell, out)| {
        let mut color = [
            (cell / (GRID * GRID)) as f64 / (GRID - 1) as f64,
            (cell / GRID % GRID) as f64 / (GRID - 1) as f64,
            (cell % GRID) as f64 / (GRID - 1) as f64,
        ];
        for stage in stages {
            color = stage_color(stage, color);
        }
        for c in 0..3 {
            out[c] = color[c] as f32;
        }
    });
    Some(out)
}
fn compose_onto(table: &mut [f32], stages: &[Stage]) {
    if stages.is_empty() {
        return;
    }
    table.par_chunks_mut(3).for_each(|pixel| {
        let mut color = [pixel[0] as f64, pixel[1] as f64, pixel[2] as f64];
        for stage in stages {
            color = stage_color(stage, color);
        }
        for c in 0..3 {
            pixel[c] = color[c] as f32;
        }
    });
}
fn identity() -> Vec<f32> {
    (0..GRID * GRID * GRID)
        .flat_map(|cell| {
            [
                (cell / (GRID * GRID)) as f32 / (GRID - 1) as f32,
                (cell / GRID % GRID) as f32 / (GRID - 1) as f32,
                (cell % GRID) as f32 / (GRID - 1) as f32,
            ]
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Brightness {
    pub brightest: f64,
    pub log_brightest: f64,
}
pub(super) fn brightness(pixels: &[u8], width: usize, height: usize) -> Brightness {
    let decoded: [f64; 256] = std::array::from_fn(|i| srgb_to_linear(i as f64 / 255.));
    let rows = height.div_ceil(32);
    let sums: Vec<[f64; 3]> = (0..32)
        .into_par_iter()
        .map(|band| {
            let mut sum = [0.; 3];
            for y in band * rows..((band + 1) * rows).min(height) {
                for p in pixels[y * width * 4..(y + 1) * width * 4]
                    .as_chunks::<4>()
                    .0
                {
                    if p[3] == 0 {
                        continue;
                    }
                    let c = if p[3] == 255 {
                        [
                            decoded[p[0] as usize],
                            decoded[p[1] as usize],
                            decoded[p[2] as usize],
                        ]
                    } else {
                        super::read(p).map(srgb_to_linear)
                    };
                    let top = c[0].max(c[1]).max(c[2]);
                    let weight = p[3] as f64 / 255.;
                    sum[0] += weight * top;
                    sum[1] += weight * (top + 3e-4).ln();
                    sum[2] += weight;
                }
            }
            sum
        })
        .collect();
    let sum = sums.iter().fold([0.; 3], |mut a, b| {
        for c in 0..3 {
            a[c] += b[c];
        }
        a
    });
    if sum[2] <= 0. {
        Brightness {
            brightest: 127.5,
            log_brightest: 127.5,
        }
    } else {
        Brightness {
            brightest: linear_to_srgb(sum[0] / sum[2]) * 255.,
            log_brightest: linear_to_srgb((sum[1] / sum[2]).exp() - 3e-4) * 255.,
        }
    }
}
fn measured_dividers(curve: &Value) -> bool {
    number(curve, "shadowSplit", 25.) == 25.
        && number(curve, "darkSplit", 50.) == 50.
        && number(curve, "lightSplit", 75.) == 75.
        && number(curve, "refineSaturation", 0.) == 0.
}
fn stages(settings: &Value, brightness: Brightness) -> (Vec<Stage>, usize) {
    let mut out = Vec::new();
    for (index, key) in [
        "redHue",
        "redSaturation",
        "greenHue",
        "greenSaturation",
        "blueHue",
        "blueSaturation",
    ]
    .iter()
    .enumerate()
    {
        let value = number(&settings["calibration"], key, 0.);
        if value != 0. {
            out.push(fifty(405 + index * 5, value));
        }
    }
    if number(settings, "exposure", 0.) != 0. {
        out.push(stage(81, number(settings, "exposure", 0.), -5., 0.5, 21));
    }
    if number(settings, "temperature", 0.) != 0. || number(settings, "tint", 0.) != 0. {
        out.push(white_balance(
            number(settings, "temperature", 0.),
            number(settings, "tint", 0.),
        ));
    }
    if number(settings, "contrast", 0.) != 0. {
        out.push(adaptive(
            138,
            number(settings, "contrast", 0.),
            brightness.brightest,
        ));
    }
    if number(settings, "highlights", 0.) != 0. {
        out.push(adaptive(
            187,
            number(settings, "highlights", 0.),
            brightness.log_brightest,
        ));
    }
    let before = out.len();
    for (start, key) in [
        (102, "whites"),
        (111, "blacks"),
        (120, "saturation"),
        (129, "vibrance"),
    ] {
        let value = number(settings, key, 0.);
        if value != 0. {
            out.push(stage(start, value, -100., 25., 9));
        }
    }
    if measured_dividers(&settings["curve"]) {
        for (index, key) in ["shadows", "darks", "lights", "highlights"]
            .iter()
            .enumerate()
        {
            let value = number(&settings["curve"], key, 0.);
            if value != 0. {
                out.push(fifty(435 + index * 5, value));
            }
        }
    }
    (out, before)
}
fn mixer_stages(mixer: &Value) -> Vec<Stage> {
    let mut out = Vec::new();
    for (kind, key) in ["luminance", "saturation", "hue"].iter().enumerate() {
        for color in 0..8 {
            let value = mixer[key][color]
                .as_f64()
                .filter(|v| v.is_finite())
                .unwrap_or(0.);
            if value != 0. {
                out.push(fifty(285 + (kind * 8 + color) * 5, value));
            }
        }
    }
    out
}
fn grading_stages(grading: &Value) -> Vec<Stage> {
    let mut out = Vec::new();
    for (index, key) in ["shadows", "midtones", "highlights", "global"]
        .iter()
        .enumerate()
    {
        let value = number(&grading[key], "luminance", 0.);
        if value != 0. {
            out.push(fifty(743 + index * 5, value));
        }
    }
    let balance = number(grading, "balance", 0.) / 100.;
    let highlight_share = (if balance < 0. {
        1. + 1.4 * balance
    } else {
        1. + 2. * balance
    })
    .max(0.);
    let shadow_share = (if balance < 0. {
        1. - 1.2 * balance
    } else {
        1. - 1.4 * balance
    })
    .max(0.);
    let (blend_low, blend_up) = between(number(grading, "blending", 50.), &[0., 50., 100.]);
    for (wheel, key, share, blends) in [
        (3, "global", 1., false),
        (2, "midtones", 1., false),
        (1, "highlights", highlight_share, true),
        (0, "shadows", shadow_share, true),
    ] {
        let color = &grading[key];
        let saturation = number(color, "saturation", 0.) * share;
        if saturation <= 0. {
            continue;
        }
        let position = number(color, "hue", 0.).rem_euclid(360.) / 30.;
        let first = position as usize % 12;
        let around = position - position.floor();
        let (level, up) = between(saturation, &[0., 25., 50., 100.]);
        let blendings = if blends {
            vec![(blend_low, 1. - blend_up), (blend_low + 1, blend_up)]
        } else {
            vec![(1, 1.)]
        };
        let mut parts = Vec::new();
        for (blend_index, blend_weight) in blendings {
            let base = 455
                + match blend_index {
                    0 => wheel * 36,
                    1 => 72 + wheel * 36,
                    _ => 216 + wheel * 36,
                };
            for (hue, hue_weight) in [(first, 1. - around), ((first + 1) % 12, around)] {
                for (sat, sat_weight) in [(level, 1. - up), (level + 1, up)] {
                    parts.push((
                        if sat == 0 {
                            None
                        } else {
                            Some(base + hue * 3 + sat - 1)
                        },
                        blend_weight * hue_weight * sat_weight,
                    ));
                }
            }
        }
        out.push(blend(&parts));
    }
    out
}
pub(super) fn lut_at(lut: &[f32; 256], value: f64) -> f64 {
    let x = value.clamp(0., 1.) * 255.;
    let lo = x as usize;
    let hi = (lo + 1).min(255);
    let t = x - lo as f64;
    lut[lo] as f64 + (lut[hi] - lut[lo]) as f64 * t
}
fn parametric_curve(curve: &Value) -> PreparedCurve {
    let bend = |tone: f64, lower: f64, low: f64, upper: f64, high: f64| {
        if tone < lower && lower > 0. {
            lower * (tone / lower).powf(2f64.powf(-low / 100. * 1.66))
        } else if tone > upper && upper < 1. {
            1. - (1. - upper) * ((1. - tone) / (1. - upper)).powf(2f64.powf(high / 100. * 1.66))
        } else {
            tone
        }
    };
    PreparedCurve::from_points(
        (0..=32)
            .map(|i| {
                let x = i as f64 / 32.;
                [
                    x,
                    bend(
                        bend(
                            x,
                            number(curve, "shadowSplit", 25.) / 100.,
                            number(curve, "shadows", 0.),
                            number(curve, "lightSplit", 75.) / 100.,
                            number(curve, "highlights", 0.),
                        ),
                        number(curve, "darkSplit", 50.) / 100.,
                        number(curve, "darks", 0.),
                        number(curve, "darkSplit", 50.) / 100.,
                        number(curve, "lights", 0.),
                    ),
                ]
            })
            .collect(),
    )
}
pub(super) fn curve_tables(settings: &Value) -> [[f32; 256]; 4] {
    let curve = &settings["curve"];
    let master = PreparedCurve::new(&curve["rgb"]);
    let anchors = if measured_dividers(curve) {
        PreparedCurve::from_points(Vec::new())
    } else {
        parametric_curve(curve)
    };
    let channels = ["red", "green", "blue"].map(|key| PreparedCurve::new(&curve[key]));
    std::array::from_fn(|channel| {
        std::array::from_fn(|i| {
            let x = i as f64 / 255.;
            if channel == 0 {
                master.value(anchors.value(x)) as f32
            } else {
                channels[channel - 1].value(x) as f32
            }
        })
    })
}

pub(super) struct PreparedColor {
    pub before: Option<Vec<f32>>,
    pub after: Option<Vec<f32>>,
    pub mixer: Option<Vec<f32>>,
    pub grading: Option<Vec<f32>>,
}
type ColorCache = Vec<(String, Arc<PreparedColor>)>;
pub(super) fn prepare(
    settings: &Value,
    brightness: Brightness,
    folded: bool,
) -> Arc<PreparedColor> {
    // Bounded by four 33³ tables per entry; never retains the source image.
    static CACHE: OnceLock<Mutex<ColorCache>> = OnceLock::new();
    let key = format!(
        "{}:{:016x}:{:016x}:{folded}",
        settings,
        brightness.brightest.to_bits(),
        brightness.log_brightest.to_bits()
    );
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    if let Some((_, value)) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(k, _)| *k == key)
    {
        return value.clone();
    }
    let (stages, before_count) = stages(settings, brightness);
    let shadows = number(settings, "shadows", 0.) != 0.;
    let before = shadows.then(|| compose(&stages[..before_count])).flatten();
    let mut after = compose(if shadows {
        &stages[before_count..]
    } else {
        &stages
    });
    let mixer_stages = mixer_stages(&settings["mixer"]);
    let grading_stages = grading_stages(&settings["grading"]);
    let (mixer, grading) = if folded {
        let curves = curve_tables(settings);
        let curve_changes = curves.iter().any(|lut| {
            lut.iter()
                .enumerate()
                .any(|(i, &v)| v != (i as f64 / 255.) as f32)
        });
        if curve_changes || !mixer_stages.is_empty() || !grading_stages.is_empty() {
            let table = after.get_or_insert_with(identity);
            if curve_changes {
                table.par_chunks_mut(3).for_each(|pixel| {
                    for c in 0..3 {
                        pixel[c] =
                            lut_at(&curves[c + 1], lut_at(&curves[0], pixel[c] as f64)) as f32;
                    }
                });
            }
            let all: Vec<_> = mixer_stages.into_iter().chain(grading_stages).collect();
            compose_onto(table, &all);
        }
        (None, None)
    } else {
        (compose(&mixer_stages), compose(&grading_stages))
    };
    let value = Arc::new(PreparedColor {
        before,
        after,
        mixer,
        grading,
    });
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() == 4 {
        cache.remove(0);
    }
    cache.push((key, value.clone()));
    value
}
pub(super) fn apply_cpu(pixels: &mut [u8], table: Option<&[f32]>, clipping: i32) {
    pixels.par_chunks_mut(16_384).for_each(|chunk| {
        for p in chunk.as_chunks_mut::<4>().0 {
            if p[3] == 0 {
                continue;
            }
            let mut color = lookup_read(p);
            if let Some(table) = table {
                color = sample(table, color);
            }
            if clipping == 1 {
                color = color.map(|v| if v >= 254.5 / 255. { 1. } else { 0. });
            } else if clipping == 2 {
                color = color.map(|v| if v <= 0.5 / 255. { 0. } else { 1. });
            }
            super::write(p, color);
        }
    });
}
fn lookup_read(p: &[u8]) -> [f64; 3] {
    std::array::from_fn(|c| (p[c] as f64 * 255. / p[3] as f64).min(255.) / 255.)
}

struct GpuBuffers {
    input: wgpu::Buffer,
    output: wgpu::Buffer,
    readback: wgpu::Buffer,
    table: wgpu::Buffer,
    parameters: wgpu::Buffer,
    group: wgpu::BindGroup,
    capacity: u64,
}
struct GpuLookup {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: Option<wgpu::ComputePipeline>,
    buffers: Option<GpuBuffers>,
    failed: bool,
}
static GPU: OnceLock<Mutex<Option<GpuLookup>>> = OnceLock::new();
static DISCARD_BUFFERS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Bind the editor's existing device; no extra adapter, device or queue is made.
/// Rebinding after device recovery also drops all old pipeline/buffer handles.
pub(super) fn bind_gpu(device: wgpu::Device, queue: wgpu::Queue) {
    *GPU.get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(GpuLookup {
        device,
        queue,
        pipeline: None,
        buffers: None,
        failed: false,
    });
}
pub(super) fn unbind_gpu() {
    if let Some(storage) = GPU.get() {
        *storage.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
pub(super) fn discard_gpu_buffers() {
    DISCARD_BUFFERS.store(true, std::sync::atomic::Ordering::Release);
    if let Some(storage) = GPU.get() {
        // A pending filter releases its buffers when it finishes, without
        // stalling the UI thread that closed its panel.
        if let Ok(mut state) = storage.try_lock()
            && let Some(state) = state.as_mut()
        {
            state.buffers = None;
        }
    }
}
pub(super) fn apply(pixels: &mut [u8], table: Option<&[f32]>, clipping: i32) {
    if pixels.len() / 4 >= 8_000_001
        && clipping == 0
        && let Some(table) = table
        && try_gpu(pixels, table)
    {
        return;
    }
    apply_cpu(pixels, table, clipping);
}
fn try_gpu(pixels: &mut [u8], table: &[f32]) -> bool {
    if pixels.is_empty()
        || !pixels.len().is_multiple_of(4)
        || table.len() != OUTPUT_ENTRIES
        || table
            .iter()
            .any(|&v| !v.is_finite() || !(0. ..=1.).contains(&v))
    {
        return false;
    }
    let Some(storage) = GPU.get() else {
        return false;
    };
    let mut storage = storage.lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = storage.as_mut().filter(|s| !s.failed) else {
        return false;
    };
    let bytes = pixels.len() as u64;
    let limits = state.device.limits();
    if bytes > limits.max_buffer_size
        || bytes > limits.max_storage_buffer_binding_size as u64
        || pixels.len() / 4 > u32::MAX as usize
    {
        return false;
    }
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.run(pixels, table)))
            .unwrap_or_else(|_| Err("GPU device or validation failure".into()));
    if DISCARD_BUFFERS.swap(false, std::sync::atomic::Ordering::AcqRel) {
        state.buffers = None;
    }
    match result {
        Ok(()) => true,
        Err(error) => {
            log::warn!("Camera Raw GPU lookup: {error}; using CPU");
            state.buffers = None;
            state.failed = true;
            false
        }
    }
}
impl GpuLookup {
    fn run(&mut self, pixels: &mut [u8], table: &[f32]) -> Result<(), String> {
        let bytes = pixels.len() as u64;
        if self.pipeline.is_none() {
            self.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let module = self
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("Camera Raw lookup"),
                    source: wgpu::ShaderSource::Wgsl(
                        include_str!("../assets/camera_raw_lookup.wgsl").into(),
                    ),
                });
            let pipeline = self
                .device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("Camera Raw lookup"),
                    layout: None,
                    module: &module,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    cache: None,
                });
            if let Some(error) = pollster::block_on(self.device.pop_error_scope()) {
                return Err(error.to_string());
            }
            self.pipeline = Some(pipeline);
        }
        let pipeline = self.pipeline.as_ref().unwrap();
        if self
            .buffers
            .as_ref()
            .is_none_or(|buffers| buffers.capacity < bytes)
        {
            self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
            let buffer = |label, size, usage| {
                self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size,
                    usage,
                    mapped_at_creation: false,
                })
            };
            let input = buffer(
                "Camera Raw source",
                bytes,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            );
            let output = buffer(
                "Camera Raw result",
                bytes,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            );
            let readback = buffer(
                "Camera Raw readback",
                bytes,
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            );
            let table = buffer(
                "Camera Raw 33 cubed table",
                (OUTPUT_ENTRIES * 4) as u64,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            );
            let parameters = buffer(
                "Camera Raw parameters",
                16,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            );
            let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Camera Raw lookup"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: input.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: table.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: output.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: parameters.as_entire_binding(),
                    },
                ],
            });
            if let Some(error) = pollster::block_on(self.device.pop_error_scope()) {
                return Err(error.to_string());
            }
            self.buffers = Some(GpuBuffers {
                input,
                output,
                readback,
                table,
                parameters,
                group,
                capacity: bytes,
            });
        }
        let buffers = self.buffers.as_ref().unwrap();
        let count = (pixels.len() / 4) as u32;
        let x = count
            .div_ceil(256)
            .min(self.device.limits().max_compute_workgroups_per_dimension);
        let row_width = x * 256;
        let y = count.div_ceil(row_width);
        if y > self.device.limits().max_compute_workgroups_per_dimension {
            return Err("dispatch exceeds device limit".into());
        }
        self.queue.write_buffer(&buffers.input, 0, pixels);
        self.queue
            .write_buffer(&buffers.table, 0, bytemuck::cast_slice(table));
        self.queue.write_buffer(
            &buffers.parameters,
            0,
            bytemuck::cast_slice(&[count, row_width, 0, 0]),
        );
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &buffers.group, &[]);
            pass.dispatch_workgroups(x, y, 1);
        }
        encoder.copy_buffer_to_buffer(&buffers.output, 0, &buffers.readback, 0, bytes);
        let submission = self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        let slice = buffers.readback.slice(..bytes);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        if let Err(error) = self.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(std::time::Duration::from_secs(10)),
        }) {
            buffers.readback.unmap();
            return Err(error.to_string());
        }
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let mapped = slice.get_mapped_range();
        // No source writes occur until all GPU work and readback have succeeded.
        pixels
            .par_chunks_mut(16_384)
            .zip(mapped.par_chunks(16_384))
            .for_each(|(out, data)| {
                for (p, gpu) in out
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(data.as_chunks::<4>().0)
                {
                    if p[3] == 0 {
                        continue;
                    }
                    if gpu[3] != 0 {
                        let color = sample(table, lookup_read(p));
                        super::write(p, color);
                    } else {
                        p[..3].copy_from_slice(&gpu[..3]);
                    }
                }
            });
        drop(mapped);
        buffers.readback.unmap();
        Ok(())
    }
}

struct Shadows {
    normal_from: f64,
    amounts: Vec<f64>,
    coefficients: Vec<f32>,
    x_count: usize,
    k_count: usize,
    x_low: f32,
    x_step: f32,
    k_low: f32,
    k_step: f32,
}
fn shadow_tables() -> &'static Shadows {
    static SHADOWS: OnceLock<Shadows> = OnceLock::new();
    SHADOWS.get_or_init(|| {
        let data = include_bytes!("../assets/camera_raw_shadows.bin");
        assert_eq!(&data[..4], b"CRS2");
        let field = |at| u16::from_le_bytes([data[at], data[at + 1]]) as usize;
        let float = |at| f32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        let (sets, amounts, x_count, k_count, terms) =
            (field(4), field(6), field(8), field(10), field(12));
        assert_eq!((sets, terms), (2, 3));
        assert_eq!(
            data.len(),
            34 + amounts * 4 + sets * amounts * x_count * k_count * terms * 4
        );
        Shadows {
            normal_from: float(14) as f64,
            amounts: (0..amounts).map(|i| float(34 + i * 4) as f64).collect(),
            coefficients: data[34 + amounts * 4..]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&p| f32::from_le_bytes(p))
                .collect(),
            x_count,
            k_count,
            x_low: float(18),
            x_step: float(22),
            k_low: float(26),
            k_step: float(30),
        }
    })
}
fn box_mean(source: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    let mut temp = vec![0.; source.len()];
    temp.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let source = &source[y * width..(y + 1) * width];
        let (mut lo, mut hi, mut sum) = (0, 0, 0.);
        for (x, out) in row.iter_mut().enumerate() {
            let want_lo = x.saturating_sub(radius);
            let want_hi = (x + radius + 1).min(width);
            while hi < want_hi {
                sum += source[hi] as f64;
                hi += 1;
            }
            while lo < want_lo {
                sum -= source[lo] as f64;
                lo += 1;
            }
            *out = (sum / (hi - lo) as f64) as f32;
        }
    });
    let mut out = vec![0.; source.len()];
    for x in 0..width {
        let (mut lo, mut hi, mut sum) = (0, 0, 0.);
        for y in 0..height {
            let want_lo = y.saturating_sub(radius);
            let want_hi = (y + radius + 1).min(height);
            while hi < want_hi {
                sum += temp[hi * width + x] as f64;
                hi += 1;
            }
            while lo < want_lo {
                sum -= temp[lo * width + x] as f64;
                lo += 1;
            }
            out[y * width + x] = (sum / (hi - lo) as f64) as f32;
        }
    }
    out
}
pub(super) fn apply_shadows(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    amount: f64,
    preview_scale: f64,
) {
    if amount == 0. || width == 0 || height == 0 {
        return;
    }
    let tables = shadow_tables();
    let plane = tables.x_count * tables.k_count;
    let per = plane * 3;
    let set = usize::from(width.max(height) as f64 / preview_scale.max(0.01) >= tables.normal_from);
    let mut amounts: Vec<_> = tables
        .amounts
        .iter()
        .enumerate()
        .map(|(i, &v)| (v, Some(i)))
        .collect();
    amounts.push((0., None));
    amounts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let points: Vec<_> = amounts.iter().map(|p| p.0).collect();
    let (index, t) = between(amount, &points);
    let t = t as f32;
    let coefficients: Vec<_> = (0..per)
        .map(|i| {
            let read = |a: Option<usize>| {
                a.map_or(0., |a| {
                    tables.coefficients[(set * tables.amounts.len() + a) * per + i]
                })
            };
            read(amounts[index].1) * (1. - t) + read(amounts[index + 1].1) * t
        })
        .collect();
    let decoded: [f64; 256] = std::array::from_fn(|i| srgb_to_linear(i as f64 / 255.));
    let linear = |p: &[u8]| {
        if p[3] == 255 {
            [
                decoded[p[0] as usize],
                decoded[p[1] as usize],
                decoded[p[2] as usize],
            ]
        } else {
            super::read(p).map(srgb_to_linear)
        }
    };
    let rows = height.div_ceil(32);
    let sums: Vec<[f64; 4]> = (0..32)
        .into_par_iter()
        .map(|band| {
            let mut sum = [0.; 4];
            for y in band * rows..((band + 1) * rows).min(height) {
                for x in 0..width {
                    let at = (y * width + x) * 4;
                    let p = &pixels[at..at + 4];
                    if p[3] == 0 {
                        continue;
                    }
                    let luminance = super::lum(linear(p));
                    sum[2] += luminance;
                    sum[3] += 1.;
                    let v = p[1];
                    if (x > 0 && pixels[at - 3] != v)
                        || (x + 1 < width && pixels[at + 5] != v)
                        || (y > 0 && pixels[at - width * 4 + 1] != v)
                        || (y + 1 < height && pixels[at + width * 4 + 1] != v)
                    {
                        sum[0] += luminance;
                        sum[1] += 1.;
                    }
                }
            }
            sum
        })
        .collect();
    let sum = sums.iter().fold([0.; 4], |mut a, b| {
        for c in 0..4 {
            a[c] += b[c];
        }
        a
    });
    let floor = 1. / 8192.;
    let key = (if sum[1] > 0. {
        sum[0] / sum[1]
    } else if sum[3] > 0. {
        sum[2] / sum[3]
    } else {
        0.18
    })
    .max(floor)
    .log2();
    let scale = (width.min(height) / 384).max(1);
    let (lw, lh) = (width.div_ceil(scale), height.div_ceil(scale));
    let mut low = vec![0f32; lw * lh];
    let mut weight = vec![0f32; low.len()];
    for y in 0..height {
        for x in 0..width {
            let p = &pixels[(y * width + x) * 4..(y * width + x + 1) * 4];
            if p[3] == 0 {
                continue;
            }
            let at = y / scale * lw + x / scale;
            low[at] += super::lum(linear(p)).max(floor).log2() as f32;
            weight[at] += 1.;
        }
    }
    for i in 0..low.len() {
        low[i] = if weight[i] > 0. {
            low[i] / weight[i]
        } else {
            key as f32
        };
    }
    drop(weight);
    let radius = ((lw.min(lh) as f64 * 0.25).round() as usize).max(1);
    let mut mean = box_mean(&low, lw, lh, radius);
    let squared: Vec<_> = low.iter().map(|v| v * v).collect();
    let mut square = box_mean(&squared, lw, lh, radius);
    drop(squared);
    let mut a = vec![0.; low.len()];
    let mut b = vec![0.; low.len()];
    for i in 0..low.len() {
        let variance = (square[i] - mean[i] * mean[i]).max(0.);
        a[i] = variance / (variance + 1.5);
        b[i] = mean[i] - a[i] * mean[i];
    }
    mean = box_mean(&a, lw, lh, radius);
    square = box_mean(&b, lw, lh, radius);
    let kp =
        ((key - tables.k_low as f64) / tables.k_step as f64).clamp(0., (tables.k_count - 1) as f64);
    let k0 = (kp as usize).min(tables.k_count - 1);
    let k1 = (k0 + 1).min(tables.k_count - 1);
    let kt = kp - k0 as f64;
    pixels
        .par_chunks_mut(width * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let fy = ((y as f64 + 0.5) / scale as f64 - 0.5).clamp(0., (lh - 1) as f64);
            let y0 = fy as usize;
            let y1 = (y0 + 1).min(lh - 1);
            let ty = fy - y0 as f64;
            for (x, p) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                if p[3] == 0 {
                    continue;
                }
                let color = linear(p);
                let luminance = super::lum(color);
                if luminance <= floor {
                    continue;
                }
                let fx = ((x as f64 + 0.5) / scale as f64 - 0.5).clamp(0., (lw - 1) as f64);
                let x0 = fx as usize;
                let x1 = (x0 + 1).min(lw - 1);
                let tx = fx - x0 as f64;
                let sampled = |plane: &[f32]| {
                    (plane[y0 * lw + x0] as f64 * (1. - tx) + plane[y0 * lw + x1] as f64 * tx)
                        * (1. - ty)
                        + (plane[y1 * lw + x0] as f64 * (1. - tx) + plane[y1 * lw + x1] as f64 * tx)
                            * ty
                };
                let level = luminance.log2();
                let region = sampled(&mean) * level + sampled(&square);
                let detail = level - region;
                let xp = ((region - key - tables.x_low as f64) / tables.x_step as f64)
                    .clamp(0., (tables.x_count - 1) as f64);
                let x0c = (xp as usize).min(tables.x_count - 2);
                let xt = xp - x0c as f64;
                let term: [f64; 3] = std::array::from_fn(|t| {
                    let c = &coefficients[t * plane..(t + 1) * plane];
                    let row0 = c[k0 * tables.x_count + x0c] as f64 * (1. - xt)
                        + c[k0 * tables.x_count + x0c + 1] as f64 * xt;
                    let row1 = c[k1 * tables.x_count + x0c] as f64 * (1. - xt)
                        + c[k1 * tables.x_count + x0c + 1] as f64 * xt;
                    row0 * (1. - kt) + row1 * kt
                });
                let gain = (term[0] + term[1] * detail.max(0.) + term[2] * detail.min(0.)).exp2();
                super::write(p, color.map(|v| linear_to_srgb((v * gain).clamp(0., 1.))));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Requires a real Windows GPU; also records CPU/GPU timing"]
    fn gpu_lookup_matches_cpu_with_rounding_repair_rebind_and_fallback() {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .expect("GPU adapter for Camera Raw validation");
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        }))
        .unwrap();
        bind_gpu(device.clone(), queue.clone());
        let make_input = |count: usize| -> Vec<u8> {
            let mut random = 0x243f6a88u32;
            (0..count)
                .flat_map(|i| {
                    random ^= random << 13;
                    random ^= random >> 17;
                    random ^= random << 5;
                    let alpha = (i % 256) as u8;
                    if alpha == 0 {
                        [19, 73, 255, 0]
                    } else {
                        [
                            (random & 255) as u8 % alpha,
                            (random >> 8 & 255) as u8 % alpha,
                            (random >> 16 & 255) as u8 % alpha,
                            alpha,
                        ]
                    }
                })
                .collect()
        };
        let settings = serde_json::json!({"exposure":0.73,"temperature":31,"tint":-43,"contrast":37,
            "mixer":{"hue":[43,0,-37,0,71,0,-15,0]},
            "grading":{"shadows":{"hue":34,"saturation":65},"highlights":{"hue":275,"saturation":37},"blending":78,"balance":-37}});
        let prepared = prepare(
            &settings,
            Brightness {
                brightest: 113.7,
                log_brightest: 76.3,
            },
            true,
        );
        let measured = prepared.after.as_ref().unwrap();
        let original = make_input(270_111);
        for table in [&identity(), &vec![0.5; OUTPUT_ENTRIES], measured] {
            let mut cpu = original.clone();
            apply_cpu(&mut cpu, Some(table), 0);
            let mut gpu = original.clone();
            assert!(try_gpu(&mut gpu, table));
            assert_eq!(gpu, cpu);
        }
        let mut invalid = original.clone();
        let mut bad = identity();
        bad[40] = f32::NAN;
        assert!(!try_gpu(&mut invalid, &bad));
        assert_eq!(invalid, original);
        discard_gpu_buffers();
        assert!(
            GPU.get()
                .unwrap()
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .buffers
                .is_none()
        );
        unbind_gpu();
        assert!(!try_gpu(&mut invalid, measured));
        assert_eq!(invalid, original);
        bind_gpu(device.clone(), queue.clone());
        assert!(try_gpu(&mut invalid, measured));
        let mut timings = Vec::new();
        for (width, height) in [(1920, 1080), (3840, 2160), (6000, 4000)] {
            let source = make_input(width * height);
            let mut cpu = source.clone();
            let start = std::time::Instant::now();
            apply_cpu(&mut cpu, Some(measured), 0);
            let cpu_ms = start.elapsed().as_secs_f64() * 1000.;
            let mut gpu = source.clone();
            // Warm/reuse the scratch buffers just as consecutive slider updates do.
            assert!(try_gpu(&mut gpu, measured));
            gpu.copy_from_slice(&source);
            let start = std::time::Instant::now();
            assert!(try_gpu(&mut gpu, measured));
            let gpu_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(gpu, cpu);
            eprintln!(
                "Camera Raw {width}x{height}: CPU {cpu_ms:.2} ms, GPU {gpu_ms:.2} ms; exact pixels"
            );
            timings.push(serde_json::json!({"width":width,"height":height,"cpuMs":cpu_ms,"gpuMs":gpu_ms,"pixelDifferences":0}));
        }
        device.destroy();
        let mut failed = original.clone();
        assert!(!try_gpu(&mut failed, measured));
        assert_eq!(failed, original);
        unbind_gpu();
        if let Some(path) = std::env::var_os("COMPOSITOR_CAMERA_RAW_BENCH") {
            std::fs::write(
                path,
                serde_json::to_vec_pretty(&serde_json::json!({"adapter":info.name,
                "backend":format!("{:?}",info.backend),"profile":"dev opt-level=1; debuginfo=0",
                "roundingRepair":true,"destroyedDeviceFallback":true,"timings":timings}))
                .unwrap(),
            )
            .unwrap();
        }
    }
    #[test]
    fn measured_pipeline_matches_actual_upstream_c_exactly() {
        let f: Value =
            serde_json::from_str(include_str!("../tests/reference/camera_raw_1_5_3.json")).unwrap();
        for case in f["cases"].as_array().unwrap() {
            let bytes = |key: &str| -> Vec<u8> {
                case[key]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u8)
                    .collect()
            };
            let mut actual = bytes("input");
            super::super::apply(
                &mut actual,
                case["width"].as_u64().unwrap() as usize,
                case["height"].as_u64().unwrap() as usize,
                &case["settings"],
            )
            .unwrap();
            let expected = bytes("expected");
            let different: Vec<_> = actual
                .iter()
                .zip(&expected)
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .take(10)
                .collect();
            assert!(different.is_empty(), "{}: {:?}", case["name"], different);
        }
        use sha2::Digest;
        for case in f["largeCases"].as_array().unwrap() {
            let width = case["width"].as_u64().unwrap() as usize;
            let height = case["height"].as_u64().unwrap() as usize;
            let mut actual: Vec<u8> = (0..width * height)
                .flat_map(|i| {
                    let alpha = [0, 1, 2, 17, 127, 128, 254, 255][i % 8];
                    [
                        ((i * 47 % 256) * alpha / 255) as u8,
                        ((i * 103 % 256) * alpha / 255) as u8,
                        ((i * 193 % 256) * alpha / 255) as u8,
                        alpha as u8,
                    ]
                })
                .collect();
            super::super::apply(&mut actual, width, height, &case["settings"]).unwrap();
            assert_eq!(
                format!("{:x}", sha2::Sha256::digest(&actual)),
                case["expectedSHA256"].as_str().unwrap(),
                "{}",
                case["name"]
            );
        }
    }
}
