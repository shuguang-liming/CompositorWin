//! Smudge/Liquify's premultiplied document-grid working copy, ported from WarpStroke.
use super::{BrushMode, BrushSettings};
use crate::model::{Raster, TILE_SIZE, Transform};
use anyhow::{Result, ensure};
use std::sync::Arc;

pub(super) struct WarpSample {
    pub diameter: f64,
    sampler: crate::render::LayerSampler,
    transform: Transform,
    working: Raster,
    hardness: f32,
    strength: f32,
    mode: BrushMode,
    last: Option<[f64; 2]>,
    carried: Vec<[f32; 4]>,
    scratch: Vec<[u8; 4]>,
}
impl WarpSample {
    pub fn new(
        source: Arc<Raster>,
        transform: Transform,
        canvas: [u32; 2],
        settings: BrushSettings,
    ) -> Self {
        Self {
            diameter: settings.diameter.max(2.),
            sampler: crate::render::LayerSampler::new(&source, transform.sampling, transform.size),
            transform,
            working: Raster::blank(canvas[0], canvas[1]),
            hardness: settings.hardness.clamp(0., 0.98) as f32,
            strength: settings.opacity.clamp(0.01, 1.) as f32,
            mode: settings.mode,
            last: None,
            carried: vec![],
            scratch: vec![],
        }
    }
    // Only tiles reached by a dab are projected. The immutable source is shared with history.
    fn ensure_region(&mut self, bounds: [i64; 4]) {
        let left = bounds[0].clamp(0, self.working.width as i64) as u32;
        let top = bounds[1].clamp(0, self.working.height as i64) as u32;
        let right = bounds[2].clamp(0, self.working.width as i64) as u32;
        let bottom = bounds[3].clamp(0, self.working.height as i64) as u32;
        if left >= right || top >= bottom {
            return;
        }
        for ty in top / TILE_SIZE..=(bottom - 1) / TILE_SIZE {
            for tx in left / TILE_SIZE..=(right - 1) / TILE_SIZE {
                if self.working.tiles.contains_key(&(tx, ty)) {
                    continue;
                }
                let mut pixels = vec![0; (TILE_SIZE * TILE_SIZE * 4) as usize];
                for y in ty * TILE_SIZE..((ty + 1) * TILE_SIZE).min(self.working.height) {
                    for x in tx * TILE_SIZE..((tx + 1) * TILE_SIZE).min(self.working.width) {
                        let uv = self
                            .transform
                            .from_document([x as f64 + 0.5, y as f64 + 0.5]);
                        let p = self.sampler.sample(uv);
                        let i = (((y % TILE_SIZE) * TILE_SIZE + x % TILE_SIZE) * 4) as usize;
                        for c in 0..4 {
                            pixels[i + c] = (p[c] * 255.).round().clamp(0., 255.) as u8;
                        }
                    }
                }
                self.working.tiles.insert((tx, ty), Arc::new(pixels));
            }
        }
    }
    fn radius(&self) -> i64 {
        (self.diameter / 2.).ceil() as i64
    }
    pub fn raster_usage(&self) -> (usize, usize) {
        (self.working.tiles.len(), self.working.byte_len())
    }
    fn weight(&self, distance: f32) -> f32 {
        if distance >= 1. {
            return 0.;
        }
        if distance <= self.hardness {
            return 1.;
        }
        let t = (1. - distance) / (1. - self.hardness);
        t * t * (3. - 2. * t)
    }
    pub fn append(&mut self, point: [f64; 2]) -> Result<Vec<[f64; 2]>> {
        let Some(from) = self.last else {
            self.last = Some(point);
            if matches!(self.mode, BrushMode::Smudge) {
                self.pick_up(point);
            }
            return Ok(vec![]);
        };
        let distance = (point[0] - from[0]).hypot(point[1] - from[1]);
        let spacing = (self.diameter
            * if matches!(self.mode, BrushMode::Smudge) {
                0.005
            } else {
                0.025
            })
        .max(1.);
        if distance < spacing {
            return Ok(vec![]);
        }
        let steps = (distance / spacing).ceil() as usize;
        ensure!(steps <= 100_000, "笔画段过长");
        let mut points = Vec::with_capacity(steps);
        let mut previous = from;
        for step in 1..=steps {
            let t = step as f64 / steps as f64;
            let next = [
                from[0] + (point[0] - from[0]) * t,
                from[1] + (point[1] - from[1]) * t,
            ];
            if matches!(self.mode, BrushMode::Smudge) {
                self.smudge(next);
            } else {
                self.push(previous, next);
            }
            points.push(next);
            previous = next;
        }
        self.last = Some(point);
        Ok(points)
    }
    fn pick_up(&mut self, center: [f64; 2]) {
        let r = self.radius();
        let side = 2 * r + 1;
        let cx = center[0].round() as i64;
        let cy = center[1].round() as i64;
        self.ensure_region([cx - r, cy - r, cx + r + 1, cy + r + 1]);
        self.carried = vec![[0.; 4]; (side * side) as usize];
        for dy in -r..=r {
            for dx in -r..=r {
                let p = self.get(cx + dx, cy + dy);
                self.carried[((dy + r) * side + dx + r) as usize] = p.map(|v| v as f32);
            }
        }
    }
    fn get(&self, x: i64, y: i64) -> [u8; 4] {
        if x < 0 || y < 0 {
            [0; 4]
        } else {
            self.working.get(x as u32, y as u32)
        }
    }
    fn smudge(&mut self, center: [f64; 2]) {
        let r = self.radius();
        let side = 2 * r + 1;
        let cx = center[0].round() as i64;
        let cy = center[1].round() as i64;
        self.ensure_region([cx - r, cy - r, cx + r + 1, cy + r + 1]);
        let inv_r = 1. / (self.diameter / 2.) as f32;
        for dy in -r..=r {
            for dx in -r..=r {
                let (x, y) = (cx + dx, cy + dy);
                if x < 0
                    || y < 0
                    || x >= self.working.width as i64
                    || y >= self.working.height as i64
                {
                    continue;
                }
                let weight =
                    self.weight(((dx * dx + dy * dy) as f32).sqrt() * inv_r) * self.strength;
                if weight <= 0. {
                    continue;
                }
                let under = self.get(x, y);
                let tip = &mut self.carried[((dy + r) * side + dx + r) as usize];
                let mut out = [0; 4];
                for c in 0..4 {
                    tip[c] = under[c] as f32 + (tip[c] - under[c] as f32) * weight;
                    out[c] = tip[c].round().clamp(0., 255.) as u8;
                }
                self.working.set(x as u32, y as u32, out);
            }
        }
    }
    fn push(&mut self, from: [f64; 2], center: [f64; 2]) {
        let r = self.radius();
        let movement = [
            (center[0] - from[0]) as f32 * self.strength,
            (center[1] - from[1]) as f32 * self.strength,
        ];
        let margin = movement[0].abs().max(movement[1].abs()).ceil() as i64 + 2;
        let cx = center[0].round() as i64;
        let cy = center[1].round() as i64;
        let left = (cx - r - margin).max(0);
        let top = (cy - r - margin).max(0);
        let right = (cx + r + margin + 1).min(self.working.width as i64);
        let bottom = (cy + r + margin + 1).min(self.working.height as i64);
        if left >= right || top >= bottom {
            return;
        }
        self.ensure_region([left, top, right, bottom]);
        let cw = (right - left) as usize;
        let ch = (bottom - top) as usize;
        if cw < 2 || ch < 2 {
            return;
        }
        self.scratch.resize(cw * ch, [0; 4]);
        for y in 0..ch {
            for x in 0..cw {
                self.scratch[y * cw + x] = self.get(left + x as i64, top + y as i64);
            }
        }
        let inv_r = 1. / (self.diameter / 2.) as f32;
        for dy in -r..=r {
            for dx in -r..=r {
                let (x, y) = (cx + dx, cy + dy);
                if x < left || y < top || x >= right || y >= bottom {
                    continue;
                }
                let w = self.weight(((dx * dx + dy * dy) as f32).sqrt() * inv_r);
                if w <= 0. {
                    continue;
                }
                let sx = ((x - left) as f32 - movement[0] * w).clamp(0., (cw - 1) as f32);
                let sy = ((y - top) as f32 - movement[1] * w).clamp(0., (ch - 1) as f32);
                let ix = (sx as usize).min(cw - 2);
                let iy = (sy as usize).min(ch - 2);
                let fx = sx - ix as f32;
                let fy = sy - iy as f32;
                let p00 = self.scratch[iy * cw + ix];
                let p10 = self.scratch[iy * cw + ix + 1];
                let p01 = self.scratch[(iy + 1) * cw + ix];
                let p11 = self.scratch[(iy + 1) * cw + ix + 1];
                let mut out = [0; 4];
                for c in 0..4 {
                    let a = p00[c] as f32 + (p10[c] as f32 - p00[c] as f32) * fx;
                    let b = p01[c] as f32 + (p11[c] as f32 - p01[c] as f32) * fx;
                    out[c] = (a + (b - a) * fy).round().clamp(0., 255.) as u8;
                }
                self.working.set(x as u32, y as u32, out);
            }
        }
    }
    pub fn sample(&mut self, point: [f64; 2]) -> [u8; 4] {
        let x = point[0] - 0.5;
        let y = point[1] - 0.5;
        let ix = x.floor() as i64;
        let iy = y.floor() as i64;
        self.ensure_region([ix, iy, ix + 2, iy + 2]);
        let fx = x - x.floor();
        let fy = y - y.floor();
        let corners = [
            (self.get(ix, iy), (1. - fx) * (1. - fy)),
            (self.get(ix + 1, iy), fx * (1. - fy)),
            (self.get(ix, iy + 1), (1. - fx) * fy),
            (self.get(ix + 1, iy + 1), fx * fy),
        ];
        let mut sums = [0.; 4];
        for (p, w) in corners {
            for c in 0..4 {
                sums[c] += p[c] as f64 * w;
            }
        }
        if sums[3] <= 0. {
            return [0; 4];
        }
        [
            (sums[0] * 255. / sums[3]).round().clamp(0., 255.) as u8,
            (sums[1] * 255. / sums[3]).round().clamp(0., 255.) as u8,
            (sums[2] * 255. / sums[3]).round().clamp(0., 255.) as u8,
            sums[3].round().clamp(0., 255.) as u8,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn integer_center_smudge_and_push_carry_the_previous_document_pixel() {
        let source = Arc::new(Raster::from_rgba(
            24,
            20,
            (0..24 * 20)
                .flat_map(|i| [(i % 24 * 10) as u8, 40, 100, 255])
                .collect(),
        ));
        for mode in [BrushMode::Smudge, BrushMode::Liquify] {
            let mut warp = WarpSample::new(
                source.clone(),
                Transform::new(0., 0., 24., 20.),
                [24, 20],
                BrushSettings {
                    diameter: 2.,
                    hardness: 1.,
                    opacity: 1.,
                    mode,
                    ..Default::default()
                },
            );
            assert!(warp.append([5., 10.]).unwrap().is_empty());
            let points = warp.append([8., 10.]).unwrap();
            assert_eq!(points, [[6., 10.], [7., 10.], [8., 10.]]);
            for x in 6..=8 {
                assert_eq!(warp.sample([x as f64 + 0.5, 10.5]), [50, 40, 100, 255]);
            }
            assert_eq!(warp.sample([9.5, 10.5]), [90, 40, 100, 255]);
            let before = warp.working.clone();
            assert!(warp.append([8.1, 10.]).unwrap().is_empty());
            assert_eq!(before, warp.working);
        }
    }
    #[test]
    fn large_canvas_only_projects_touched_tiles_and_keeps_premultiplied_alpha() {
        let source = Arc::new(Raster::solid(8000, 8000, [100, 200, 40, 128]));
        let mut warp = WarpSample::new(
            source.clone(),
            Transform::new(0., 0., 8000., 8000.),
            [8000, 8000],
            BrushSettings {
                diameter: 6.,
                mode: BrushMode::Smudge,
                ..Default::default()
            },
        );
        assert_eq!(warp.working.byte_len(), 0);
        warp.append([40., 40.]).unwrap();
        warp.append([44., 40.]).unwrap();
        assert_eq!(warp.working.tiles.len(), 1);
        assert_eq!(warp.working.byte_len(), 128 * 128 * 4);
        assert_eq!(warp.get(44, 40), [50, 100, 20, 128]);
        assert_eq!(Arc::strong_count(&source.pixels), 2);
    }
    #[test]
    fn high_minification_working_grid_matches_area_prefiltered_canvas() {
        let source = Arc::new(Raster::from_rgba(
            64,
            64,
            (0..4096)
                .flat_map(|i| {
                    let v = if (i % 64 + i / 64) % 2 == 0 { 0 } else { 255 };
                    [v, v, v, 255]
                })
                .collect(),
        ));
        let mut warp = WarpSample::new(
            source,
            Transform::new(0., 0., 8., 8.),
            [8, 8],
            BrushSettings {
                diameter: 2.,
                mode: BrushMode::Smudge,
                ..Default::default()
            },
        );
        warp.append([4., 4.]).unwrap();
        assert_eq!(warp.get(4, 4), [128, 128, 128, 255]);
        warp.append([5., 4.]).unwrap();
        assert_eq!(warp.get(5, 4), [128, 128, 128, 255]);
    }
}
