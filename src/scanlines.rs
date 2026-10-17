//! Standalone Scanlines filter, ported from Scanlines.swift and DitherPixels.c.
//! Original implementation: Copyright (c) 2026 Wonder Assembly LLC, MIT.
use super::{color, number, string};
use crate::pixels;
use rayon::prelude::*;
use serde_json::Value;

struct Settings {
    spacing: usize,
    thickness: f32,
    dots: f32,
    wobble: f32,
    displace: f32,
    threshold: f32,
    split: usize,
    density: f32,
    contrast: f32,
    black_level: f32,
    smoothness: f32,
    glow: f32,
    original: bool,
    dark: [f32; 3],
    light: [f32; 3],
}

impl Settings {
    fn read(value: &Value) -> Self {
        let amount = |key, default| (number(value, key, default).clamp(0., 100.) / 100.) as f32;
        let colors = string(value, "colors", "Black & White");
        let bytes = |key, default| color(&value[key], default).map(|v| (v * 255.).round() as u8);
        let (dark, light) = if colors == "Two Colors" {
            (bytes("dark", [0.; 3]), bytes("light", [1.; 3]))
        } else {
            ([0; 3], [255; 3])
        };
        Self {
            spacing: number(value, "lineSpacing", 4.).clamp(2., 32.).round() as usize,
            thickness: (number(value, "thickness", 70.).clamp(5., 100.) / 100.) as f32,
            dots: amount("dots", 0.),
            wobble: number(value, "wobble", 0.).clamp(0., 64.) as f32,
            displace: number(value, "displace", 0.).clamp(-100., 100.) as f32,
            threshold: amount("threshold", 0.),
            split: number(value, "split", 0.).clamp(0., 16.).round() as usize,
            density: (number(value, "density", 0.).clamp(-100., 100.) / 100.) as f32,
            contrast: (number(value, "contrast", 0.).clamp(-100., 100.) / 100.) as f32,
            black_level: amount("blackLevel", 0.),
            smoothness: amount("smoothness", 50.),
            glow: amount("glow", 0.),
            original: colors == "Original",
            dark: dark.map(|v| f32::from(v) / 255.),
            light: light.map(|v| f32::from(v) / 255.),
        }
    }
}

fn zeros<T: Default + Clone>(count: usize) -> Result<Vec<T>, String> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| "扫描线滤镜没有足够的工作内存".to_owned())?;
    values.resize(count, T::default());
    Ok(values)
}

fn write(pixel: &mut [u8], colors: [f32; 3]) {
    let alpha = f32::from(pixel[3]) / 255.;
    for c in 0..3 {
        pixel[c] = (colors[c].clamp(0., 1.) * alpha * 255.).round() as u8;
    }
}

pub fn apply(pixels: &mut [u8], width: usize, height: usize, value: &Value) -> Result<(), String> {
    let count = width.checked_mul(height).ok_or("扫描线图像尺寸过大")?;
    if count.checked_mul(4) != Some(pixels.len()) {
        return Err("扫描线图像像素长度无效".into());
    }
    if count == 0 {
        return Ok(());
    }
    let settings = Settings::read(value);
    let lines = height.div_ceil(settings.spacing);
    let planes = if settings.original { 3 } else { 1 };
    let mut tone = zeros::<f32>(count.checked_mul(planes).ok_or("扫描线图像尺寸过大")?)?;
    let mut scan = zeros::<f32>(lines * width * planes)?;
    let gamma = (settings.density * 1.5).exp2();
    let contrast = if settings.contrast >= 0. {
        1. / (1. - 0.95 * settings.contrast)
    } else {
        1. + settings.contrast
    };
    tone.par_chunks_mut(count)
        .enumerate()
        .for_each(|(plane, values)| {
            for (out, pixel) in values.iter_mut().zip(pixels.as_chunks::<4>().0) {
                let scale = if pixel[3] == 0 {
                    0.
                } else {
                    1. / f32::from(pixel[3])
                };
                let input = if settings.original {
                    f32::from(pixel[plane]) * scale
                } else {
                    0.2126 * (f32::from(pixel[0]) * scale)
                        + 0.7152 * (f32::from(pixel[1]) * scale)
                        + 0.0722 * (f32::from(pixel[2]) * scale)
                };
                *out = ((input.clamp(0., 1.).powf(gamma) - 0.5) * contrast + 0.5).clamp(0., 1.);
            }
        });
    scan.par_chunks_mut(width)
        .enumerate()
        .for_each(|(row, values)| {
            let (plane, line) = (row / lines, row % lines);
            let top = line * settings.spacing;
            let bottom = (top + settings.spacing).min(height);
            let wave = (line as f32 * 0.45).sin() * 0.7 + (line as f32 * 1.7 + 1.3).sin() * 0.3;
            let shift = (settings.wobble * wave).round() as isize;
            for (x, out) in values.iter_mut().enumerate() {
                let sx = x as isize - shift;
                if !(0..width as isize).contains(&sx) {
                    continue;
                }
                let mut sum = 0.;
                let mut samples = 0;
                for y in top..bottom {
                    let at = y * width + sx as usize;
                    if pixels[at * 4 + 3] != 0 {
                        sum += tone[plane * count + at];
                        samples += 1;
                    }
                }
                if samples > 0 {
                    *out = sum / samples as f32;
                }
            }
        });
    drop(tone);
    draw_lines(pixels, width, height, &settings, &scan, lines)?;
    if settings.split > 0 {
        let mut copy = zeros::<u8>(width * 4)?;
        for row in pixels.chunks_exact_mut(width * 4) {
            copy.copy_from_slice(row);
            for x in 0..width {
                let alpha = row[x * 4 + 3];
                row[x * 4] = x
                    .checked_sub(settings.split)
                    .map_or(0, |from| copy[from * 4])
                    .min(alpha);
                row[x * 4 + 2] = if x + settings.split < width {
                    copy[(x + settings.split) * 4 + 2].min(alpha)
                } else {
                    0
                };
            }
        }
    }
    if settings.glow > 0. {
        glow(pixels, width, height, settings.spacing, settings.glow)?;
    }
    Ok(())
}

fn draw_lines(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    settings: &Settings,
    scan: &[f32],
    lines: usize,
) -> Result<(), String> {
    let plane = lines * width;
    let mut tone = zeros::<f32>(plane)?;
    let mut lift = zeros::<f32>(plane)?;
    let mut spread = zeros::<f32>(plane)?;
    for (at, out) in tone.iter_mut().enumerate() {
        *out = if settings.original {
            0.2126 * scan[at] + 0.7152 * scan[plane + at] + 0.0722 * scan[2 * plane + at]
        } else {
            scan[at]
        };
    }
    lift.copy_from_slice(&tone);
    let radius = (settings.smoothness * settings.spacing as f32 * 2.).round() as usize;
    if radius > 0 && settings.displace != 0. {
        lift.par_chunks_mut(width)
            .try_for_each(|row| -> Result<(), String> {
                let mut copy = zeros::<f32>(width)?;
                for _ in 0..3 {
                    copy.copy_from_slice(row);
                    let mut sum = 0.;
                    for x in -(radius as isize)..=radius as isize {
                        sum += copy[x.clamp(0, width as isize - 1) as usize];
                    }
                    for (x, out) in row.iter_mut().enumerate() {
                        *out = sum / (2 * radius + 1) as f32;
                        sum +=
                            copy[(x + radius + 1).min(width - 1)] - copy[x.saturating_sub(radius)];
                    }
                }
                Ok(())
            })?;
        for _ in 0..(settings.smoothness * 2.).round() as usize {
            spread
                .par_chunks_mut(width)
                .enumerate()
                .for_each(|(line, row)| {
                    let above = line.saturating_sub(1) * width;
                    let here = line * width;
                    let below = (line + 1).min(lines - 1) * width;
                    for (x, out) in row.iter_mut().enumerate() {
                        *out = (lift[above + x] + 2. * lift[here + x] + lift[below + x]) / 4.;
                    }
                });
            lift.copy_from_slice(&spread);
        }
    }
    drop(spread);
    let middle = settings.spacing as f32 / 2.;
    let rising = settings.displace >= 0.;
    let screen = if settings.original {
        [0.; 3]
    } else {
        settings.dark
    };
    let mut cover = zeros::<f32>(height)?;
    let mut colors = zeros::<[f32; 3]>(height)?;
    for x in 0..width {
        cover.fill(0.);
        let along = (x as f32 + 0.5) % settings.spacing as f32 - middle;
        let mut horizon = if rising {
            f32::INFINITY
        } else {
            f32::NEG_INFINITY
        };
        for step in 0..lines {
            let line = if rising { lines - 1 - step } else { step };
            let beading = if settings.dots > 0. {
                ((settings.dots - tone[line * width + x]) / 0.2).clamp(0., 1.)
            } else {
                0.
            };
            let across = along * beading;
            let at = (x as f32 - across).round().clamp(0., (width - 1) as f32) as usize;
            let i = line * width + at;
            let t = tone[i];
            let lit = if settings.threshold > 0. {
                ((t - settings.threshold) / 0.04).clamp(0., 1.)
            } else {
                1.
            };
            if lit <= 0. {
                continue;
            }
            let base = line as f32 * settings.spacing as f32 + middle;
            let here = base - settings.displace * lift[i];
            let before = if at > 0 {
                base - settings.displace * lift[i - 1]
            } else {
                here
            };
            let (lo, hi) = (here.min(before), here.max(before));
            let level = settings.black_level + (1. - settings.black_level) * t;
            let color = std::array::from_fn(|c| {
                if settings.original {
                    settings.black_level + (1. - settings.black_level) * scan[c * plane + i]
                } else {
                    screen[c] + (settings.light[c] - screen[c]) * level
                }
            });
            let beam = middle * settings.thickness * (0.29 + 0.71 * level.clamp(0., 1.).sqrt());
            let top = if rising {
                lo - beam - 1.
            } else {
                (lo - beam - 1.).max(horizon - 1.)
            }
            .floor()
            .max(0.) as isize;
            let bottom = if rising {
                (hi + beam + 1.).min(horizon + 1.)
            } else {
                hi + beam + 1.
            }
            .ceil()
            .min(height as f32) as isize;
            for y in top..bottom {
                let yy = y as f32 + 0.5;
                let off = if yy < lo {
                    lo - yy
                } else if yy > hi {
                    yy - hi
                } else {
                    0.
                };
                let distance = (off * off + across * across).sqrt();
                let hidden = if rising {
                    horizon - yy + 0.5
                } else {
                    yy - horizon + 0.5
                }
                .clamp(0., 1.);
                let shown = (beam - distance + 0.5).clamp(0., 1.) * lit * hidden;
                if shown > cover[y as usize] {
                    cover[y as usize] = shown;
                    colors[y as usize] = color;
                }
            }
            if lit > 0.5 {
                horizon = if rising {
                    horizon.min(lo - beam)
                } else {
                    horizon.max(hi + beam)
                };
            }
        }
        for y in 0..height {
            let at = (y * width + x) * 4;
            if pixels[at + 3] == 0 {
                continue;
            }
            write(
                &mut pixels[at..at + 4],
                std::array::from_fn(|c| screen[c] + (colors[y][c] * 1.35 - screen[c]) * cover[y]),
            );
        }
    }
    Ok(())
}

/// Match the original's reduced-resolution soft bloom and its exponential headroom.
fn glow(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    spacing: usize,
    amount: f32,
) -> Result<(), String> {
    let sigma = spacing as f64 * 3. + 3.;
    let shrink = (sigma / 4.).floor().max(1.) as usize;
    let (sw, sh) = (width.div_ceil(shrink), height.div_ceil(shrink));
    let mut small = zeros::<u8>(sw * sh * 4)?;
    for y in 0..sh {
        for x in 0..sw {
            let mut sum = [0_u64; 4];
            let mut count = 0_u64;
            for sy in y * shrink..((y + 1) * shrink).min(height) {
                for sx in x * shrink..((x + 1) * shrink).min(width) {
                    for c in 0..4 {
                        sum[c] += u64::from(pixels[(sy * width + sx) * 4 + c]);
                    }
                    count += 1;
                }
            }
            for c in 0..4 {
                small[(y * sw + x) * 4 + c] = ((sum[c] + count / 2) / count) as u8;
            }
        }
    }
    let blurred = pixels::gaussian_blur(&small, sw, sh, sigma / shrink as f64);
    for y in 0..height {
        for x in 0..width {
            let u = ((x as f32 + 0.5) / shrink as f32 - 0.5).clamp(0., (sw - 1) as f32);
            let v = ((y as f32 + 0.5) / shrink as f32 - 0.5).clamp(0., (sh - 1) as f32);
            let (x0, y0) = (u.floor() as usize, v.floor() as usize);
            let (fx, fy) = (u - x0 as f32, v - y0 as f32);
            let sample = |c| {
                let row = |yy| {
                    f32::from(blurred[(yy * sw + x0) * 4 + c]) * (1. - fx)
                        + f32::from(blurred[(yy * sw + (x0 + 1).min(sw - 1)) * 4 + c]) * fx
                };
                (row(y0) * (1. - fy) + row((y0 + 1).min(sh - 1)) * fy).round()
            };
            let pixel = &mut pixels[(y * width + x) * 4..(y * width + x + 1) * 4];
            let alpha = f32::from(pixel[3]);
            for (c, component) in pixel.iter_mut().enumerate().take(3) {
                let value = f32::from(*component);
                let added = sample(c) * (amount * 2.5) * alpha / 255.;
                let room = (alpha - value) * 0.7;
                let result = if room > 0. {
                    value + room * (1. - (-added / room).exp())
                } else {
                    value
                };
                *component = result.round().min(alpha) as u8;
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
    fn flat_light_draws_bright_beams_and_dark_gaps() {
        let render = |gray| {
            let mut pixels = [gray, gray, gray, 255].repeat(16 * 32);
            apply(&mut pixels, 16, 32, &json!({"lineSpacing":8})).unwrap();
            (0..32)
                .map(|y| pixels[(y * 16 + 5) * 4] as usize)
                .collect::<Vec<_>>()
        };
        let (white, gray, black) = (render(255), render(89), render(0));
        for rows in white.as_chunks::<8>().0 {
            assert_eq!(&rows[3..5], &[255, 255]);
            assert!(rows.iter().any(|&v| v < 40));
        }
        assert!(gray.iter().sum::<usize>() * 2 < white.iter().sum::<usize>());
        assert!(black.iter().all(|&v| v == 0));
    }

    #[test]
    fn threshold_alpha_and_glow_keep_the_screen_valid() {
        let source = [38, 38, 38, 128].repeat(32 * 32);
        let mut hidden = source.clone();
        apply(&mut hidden, 32, 32, &json!({"threshold":60})).unwrap();
        assert!(
            hidden
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 0, 0, 128])
        );
        let mut glowing = source;
        apply(&mut glowing, 32, 32, &json!({"glow":60,"blackLevel":10})).unwrap();
        assert!(
            glowing
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[3] == 128 && p[..3].iter().all(|&v| v <= 128))
        );
        assert!(glowing.as_chunks::<4>().0.iter().any(|p| p[0] > 0));
        assert!(apply(&mut [], 0, 0, &json!({})).is_ok());
        assert!(apply(&mut [0; 3], 1, 1, &json!({})).is_err());
    }

    #[test]
    fn scanlines_matches_original_c_golden_outputs() {
        // FNV-1a checksums from the unmodified C pixel arithmetic on this fixture.
        let source = (0..17 * 19)
            .flat_map(|i| {
                let alpha = [0, 17, 85, 128, 255][i % 5];
                [
                    (i * 37 + 11) % (alpha + 1),
                    (i * 17 + 5) % (alpha + 1),
                    (i * 53 + 3) % (alpha + 1),
                    alpha,
                ]
                .map(|v| v as u8)
            })
            .collect::<Vec<_>>();
        for (settings, expected) in [
            (json!({}), 0xb30f6a4421369836_u64),
            (
                json!({"lineSpacing":3,"thickness":95,"dots":90,"wobble":6,"displace":-27,"smoothness":100,"threshold":4,"split":1,"density":45,"contrast":-30,"blackLevel":31,"colors":"Original"}),
                0x61261288ab70fc69,
            ),
            (
                json!({"lineSpacing":16,"displace":40,"smoothness":0,"colors":"Two Colors","dark":{"red":0.1,"green":0.2,"blue":0.3},"light":{"red":0.9,"green":0.7,"blue":0.4}}),
                0x3c1f31f8e6c66770,
            ),
        ] {
            let mut pixels = source.clone();
            apply(&mut pixels, 17, 19, &settings).unwrap();
            let checksum = pixels.iter().fold(0xcbf29ce484222325_u64, |hash, value| {
                (hash ^ u64::from(*value)).wrapping_mul(0x100000001b3)
            });
            assert_eq!(checksum, expected);
        }
    }
}
