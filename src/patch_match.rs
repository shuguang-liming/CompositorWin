//! Multi-scale 7×7 PatchMatch with texture channels and overlapping patch votes.
//! Port of Compositor 1.5.3 PatchPixels.c, Copyright (c) 2026 Wonder Assembly LLC
//! (MIT; attribution and original license retained in LICENSES.md).
use anyhow::{Context, Result};
use rayon::prelude::*;

const RADIUS: usize = 3;
const CHANNELS: usize = 6;
type Color = [f32; CHANNELS];

fn allocate<T: Clone>(length: usize, value: T) -> Result<Vec<T>> {
    let mut data = Vec::new();
    data.try_reserve_exact(length)
        .context("内容识别填充缓冲区内存不足")?;
    data.resize(length, value);
    Ok(data)
}

fn random(state: &mut u32) -> u32 {
    let mut value = *state;
    value ^= value << 13;
    value ^= value >> 17;
    value ^= value << 5;
    *state = if value == 0 { 0x9e3779b9 } else { value };
    *state
}

struct Level {
    width: usize,
    height: usize,
    color: Vec<Color>,
    hole: Vec<u8>,
    known: Vec<u8>,
    source: Vec<bool>,
    target: Vec<bool>,
    counts: Vec<u8>,
    matches: Vec<usize>,
    distance: Vec<f32>,
    sources: Vec<usize>,
}

impl Level {
    fn new(width: usize, height: usize) -> Result<Self> {
        let n = width.checked_mul(height).context("内容识别填充尺寸溢出")?;
        Ok(Self {
            width,
            height,
            color: allocate(n, [0.; CHANNELS])?,
            hole: allocate(n, 0)?,
            known: allocate(n, 0)?,
            source: allocate(n, false)?,
            target: allocate(n, false)?,
            counts: allocate(n, 0)?,
            matches: allocate(n, 0)?,
            distance: allocate(n, f32::MAX)?,
            sources: Vec::new(),
        })
    }

    fn classify(&mut self) -> Result<()> {
        let (w, h) = (self.width, self.height);
        let stride = w + 1;
        let mut known = allocate(stride * (h + 1), 0_i64)?;
        let mut holes = allocate(stride * (h + 1), 0_i64)?;
        for y in 0..h {
            let mut row_known = 0;
            let mut row_holes = 0;
            for x in 0..w {
                row_known += i64::from(self.known[y * w + x]);
                row_holes += i64::from(self.hole[y * w + x]);
                known[(y + 1) * stride + x + 1] = known[y * stride + x + 1] + row_known;
                holes[(y + 1) * stride + x + 1] = holes[y * stride + x + 1] + row_holes;
            }
        }
        let sum = |sat: &[i64], x0, y0, x1, y1| {
            sat[y1 * stride + x1] - sat[y0 * stride + x1] - sat[y1 * stride + x0]
                + sat[y0 * stride + x0]
        };
        self.sources.clear();
        self.sources
            .try_reserve(w * h)
            .context("内容识别来源列表内存不足")?;
        for y in 0..h {
            for x in 0..w {
                let p = y * w + x;
                let x0 = x.saturating_sub(RADIUS);
                let y0 = y.saturating_sub(RADIUS);
                let x1 = (x + RADIUS + 1).min(w);
                let y1 = (y + RADIUS + 1).min(h);
                let missing = sum(&holes, x0, y0, x1, y1);
                self.target[p] = missing > 0;
                self.counts[p] = (missing + sum(&known, x0, y0, x1, y1)) as u8;
                self.source[p] = x >= RADIUS
                    && y >= RADIUS
                    && x + RADIUS < w
                    && y + RADIUS < h
                    && sum(&known, x0, y0, x1, y1) == 49;
                if self.source[p] {
                    self.sources.push(p);
                }
            }
        }
        Ok(())
    }

    fn texture(&mut self) -> Result<()> {
        let (w, h) = (self.width, self.height);
        let stride = w + 1;
        let mut edge = allocate(w * h, [0_f32; 2])?;
        let mut sat = allocate(stride * (h + 1), [0_f64; 3])?;
        let luma = |p: usize| (self.color[p][0] + self.color[p][1] + self.color[p][2]) / 3.;
        for y in 0..h {
            for x in 0..w {
                let p = y * w + x;
                if self.known[p] == 0 {
                    continue;
                }
                if x > 0 && x + 1 < w && self.known[p - 1] != 0 && self.known[p + 1] != 0 {
                    edge[p][0] = (luma(p + 1) - luma(p - 1)).abs() / 2.;
                }
                if y > 0 && y + 1 < h && self.known[p - w] != 0 && self.known[p + w] != 0 {
                    edge[p][1] = (luma(p + w) - luma(p - w)).abs() / 2.;
                }
            }
        }
        for y in 0..h {
            let mut row = [0.; 3];
            for x in 0..w {
                let p = y * w + x;
                row[0] += f64::from(edge[p][0]);
                row[1] += f64::from(edge[p][1]);
                row[2] += f64::from(self.known[p]);
                for (c, &value) in row.iter().enumerate() {
                    sat[(y + 1) * stride + x + 1][c] = sat[y * stride + x + 1][c] + value;
                }
            }
        }
        for y in 0..h {
            for x in 0..w {
                let p = y * w + x;
                if self.known[p] == 0 {
                    continue;
                }
                let (x0, y0, x1, y1) = (
                    x.saturating_sub(2),
                    y.saturating_sub(2),
                    (x + 3).min(w),
                    (y + 3).min(h),
                );
                let values: [f64; 3] = std::array::from_fn(|c| {
                    sat[y1 * stride + x1][c] - sat[y0 * stride + x1][c] - sat[y1 * stride + x0][c]
                        + sat[y0 * stride + x0][c]
                });
                self.color[p][4] = (values[0] / values[2]) as f32 * 6.;
                self.color[p][5] = (values[1] / values[2]) as f32 * 6.;
            }
        }
        Ok(())
    }

    fn downsample(&self) -> Result<Self> {
        let mut coarse = Self::new(self.width.div_ceil(2), self.height.div_ceil(2))?;
        for y in 0..coarse.height {
            for x in 0..coarse.width {
                let mut present = 0;
                let mut known = 0;
                let mut hole = 0;
                let mut all = [0.; CHANNELS];
                let mut sure = [0.; CHANNELS];
                for j in 0..2 {
                    for i in 0..2 {
                        let (fx, fy) = (x * 2 + i, y * 2 + j);
                        if fx >= self.width || fy >= self.height {
                            continue;
                        }
                        let f = fy * self.width + fx;
                        present += 1;
                        known += self.known[f];
                        hole |= self.hole[f];
                        for c in 0..CHANNELS {
                            all[c] += self.color[f][c];
                            if self.known[f] != 0 {
                                sure[c] += self.color[f][c];
                            }
                        }
                    }
                }
                let p = y * coarse.width + x;
                coarse.hole[p] = hole;
                coarse.known[p] = u8::from(hole == 0 && known == present);
                for c in 0..CHANNELS {
                    coarse.color[p][c] = if known > 0 {
                        sure[c] / f32::from(known)
                    } else {
                        all[c] / f32::from(present)
                    };
                }
            }
        }
        coarse.classify()?;
        Ok(coarse)
    }

    fn fill_inward(&mut self) -> Result<()> {
        let (w, h) = (self.width, self.height);
        let mut done = self.known.clone();
        let mut next = allocate(done.len(), 0)?;
        let mut mean = [0_f64; CHANNELS];
        let mut known = 0;
        for (p, &is_known) in self.known.iter().enumerate() {
            if is_known != 0 {
                known += 1;
                for (c, sum) in mean.iter_mut().enumerate() {
                    *sum += f64::from(self.color[p][c]);
                }
            }
        }
        loop {
            let mut changed = false;
            next.copy_from_slice(&done);
            for y in 0..h {
                for x in 0..w {
                    let p = y * w + x;
                    if self.hole[p] == 0 || done[p] != 0 {
                        continue;
                    }
                    let mut sum = [0.; CHANNELS];
                    let mut around = 0;
                    for j in -1_isize..=1 {
                        for i in -1_isize..=1 {
                            let (qx, qy) = (x as isize + i, y as isize + j);
                            if (i == 0 && j == 0)
                                || qx < 0
                                || qy < 0
                                || qx >= w as isize
                                || qy >= h as isize
                            {
                                continue;
                            }
                            let q = qy as usize * w + qx as usize;
                            if done[q] == 0 {
                                continue;
                            }
                            for (c, value) in sum.iter_mut().enumerate() {
                                *value += self.color[q][c];
                            }
                            around += 1;
                        }
                    }
                    if around > 0 {
                        self.color[p] = sum.map(|v| v / around as f32);
                        next[p] = 1;
                        changed = true;
                    }
                }
            }
            done.copy_from_slice(&next);
            if !changed {
                break;
            }
        }
        for (p, &is_done) in done.iter().enumerate() {
            if self.hole[p] != 0 && is_done == 0 {
                self.color[p] = mean.map(|v| {
                    if known > 0 {
                        (v / f64::from(known)) as f32
                    } else {
                        0.
                    }
                });
            }
        }
        Ok(())
    }

    fn random_matches(&mut self, seed: u32) {
        let mut state = seed | 1;
        for p in 0..self.color.len() {
            if self.target[p] {
                self.matches[p] = self.sources[random(&mut state) as usize % self.sources.len()];
            }
        }
    }

    fn upsample(&mut self, coarse: &Self, seed: u32) {
        let mut state = seed | 1;
        for y in 0..self.height {
            for x in 0..self.width {
                let p = y * self.width + x;
                let c = (y / 2) * coarse.width + x / 2;
                if self.hole[p] != 0 {
                    self.color[p] = coarse.color[c];
                }
                if !self.target[p] {
                    continue;
                }
                let mut candidate = None;
                if coarse.target[c] {
                    let m = coarse.matches[c];
                    let mx = 2 * (m % coarse.width) + (x & 1);
                    let my = 2 * (m / coarse.width) + (y & 1);
                    if mx < self.width && my < self.height && self.source[my * self.width + mx] {
                        candidate = Some(my * self.width + mx);
                    }
                }
                self.matches[p] = candidate.unwrap_or_else(|| {
                    self.sources[random(&mut state) as usize % self.sources.len()]
                });
            }
        }
    }

    fn measure(&mut self) {
        let search = Search {
            width: self.width,
            height: self.height,
            color: &self.color,
            hole: &self.hole,
            known: &self.known,
            source: &self.source,
            target: &self.target,
            counts: &self.counts,
        };
        self.distance
            .par_iter_mut()
            .enumerate()
            .for_each(|(p, distance)| {
                if search.target[p] {
                    *distance = search.distance(p, self.matches[p], f32::MAX);
                }
            });
    }

    fn pass(&mut self, forward: bool, seed: u32, before: &[usize]) {
        let (w, h) = (self.width, self.height);
        let bands = if h < 32 { 1 } else { 16 };
        let size = h.div_ceil(bands);
        let search = Search {
            width: w,
            height: h,
            color: &self.color,
            hole: &self.hole,
            known: &self.known,
            source: &self.source,
            target: &self.target,
            counts: &self.counts,
        };
        self.matches
            .par_chunks_mut(size * w)
            .zip(self.distance.par_chunks_mut(size * w))
            .enumerate()
            .for_each(|(band, (matches, distances))| {
                let start = band * size;
                let end = (start + size).min(h);
                let mut state = seed ^ (band as u32).wrapping_mul(0x9e3779b9) ^ 0x85ebca6b;
                let step = if forward { 1_isize } else { -1 };
                for i in 0..end - start {
                    let y = if forward { start + i } else { end - 1 - i };
                    for j in 0..w {
                        let x = if forward { j } else { w - 1 - j };
                        let p = y * w + x;
                        if !search.target[p] {
                            continue;
                        }
                        let local = p - start * w;
                        let mut best = matches[local];
                        let mut best_distance = distances[local];
                        let (nx, ny) = (x as isize - step, y as isize - step);
                        if nx >= 0 && nx < w as isize && search.target[y * w + nx as usize] {
                            let m = matches[y * w + nx as usize - start * w];
                            let mx = (m % w) as isize + step;
                            if mx >= 0 && mx < w as isize {
                                let c = (m as isize + step) as usize;
                                search.consider(p, c, &mut best, &mut best_distance);
                            }
                        }
                        if ny >= 0 && ny < h as isize && search.target[ny as usize * w + x] {
                            let q = ny as usize * w + x;
                            let m = if ny as usize >= start && (ny as usize) < end {
                                matches[q - start * w]
                            } else {
                                before[q]
                            };
                            let my = (m / w) as isize + step;
                            if my >= 0 && my < h as isize {
                                let c = (m as isize + step * w as isize) as usize;
                                search.consider(p, c, &mut best, &mut best_distance);
                            }
                        }
                        let mut reach = w.max(h);
                        while reach >= 1 {
                            let bx = best % w;
                            let by = best / w;
                            let cx = bx as isize
                                + (random(&mut state) as usize % (2 * reach + 1)) as isize
                                - reach as isize;
                            let cy = by as isize
                                + (random(&mut state) as usize % (2 * reach + 1)) as isize
                                - reach as isize;
                            if cx >= 0 && cy >= 0 && cx < w as isize && cy < h as isize {
                                search.consider(
                                    p,
                                    cy as usize * w + cx as usize,
                                    &mut best,
                                    &mut best_distance,
                                );
                            }
                            reach /= 2;
                        }
                        matches[local] = best;
                        distances[local] = best_distance;
                    }
                }
            });
    }

    fn spread(&self, sharpness: f32) -> f32 {
        let mut total = 0_f64;
        let mut count = 0;
        for p in 0..self.color.len() {
            if self.target[p] && self.distance[p] < f32::MAX {
                total += f64::from(self.distance[p]);
                count += 1;
            }
        }
        (2. * (if count > 0 {
            total / f64::from(count)
        } else {
            1.
        }) as f32
            + 1.)
            / sharpness
    }

    fn prediction(&self, x: usize, y: usize, spread: f32) -> Option<Color> {
        let w = self.width;
        let h = self.height;
        let mut sum = [0.; CHANNELS];
        let mut weights = 0.;
        for dy in -(RADIUS as isize)..=RADIUS as isize {
            let py = y as isize - dy;
            if py < 0 || py >= h as isize {
                continue;
            }
            for dx in -(RADIUS as isize)..=RADIUS as isize {
                let px = x as isize - dx;
                if px < 0 || px >= w as isize {
                    continue;
                }
                let p = py as usize * w + px as usize;
                if !self.target[p] || self.distance[p] == f32::MAX {
                    continue;
                }
                let q = self.matches[p];
                let index = ((q / w) as isize + dy) as usize * w + ((q % w) as isize + dx) as usize;
                let weight = (-self.distance[p] / spread).exp();
                for (c, value) in sum.iter_mut().enumerate() {
                    *value += self.color[index][c] * weight;
                }
                weights += weight;
            }
        }
        if weights > 0. {
            Some(sum.map(|v| v / weights))
        } else {
            None
        }
    }

    fn vote(&mut self, sharpness: f32) -> Result<()> {
        let spread = self.spread(sharpness);
        // Donor patches lie entirely in known image, so predictions only read
        // immutable donor pixels. Collect hole updates before writing them.
        let mut updates = Vec::new();
        updates
            .try_reserve_exact(self.hole.iter().filter(|&&v| v != 0).count())
            .context("内容识别投票缓冲区内存不足")?;
        for p in 0..self.color.len() {
            if self.hole[p] != 0
                && let Some(value) = self.prediction(p % self.width, p / self.width, spread)
            {
                updates.push((p, value));
            }
        }
        for (p, value) in updates {
            self.color[p] = value;
        }
        Ok(())
    }

    fn settle(&mut self, iterations: u32, seed: u32) -> Result<()> {
        let mut before = allocate(self.matches.len(), 0)?;
        for i in 0..iterations {
            before.copy_from_slice(&self.matches);
            self.pass(
                i % 2 == 0,
                seed.wrapping_add(i.wrapping_mul(0x632be5ab)),
                &before,
            );
            self.vote(1.)?;
            self.measure();
        }
        Ok(())
    }

    fn adapt_color(&mut self) -> Result<()> {
        let (w, h) = (self.width, self.height);
        let spread = self.spread(1.);
        let mut role = allocate(w * h, 0_u8)?;
        let mut offset = allocate(w * h * 4, 0_f32)?;
        let mut mean = [0_f64; 4];
        let mut edges = 0;
        for y in 0..h {
            for x in 0..w {
                let p = y * w + x;
                if self.hole[p] != 0 {
                    role[p] = 2;
                    continue;
                }
                if self.known[p] == 0 {
                    continue;
                }
                let mut touches = false;
                for j in -1_isize..=1 {
                    for i in -1_isize..=1 {
                        let (qx, qy) = (x as isize + i, y as isize + j);
                        if qx >= 0
                            && qy >= 0
                            && qx < w as isize
                            && qy < h as isize
                            && self.hole[qy as usize * w + qx as usize] != 0
                        {
                            touches = true;
                        }
                    }
                }
                if !touches {
                    continue;
                }
                let Some(predicted) = self.prediction(x, y, spread) else {
                    continue;
                };
                role[p] = 1;
                for c in 0..4 {
                    offset[p * 4 + c] = self.color[p][c] - predicted[c];
                    mean[c] += f64::from(offset[p * 4 + c]);
                }
                edges += 1;
            }
        }
        if edges > 0 {
            for p in 0..w * h {
                if role[p] == 2 {
                    for c in 0..4 {
                        offset[p * 4 + c] = (mean[c] / f64::from(edges)) as f32;
                    }
                }
            }
            crate::brush::healing::solve(&mut offset, &role, w, h, 0);
            for p in 0..w * h {
                if role[p] == 2 {
                    for c in 0..4 {
                        self.color[p][c] += offset[p * 4 + c];
                    }
                }
            }
        }
        Ok(())
    }
}

struct Search<'a> {
    width: usize,
    height: usize,
    color: &'a [Color],
    hole: &'a [u8],
    known: &'a [u8],
    source: &'a [bool],
    target: &'a [bool],
    counts: &'a [u8],
}
impl Search<'_> {
    fn distance(&self, p: usize, q: usize, best: f32) -> f32 {
        let w = self.width;
        let (px, py, qx, qy) = (p % w, p / w, q % w, q / w);
        let limit = if best == f32::MAX {
            f32::MAX
        } else {
            best * f32::from(self.counts[p])
        };
        let mut sum = 0.;
        for dy in -(RADIUS as isize)..=RADIUS as isize {
            let y = py as isize + dy;
            if y < 0 || y >= self.height as isize {
                continue;
            }
            for dx in -(RADIUS as isize)..=RADIUS as isize {
                let x = px as isize + dx;
                if x < 0 || x >= w as isize {
                    continue;
                }
                let a = y as usize * w + x as usize;
                if self.hole[a] == 0 && self.known[a] == 0 {
                    continue;
                }
                let b = (qy as isize + dy) as usize * w + (qx as isize + dx) as usize;
                for c in 0..CHANNELS {
                    let d = self.color[a][c] - self.color[b][c];
                    sum += d * d;
                }
            }
            if sum >= limit {
                return f32::MAX;
            }
        }
        if self.counts[p] > 0 {
            sum / f32::from(self.counts[p])
        } else {
            f32::MAX
        }
    }
    fn consider(&self, p: usize, c: usize, best: &mut usize, distance: &mut f32) {
        if !self.source[c] || c == *best {
            return;
        }
        let value = self.distance(p, c, *distance);
        if value < *distance {
            *best = c;
            *distance = value;
        }
    }
}

/// Fills nonzero hole pixels in tightly packed premultiplied RGBA8. Transparent
/// unselected pixels remain unchanged and cannot donate. No source returns false;
/// allocation/layout failures are explicit and leave the caller's bytes intact.
pub(crate) fn fill(
    rgba: &mut [u8],
    hole: &[u8],
    width: usize,
    height: usize,
    seed: u32,
) -> Result<bool> {
    let n = width.checked_mul(height).context("内容识别填充尺寸溢出")?;
    anyhow::ensure!(
        n.checked_mul(4) == Some(rgba.len()) && hole.len() == n,
        "内容识别填充像素数量无效"
    );
    let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0, 0);
    for y in 0..height {
        for x in 0..width {
            if hole[y * width + x] != 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 <= x0 {
        return Ok(true);
    }
    let size = (x1 - x0).max(y1 - y0);
    let margin = size.max(64);
    let (bx0, by0, bx1, by1) = (
        x0.saturating_sub(margin),
        y0.saturating_sub(margin),
        x1.saturating_add(margin).min(width),
        y1.saturating_add(margin).min(height),
    );
    let mut fine = Level::new(bx1 - bx0, by1 - by0)?;
    for y in 0..fine.height {
        for x in 0..fine.width {
            let p = y * fine.width + x;
            let source = (by0 + y) * width + bx0 + x;
            fine.hole[p] = u8::from(hole[source] != 0);
            fine.known[p] = u8::from(fine.hole[p] == 0 && rgba[source * 4 + 3] == 255);
            for c in 0..4 {
                fine.color[p][c] = f32::from(rgba[source * 4 + c]);
            }
        }
    }
    fine.classify()?;
    fine.texture()?;
    if fine.sources.is_empty() {
        return Ok(false);
    }
    let mut levels = vec![fine];
    let mut span = size;
    while span > 28 && levels.len() < 12 {
        let last = levels.last().unwrap();
        if last.width < 56 || last.height < 56 {
            break;
        }
        let coarse = last.downsample()?;
        if coarse.sources.is_empty() {
            break;
        }
        levels.push(coarse);
        span /= 2;
    }
    let coarsest = levels.last_mut().unwrap();
    coarsest.fill_inward()?;
    coarsest.random_matches(seed);
    coarsest.measure();
    coarsest.settle(12, seed)?;
    for i in (0..levels.len() - 1).rev() {
        let (fine, coarse) = levels.split_at_mut(i + 1);
        let fine = &mut fine[i];
        fine.upsample(&coarse[0], seed.wrapping_add(i as u32));
        fine.measure();
        fine.vote(1.)?;
        fine.measure();
        fine.settle(
            if i == 0 { 4 } else { 6 },
            seed.wrapping_add(0x1000_u32.wrapping_mul(i as u32 + 1)),
        )?;
    }
    let fine = &mut levels[0];
    fine.vote(8.)?;
    fine.adapt_color()?;
    for y in 0..fine.height {
        for x in 0..fine.width {
            let p = y * fine.width + x;
            if fine.hole[p] == 0 {
                continue;
            }
            let target = ((by0 + y) * width + bx0 + x) * 4;
            rgba[target + 3] = fine.color[p][3].clamp(0., 255.).round_ties_even() as u8;
            for c in 0..3 {
                rgba[target + c] = fine.color[p][c]
                    .clamp(0., f32::from(rgba[target + 3]))
                    .round_ties_even() as u8;
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> serde_json::Value {
        serde_json::from_str(include_str!("../tests/reference/patch_match_1_5_3.json")).unwrap()
    }
    fn bytes(value: &serde_json::Value, key: &str) -> Vec<u8> {
        value[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect()
    }

    #[test]
    fn fill_and_content_aware_healing_match_original_153_c_pixels() {
        let reference = reference();
        let input = bytes(&reference, "healInput");
        let mask = bytes(&reference, "healMask");
        let mut filled = input.clone();
        assert!(fill(&mut filled, &mask, 24, 24, 1).unwrap());
        let expected = bytes(&reference, "contentFill");
        let worst = filled
            .iter()
            .zip(&expected)
            .map(|(&a, &b)| a.abs_diff(b))
            .max()
            .unwrap();
        assert!(worst <= 1, "fill differs from C by {worst}");
        let mut healed = input.clone();
        crate::brush::healing::spot_heal(&mut healed, &mask, 24, 24, 0.75, 0, 123456);
        let expected = bytes(&reference, "heal0");
        let worst = healed
            .iter()
            .zip(&expected)
            .map(|(&a, &b)| a.abs_diff(b))
            .max()
            .unwrap();
        assert!(worst <= 1, "healing differs from C by {worst}");
        for (p, &coverage) in mask.iter().enumerate() {
            if coverage == 0 {
                assert_eq!(&filled[p * 4..p * 4 + 4], &input[p * 4..p * 4 + 4]);
                assert_eq!(&healed[p * 4..p * 4 + 4], &input[p * 4..p * 4 + 4]);
            }
        }
    }

    #[test]
    fn content_aware_rebuilds_the_curved_ridge_and_preserves_outside_pixels() {
        let (w, h) = (320_usize, 200_usize);
        let cy = 110. + 28. * (160_f64 / 37.).sin();
        let mut seed = 7_u32;
        let mut truth = vec![0; w * h * 4];
        let mut damaged = truth.clone();
        let mut coverage = vec![0; w * h];
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let grain = f64::from((seed >> 28) as i32 - 8) * 2.;
                let sky = (y as f64) < 110. + 28. * (x as f64 / 37.).sin();
                let color = if sky {
                    [120. + y as f64 * 0.5, 160. + y as f64 * 0.3, 230.]
                } else {
                    [60. + grain, 110. + grain, 50. + grain]
                };
                let dx = x as f64 - 160.;
                let dy = y as f64 - cy;
                let disc = dx * dx + dy * dy < 22. * 22.;
                let p = (y * w + x) * 4;
                for c in 0..3 {
                    truth[p + c] = color[c] as u8;
                    damaged[p + c] = if disc {
                        if c == 0 { 220 } else { 30 }
                    } else {
                        truth[p + c]
                    };
                }
                truth[p + 3] = 255;
                damaged[p + 3] = 255;
                let px = x as f64 + 0.5 - 160.;
                let py = y as f64 + 0.5 - cy;
                if px * px + py * py < 28. * 28. || (px - 0.5) * (px - 0.5) + py * py < 28. * 28. {
                    coverage[y * w + x] = 255;
                }
            }
        }
        let mut errors = [0_f64; 2];
        for (i, mode) in [0, 2].into_iter().enumerate() {
            let mut healed = damaged.clone();
            crate::brush::healing::spot_heal(&mut healed, &coverage, w, h, 1., mode, 1);
            let mut sum = 0.;
            let mut count = 0;
            for y in 0..h {
                for x in 0..w {
                    let p = (y * w + x) * 4;
                    let dx = x as f64 - 160.;
                    let dy = y as f64 - cy;
                    if dx * dx + dy * dy < 22. * 22. {
                        for c in 0..3 {
                            sum += f64::from(healed[p + c].abs_diff(truth[p + c]));
                            count += 1;
                        }
                    }
                    if coverage[y * w + x] == 0 {
                        assert_eq!(&healed[p..p + 4], &damaged[p..p + 4]);
                    }
                }
            }
            errors[i] = sum / f64::from(count);
        }
        eprintln!(
            "ridge Content-Aware MAE={}, Proximity MAE={}",
            errors[0], errors[1]
        );
        assert!(errors[0] < 12., "Content-Aware MAE {}", errors[0]);
        assert!(errors[0] < errors[1] / 2., "{errors:?}");
        let reference = reference();
        assert!((errors[0] - reference["ridgeContentError"].as_f64().unwrap()).abs() < 0.01);
        assert!((errors[1] - reference["ridgeProximityError"].as_f64().unwrap()).abs() < 0.01);
    }

    #[test]
    fn row_bands_are_deterministic_across_worker_counts() {
        let (w, h) = (96, 80);
        let input: Vec<_> = (0..w * h)
            .flat_map(|p| {
                let x = p % w;
                let y = p / w;
                [
                    (x * 7 % 256) as u8,
                    (y * 9 % 256) as u8,
                    ((x + y) * 5 % 256) as u8,
                    255,
                ]
            })
            .collect();
        let mut mask = vec![0; w * h];
        for y in 25..58 {
            for x in 32..65 {
                mask[y * w + x] = 255;
            }
        }
        let run = |threads| {
            let mut output = input.clone();
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap()
                .install(|| assert!(fill(&mut output, &mask, w, h, 123).unwrap()));
            output
        };
        assert_eq!(run(1), run(4));
    }

    #[test]
    fn missing_donors_and_invalid_layout_leave_input_unchanged() {
        let input = [30, 40, 50, 100].repeat(32 * 32);
        let mut output = input.clone();
        let mask = vec![255; 32 * 32];
        assert!(!fill(&mut output, &mask, 32, 32, 1).unwrap());
        assert_eq!(output, input);
        assert!(fill(&mut output, &mask[..10], 32, 32, 1).is_err());
        assert_eq!(output, input);
        assert!(fill(&mut output, &vec![0; 32 * 32], 32, 32, 1).unwrap());
        assert_eq!(output, input);
    }
}
