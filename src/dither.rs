use super::{boolean, color, number, string};
use crate::pixels;
use serde_json::Value;

const STYLES: [&str; 11] = [
    "Atkinson (Classic Mac)",
    "Floyd–Steinberg",
    "Bayer 2 × 2",
    "Bayer 4 × 4",
    "Bayer 8 × 8",
    "Halftone Dots",
    "Halftone Lines",
    "Halftone Diamonds",
    "Mac Patterns",
    "ASCII",
    "Scanlines (CRT)",
];
const BAYER8: [u8; 64] = [
    0, 32, 8, 40, 2, 34, 10, 42, 48, 16, 56, 24, 50, 18, 58, 26, 12, 44, 4, 36, 14, 46, 6, 38, 60,
    28, 52, 20, 62, 30, 54, 22, 3, 35, 11, 43, 1, 33, 9, 41, 51, 19, 59, 27, 49, 17, 57, 25, 15,
    47, 7, 39, 13, 45, 5, 37, 63, 31, 55, 23, 61, 29, 53, 21,
];
const PATTERNS: [[u8; 8]; 17] = [
    [0; 8],
    [0x80, 0, 0, 0, 8, 0, 0, 0],
    [0x88, 0, 0x22, 0, 0x88, 0, 0x22, 0],
    [128, 64, 32, 16, 8, 4, 2, 1],
    [0x88, 0x22, 0x88, 0x22, 0x88, 0x22, 0x88, 0x22],
    [0, 255, 0, 0, 0, 255, 0, 0],
    [0x11, 0x22, 0x44, 0x88, 0x11, 0x22, 0x44, 0x88],
    [0xaa, 0, 0xaa, 0, 0xaa, 0, 0xaa, 0],
    [0x88, 0x55, 0x22, 0x55, 0x88, 0x55, 0x22, 0x55],
    [255, 128, 128, 128, 255, 8, 8, 8],
    [0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55],
    [0x81, 0x42, 0x24, 0x18, 0x18, 0x24, 0x42, 0x81],
    [0x77, 0xaa, 0xdd, 0xaa, 0x77, 0xaa, 0xdd, 0xaa],
    [0xee, 0xdd, 0xbb, 0x77, 0xee, 0xdd, 0xbb, 0x77],
    [0x77, 255, 0xdd, 255, 0x77, 255, 0xdd, 255],
    [0x7f, 255, 255, 255, 0xf7, 255, 255, 255],
    [255; 8],
];
fn write(p: &mut [u8], c: [f32; 3]) {
    let a = p[3] as f32;
    for i in 0..3 {
        p[i] = (c[i].clamp(0.0, 1.0) * a).round() as u8;
    }
}

pub fn apply(p: &mut [u8], w: usize, h: usize, s: &Value) -> Result<(), String> {
    let style = STYLES
        .iter()
        .position(|&x| x == string(s, "style", STYLES[0]))
        .ok_or("未知抖色样式")?;
    let block = if style >= 9 {
        1
    } else {
        number(s, "pixelSize", 2.0).round().clamp(1.0, 32.0) as usize
    };
    let sw = w.div_ceil(block);
    let sh = h.div_ceil(block);
    let mut small = vec![0u8; sw * sh * 4];
    for y in 0..sh {
        for x in 0..sw {
            let mut sum = [0u64; 4];
            let mut n = 0u64;
            for sy in y * block..((y + 1) * block).min(h) {
                for sx in x * block..((x + 1) * block).min(w) {
                    for c in 0..4 {
                        sum[c] += p[(sy * w + sx) * 4 + c] as u64;
                    }
                    n += 1;
                }
            }
            for c in 0..4 {
                small[(y * sw + x) * 4 + c] = ((sum[c] + n / 2) / n.max(1)) as u8;
            }
        }
    }
    kernel(&mut small, sw, sh, s, style)?;
    let dot = string(s, "pixelShape", "Square") == "Dot";
    let dark = color(&s["dark"], [0.0; 3]);
    for y in 0..h {
        for x in 0..w {
            let olda = p[(y * w + x) * 4 + 3];
            let q = &small[((y / block) * sw + x / block) * 4..];
            let mut c = std::array::from_fn(|i| {
                if q[3] == 0 {
                    0.0
                } else {
                    q[i] as f32 / q[3] as f32
                }
            });
            if dot && block > 1 {
                let middle = block as f32 / 2.0;
                let dx = (x % block) as f32 + 0.5 - middle;
                let dy = (y % block) as f32 + 0.5 - middle;
                let coverage = (block as f32 * 0.42 - dx.hypot(dy) + 0.5).clamp(0.0, 1.0);
                for i in 0..3 {
                    c[i] = c[i] * coverage + dark[i] as f32 * (1.0 - coverage);
                }
            }
            let out = &mut p[(y * w + x) * 4..(y * w + x + 1) * 4];
            out[3] = olda;
            write(out, c);
        }
    }
    if style == 10 {
        let amount = number(s, "glow", 35.0) / 100.0;
        if amount > 0.0 {
            let glow = pixels::gaussian_blur(p, w, h, number(s, "lineSpacing", 4.0) * 3.0 + 3.0);
            for (q, g) in p
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(glow.as_chunks::<4>().0.iter())
            {
                for c in 0..3 {
                    q[c] = (q[c] as f64 + g[c] as f64 * amount * q[3] as f64 / 255.0)
                        .round()
                        .min(q[3] as f64) as u8;
                }
            }
        }
    }
    Ok(())
}
fn threshold(style: usize, x: usize, y: usize) -> f32 {
    match style {
        2 => {
            let m = [0, 2, 3, 1];
            (m[(y & 1) * 2 + (x & 1)] as f32 + 0.5) / 4.0
        }
        3 => {
            let m = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
            (m[(y & 3) * 4 + (x & 3)] as f32 + 0.5) / 16.0
        }
        _ => (BAYER8[(y & 7) * 8 + (x & 7)] as f32 + 0.5) / 64.0,
    }
}
pub fn kernel(p: &mut [u8], w: usize, h: usize, s: &Value, style: usize) -> Result<(), String> {
    let count = w * h;
    if count == 0 {
        return Ok(());
    }
    let original = string(s, "colors", "Black & White") == "Original";
    let planes = if original { 3 } else { 1 };
    let mut tone = vec![0f32; count * planes];
    let mut source = vec![[0.0f32; 3]; count];
    let gamma = 2f32.powf(number(s, "density", 0.0) as f32 / 100.0 * 1.5);
    let contrast = number(s, "contrast", 0.0) as f32 / 100.0;
    let contrast = if contrast >= 0.0 {
        1.0 / (1.0 - 0.95 * contrast)
    } else {
        1.0 + contrast
    };
    let adjust = |v: f32| ((v.clamp(0.0, 1.0).powf(gamma) - 0.5) * contrast + 0.5).clamp(0.0, 1.0);
    let alpha = pixels::extract_alpha(p);
    for (i, q) in p.as_chunks::<4>().0.iter().enumerate() {
        if q[3] > 0 {
            source[i] = std::array::from_fn(|c| q[c] as f32 / q[3] as f32);
        }
        if original {
            for c in 0..3 {
                tone[c * count + i] = adjust(source[i][c]);
            }
        } else {
            tone[i] = adjust(0.2126 * source[i][0] + 0.7152 * source[i][1] + 0.0722 * source[i][2]);
        }
    }
    let two = string(s, "colors", "Black & White") == "Two Colors";
    let dark = if two {
        color(&s["dark"], [0.0; 3]).map(|v| v as f32)
    } else {
        [0.0; 3]
    };
    let light = if two {
        color(&s["light"], [1.0; 3]).map(|v| v as f32)
    } else {
        [1.0; 3]
    };
    let levels = number(s, "levels", 2.0).round().clamp(2.0, 8.0) as f32;
    let steps = levels - 1.0;
    if style <= 4 {
        if style <= 1 {
            let atkinson = [
                (1, 0, 1.0),
                (2, 0, 1.0),
                (-1, 1, 1.0),
                (0, 1, 1.0),
                (1, 1, 1.0),
                (0, 2, 1.0),
            ];
            let floyd = [(1, 0, 7.0), (-1, 1, 3.0), (0, 1, 5.0), (1, 1, 1.0)];
            let taps = &if style == 0 {
                &atkinson[..]
            } else {
                &floyd[..]
            };
            let divisor = if style == 0 { 8.0 } else { 16.0 };
            let diffusion = number(s, "diffusion", 100.0) as f32 / 100.0;
            for c in 0..planes {
                let plane = &mut tone[c * count..(c + 1) * count];
                for y in 0..h {
                    let reverse = y & 1 != 0;
                    for i in 0..w {
                        let x = if reverse { w - 1 - i } else { i };
                        let at = y * w + x;
                        if alpha[at] == 0 {
                            continue;
                        }
                        let old = plane[at];
                        let q = (old.clamp(0.0, 1.0) * steps).round() / steps;
                        plane[at] = q;
                        let error = (old - q) * diffusion / divisor;
                        for &(dx, dy, weight) in *taps {
                            let nx = x as i64 + if reverse { -dx } else { dx };
                            let ny = y as i64 + dy;
                            if nx >= 0 && nx < w as i64 && ny < h as i64 {
                                plane[ny as usize * w + nx as usize] += error * weight;
                            }
                        }
                    }
                }
            }
        } else {
            for c in 0..planes {
                for y in 0..h {
                    for x in 0..w {
                        let i = c * count + y * w + x;
                        tone[i] = (tone[i].clamp(0.0, 1.0) * steps + threshold(style, x, y))
                            .floor()
                            .min(steps)
                            / steps;
                    }
                }
            }
        }
        for (i, q) in p.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            if alpha[i] == 0 {
                continue;
            }
            let out = if original {
                std::array::from_fn(|c| tone[c * count + i])
            } else {
                std::array::from_fn(|c| dark[c] + (light[c] - dark[c]) * tone[i])
            };
            write(q, out);
        }
    } else if style == 10 {
        let spacing = number(s, "lineSpacing", 4.0).round().clamp(2.0, 32.0) as usize;
        let middle = spacing as f32 / 2.0;
        let dots = number(s, "dots", 0.0) as f32 / 100.0;
        for line in 0..h.div_ceil(spacing) {
            let top = line * spacing;
            let bottom = (top + spacing).min(h);
            let wave = (line as f32 * 0.45).sin() * 0.7 + (line as f32 * 1.7 + 1.3).sin() * 0.3;
            let shift = (number(s, "wobble", 0.0) as f32 * wave).round() as i64;
            let mut scan = vec![0.0f32; w * planes];
            for x in 0..w {
                let sx = x as i64 - shift;
                if sx < 0 || sx >= w as i64 {
                    continue;
                }
                let mut n = 0;
                for y in top..bottom {
                    if alpha[y * w + sx as usize] == 0 {
                        continue;
                    }
                    for c in 0..planes {
                        scan[c * w + x] += tone[c * count + y * w + sx as usize];
                    }
                    n += 1;
                }
                if n > 0 {
                    for c in 0..planes {
                        scan[c * w + x] /= n as f32;
                    }
                }
            }
            for y in top..bottom {
                let offset = ((y - top) as f32 + 0.5 - middle).abs();
                for x in 0..w {
                    if alpha[y * w + x] == 0 {
                        continue;
                    }
                    let along = (x as f32 + 0.5) % spacing as f32 - middle;
                    let at = (x as f32 - along * dots).round().clamp(0.0, (w - 1) as f32) as usize;
                    let c: [f32; 3] = if original {
                        std::array::from_fn(|c| scan[c * w + at])
                    } else {
                        std::array::from_fn(|c| dark[c] + (light[c] - dark[c]) * scan[at])
                    };
                    let t = if original {
                        0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
                    } else {
                        scan[at]
                    };
                    let beam = middle * (0.2 + 0.5 * t.clamp(0.0, 1.0).sqrt());
                    let cover = (beam - offset.hypot(along * dots) + 0.5).clamp(0.0, 1.0);
                    write(
                        &mut p[(y * w + x) * 4..],
                        std::array::from_fn(|i| {
                            let bg = if original { 0.0 } else { dark[i] };
                            bg + (c[i] * 1.35 - bg) * cover
                        }),
                    );
                }
            }
        }
    } else {
        let marks: Vec<f32> = (0..count)
            .map(|i| {
                if original {
                    0.2126 * tone[i] + 0.7152 * tone[count + i] + 0.0722 * tone[2 * count + i]
                } else {
                    tone[i]
                }
            })
            .collect();
        let light_on_dark = boolean(s, "lightOnDark", true);
        let (ink, paper) = if light_on_dark {
            (light, dark)
        } else {
            (dark, light)
        };
        let cell = number(s, "cellSize", 8.0).round().max(2.0) as f32;
        let angle = number(s, "angle", 45.0).to_radians() as f32;
        let glyphs = if style == 9 {
            Some(make_glyphs(
                string(s, "characters", " .:-=+*#%@"),
                number(s, "textSize", 14.0).round().clamp(6.0, 64.0) as u32,
            )?)
        } else {
            None
        };
        let mut picked = Vec::new();
        if let Some((gw, gh, glyphs)) = &glyphs {
            let columns = w.div_ceil(*gw);
            picked = vec![0; columns * h.div_ceil(*gh)];
            let max = glyphs.last().map_or(1.0, |x| x.1);
            for row in 0..h.div_ceil(*gh) {
                for col in 0..columns {
                    let mut sum = 0.0;
                    let mut n = 0;
                    for y in row * gh..((row + 1) * gh).min(h) {
                        for x in col * gw..((col + 1) * gw).min(w) {
                            if alpha[y * w + x] != 0 {
                                sum += marks[y * w + x];
                                n += 1;
                            }
                        }
                    }
                    let t = if n == 0 { 1.0 } else { sum / n as f32 };
                    let wanted = if light_on_dark { t } else { 1.0 - t } * max;
                    picked[row * columns + col] = glyphs
                        .iter()
                        .enumerate()
                        .min_by(|(_, a), (_, b)| {
                            (a.1 - wanted).abs().total_cmp(&(b.1 - wanted).abs())
                        })
                        .map_or(0, |(i, _)| i);
                }
            }
        }
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if alpha[i] == 0 {
                    continue;
                }
                let amount = if let Some((gw, gh, glyphs)) = &glyphs {
                    let which = picked[(y / gh) * w.div_ceil(*gw) + x / gw];
                    glyphs[which].0[(y % gh) * gw + x % gw] as f32 / 255.0
                } else if style == 8 {
                    let coverage = if light_on_dark {
                        marks[i]
                    } else {
                        1.0 - marks[i]
                    };
                    let idx = (coverage * 16.0).round().clamp(0.0, 16.0) as usize;
                    ((PATTERNS[idx][y & 7] >> (7 - (x & 7))) & 1) as f32
                } else {
                    let fx = x as f32 + 0.5;
                    let fy = y as f32 + 0.5;
                    let u = (fx * angle.cos() + fy * angle.sin()) / cell;
                    let v = (-fx * angle.sin() + fy * angle.cos()) / cell;
                    let u = u - u.floor() - 0.5;
                    let v = v - v.floor() - 0.5;
                    let spot = if style == 5 {
                        std::f32::consts::PI * (u * u + v * v)
                    } else if style == 6 {
                        v.abs() * 2.0
                    } else {
                        u.abs() + v.abs()
                    };
                    if if light_on_dark {
                        marks[i]
                    } else {
                        1.0 - marks[i]
                    } > spot
                    {
                        1.0
                    } else {
                        0.0
                    }
                };
                let out = if original {
                    let paper = if light_on_dark { 0.0 } else { 1.0 };
                    source[i].map(|v| paper + (v - paper) * amount)
                } else {
                    std::array::from_fn(|c| paper[c] + (ink[c] - paper[c]) * amount)
                };
                write(&mut p[i * 4..], out);
            }
        }
    }
    Ok(())
}
type Glyphs = (usize, usize, Vec<(Vec<u8>, f32)>);
fn make_glyphs(characters: &str, size: u32) -> Result<Glyphs, String> {
    use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache};
    let mut fonts = FontSystem::new();
    let mut cache = SwashCache::new();
    let gw = (size as f32 * 0.6).round().max(1.0) as usize;
    let gh = size as usize;
    let mut glyphs = Vec::new();
    for ch in characters.chars().filter(|c| !c.is_control()).take(64) {
        let mut buffer = Buffer::new(&mut fonts, Metrics::new(size as f32, size as f32));
        buffer.set_size(&mut fonts, Some(gw as f32), Some(gh as f32));
        let text = ch.to_string();
        buffer.set_text(
            &mut fonts,
            &text,
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
        );
        buffer.shape_until_scroll(&mut fonts, false);
        let mut coverage = vec![0; gw * gh];
        buffer.draw(
            &mut fonts,
            &mut cache,
            Color::rgb(255, 255, 255),
            |x, y, w, h, c| {
                for yy in y..y + h as i32 {
                    for xx in x..x + w as i32 {
                        if xx >= 0 && yy >= 0 && (xx as usize) < gw && (yy as usize) < gh {
                            coverage[yy as usize * gw + xx as usize] = c.a();
                        }
                    }
                }
            },
        );
        let mean =
            coverage.iter().map(|&v| v as f32).sum::<f32>() / (255.0 * coverage.len() as f32);
        glyphs.push((coverage, mean));
    }
    if glyphs.is_empty() {
        return Err("ASCII 抖色至少需要一个可打印字符".into());
    }
    glyphs.sort_by(|a, b| a.1.total_cmp(&b.1));
    Ok((gw, gh, glyphs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn diffusion_preserves_alpha_and_binary_tones() {
        for style in 0..2 {
            let mut p = [64, 64, 64, 128].repeat(49);
            kernel(&mut p, 7, 7, &json!({}), style).unwrap();
            assert!(
                p.as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| p[3] == 128 && (p[0] == 0 || p[0] == 128))
            );
        }
    }
    #[test]
    fn every_nonfont_style_executes() {
        for style in (0..11).filter(|&s| s != 9) {
            let mut p = [127, 64, 33, 255].repeat(64);
            kernel(&mut p, 8, 8, &Value::Null, style).unwrap();
            assert!(p.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        }
    }
}
