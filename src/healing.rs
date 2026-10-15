//! Rust translation of Rendering/HealPixels.c, operating on premultiplied RGBA8.
use crate::pixels;

const RING: u8 = 1;
const HOLE: u8 = 2;

fn solve(value: &mut [f32], role: &[u8], w: usize, h: usize, depth: u32) {
    let mut iterations = 300;
    if w > 32 && h > 32 && depth < 16 {
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        let mut coarse = vec![0.; cw * ch * 4];
        let mut coarse_role = vec![0; cw * ch];
        for y in 0..ch {
            for x in 0..cw {
                let mut known = 0;
                let mut hole = 0;
                let mut ks = [0.; 4];
                let mut hs = [0.; 4];
                for j in 0..2 {
                    for i in 0..2 {
                        let fx = x * 2 + i;
                        let fy = y * 2 + j;
                        if fx >= w || fy >= h {
                            continue;
                        }
                        let p = fy * w + fx;
                        if role[p] == RING {
                            known += 1;
                            for c in 0..4 {
                                ks[c] += value[p * 4 + c];
                            }
                        } else if role[p] == HOLE {
                            hole += 1;
                            for c in 0..4 {
                                hs[c] += value[p * 4 + c];
                            }
                        }
                    }
                }
                let q = y * cw + x;
                if known > 0 {
                    coarse_role[q] = RING;
                    for c in 0..4 {
                        coarse[q * 4 + c] = ks[c] / known as f32;
                    }
                } else if hole > 0 {
                    coarse_role[q] = HOLE;
                    for c in 0..4 {
                        coarse[q * 4 + c] = hs[c] / hole as f32;
                    }
                }
            }
        }
        solve(&mut coarse, &coarse_role, cw, ch, depth + 1);
        for y in 0..h {
            for x in 0..w {
                let p = y * w + x;
                let q = y / 2 * cw + x / 2;
                if role[p] == HOLE && coarse_role[q] == HOLE {
                    value[p * 4..p * 4 + 4].copy_from_slice(&coarse[q * 4..q * 4 + 4]);
                }
            }
        }
        iterations = 40;
    }
    for _ in 0..iterations {
        for y in 0..h {
            for x in 0..w {
                let p = y * w + x;
                if role[p] != HOLE {
                    continue;
                }
                let mut sum = [0.; 4];
                let mut n = 0;
                for (nx, ny) in [
                    (x as i64 - 1, y as i64),
                    (x as i64 + 1, y as i64),
                    (x as i64, y as i64 - 1),
                    (x as i64, y as i64 + 1),
                ] {
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let q = ny as usize * w + nx as usize;
                    if role[q] == 0 {
                        continue;
                    }
                    for c in 0..4 {
                        sum[c] += value[q * 4 + c];
                    }
                    n += 1;
                }
                if n > 0 {
                    for c in 0..4 {
                        value[p * 4 + c] += 1.8 * (sum[c] / n as f32 - value[p * 4 + c]);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn score(
    rgba: &[u8],
    role: &[u8],
    wx: usize,
    wy: usize,
    ww: usize,
    wh: usize,
    dx: i64,
    dy: i64,
    w: usize,
    h: usize,
) -> f64 {
    if (dx.unsigned_abs() as usize) < ww && (dy.unsigned_abs() as usize) < wh {
        return f64::INFINITY;
    }
    if wx as i64 + dx < 0
        || wy as i64 + dy < 0
        || wx as i64 + ww as i64 + dx > w as i64
        || wy as i64 + wh as i64 + dy > h as i64
    {
        return f64::INFINITY;
    }
    let mut sum = 0.;
    let mut n = 0;
    for y in 0..wh {
        for x in 0..ww {
            if role[y * ww + x] != RING {
                continue;
            }
            let t = ((wy + y) * w + wx + x) * 4;
            let s = (((wy + y) as i64 + dy) as usize * w + ((wx + x) as i64 + dx) as usize) * 4;
            for c in 0..4 {
                let d = rgba[t + c] as f64 - rgba[s + c] as f64;
                sum += d * d;
            }
            n += 1;
        }
    }
    if n > 0 { sum / n as f64 } else { f64::INFINITY }
}
fn unit(key: u32) -> f64 {
    (pixels::hash32(key) >> 8) as f64 / 16777216.
}

/// mode 0 Content-Aware, 1 Create Texture, 2 Proximity Match, matching the original C API.
pub fn spot_heal(
    rgba: &mut [u8],
    coverage: &[u8],
    w: usize,
    h: usize,
    opacity: f32,
    mode: i32,
    seed: u32,
) {
    assert_eq!(rgba.len(), w * h * 4);
    assert_eq!(coverage.len(), w * h);
    let b = pixels::coverage_bounds(coverage, w, h);
    if b[2] <= b[0] {
        return;
    }
    let ring = ((b[2] - b[0]).max(b[3] - b[1]) / 8).clamp(2, 16);
    let wx = b[0].saturating_sub(ring);
    let wy = b[1].saturating_sub(ring);
    let x1 = (b[2] + ring).min(w);
    let y1 = (b[3] + ring).min(h);
    let ww = x1 - wx;
    let wh = y1 - wy;
    let n = ww * wh;
    let mut role = vec![0; n];
    let mut near = vec![0; n];
    let mut prefix = vec![0; ww.max(wh) + 1];
    let mut value = vec![0.; n * 4];
    for y in 0..wh {
        for x in 0..ww {
            role[y * ww + x] = if coverage[(wy + y) * w + wx + x] > 0 {
                HOLE
            } else {
                0
            };
        }
    }
    for y in 0..wh {
        prefix[0] = 0;
        for x in 0..ww {
            prefix[x + 1] = prefix[x] + usize::from(role[y * ww + x] == HOLE);
        }
        for x in 0..ww {
            near[y * ww + x] =
                u8::from(prefix[(x + ring + 1).min(ww)] > prefix[x.saturating_sub(ring)]);
        }
    }
    for x in 0..ww {
        prefix[0] = 0;
        for y in 0..wh {
            prefix[y + 1] = prefix[y] + near[y * ww + x] as usize;
        }
        for y in 0..wh {
            if role[y * ww + x] == 0
                && prefix[(y + ring + 1).min(wh)] > prefix[y.saturating_sub(ring)]
            {
                role[y * ww + x] = RING;
            }
        }
    }
    let ring_count = role.iter().filter(|&&r| r == RING).count();
    if ring_count == 0 {
        return;
    }
    let mut ox = 0;
    let mut oy = 0;
    let mut have_source = false;
    if mode != 1 {
        let factors = [1.05, 1.35, 1.75, 2.25, 2.8];
        let count = if mode == 2 { 2 } else { 5 };
        let mut best = f64::INFINITY;
        for (f, &factor) in factors.iter().take(count).enumerate() {
            for a in 0..24 {
                let angle = a as f64 * std::f64::consts::PI / 12.;
                let dx = (angle.cos() * factor * ww as f64).round() as i64;
                let dy = (angle.sin() * factor * wh as f64).round() as i64;
                let s = score(rgba, &role, wx, wy, ww, wh, dx, dy, w, h)
                    * (1. + if mode == 2 { 0.6 } else { 0.1 } * f as f64);
                if s < best {
                    best = s;
                    ox = dx;
                    oy = dy;
                }
            }
        }
        if best.is_finite() {
            let cx = ox;
            let cy = oy;
            let mut refined = score(rgba, &role, wx, wy, ww, wh, cx, cy, w, h);
            for j in -3..=3 {
                for i in -3..=3 {
                    let s = score(rgba, &role, wx, wy, ww, wh, cx + i, cy + j, w, h);
                    if s < refined {
                        refined = s;
                        ox = cx + i;
                        oy = cy + j;
                    }
                }
            }
            have_source = true;
        }
    }
    let mut mean = [0.; 4];
    let mut detail = [0.; 3];
    for y in 0..wh {
        for x in 0..ww {
            let p = y * ww + x;
            if role[p] != RING {
                continue;
            }
            let ix = wx + x;
            let iy = wy + y;
            let t = (iy * w + ix) * 4;
            let s = if have_source {
                Some((((iy as i64 + oy) as usize) * w + (ix as i64 + ox) as usize) * 4)
            } else {
                None
            };
            for c in 0..4 {
                value[p * 4 + c] = rgba[t + c] as f32 - s.map_or(0., |s| rgba[s + c] as f32);
                mean[c] += value[p * 4 + c] as f64;
            }
            if !have_source {
                for c in 0..3 {
                    let mut around = 0.;
                    let mut count = 0;
                    for (xx, yy) in [
                        (ix as i64 - 1, iy as i64),
                        (ix as i64 + 1, iy as i64),
                        (ix as i64, iy as i64 - 1),
                        (ix as i64, iy as i64 + 1),
                    ] {
                        if xx >= 0 && yy >= 0 && xx < w as i64 && yy < h as i64 {
                            around += rgba[(yy as usize * w + xx as usize) * 4 + c] as f64;
                            count += 1;
                        }
                    }
                    if count > 0 {
                        let d = rgba[t + c] as f64 - around / count as f64;
                        detail[c] += d * d;
                    }
                }
            }
        }
    }
    for m in &mut mean {
        *m /= ring_count as f64;
    }
    for p in 0..n {
        if role[p] == HOLE {
            for c in 0..4 {
                value[p * 4 + c] = mean[c] as f32;
            }
        }
    }
    solve(&mut value, &role, ww, wh, 0);
    for d in &mut detail {
        *d = (*d / ring_count as f64).sqrt() * 0.9;
    }
    for y in 0..wh {
        for x in 0..ww {
            let p = y * ww + x;
            if role[p] != HOLE {
                continue;
            }
            let ix = wx + x;
            let iy = wy + y;
            let t = (iy * w + ix) * 4;
            let s = if have_source {
                Some((((iy as i64 + oy) as usize) * w + (ix as i64 + ox) as usize) * 4)
            } else {
                None
            };
            let amount = coverage[iy * w + ix] as f64 / 255. * opacity as f64;
            let grain = if !have_source {
                let key = pixels::hash32(seed ^ pixels::hash32((iy * w + ix) as u32));
                (-2. * (1. - unit(key)).ln()).sqrt()
                    * (std::f64::consts::TAU * unit(key ^ 0x68e31da4)).cos()
            } else {
                0.
            };
            let out: [f64; 4] = std::array::from_fn(|c| {
                let healed = s.map_or(0., |s| rgba[s + c] as f64)
                    + value[p * 4 + c] as f64
                    + if c < 3 { grain * detail[c] } else { 0. };
                rgba[t + c] as f64 + (healed - rgba[t + c] as f64) * amount
            });
            rgba[t + 3] = out[3].clamp(0., 255.).round() as u8;
            for c in 0..3 {
                rgba[t + c] = out[c].clamp(0., rgba[t + 3] as f64).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn healing_restores_a_spot_without_touching_background() {
        for mode in 0..3 {
            let mut p = [120, 80, 60, 255].repeat(64 * 64);
            for y in 28..36 {
                for x in 28..36 {
                    p[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
                }
            }
            let mut s = vec![0; 64 * 64];
            for y in 28..36 {
                for x in 28..36 {
                    s[y * 64 + x] = 255;
                }
            }
            spot_heal(&mut p, &s, 64, 64, 1., mode, 1);
            let center = &p[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 4];
            if mode == 1 {
                assert!(center[0] < 180 && center[1] > 40 && center[2] > 30);
                assert_eq!(center[3], 255);
            } else {
                assert_eq!(center, &[120, 80, 60, 255]);
            }
            assert_eq!(&p[..4], &[120, 80, 60, 255]);
        }
    }
    #[test]
    fn empty_or_full_coverage_has_no_ring_and_does_not_change_pixels() {
        let original = [80, 60, 40, 128].repeat(3 * 3);
        for coverage in [vec![0; 9], vec![255; 9]] {
            let mut p = original.clone();
            spot_heal(&mut p, &coverage, 3, 3, 1., 0, 0);
            assert_eq!(p, original);
        }
    }
}
