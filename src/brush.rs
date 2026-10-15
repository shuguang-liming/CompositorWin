use crate::model::{Document, Layer, MAX_PIXELS, Mask, Raster, Selection, TILE_SIZE, Transform};
use anyhow::{Result, ensure};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};
use uuid::Uuid;
#[path = "content_fill.rs"]
mod content_fill;
#[path = "healing.rs"]
mod healing;
#[path = "warp.rs"]
mod warp;
pub use content_fill::content_fill;
pub use healing::spot_heal;
use warp::WarpSample;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealingMode {
    ContentAware,
    CreateTexture,
    ProximityMatch,
}
#[derive(Clone, Copy, Debug)]
pub enum BrushMode {
    Paint,
    Erase,
    Clone { offset: [f64; 2] },
    Heal(HealingMode),
    Blur { radius: f64 },
    Smudge,
    Liquify,
}
#[derive(Clone, Copy, Debug)]
pub struct BrushSettings {
    pub diameter: f64,
    pub hardness: f64,
    pub opacity: f64,
    pub color: [u8; 4],
    pub smoothing: f64,
    pub mode: BrushMode,
}
impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            diameter: 40.,
            hardness: 1.,
            opacity: 1.,
            color: [0, 0, 0, 255],
            smoothing: 0.,
            mode: BrushMode::Paint,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PaintResult {
    pub bounds: Option<[u32; 4]>,
    pub changed_pixels: usize,
}

struct TailBackup {
    raster: Raster,
    transform: Transform,
    bounds: Option<[u32; 4]>,
    last: Option<[f64; 2]>,
    coverage: HashMap<(i32, i32), Option<f64>>,
}

/// One immutable source and sparse changed tiles for an entire pointer gesture.
pub struct BrushStroke {
    pub layer_id: Uuid,
    pub mask: bool,
    pub original: Layer,
    pub raster: Raster,
    pub transform: Transform,
    settings: BrushSettings,
    selection: Option<Selection>,
    source: Arc<Raster>,
    coverage: HashMap<(u32, u32), f64>,
    last: Option<[f64; 2]>,
    anchor: Option<[f64; 2]>,
    bounds: Option<[u32; 4]>,
    clone_document_sample: bool,
    canvas: [u32; 2],
    warp: Option<WarpSample>,
    samples: Vec<[f64; 2]>,
    tail: Option<TailBackup>,
    blur: Option<BlurSample>,
    mask_background: u8,
}
impl BrushStroke {
    pub fn new(
        doc: &Document,
        layer_id: Uuid,
        mask: bool,
        settings: BrushSettings,
        clone_sample: Option<Arc<Raster>>,
    ) -> Result<Self> {
        ensure!(
            settings.diameter.is_finite() && settings.diameter > 0. && settings.diameter <= 5000.,
            "画笔直径无效"
        );
        ensure!(
            settings.hardness.is_finite() && settings.opacity.is_finite(),
            "画笔设置无效"
        );
        let layer = doc
            .layer(layer_id)
            .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?;
        ensure!(
            layer.visible && (mask || (!layer.is_group && layer.adjustment.is_none())),
            "请选择可见的像素图层或蒙版进行绘画"
        );
        ensure!(
            !mask || !matches!(settings.mode, BrushMode::Smudge | BrushMode::Liquify),
            "涂抹和液化需要编辑图层像素"
        );
        let (raster, transform) = if mask {
            let m = layer
                .mask
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("图层没有蒙版"))?;
            ensure!(m.enabled, "蒙版已停用");
            let transform = m.placement.unwrap_or(layer.transform);
            let raster = if m.raster.width == 1 && m.raster.height == 1 {
                let w = transform.size[0].ceil().max(1.) as u32;
                let h = transform.size[1].ceil().max(1.) as u32;
                ensure!(w as u64 * h as u64 <= MAX_PIXELS, "蒙版超出图像大小上限");
                Raster::solid(w, h, m.raster.get(0, 0))
            } else {
                m.raster.as_ref().clone()
            };
            (raster, transform)
        } else {
            (
                layer
                    .image
                    .as_deref()
                    .cloned()
                    .unwrap_or_else(|| Raster::blank(doc.width, doc.height)),
                layer.transform,
            )
        };
        ensure!(
            transform.is_valid() && Document::valid_size(raster.width, raster.height),
            "画笔源图像的尺寸或变换无效"
        );
        let clone_document_sample = clone_sample.is_some();
        let source = clone_sample.unwrap_or_else(|| Arc::new(raster.clone()));
        let blur = if let BrushMode::Blur { radius } = settings.mode {
            ensure!(radius.is_finite() && radius > 0., "模糊半径无效");
            Some(BlurSample::new(source.clone(), transform, radius, mask))
        } else {
            None
        };
        let mask_background = if mask {
            crate::render::mask_background(&source)
        } else {
            0
        };
        let warp = if matches!(settings.mode, BrushMode::Smudge | BrushMode::Liquify) {
            ensure!(layer.image.is_some(), "变形工具需要源像素");
            ensure!(settings.diameter <= 2000., "变形画笔直径超出 2000 像素");
            Some(WarpSample::new(
                source.clone(),
                transform,
                [doc.width, doc.height],
                settings,
            ))
        } else {
            None
        };
        Ok(Self {
            layer_id,
            mask,
            original: layer.clone(),
            raster,
            transform,
            settings,
            selection: doc.selection.clone(),
            source,
            coverage: HashMap::new(),
            last: None,
            anchor: None,
            bounds: None,
            clone_document_sample,
            canvas: [doc.width, doc.height],
            warp,
            samples: vec![],
            tail: None,
            blur,
            mask_background,
        })
    }

    pub fn append(&mut self, point: [f64; 2]) -> Result<PaintResult> {
        ensure!(point.iter().all(|x| x.is_finite()), "笔画坐标无效");
        if let Some(mut warp) = self.warp.take() {
            let result = warp
                .append(point)
                .and_then(|points| self.project_warp(&mut warp, &points));
            self.warp = Some(warp);
            result?;
            return Ok(PaintResult {
                bounds: self.bounds,
                changed_pixels: self.coverage.len(),
            });
        }
        let smoothing = (self.settings.smoothing.clamp(0., 100.) / 100.).powi(2) * 50.;
        let point = if let Some(a) = self.anchor {
            let dx = point[0] - a[0];
            let dy = point[1] - a[1];
            let len = dx.hypot(dy);
            if len > smoothing && smoothing > 0. {
                [
                    point[0] - dx / len * smoothing,
                    point[1] - dy / len * smoothing,
                ]
            } else if smoothing > 0. {
                a
            } else {
                point
            }
        } else {
            point
        };
        self.anchor = Some(point);
        if !matches!(self.settings.mode, BrushMode::Smudge | BrushMode::Liquify) {
            if self.samples.last() == Some(&point) {
                return Ok(PaintResult {
                    bounds: self.bounds,
                    changed_pixels: self.coverage.len(),
                });
            }
            self.restore_tail();
            self.samples.push(point);
            if self.samples.len() > 4 {
                self.samples.remove(0);
            }
            let n = self.samples.len();
            if n >= 3 {
                self.curve(
                    self.samples[n - 3],
                    self.samples[n - 2],
                    self.samples[n.saturating_sub(4)],
                    point,
                )?;
            }
            if n >= 2 {
                self.tail = Some(TailBackup {
                    raster: self.raster.clone(),
                    transform: self.transform,
                    bounds: self.bounds,
                    last: self.last,
                    coverage: HashMap::new(),
                });
                self.walk(point)?;
            } else {
                self.dab(point, point)?;
                self.last = Some(point);
            }
        } else {
            self.walk(point)?;
        }
        if let BrushMode::Heal(mode) = self.settings.mode {
            self.refresh_healing(mode);
        }
        Ok(PaintResult {
            bounds: self.bounds,
            changed_pixels: self.coverage.len(),
        })
    }

    /// Materialized document-grid tiles only, excluding the shared source, carry/scratch and sampler mips.
    pub fn warp_working_raster_usage(&self) -> Option<(usize, usize)> {
        self.warp.as_ref().map(WarpSample::raster_usage)
    }

    fn walk(&mut self, point: [f64; 2]) -> Result<()> {
        if let Some(last) = self.last {
            let distance = (point[0] - last[0]).hypot(point[1] - last[1]);
            let spacing = if matches!(self.settings.mode, BrushMode::Smudge) {
                (self.settings.diameter * 0.005).max(1.)
            } else {
                (self.settings.diameter * 0.025).clamp(0.5, 4.)
            };
            let steps = (distance / spacing).ceil().max(1.) as usize;
            ensure!(steps <= 100_000, "笔画段过长");
            let mut previous = last;
            for i in 1..=steps {
                let t = i as f64 / steps as f64;
                let p = [
                    last[0] + (point[0] - last[0]) * t,
                    last[1] + (point[1] - last[1]) * t,
                ];
                self.dab(p, previous)?;
                previous = p;
            }
        } else {
            self.dab(point, point)?;
        }
        self.last = Some(point);
        Ok(())
    }

    pub fn finish_at(&mut self, point: [f64; 2]) -> Result<PaintResult> {
        self.settings.smoothing = 0.;
        self.append(point)?;
        if !matches!(self.settings.mode, BrushMode::Smudge | BrushMode::Liquify) {
            let n = self.samples.len();
            if n >= 2 {
                self.restore_tail();
                self.curve(
                    self.samples[n - 2],
                    self.samples[n - 1],
                    self.samples[n.saturating_sub(3)],
                    self.samples[n - 1],
                )?;
                self.samples = vec![self.samples[n - 1]];
            }
        }
        if let BrushMode::Heal(mode) = self.settings.mode {
            self.refresh_healing(mode);
        }
        Ok(PaintResult {
            bounds: self.bounds,
            changed_pixels: self.coverage.len(),
        })
    }

    fn restore_tail(&mut self) {
        let Some(tail) = self.tail.take() else {
            return;
        };
        let offset = tail
            .transform
            .from_document(self.transform.to_document([0., 0.]));
        let dx = (offset[0] * tail.raster.width as f64).round() as i32;
        let dy = (offset[1] * tail.raster.height as f64).round() as i32;
        if dx != 0 || dy != 0 {
            self.coverage = self
                .coverage
                .drain()
                .filter_map(|((x, y), value)| {
                    let xx = x as i64 + dx as i64;
                    let yy = y as i64 + dy as i64;
                    (xx >= 0
                        && yy >= 0
                        && xx < tail.raster.width as i64
                        && yy < tail.raster.height as i64)
                        .then_some(((xx as u32, yy as u32), value))
                })
                .collect();
        }
        for ((x, y), value) in tail.coverage {
            if x < 0 || y < 0 {
                continue;
            }
            let key = (x as u32, y as u32);
            if let Some(value) = value {
                self.coverage.insert(key, value);
            } else {
                self.coverage.remove(&key);
            }
        }
        self.raster = tail.raster;
        self.transform = tail.transform;
        self.bounds = tail.bounds;
        self.last = tail.last;
    }

    fn curve(
        &mut self,
        start: [f64; 2],
        end: [f64; 2],
        before: [f64; 2],
        after: [f64; 2],
    ) -> Result<()> {
        fn knot(t: f64, a: [f64; 2], b: [f64; 2]) -> f64 {
            t + (a[0] - b[0]).hypot(a[1] - b[1]).sqrt().max(0.0001)
        }
        fn mix(a: [f64; 2], b: [f64; 2], ta: f64, tb: f64, t: f64) -> [f64; 2] {
            let wa = (tb - t) / (tb - ta);
            let wb = (t - ta) / (tb - ta);
            [a[0] * wa + b[0] * wb, a[1] * wa + b[1] * wb]
        }
        let t0 = 0.;
        let t1 = knot(t0, before, start);
        let t2 = knot(t1, start, end);
        let t3 = knot(t2, end, after);
        let pieces = ((end[0] - start[0]).hypot(end[1] - start[1]) / 2.)
            .ceil()
            .max(1.) as usize;
        ensure!(pieces <= 100_000, "曲线段过长");
        for i in 1..=pieces {
            let t = t1 + (t2 - t1) * i as f64 / pieces as f64;
            let a = mix(before, start, t0, t1, t);
            let b = mix(start, end, t1, t2, t);
            let c = mix(end, after, t2, t3, t);
            let p = if i == pieces {
                end
            } else {
                mix(mix(a, b, t0, t2, t), mix(b, c, t1, t3, t), t1, t2, t)
            };
            self.walk(p)?;
        }
        Ok(())
    }

    fn dab(&mut self, point: [f64; 2], _previous: [f64; 2]) -> Result<()> {
        let radius = self.settings.diameter / 2.;
        self.grow_for_dab(point, radius)?;
        let corners = [
            [point[0] - radius, point[1] - radius],
            [point[0] + radius, point[1] - radius],
            [point[0] + radius, point[1] + radius],
            [point[0] - radius, point[1] + radius],
        ]
        .map(|p| self.transform.from_document(p));
        let x0 = (corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min)
            * self.raster.width as f64)
            .floor()
            .max(0.) as u32;
        let x1 = (corners
            .iter()
            .map(|p| p[0])
            .fold(f64::NEG_INFINITY, f64::max)
            * self.raster.width as f64)
            .ceil()
            .clamp(0., self.raster.width as f64) as u32;
        let y0 = (corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min)
            * self.raster.height as f64)
            .floor()
            .max(0.) as u32;
        let y1 = (corners
            .iter()
            .map(|p| p[1])
            .fold(f64::NEG_INFINITY, f64::max)
            * self.raster.height as f64)
            .ceil()
            .clamp(0., self.raster.height as f64) as u32;
        let mut row_pixels = [(0, [0; 4]); TILE_SIZE as usize];
        for y in y0..y1 {
            for tx in x0 / TILE_SIZE..x1.div_ceil(TILE_SIZE) {
                let mut count = 0;
                for x in x0.max(tx * TILE_SIZE)..x1.min((tx + 1) * TILE_SIZE) {
                    let docp = self.transform.to_document([
                        (x as f64 + 0.5) / self.raster.width as f64,
                        (y as f64 + 0.5) / self.raster.height as f64,
                    ]);
                    if docp[0] < 0.
                        || docp[1] < 0.
                        || docp[0] >= self.canvas[0] as f64
                        || docp[1] >= self.canvas[1] as f64
                    {
                        continue;
                    }
                    let u = (docp[0] - point[0]).hypot(docp[1] - point[1]) / radius;
                    if u >= 1. {
                        continue;
                    }
                    let hard = self.settings.hardness.clamp(0., 1.);
                    let weight = if u <= hard {
                        1.
                    } else {
                        let t = (1. - u) / (1. - hard);
                        t * t * (3. - 2. * t)
                    };
                    let select = self.selection.as_ref().map_or(1., |s| {
                        if docp[0] < 0. || docp[1] < 0. {
                            0.
                        } else {
                            s.get(docp[0].floor() as u32, docp[1].floor() as u32) as f64 / 255.
                        }
                    });
                    if select == 0. {
                        continue;
                    }
                    if let Some(tail) = &mut self.tail {
                        let unit = tail.transform.from_document(docp);
                        let key = (
                            (unit[0] * tail.raster.width as f64).floor() as i32,
                            (unit[1] * tail.raster.height as f64).floor() as i32,
                        );
                        tail.coverage
                            .entry(key)
                            .or_insert_with(|| self.coverage.get(&(x, y)).copied());
                    }
                    let covered = self.coverage.entry((x, y)).or_default();
                    *covered = 1. - (1. - *covered) * (1. - weight);
                    let amount = *covered * self.settings.opacity.clamp(0., 1.) * select;
                    let base = self.base_pixel(x, y);
                    let pixel = match self.settings.mode {
                        BrushMode::Paint => over(base, self.settings.color, amount),
                        BrushMode::Erase => {
                            if self.mask {
                                over(base, [255; 4], amount)
                            } else {
                                let mut p = base;
                                p[3] = (base[3] as f64 * (1. - amount)).round() as u8;
                                p
                            }
                        }
                        BrushMode::Clone { offset } => {
                            let source_doc = [docp[0] + offset[0], docp[1] + offset[1]];
                            let p = if !self.clone_document_sample {
                                let unit = self.original.transform.from_document(source_doc);
                                sample(
                                    &self.source,
                                    unit[0] * self.source.width as f64 - 0.5,
                                    unit[1] * self.source.height as f64 - 0.5,
                                )
                            } else {
                                sample(&self.source, source_doc[0] - 0.5, source_doc[1] - 0.5)
                            };
                            over(base, p, amount)
                        }
                        BrushMode::Blur { .. } => {
                            let blur = self.blur.as_mut().unwrap().sample(docp);
                            interpolate(base, blur, amount)
                        }
                        BrushMode::Heal(_) => base,
                        BrushMode::Smudge | BrushMode::Liquify => {
                            unreachable!("变形使用文档网格工作样本")
                        }
                    };
                    let pixel = if self.mask {
                        let gray = (0.2126 * pixel[0] as f64
                            + 0.7152 * pixel[1] as f64
                            + 0.0722 * pixel[2] as f64)
                            .round() as u8;
                        [gray, gray, gray, 255]
                    } else {
                        pixel
                    };
                    row_pixels[count] = (x, pixel);
                    count += 1;
                    if pixel != base
                        || matches!(
                            self.settings.mode,
                            BrushMode::Heal(_) | BrushMode::Smudge | BrushMode::Liquify
                        )
                    {
                        self.bounds = Some(match self.bounds {
                            Some(b) => [b[0].min(x), b[1].min(y), b[2].max(x + 1), b[3].max(y + 1)],
                            None => [x, y, x + 1, y + 1],
                        });
                    }
                }
                self.raster.set_tile_row(y, &row_pixels[..count]);
            }
        }
        Ok(())
    }

    fn refresh_healing(&mut self, mode: HealingMode) {
        let Some(b) = self.bounds else {
            return;
        };
        let reach = (b[2] - b[0]).max(b[3] - b[1]).saturating_mul(4).max(16);
        let x0 = b[0].saturating_sub(reach);
        let y0 = b[1].saturating_sub(reach);
        let x1 = b[2].saturating_add(reach).min(self.raster.width);
        let y1 = b[3].saturating_add(reach).min(self.raster.height);
        let w = x1 - x0;
        let h = y1 - y0;
        let mut data = vec![0; w as usize * h as usize * 4];
        let mut coverage = vec![0; w as usize * h as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                data[i * 4..i * 4 + 4].copy_from_slice(&self.base_pixel(x + x0, y + y0));
                if let Some(&amount) = self.coverage.get(&(x + x0, y + y0)) {
                    let p = self.transform.to_document([
                        (x as f64 + x0 as f64 + 0.5) / self.raster.width as f64,
                        (y as f64 + y0 as f64 + 0.5) / self.raster.height as f64,
                    ]);
                    let selection = self.selection.as_ref().map_or(1., |s| {
                        if p[0] < 0. || p[1] < 0. {
                            0.
                        } else {
                            s.get(p[0] as u32, p[1] as u32) as f64 / 255.
                        }
                    });
                    coverage[i] = (amount * self.settings.opacity.clamp(0., 1.) * selection * 255.)
                        .round() as u8;
                }
            }
        }
        crate::pixels::premultiply(&mut data);
        spot_heal(
            &mut data,
            &coverage,
            w as usize,
            h as usize,
            1.,
            match mode {
                HealingMode::ContentAware => 0,
                HealingMode::CreateTexture => 1,
                HealingMode::ProximityMatch => 2,
            },
            1,
        );
        crate::pixels::unpremultiply(&mut data);
        for y in b[1]..b[3] {
            for x in b[0]..b[2] {
                let i = (((y - y0) * w + x - x0) * 4) as usize;
                let p = data[i..i + 4].try_into().unwrap();
                if self.raster.get(x, y) != p {
                    self.raster.set(x, y, p);
                }
            }
        }
    }

    fn base_pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let p = self.transform.to_document([
            (x as f64 + 0.5) / self.raster.width as f64,
            (y as f64 + 0.5) / self.raster.height as f64,
        ]);
        let original_transform = if self.mask {
            self.original
                .mask
                .as_ref()
                .and_then(|m| m.placement)
                .unwrap_or(self.original.transform)
        } else {
            self.original.transform
        };
        let unit = original_transform.from_document(p);
        let image = if self.mask {
            self.original.mask.as_ref().map(|m| m.raster.as_ref())
        } else {
            self.original.image.as_deref()
        };
        image.map_or([0; 4], |r| {
            if self.mask {
                let value = (crate::render::sample_mask(
                    r,
                    unit[0] * r.width as f64 - 0.5,
                    unit[1] * r.height as f64 - 0.5,
                    self.mask_background,
                ) * 255.)
                    .round() as u8;
                [value, value, value, 255]
            } else {
                sample(
                    r,
                    unit[0] * r.width as f64 - 0.5,
                    unit[1] * r.height as f64 - 0.5,
                )
            }
        })
    }

    // WarpStroke operates in document pixels; its final clone replaces native pixels through a hard
    // tip four pixels wider than the working brush. Selection is applied here, once, after warping.
    fn project_warp(&mut self, warp: &mut WarpSample, points: &[[f64; 2]]) -> Result<()> {
        if points.is_empty() {
            return Ok(());
        }
        let radius = warp.diameter / 2. + 2.;
        let mut extent = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for point in points {
            extent[0] = extent[0].min(point[0] - radius);
            extent[1] = extent[1].min(point[1] - radius);
            extent[2] = extent[2].max(point[0] + radius);
            extent[3] = extent[3].max(point[1] + radius);
        }
        self.grow_for_document_bounds(extent)?;
        let mut dirty = HashSet::new();
        for &point in points {
            let corners = [
                [point[0] - radius, point[1] - radius],
                [point[0] + radius, point[1] - radius],
                [point[0] + radius, point[1] + radius],
                [point[0] - radius, point[1] + radius],
            ]
            .map(|p| self.transform.from_document(p));
            let x0 = (corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min)
                * self.raster.width as f64)
                .floor()
                .clamp(0., self.raster.width as f64) as u32;
            let x1 = (corners
                .iter()
                .map(|p| p[0])
                .fold(f64::NEG_INFINITY, f64::max)
                * self.raster.width as f64)
                .ceil()
                .clamp(0., self.raster.width as f64) as u32;
            let y0 = (corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min)
                * self.raster.height as f64)
                .floor()
                .clamp(0., self.raster.height as f64) as u32;
            let y1 = (corners
                .iter()
                .map(|p| p[1])
                .fold(f64::NEG_INFINITY, f64::max)
                * self.raster.height as f64)
                .ceil()
                .clamp(0., self.raster.height as f64) as u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = self.transform.to_document([
                        (x as f64 + 0.5) / self.raster.width as f64,
                        (y as f64 + 0.5) / self.raster.height as f64,
                    ]);
                    if p[0] < 0.
                        || p[1] < 0.
                        || p[0] >= self.canvas[0] as f64
                        || p[1] >= self.canvas[1] as f64
                        || (p[0] - point[0]).hypot(p[1] - point[1]) >= radius
                    {
                        continue;
                    }
                    let selection = self.selection.as_ref().map_or(1., |s| {
                        s.get(p[0].floor() as u32, p[1].floor() as u32) as f64 / 255.
                    });
                    if selection > 0. {
                        self.coverage.insert((x, y), selection);
                        dirty.insert((x, y));
                    }
                }
            }
        }
        for (x, y) in dirty {
            let selection = self.coverage[&(x, y)];
            let p = self.transform.to_document([
                (x as f64 + 0.5) / self.raster.width as f64,
                (y as f64 + 0.5) / self.raster.height as f64,
            ]);
            let pixel = interpolate(self.base_pixel(x, y), warp.sample(p), selection);
            if pixel != self.raster.get(x, y) {
                self.raster.set(x, y, pixel);
            }
            self.bounds = Some(match self.bounds {
                Some(b) => [b[0].min(x), b[1].min(y), b[2].max(x + 1), b[3].max(y + 1)],
                None => [x, y, x + 1, y + 1],
            });
        }
        Ok(())
    }

    fn grow_for_dab(&mut self, p: [f64; 2], radius: f64) -> Result<()> {
        self.grow_for_document_bounds([p[0] - radius, p[1] - radius, p[0] + radius, p[1] + radius])
    }
    fn grow_for_document_bounds(&mut self, bounds: [f64; 4]) -> Result<()> {
        let t = self.transform;
        let w = self.raster.width as f64;
        let h = self.raster.height as f64;
        let dx0 = bounds[0].max(0.);
        let dy0 = bounds[1].max(0.);
        let dx1 = bounds[2].min(self.canvas[0] as f64);
        let dy1 = bounds[3].min(self.canvas[1] as f64);
        if dx0 >= dx1 || dy0 >= dy1 {
            return Ok(());
        }
        let corners = [[dx0, dy0], [dx1, dy0], [dx1, dy1], [dx0, dy1]].map(|p| t.from_document(p));
        let left = (corners.iter().map(|p| p[0] * w).fold(0., f64::min)).floor() as i64;
        let top = (corners.iter().map(|p| p[1] * h).fold(0., f64::min)).floor() as i64;
        let right = (corners.iter().map(|p| p[0] * w).fold(w, f64::max)).ceil() as i64;
        let bottom = (corners.iter().map(|p| p[1] * h).fold(h, f64::max)).ceil() as i64;
        if left == 0 && top == 0 && right == w as i64 && bottom == h as i64 {
            return Ok(());
        }
        let nw = right - left;
        let nh = bottom - top;
        ensure!(
            nw > 0
                && nh > 0
                && nw <= crate::model::MAX_SIDE as i64
                && nh <= crate::model::MAX_SIDE as i64
                && nw as u64 * nh as u64 <= MAX_PIXELS,
            "笔画超出图像内存上限"
        );
        let background = if self.mask {
            [
                self.mask_background,
                self.mask_background,
                self.mask_background,
                255,
            ]
        } else {
            [0; 4]
        };
        let mut grown = if self.mask {
            Raster::solid(nw as u32, nh as u32, background)
        } else {
            Raster::blank(nw as u32, nh as u32)
        };
        let base = if self.raster.pixels.len() == 4 {
            self.raster.pixels[..].try_into().unwrap()
        } else {
            [0; 4]
        };
        if self.raster.pixels.len() <= 4 && base == background {
            for (&(tx, ty), tile) in &self.raster.tiles {
                for y in ty * crate::model::TILE_SIZE
                    ..((ty + 1) * crate::model::TILE_SIZE).min(self.raster.height)
                {
                    for x in tx * crate::model::TILE_SIZE
                        ..((tx + 1) * crate::model::TILE_SIZE).min(self.raster.width)
                    {
                        let i = (((y % crate::model::TILE_SIZE) * crate::model::TILE_SIZE
                            + x % crate::model::TILE_SIZE)
                            * 4) as usize;
                        let color = tile[i..i + 4].try_into().unwrap();
                        if color != background {
                            grown.set((x as i64 - left) as u32, (y as i64 - top) as u32, color);
                        }
                    }
                }
            }
        } else {
            for y in 0..self.raster.height {
                for x in 0..self.raster.width {
                    let color = self.raster.get(x, y);
                    if color != background {
                        grown.set((x as i64 - left) as u32, (y as i64 - top) as u32, color);
                    }
                }
            }
        }
        let center = t.to_document([
            ((left + right) as f64 / 2.) / w,
            ((top + bottom) as f64 / 2.) / h,
        ]);
        self.transform.size = [nw as f64 * t.size[0] / w, nh as f64 * t.size[1] / h];
        self.transform.origin = [
            center[0] - self.transform.size[0] / 2.,
            center[1] - self.transform.size[1] / 2.,
        ];
        self.raster = grown;
        self.coverage = self
            .coverage
            .drain()
            .map(|((x, y), c)| (((x as i64 - left) as u32, (y as i64 - top) as u32), c))
            .collect();
        if let Some(b) = self.bounds {
            self.bounds = Some([
                (b[0] as i64 - left) as u32,
                (b[1] as i64 - top) as u32,
                (b[2] as i64 - left) as u32,
                (b[3] as i64 - top) as u32,
            ]);
        }
        Ok(())
    }

    pub fn commit(self, doc: &mut Document) -> Result<PaintResult> {
        let result = PaintResult {
            bounds: self.bounds,
            changed_pixels: self.coverage.len(),
        };
        if self.bounds.is_none() {
            return Ok(result);
        }
        let layer = doc
            .layer_mut(self.layer_id)
            .ok_or_else(|| anyhow::anyhow!("绘画期间图层已被删除"))?;
        ensure!(*layer == self.original, "绘画期间图层发生变化");
        if self.mask {
            let mask = layer.mask.as_mut().unwrap();
            mask.raster = Arc::new(self.raster);
            mask.placement = Some(self.transform);
        } else {
            if let Some(m) = &mut layer.mask
                && m.placement.is_none()
            {
                m.placement = Some(layer.transform);
            }
            layer.image = Some(Arc::new(self.raster));
            layer.transform = self.transform;
            layer.text = None;
            layer.shape = None;
        }
        Ok(result)
    }
}

pub fn paint(
    doc: &mut Document,
    id: Uuid,
    mask: bool,
    points: &[[f64; 2]],
    settings: BrushSettings,
    clone_sample: Option<Arc<Raster>>,
) -> Result<PaintResult> {
    let mut stroke = BrushStroke::new(doc, id, mask, settings, clone_sample)?;
    for &point in points {
        stroke.append(point)?;
    }
    if let Some(&last) = points.last() {
        stroke.finish_at(last)?;
    }
    stroke.commit(doc)
}

pub fn over(base: [u8; 4], source: [u8; 4], coverage: f64) -> [u8; 4] {
    let a = source[3] as f64 / 255. * coverage.clamp(0., 1.);
    let b = base[3] as f64 / 255.;
    let out = a + b * (1. - a);
    if out < 1e-12 {
        return [0; 4];
    }
    let mut p = [0; 4];
    for c in 0..3 {
        p[c] = ((source[c] as f64 * a + base[c] as f64 * b * (1. - a)) / out)
            .round()
            .clamp(0., 255.) as u8;
    }
    p[3] = (out * 255.).round() as u8;
    p
}
pub fn interpolate(a: [u8; 4], b: [u8; 4], t: f64) -> [u8; 4] {
    let t = t.clamp(0., 1.);
    let aa = a[3] as f64 / 255.;
    let ba = b[3] as f64 / 255.;
    let alpha = aa * (1. - t) + ba * t;
    if alpha <= 0. {
        return [0; 4];
    }
    let mut p = [0; 4];
    for c in 0..3 {
        p[c] = ((a[c] as f64 * aa * (1. - t) + b[c] as f64 * ba * t) / alpha)
            .round()
            .clamp(0., 255.) as u8;
    }
    p[3] = (alpha * 255.).round() as u8;
    p
}
pub fn sample(r: &Raster, x: f64, y: f64) -> [u8; 4] {
    if !x.is_finite() || !y.is_finite() {
        return [0; 4];
    }
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let tx = x - x.floor();
    let ty = y - y.floor();
    let get = |xx: i64, yy: i64| {
        if xx < 0 || yy < 0 {
            [0; 4]
        } else {
            r.get(xx as u32, yy as u32)
        }
    };
    let points = [
        (get(x0, y0), (1. - tx) * (1. - ty)),
        (get(x0 + 1, y0), tx * (1. - ty)),
        (get(x0, y0 + 1), (1. - tx) * ty),
        (get(x0 + 1, y0 + 1), tx * ty),
    ];
    let mut sums = [0.; 4];
    for (p, w) in points {
        let a = p[3] as f64 / 255.;
        for c in 0..3 {
            sums[c] += p[c] as f64 * a * w;
        }
        sums[3] += a * w;
    }
    if sums[3] <= 0. {
        [0; 4]
    } else {
        [
            (sums[0] / sums[3]).round() as u8,
            (sums[1] / sums[3]).round() as u8,
            (sums[2] / sums[3]).round() as u8,
            (sums[3] * 255.).round() as u8,
        ]
    }
}
/// Immutable blur sample, evaluated only around painted pixels. Cached rows bound stroke memory.
struct BlurSample {
    source: Arc<Raster>,
    transform: Transform,
    step: f64,
    weights: Vec<f64>,
    background: [u8; 4],
    horizontal: HashMap<(i32, i32), Vec<[f64; 4]>>,
    output: HashMap<(i32, i32), [f64; 4]>,
}
impl BlurSample {
    fn new(source: Arc<Raster>, transform: Transform, radius: f64, mask: bool) -> Self {
        let per_pixel = (transform.size[0] * transform.size[1]
            / (source.width as f64 * source.height as f64))
            .sqrt()
            .max(1e-6);
        let sigma =
            (radius.clamp(0.5, 50.) / per_pixel).min(source.width.max(source.height) as f64 / 2.);
        // Extreme downscaling uses a coarser sample, keeping the kernel and per-dab work bounded.
        let step = (sigma / 64.).max(1.);
        let sigma = (sigma / step).max(0.01);
        let extent = (3. * sigma).ceil() as i32;
        let mut weights = (-extent..=extent)
            .map(|d| (-(d * d) as f64 / (2. * sigma * sigma)).exp())
            .collect::<Vec<_>>();
        let sum = weights.iter().sum::<f64>();
        for weight in &mut weights {
            *weight /= sum;
        }
        let background = if mask {
            let value = crate::render::mask_background(&source);
            [value, value, value, 255]
        } else {
            [0; 4]
        };
        Self {
            source,
            transform,
            step,
            weights,
            background,
            horizontal: HashMap::new(),
            output: HashMap::new(),
        }
    }
    fn sample(&mut self, document_point: [f64; 2]) -> [u8; 4] {
        let unit = self.transform.from_document(document_point);
        let x = (unit[0] * self.source.width as f64 - 0.5) / self.step;
        let y = (unit[1] * self.source.height as f64 - 0.5) / self.step;
        let ix = x.floor() as i32;
        let iy = y.floor() as i32;
        let fx = x - x.floor();
        let fy = y - y.floor();
        let mut out = [0.; 4];
        for dy in 0..2 {
            for dx in 0..2 {
                let weight =
                    if dx == 0 { 1. - fx } else { fx } * if dy == 0 { 1. - fy } else { fy };
                if weight == 0. {
                    continue;
                }
                let pixel = self.pixel(ix + dx, iy + dy);
                for c in 0..4 {
                    out[c] += pixel[c] * weight;
                }
            }
        }
        if out[3] <= 0. {
            return [0; 4];
        }
        [
            (out[0] / out[3] * 255.).round().clamp(0., 255.) as u8,
            (out[1] / out[3] * 255.).round().clamp(0., 255.) as u8,
            (out[2] / out[3] * 255.).round().clamp(0., 255.) as u8,
            out[3].round().clamp(0., 255.) as u8,
        ]
    }
    fn pixel(&mut self, x: i32, y: i32) -> [f64; 4] {
        if let Some(&pixel) = self.output.get(&(x, y)) {
            return pixel;
        }
        let extent = (self.weights.len() / 2) as i32;
        let mut out = [0.; 4];
        for index in 0..self.weights.len() {
            let weight = self.weights[index];
            let p = self.row_pixel(x, y + index as i32 - extent);
            for c in 0..4 {
                out[c] += p[c] * weight;
            }
        }
        if self.output.len() >= 262_144 {
            self.output.clear();
        }
        self.output.insert((x, y), out);
        out
    }
    fn row_pixel(&mut self, x: i32, y: i32) -> [f64; 4] {
        const BLOCK: i32 = 32;
        let block = x.div_euclid(BLOCK);
        if !self.horizontal.contains_key(&(block, y)) {
            let extent = (self.weights.len() / 2) as i32;
            let mut row = vec![[0.; 4]; BLOCK as usize];
            for (offset, out) in row.iter_mut().enumerate() {
                for (index, &weight) in self.weights.iter().enumerate() {
                    let sx =
                        (block * BLOCK + offset as i32 + index as i32 - extent) as f64 * self.step;
                    let sy = y as f64 * self.step;
                    let p = self.source_pixel(sx, sy);
                    for c in 0..4 {
                        out[c] += p[c] * weight;
                    }
                }
            }
            if self.horizontal.len() >= 8192 {
                self.horizontal.clear();
            }
            self.horizontal.insert((block, y), row);
        }
        self.horizontal[&(block, y)][x.rem_euclid(BLOCK) as usize]
    }
    fn source_pixel(&self, x: f64, y: f64) -> [f64; 4] {
        let ix = x.floor() as i64;
        let iy = y.floor() as i64;
        let fx = x - x.floor();
        let fy = y - y.floor();
        let mut out = [0.; 4];
        for dy in 0..2 {
            for dx in 0..2 {
                let weight =
                    if dx == 0 { 1. - fx } else { fx } * if dy == 0 { 1. - fy } else { fy };
                if weight == 0. {
                    continue;
                }
                let x = ix + dx;
                let y = iy + dy;
                let p = if x >= 0
                    && y >= 0
                    && x < self.source.width as i64
                    && y < self.source.height as i64
                {
                    self.source.get(x as u32, y as u32)
                } else {
                    self.background
                };
                let alpha = p[3] as f64 / 255.;
                for c in 0..3 {
                    out[c] += p[c] as f64 * alpha * weight;
                }
                out[3] += p[3] as f64 * weight;
            }
        }
        out
    }
}

#[derive(Clone, Copy, Debug)]
pub enum GradientShape {
    Linear,
    Radial,
    Angle,
    Reflected,
    Diamond,
}
#[allow(clippy::too_many_arguments)]
pub fn gradient(
    doc: &mut Document,
    id: Uuid,
    mask: bool,
    start: [f64; 2],
    end: [f64; 2],
    from: [u8; 4],
    to: [u8; 4],
    shape: GradientShape,
    opacity: f64,
) -> Result<()> {
    ensure!(
        start.iter().chain(end.iter()).all(|x| x.is_finite()),
        "渐变端点无效"
    );
    let delta = [end[0] - start[0], end[1] - start[1]];
    let length = delta[0].hypot(delta[1]);
    if length < 0.5 {
        return Ok(());
    }
    let original = doc
        .layer(id)
        .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?
        .clone();
    let (mut raster, t) = if mask {
        let m = original
            .mask
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("图层没有蒙版"))?;
        let t = m.placement.unwrap_or(original.transform);
        let r = if m.raster.width == 1 && m.raster.height == 1 {
            let w = t.size[0].ceil() as u32;
            let h = t.size[1].ceil() as u32;
            ensure!(w as u64 * h as u64 <= MAX_PIXELS, "蒙版超出图像大小上限");
            Raster::solid(w, h, m.raster.get(0, 0))
        } else {
            m.raster.as_ref().clone()
        };
        (r, t)
    } else {
        (
            original
                .image
                .as_deref()
                .cloned()
                .unwrap_or_else(|| Raster::blank(doc.width, doc.height)),
            original.transform,
        )
    };
    for y in 0..raster.height {
        for x in 0..raster.width {
            let p = t.to_document([
                (x as f64 + 0.5) / raster.width as f64,
                (y as f64 + 0.5) / raster.height as f64,
            ]);
            let dx = p[0] - start[0];
            let dy = p[1] - start[1];
            let projection = (dx * delta[0] + dy * delta[1]) / (length * length);
            let u = match shape {
                GradientShape::Linear => projection,
                GradientShape::Radial => dx.hypot(dy) / length,
                GradientShape::Reflected => projection.abs(),
                GradientShape::Angle => {
                    (dy.atan2(dx) - delta[1].atan2(delta[0])).rem_euclid(std::f64::consts::TAU)
                        / std::f64::consts::TAU
                }
                GradientShape::Diamond => {
                    projection.abs() + ((-dx * delta[1] + dy * delta[0]) / (length * length)).abs()
                }
            }
            .clamp(0., 1.);
            let color = std::array::from_fn(|c| {
                (from[c] as f64 * (1. - u) + to[c] as f64 * u).round() as u8
            });
            let selection = doc.selection.as_ref().map_or(1., |s| {
                if p[0] < 0. || p[1] < 0. {
                    0.
                } else {
                    s.get(p[0] as u32, p[1] as u32) as f64 / 255.
                }
            });
            let out = over(raster.get(x, y), color, opacity * selection);
            raster.set(
                x,
                y,
                if mask {
                    [out[0], out[0], out[0], 255]
                } else {
                    out
                },
            );
        }
    }
    let layer = doc.layer_mut(id).unwrap();
    if mask {
        layer.mask.as_mut().unwrap().raster = Arc::new(raster);
    } else {
        layer.image = Some(Arc::new(raster));
        layer.text = None;
        layer.shape = None;
    }
    Ok(())
}

/// Coherent randomized patch search ported from the original C implementation.
pub fn content_aware_fill(image: &Raster, selection: &Selection) -> Result<Raster> {
    ensure!(
        image.width == selection.width && image.height == selection.height,
        "填充蒙版尺寸不一致"
    );
    let mut data = image.to_rgba();
    let coverage = selection.canvas_coverage();
    crate::pixels::premultiply(&mut data);
    ensure!(
        content_fill(
            &mut data,
            &coverage,
            image.width as usize,
            image.height as usize
        ),
        "内容识别填充需要选区外完整、不透明的源像素块"
    );
    crate::pixels::unpremultiply(&mut data);
    for (i, &s) in coverage.iter().enumerate() {
        if s < 255 {
            let a = image.get(i as u32 % image.width, i as u32 / image.width);
            if s == 0 {
                data[i * 4..i * 4 + 4].copy_from_slice(&a);
                continue;
            }
            let b = data[i * 4..i * 4 + 4].try_into().unwrap();
            data[i * 4..i * 4 + 4].copy_from_slice(&interpolate(a, b, s as f64 / 255.));
        }
    }
    Ok(Raster::from_rgba(image.width, image.height, data))
}
fn neighbors(x: usize, y: usize, w: usize, h: usize) -> Vec<(usize, usize)> {
    let mut n = Vec::with_capacity(4);
    if x > 0 {
        n.push((x - 1, y));
    }
    if x + 1 < w {
        n.push((x + 1, y));
    }
    if y > 0 {
        n.push((x, y - 1));
    }
    if y + 1 < h {
        n.push((x, y + 1));
    }
    n
}

/// Border-seeded, color/edge-aware foreground segmentation. This is a Windows algorithm,
/// not the Apple Vision model; results on similar-color complex backgrounds differ.
pub fn select_subject(image: &Raster, tolerance: u8) -> Selection {
    let w = image.width;
    let h = image.height;
    let mut background = vec![false; w as usize * h as usize];
    let mut queue = VecDeque::new();
    for x in 0..w {
        queue.push_back((x, 0));
        queue.push_back((x, h - 1));
    }
    for y in 0..h {
        queue.push_back((0, y));
        queue.push_back((w - 1, y));
    }
    let border = [
        image.get(0, 0),
        image.get(w - 1, 0),
        image.get(0, h - 1),
        image.get(w - 1, h - 1),
    ];
    while let Some((x, y)) = queue.pop_front() {
        let i = (y * w + x) as usize;
        if background[i] {
            continue;
        }
        let p = image.get(x, y);
        let matches = p[3] == 0
            || border
                .iter()
                .any(|b| (0..3).all(|c| p[c].abs_diff(b[c]) <= tolerance));
        if !matches {
            continue;
        }
        background[i] = true;
        for (nx, ny) in neighbors(x as usize, y as usize, w as usize, h as usize) {
            queue.push_back((nx as u32, ny as u32));
        }
    }
    Selection::from_coverage(
        w,
        h,
        background
            .into_iter()
            .enumerate()
            .map(|(i, b)| {
                if b {
                    0
                } else {
                    image.get(i as u32 % w, i as u32 / w)[3]
                }
            })
            .collect::<Vec<_>>(),
    )
}
pub fn select_object(image: &Raster, point: [f64; 2], tolerance: u8) -> Selection {
    crate::selection::wand(
        image,
        point,
        crate::selection::WandSettings {
            tolerance,
            sample_radius: 1,
            contiguous: true,
        },
    )
}

pub fn add_mask(doc: &mut Document, id: Uuid, revealing: bool) -> Result<()> {
    let layer = doc
        .layer_mut(id)
        .ok_or_else(|| anyhow::anyhow!("图层已不存在"))?;
    let gray = if revealing { 255 } else { 0 };
    layer.mask = Some(Mask {
        raster: Arc::new(Raster::solid(1, 1, [gray, gray, gray, 255])),
        enabled: true,
        linked: true,
        placement: None,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn soft_dabs_across_tile_edges_preserve_coverage_selection_and_history() {
        let (width, height) = (259, 131);
        let mut doc = Document::new(width, height);
        let base = [87, 139, 203, 173];
        doc.active_mut().unwrap().image = Some(Arc::new(Raster::solid(width, height, base)));
        let selection: Vec<_> = (0..width * height).map(|i| (i % 256) as u8).collect();
        doc.selection = Some(Selection::from_coverage(width, height, selection.clone()));
        let snapshot = doc.clone();
        let settings = BrushSettings {
            diameter: 49.,
            hardness: 0.35,
            opacity: 0.6,
            color: [211, 73, 129, 219],
            ..Default::default()
        };
        let mut stroke =
            BrushStroke::new(&doc, doc.active_layer_id.unwrap(), false, settings, None).unwrap();
        let points = [[126.5, 126.5], [130.5, 126.5], [257.5, 130.5]];
        for &point in &points {
            stroke.dab(point, point).unwrap();
        }
        for y in 0..height {
            for x in 0..width {
                let docp = snapshot.active().unwrap().transform.to_document([
                    (x as f64 + 0.5) / width as f64,
                    (y as f64 + 0.5) / height as f64,
                ]);
                let mut covered = 0.;
                for point in points {
                    let u =
                        (docp[0] - point[0]).hypot(docp[1] - point[1]) / (settings.diameter / 2.);
                    if u < 1. {
                        let weight = if u <= settings.hardness {
                            1.
                        } else {
                            let t = (1. - u) / (1. - settings.hardness);
                            t * t * (3. - 2. * t)
                        };
                        covered = 1. - (1. - covered) * (1. - weight);
                    }
                }
                let select = selection[(y * width + x) as usize] as f64 / 255.;
                let expected = over(base, settings.color, covered * settings.opacity * select);
                assert_eq!(stroke.raster.get(x, y), expected, "pixel ({x}, {y})");
            }
        }
        stroke.commit(&mut doc).unwrap();
        let retained = snapshot.active().unwrap().image.as_ref().unwrap();
        assert!(retained.tiles.is_empty());
        assert_eq!(retained.get(128, 128), base);
        assert!(Arc::ptr_eq(
            &retained.pixels,
            &doc.active().unwrap().image.as_ref().unwrap().pixels
        ));
    }
    #[test]
    fn stroke_opacity_caps_overlap_and_shares_original() {
        let mut d = Document::new(64, 64);
        let id = d.active_layer_id.unwrap();
        let settings = BrushSettings {
            diameter: 10.,
            opacity: 0.5,
            color: [255, 0, 0, 255],
            ..Default::default()
        };
        paint(
            &mut d,
            id,
            false,
            &[[20., 20.], [25., 20.], [20., 20.]],
            settings,
            None,
        )
        .unwrap();
        let r = d.active().unwrap().image.as_ref().unwrap();
        assert_eq!(r.get(20, 20)[3], 128);
        assert_eq!(r.tiles.len(), 1);
        assert!(r.pixels.is_empty());
    }
    #[test]
    fn selection_empty_blocks_paint() {
        let mut d = Document::new(20, 20);
        let id = d.active_layer_id.unwrap();
        d.selection = Some(Selection::empty(20, 20));
        let before = d.clone();
        paint(
            &mut d,
            id,
            false,
            &[[10., 10.]],
            BrushSettings::default(),
            None,
        )
        .unwrap();
        assert_eq!(d, before);
    }
    #[test]
    fn retained_off_canvas_selection_blocks_edits_until_moved_back() {
        let mut doc = Document::new(12, 12);
        let id = doc.active_layer_id.unwrap();
        let original = crate::selection::marquee(12, 12, [3., 3., 4., 4.], false, false);
        doc.selection = Some(crate::selection::translated(&original, -20, 0, 12, 12));
        paint(
            &mut doc,
            id,
            false,
            &[[5., 5.]],
            BrushSettings {
                diameter: 12.,
                hardness: 1.,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(doc.active().unwrap().image.is_none());
        let retained = doc.selection.as_ref().unwrap();
        doc.selection = Some(crate::selection::translated(retained, 20, 0, 12, 12));
        paint(
            &mut doc,
            id,
            false,
            &[[5., 5.]],
            BrushSettings {
                diameter: 12.,
                hardness: 1.,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let image = doc.active().unwrap().image.as_ref().unwrap();
        assert_eq!(image.get(4, 4)[3], 255);
        assert_eq!(image.get(2, 4)[3], 0);
        assert_eq!(doc.selection.as_ref().unwrap(), &original);
    }
    #[test]
    fn clone_copies_source_snapshot() {
        let mut d = Document::new(20, 10);
        let id = d.active_layer_id.unwrap();
        d.active_mut().unwrap().image = Some(Arc::new(Raster::solid(20, 10, [255, 0, 0, 255])));
        paint(
            &mut d,
            id,
            false,
            &[[15., 5.]],
            BrushSettings {
                diameter: 4.,
                mode: BrushMode::Clone { offset: [-10., 0.] },
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(
            d.active().unwrap().image.as_ref().unwrap().get(15, 5),
            [255, 0, 0, 255]
        );
    }
    #[test]
    fn erase_keeps_straight_color_and_reduces_alpha() {
        let mut d = Document::new(30, 30);
        let id = d.active_layer_id.unwrap();
        d.active_mut().unwrap().image = Some(Arc::new(Raster::solid(30, 30, [80, 120, 160, 255])));
        paint(
            &mut d,
            id,
            false,
            &[[15., 15.]],
            BrushSettings {
                diameter: 4.,
                mode: BrushMode::Erase,
                opacity: 0.5,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(
            d.active().unwrap().image.as_ref().unwrap().get(15, 15),
            [80, 120, 160, 128]
        );
    }
    #[test]
    fn gradient_lives_in_document_coordinates() {
        let mut d = Document::new(10, 2);
        let id = d.active_layer_id.unwrap();
        gradient(
            &mut d,
            id,
            false,
            [0., 0.],
            [10., 0.],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            GradientShape::Linear,
            1.,
        )
        .unwrap();
        assert_eq!(
            d.active().unwrap().image.as_ref().unwrap().get(5, 0),
            [140, 140, 140, 255]
        );
    }
    #[test]
    fn fill_preserves_unselected_pixels() {
        let mut r = Raster::solid(15, 15, [100, 80, 60, 255]);
        r.set(7, 7, [255, 0, 0, 255]);
        let s = crate::selection::marquee(15, 15, [7., 7., 1., 1.], false, false);
        let out = content_aware_fill(&r, &s).unwrap();
        assert_eq!(out.get(7, 7), [100, 80, 60, 255]);
        assert_eq!(out.get(0, 0), r.get(0, 0));
    }
    #[test]
    fn subject_excludes_connected_uniform_border() {
        let mut r = Raster::solid(9, 9, [255; 4]);
        for y in 3..6 {
            for x in 3..6 {
                r.set(x, y, [255, 0, 0, 255]);
            }
        }
        let s = select_subject(&r, 16);
        assert_eq!(s.get(0, 0), 0);
        assert_eq!(s.get(4, 4), 255);
    }

    #[test]
    fn sparse_samples_follow_a_curve_and_replace_provisional_tail() {
        let mut d = Document::new(100, 100);
        let id = d.active_layer_id.unwrap();
        let mut stroke = BrushStroke::new(
            &d,
            id,
            false,
            BrushSettings {
                diameter: 3.,
                color: [255, 0, 0, 255],
                ..Default::default()
            },
            None,
        )
        .unwrap();
        stroke.append([20., 70.]).unwrap();
        stroke.append([40., 30.]).unwrap();
        stroke.append([60., 30.]).unwrap();
        stroke.append([80., 70.]).unwrap();
        assert!(stroke.raster.get(50, 27)[3] > 0);
        assert_eq!(stroke.raster.get(50, 30)[3], 0);
        stroke.finish_at([80., 70.]).unwrap();
        let first = stroke.raster.clone();
        stroke.finish_at([80., 70.]).unwrap();
        assert_eq!(first, stroke.raster);
        stroke.commit(&mut d).unwrap();
    }

    #[test]
    fn blur_sample_uses_dense_gaussian_and_document_pixel_area_scale() {
        let source = Arc::new(Raster::from_rgba(
            9,
            9,
            (0..81)
                .flat_map(|i| if i == 40 { [255, 40, 0, 255] } else { [0; 4] })
                .collect(),
        ));
        let mut blur = BlurSample::new(source, Transform::new(0., 0., 18., 4.5), 1., false);
        let center = blur.sample([9., 2.25]);
        assert_eq!(&center[..3], &[255, 40, 0]);
        assert!((center[3] as i32 - 41).abs() <= 1, "center={center:?}");
        let adjacent = blur.sample([11., 2.25]);
        assert!(
            (adjacent[3] as i32 - 25).abs() <= 1,
            "adjacent={adjacent:?}"
        );
        let far = blur.sample([17., 2.25]);
        assert_eq!(far[3], 0);
    }

    #[test]
    fn blur_masks_keep_background_and_only_selected_pixels_change() {
        let mut blur = BlurSample::new(
            Arc::new(Raster::solid(4, 4, [255; 4])),
            Transform::new(0., 0., 4., 4.),
            2.,
            true,
        );
        assert_eq!(blur.sample([0.5, 0.5]), [255; 4]);
        assert_eq!(blur.sample([-4., -4.]), [255; 4]);
        let mut d = Document::new(32, 32);
        let id = d.active_layer_id.unwrap();
        d.active_mut().unwrap().image = Some(Arc::new(Raster::from_rgba(
            32,
            32,
            (0..1024)
                .flat_map(|i| {
                    if i % 32 < 16 {
                        [0, 0, 0, 255]
                    } else {
                        [255; 4]
                    }
                })
                .collect(),
        )));
        d.selection = Some(crate::selection::marquee(
            32,
            32,
            [14., 10., 3., 10.],
            false,
            false,
        ));
        paint(
            &mut d,
            id,
            false,
            &[[16., 16.]],
            BrushSettings {
                diameter: 16.,
                mode: BrushMode::Blur { radius: 2. },
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let image = d.active().unwrap().image.as_ref().unwrap();
        assert_eq!(image.get(13, 16), [0, 0, 0, 255]);
        assert_eq!(image.get(17, 16), [255; 4]);
        assert!(image.get(15, 16)[0] > 0);
        assert!(image.get(16, 16)[0] < 255);
    }

    #[test]
    fn group_mask_paint_is_sparse_and_erasing_reveals() {
        let mut d = Document::new(4096, 4096);
        let group = crate::edit::add_group(&mut d, "Group", &[]).unwrap();
        add_mask(&mut d, group, true).unwrap();
        paint(
            &mut d,
            group,
            true,
            &[[200., 200.]],
            BrushSettings {
                diameter: 8.,
                color: [0, 0, 0, 255],
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let mask = d.layer(group).unwrap().mask.as_ref().unwrap();
        assert_eq!(mask.raster.get(200, 200)[0], 0);
        assert_eq!(mask.raster.tiles.len(), 1);
        assert_eq!(mask.raster.pixels.len(), 4);
        paint(
            &mut d,
            group,
            true,
            &[[200., 200.]],
            BrushSettings {
                diameter: 8.,
                mode: BrushMode::Erase,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(
            d.layer(group)
                .unwrap()
                .mask
                .as_ref()
                .unwrap()
                .raster
                .get(200, 200)[0],
            255
        );
    }

    #[test]
    fn image_growth_preserves_linked_mask_placement_and_black_exterior() {
        let mut d = Document::new(32, 32);
        let id = d.active_layer_id.unwrap();
        let layer = d.active_mut().unwrap();
        layer.transform = Transform::new(10., 10., 4., 4.);
        layer.image = Some(Arc::new(Raster::solid(4, 4, [255; 4])));
        let mut mask = Raster::solid(4, 4, [0, 0, 0, 255]);
        mask.set(1, 1, [255; 4]);
        layer.mask = Some(Mask {
            raster: Arc::new(mask),
            enabled: true,
            linked: true,
            placement: None,
        });
        paint(
            &mut d,
            id,
            false,
            &[[20., 12.]],
            BrushSettings {
                diameter: 4.,
                color: [255, 0, 0, 255],
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(
            d.active().unwrap().mask.as_ref().unwrap().placement,
            Some(Transform::new(10., 10., 4., 4.))
        );
        let shown = crate::render::render_document(&d);
        assert_eq!(shown.get(11, 11), [255; 4]);
        assert_eq!(shown.get(20, 12)[3], 0);
        paint(
            &mut d,
            id,
            true,
            &[[20., 12.]],
            BrushSettings {
                diameter: 2.,
                color: [255; 4],
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let shown = crate::render::render_document(&d);
        assert!(shown.get(20, 12)[3] > 0);
        assert_eq!(shown.get(21, 11)[3], 0);
    }
    #[test]
    fn warp_uses_unselected_pickup_then_blends_soft_selection_only_once() {
        let mut doc = Document::new(12, 8);
        let id = doc.active_layer_id.unwrap();
        let mut pixels = vec![0; 12 * 8 * 4];
        for y in 0..8 {
            for x in 0..12 {
                let p = if x == 4 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                };
                pixels[(y * 12 + x) * 4..(y * 12 + x) * 4 + 4].copy_from_slice(&p);
            }
        }
        let original = Arc::new(Raster::from_rgba(12, 8, pixels));
        doc.active_mut().unwrap().image = Some(original.clone());
        let mut coverage = vec![0; 12 * 8];
        coverage[4 * 12 + 5] = 128;
        doc.selection = Some(Selection::from_coverage(12, 8, coverage));
        for mode in [BrushMode::Smudge, BrushMode::Liquify] {
            let mut edited = doc.clone();
            let settings = BrushSettings {
                diameter: 2.,
                hardness: 1.,
                opacity: 1.,
                mode,
                ..Default::default()
            };
            let mut stroke = BrushStroke::new(&edited, id, false, settings, None).unwrap();
            assert_eq!(stroke.append([4., 4.]).unwrap().changed_pixels, 0);
            stroke.append([5., 4.]).unwrap();
            stroke.finish_at([5., 4.]).unwrap();
            stroke.commit(&mut edited).unwrap();
            assert_eq!(
                edited.active().unwrap().image.as_ref().unwrap().get(5, 4),
                [128, 0, 127, 255]
            );
            assert_eq!(
                edited.active().unwrap().image.as_ref().unwrap().get(4, 4),
                [255, 0, 0, 255]
            );
            assert_eq!(
                edited.active().unwrap().image.as_ref().unwrap().get(5, 3),
                [0, 0, 255, 255]
            );
            let mut stroke = BrushStroke::new(&edited, id, false, settings, None).unwrap();
            stroke.append([4., 4.]).unwrap();
            stroke.finish_at([4., 4.]).unwrap();
            let before = edited.clone();
            stroke.commit(&mut edited).unwrap();
            assert_eq!(edited, before);
        }
    }
    #[test]
    fn rotated_scaled_warp_commits_working_sample_on_the_native_grid() {
        let mut doc = Document::new(64, 64);
        let id = doc.active_layer_id.unwrap();
        let image = Arc::new(Raster::from_rgba(
            12,
            10,
            (0..120)
                .flat_map(|i| [(i % 12 * 20) as u8, (i / 12 * 24) as u8, 30, 255])
                .collect(),
        ));
        let mut transform = Transform::new(15., 17., 24., 20.);
        transform.rotation = 37.;
        transform.flip_x = true;
        doc.active_mut().unwrap().image = Some(image.clone());
        doc.active_mut().unwrap().transform = transform;
        let mut stroke = BrushStroke::new(
            &doc,
            id,
            false,
            BrushSettings {
                diameter: 8.,
                opacity: 0.65,
                hardness: 0.4,
                mode: BrushMode::Liquify,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        stroke.append([27., 27.]).unwrap();
        stroke.append([32., 29.]).unwrap();
        let keys = stroke.coverage.keys().copied().collect::<Vec<_>>();
        assert!(!keys.is_empty());
        for (x, y) in keys {
            let p = stroke.transform.to_document([
                (x as f64 + 0.5) / stroke.raster.width as f64,
                (y as f64 + 0.5) / stroke.raster.height as f64,
            ]);
            assert_eq!(
                stroke.raster.get(x, y),
                stroke.warp.as_mut().unwrap().sample(p)
            );
        }
        stroke.commit(&mut doc).unwrap();
        let layer = doc.active().unwrap();
        assert_eq!(layer.transform.rotation, 37.);
        assert!(layer.transform.flip_x);
        assert!(
            (layer.transform.size[0] / layer.image.as_ref().unwrap().width as f64 - 2.).abs()
                < 1e-12
        );
        assert!(
            (layer.transform.size[1] / layer.image.as_ref().unwrap().height as f64 - 2.).abs()
                < 1e-12
        );
        assert_eq!(image.get(0, 0), [0, 0, 30, 255]);
    }
}
