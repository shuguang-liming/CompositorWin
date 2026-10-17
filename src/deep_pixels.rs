//! Authoritative 16-bit/half-float straight-alpha extended-sRGB pixels.
//! 8-bit views are derived previews; sparse edits retain untouched source bits.
//! The split/merge and half-float kernels follow Compositor's DeepPixels.c,
//! Copyright (c) 2026 Wonder Assembly LLC (MIT), retained in LICENSES.md.
use std::{collections::BTreeMap, sync::Arc};

const TILE: u32 = 128;
const HALF_MAX: f32 = 65_504.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    U16,
    Hdr,
}

/// Eight bytes per stored pixel for either representation. Pixel values are
/// straight alpha, matching Raster; only adapters to old byte kernels premultiply.
#[derive(Clone, Debug)]
pub struct DeepRaster {
    pub width: u32,
    pub height: u32,
    pub depth: Depth,
    pub base: Arc<Vec<u16>>,
    pub tiles: BTreeMap<(u32, u32), Arc<Vec<u16>>>,
}
impl DeepRaster {
    pub fn validate(&self) -> Result<(), String> {
        let expected = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|v| v.checked_mul(4))
            .ok_or("高位深图像尺寸溢出")?;
        if self.width == 0 || self.height == 0 || ![0, 4, expected].contains(&self.base.len()) {
            return Err("高位深栅格存储长度无效".into());
        }
        for (&(tx, ty), tile) in &self.tiles {
            if tx >= self.width.div_ceil(TILE)
                || ty >= self.height.div_ceil(TILE)
                || tile.len() != (TILE * TILE * 4) as usize
            {
                return Err("高位深编辑块无效".into());
            }
        }
        if self.depth == Depth::Hdr {
            for data in std::iter::once(&self.base).chain(self.tiles.values()) {
                if data.as_chunks::<4>().0.iter().any(|p| {
                    p.iter().any(|&v| !half_to_float(v).is_finite())
                        || !(0. ..=1.).contains(&half_to_float(p[3]))
                }) {
                    return Err("HDR 像素含非有限值或无效透明度".into());
                }
            }
        }
        Ok(())
    }
    pub fn blank(width: u32, height: u32, depth: Depth) -> Self {
        Self {
            width,
            height,
            depth,
            base: Arc::new(Vec::new()),
            tiles: BTreeMap::new(),
        }
    }
    pub fn from_u16(width: u32, height: u32, samples: Vec<u16>) -> Result<Self, String> {
        Self::from_bits(width, height, Depth::U16, samples)
    }
    pub fn from_bits(
        width: u32,
        height: u32,
        depth: Depth,
        samples: Vec<u16>,
    ) -> Result<Self, String> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|v| v.checked_mul(4))
            .ok_or("高位深图像尺寸溢出")?;
        if width == 0 || height == 0 || samples.len() != expected {
            return Err("高位深像素数量与图像尺寸不符".into());
        }
        if depth == Depth::Hdr
            && samples.as_chunks::<4>().0.iter().any(|p| {
                p.iter().any(|&v| !half_to_float(v).is_finite())
                    || !(0. ..=1.).contains(&half_to_float(p[3]))
            })
        {
            return Err("HDR 像素含非有限值或无效透明度".into());
        }
        Ok(Self {
            width,
            height,
            depth,
            base: Arc::new(samples),
            tiles: BTreeMap::new(),
        })
    }
    pub fn from_f32(
        width: u32,
        height: u32,
        depth: Depth,
        samples: &[f32],
    ) -> Result<Self, String> {
        if samples
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| !(0. ..=1.).contains(&p[3]))
        {
            return Err("高位深图像透明度超出 0 到 1".into());
        }
        if samples.iter().any(|v| !v.is_finite() || v.abs() > HALF_MAX) {
            return Err("高位深颜色超出有限半浮点范围".into());
        }
        Self::from_bits(
            width,
            height,
            depth,
            samples
                .iter()
                .enumerate()
                .map(|(i, &v)| encode(v, depth, i % 4 == 3))
                .collect(),
        )
    }
    pub fn solid(width: u32, height: u32, depth: Depth, color: [f32; 4]) -> Self {
        let bits: Vec<_> = color
            .into_iter()
            .enumerate()
            .map(|(i, v)| encode(v, depth, i == 3))
            .collect();
        Self {
            width,
            height,
            depth,
            base: Arc::new(bits),
            tiles: BTreeMap::new(),
        }
    }
    pub fn get_bits(&self, x: u32, y: u32) -> [u16; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        if let Some(tile) = self.tiles.get(&(x / TILE, y / TILE)) {
            let at = (((y % TILE) * TILE + x % TILE) * 4) as usize;
            return tile[at..at + 4].try_into().unwrap();
        }
        if self.base.is_empty() {
            return [0; 4];
        }
        let at = if self.base.len() == 4 {
            0
        } else {
            ((y * self.width + x) * 4) as usize
        };
        self.base[at..at + 4].try_into().unwrap()
    }
    pub fn get(&self, x: u32, y: u32) -> [f32; 4] {
        self.get_bits(x, y).map(|v| decode(v, self.depth))
    }
    pub fn set_bits(&mut self, x: u32, y: u32, color: [u16; 4]) {
        if x >= self.width || y >= self.height || self.get_bits(x, y) == color {
            return;
        }
        let key = (x / TILE, y / TILE);
        if !self.tiles.contains_key(&key) {
            let mut tile = vec![0; (TILE * TILE * 4) as usize];
            for row in 0..TILE.min(self.height - key.1 * TILE) {
                for col in 0..TILE.min(self.width - key.0 * TILE) {
                    let at = ((row * TILE + col) * 4) as usize;
                    tile[at..at + 4]
                        .copy_from_slice(&self.get_bits(key.0 * TILE + col, key.1 * TILE + row));
                }
            }
            self.tiles.insert(key, Arc::new(tile));
        }
        let tile = Arc::make_mut(self.tiles.get_mut(&key).unwrap());
        let at = (((y % TILE) * TILE + x % TILE) * 4) as usize;
        tile[at..at + 4].copy_from_slice(&color);
    }
    pub fn set(&mut self, x: u32, y: u32, color: [f32; 4]) {
        self.set_bits(
            x,
            y,
            std::array::from_fn(|c| encode(color[c], self.depth, c == 3)),
        );
    }
    pub fn to_bits(&self) -> Vec<u16> {
        if self.tiles.is_empty() && self.base.len() > 4 {
            return self.base.as_ref().clone();
        }
        let mut out = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for y in 0..self.height {
            for x in 0..self.width {
                out.extend_from_slice(&self.get_bits(x, y));
            }
        }
        out
    }
    pub fn to_f32(&self) -> Vec<f32> {
        self.to_bits()
            .into_iter()
            .map(|v| decode(v, self.depth))
            .collect()
    }
    pub fn to_u16(&self) -> Vec<u16> {
        if self.depth == Depth::U16 {
            self.to_bits()
        } else {
            self.to_f32()
                .into_iter()
                .map(|v| (v.clamp(0., 1.) * 65535.).round() as u16)
                .collect()
        }
    }
    pub fn materialized(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            depth: self.depth,
            base: Arc::new(self.to_bits()),
            tiles: BTreeMap::new(),
        }
    }
    pub fn byte_len(&self) -> usize {
        (self.base.len() + self.tiles.values().map(|v| v.len()).sum::<usize>()) * 2
    }
    pub fn shares(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.depth == other.depth
            && Arc::ptr_eq(&self.base, &other.base)
            && self.tiles.len() == other.tiles.len()
            && self.tiles.iter().all(|(key, tile)| {
                other
                    .tiles
                    .get(key)
                    .is_some_and(|other| Arc::ptr_eq(tile, other))
            })
    }
    pub fn peak(&self) -> f32 {
        let mut peak: f32 = 0.;
        for y in 0..self.height {
            for x in 0..self.width {
                let p = self.get(x, y);
                if p[3] > 0. {
                    peak = peak.max(p[0]).max(p[1]).max(p[2]);
                }
            }
        }
        peak
    }
    pub fn preview_pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let pixel = self.get(x, y);
        let alpha = (pixel[3].clamp(0., 1.) * 255.).round();
        if alpha == 0. {
            return [0; 4];
        }
        // Fixed triangular noise, mirroring the original dithered display.
        let mut out = [0; 4];
        for c in 0..3 {
            let v =
                pixel[c].clamp(0., 1.) * 255. + noise(x, y, c as u32) + noise(x, y, c as u32 + 3)
                    - 1.;
            out[c] = v.round().clamp(0., 255.) as u8;
        }
        out[3] = alpha as u8;
        out
    }
    pub fn preview_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for y in 0..self.height {
            for x in 0..self.width {
                out.extend_from_slice(&self.preview_pixel(x, y));
            }
        }
        out
    }
    /// Process at full precision. HDR values retain their extended range.
    pub fn map_colors(&self, operation: impl Fn([f32; 3]) -> [f32; 3]) -> Self {
        let mut out = self.to_bits();
        for p in out.as_chunks_mut::<4>().0 {
            let color = [
                decode(p[0], self.depth),
                decode(p[1], self.depth),
                decode(p[2], self.depth),
            ];
            let changed = operation(color);
            for c in 0..3 {
                p[c] = encode(changed[c], self.depth, false);
            }
        }
        Self {
            width: self.width,
            height: self.height,
            depth: self.depth,
            base: Arc::new(out),
            tiles: BTreeMap::new(),
        }
    }
    pub fn exposure(&self, stops: f32, offset: f32, gamma: f32) -> Self {
        let scale = 2f32.powf(stops);
        let inverse_gamma = 1. / gamma.max(0.01);
        self.map_colors(|color| {
            color.map(|v| encoded((linear(v) * scale + offset).max(0.).powf(inverse_gamma)))
        })
    }
    pub fn converted(&self, depth: Depth) -> Self {
        if depth == self.depth {
            return self.clone();
        }
        let bits = self
            .to_bits()
            .into_iter()
            .enumerate()
            .map(|(i, v)| encode(decode(v, self.depth), depth, i % 4 == 3))
            .collect();
        Self {
            width: self.width,
            height: self.height,
            depth,
            base: Arc::new(bits),
            tiles: BTreeMap::new(),
        }
    }
    /// Copy real painted pixels only. Unchanged preview bytes retain low bits,
    /// including untouched pixels sharing a stroke's 128-pixel tile.
    pub fn take_painted(&mut self, painted: &[u8], unpainted: &[u8]) -> Result<(), String> {
        if painted.len() != unpainted.len()
            || painted.len() != self.width as usize * self.height as usize * 4
        {
            return Err("绘画像素尺寸不符".into());
        }
        for (i, (p, b)) in painted
            .as_chunks::<4>()
            .0
            .iter()
            .zip(unpainted.as_chunks::<4>().0)
            .enumerate()
        {
            if p != b {
                self.set(
                    i as u32 % self.width,
                    i as u32 / self.width,
                    p.map(|v| v as f32 / 255.),
                );
            }
        }
        Ok(())
    }
    pub fn crop(&self, left: u32, top: u32, width: u32, height: u32) -> Self {
        let mut out = Self::blank(width, height, self.depth);
        let mut bits = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                bits.extend_from_slice(
                    &self.get_bits(left.saturating_add(x), top.saturating_add(y)),
                );
            }
        }
        out.base = Arc::new(bits);
        out
    }
    pub fn padded(&self, width: u32, height: u32, left: i64, top: i64) -> Self {
        if width == self.width && height == self.height && left == 0 && top == 0 {
            return self.clone();
        }
        let mut out = Self::blank(width, height, self.depth);
        if self.base.is_empty() || self.base.as_slice() == [0; 4] {
            for (&(tx, ty), tile) in &self.tiles {
                if left.rem_euclid(TILE as i64) == 0 && top.rem_euclid(TILE as i64) == 0 {
                    let dx = tx as i64 * TILE as i64 - left;
                    let dy = ty as i64 * TILE as i64 - top;
                    if dx >= 0
                        && dy >= 0
                        && dx + TILE as i64 <= width as i64
                        && dy + TILE as i64 <= height as i64
                    {
                        out.tiles
                            .insert((dx as u32 / TILE, dy as u32 / TILE), tile.clone());
                        continue;
                    }
                }
                for y in ty * TILE..((ty + 1) * TILE).min(self.height) {
                    for x in tx * TILE..((tx + 1) * TILE).min(self.width) {
                        let dx = x as i64 - left;
                        let dy = y as i64 - top;
                        if dx >= 0 && dy >= 0 && dx < width as i64 && dy < height as i64 {
                            let bits = self.get_bits(x, y);
                            if bits != [0; 4] {
                                out.set_bits(dx as u32, dy as u32, bits);
                            }
                        }
                    }
                }
            }
            return out;
        }
        for y in 0..self.height {
            for x in 0..self.width {
                let dx = x as i64 - left;
                let dy = y as i64 - top;
                if dx >= 0 && dy >= 0 && dx < width as i64 && dy < height as i64 {
                    let bits = self.get_bits(x, y);
                    if bits != [0; 4] {
                        out.set_bits(dx as u32, dy as u32, bits);
                    }
                }
            }
        }
        out
    }
    pub fn flipped(&self, horizontal: bool, vertical: bool) -> Self {
        let mut bits = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for y in 0..self.height {
            for x in 0..self.width {
                bits.extend_from_slice(&self.get_bits(
                    if horizontal { self.width - 1 - x } else { x },
                    if vertical { self.height - 1 - y } else { y },
                ));
            }
        }
        Self {
            width: self.width,
            height: self.height,
            depth: self.depth,
            base: Arc::new(bits),
            tiles: BTreeMap::new(),
        }
    }
    pub fn rotated_quarter(&self, clockwise: bool) -> Self {
        let mut bits = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for y in 0..self.width {
            for x in 0..self.height {
                let (sx, sy) = if clockwise {
                    (y, self.height - 1 - x)
                } else {
                    (self.width - 1 - y, x)
                };
                bits.extend_from_slice(&self.get_bits(sx, sy));
            }
        }
        Self {
            width: self.height,
            height: self.width,
            depth: self.depth,
            base: Arc::new(bits),
            tiles: BTreeMap::new(),
        }
    }
    /// Bilinear premultiplied interpolation, then return straight extended RGB.
    pub fn sample(&self, x: f64, y: f64) -> [f32; 4] {
        if !x.is_finite() || !y.is_finite() {
            return [0.; 4];
        }
        let (ix, iy) = (x.floor() as i64, y.floor() as i64);
        let (fx, fy) = ((x - x.floor()) as f32, (y - y.floor()) as f32);
        let mut out = [0.; 4];
        for dy in 0..2 {
            for dx in 0..2 {
                let (xx, yy) = (ix + dx, iy + dy);
                if xx < 0 || yy < 0 || xx >= self.width as i64 || yy >= self.height as i64 {
                    continue;
                }
                let p = self.get(xx as u32, yy as u32);
                let weight =
                    if dx == 0 { 1. - fx } else { fx } * if dy == 0 { 1. - fy } else { fy };
                for c in 0..3 {
                    out[c] += p[c] * p[3] * weight;
                }
                out[3] += p[3] * weight;
            }
        }
        if out[3] > 0. {
            for c in 0..3 {
                out[c] /= out[3];
            }
        }
        out
    }
    pub fn resized(&self, width: u32, height: u32) -> Self {
        if width == self.width && height == self.height {
            return self.clone();
        }
        let x_weights = resample_weights(self.width, width);
        let y_weights = resample_weights(self.height, height);
        let mut bits = Vec::with_capacity(width as usize * height as usize * 4);
        for yw in &y_weights {
            for xw in &x_weights {
                let mut sum = [0f64; 4];
                for &(y, wy) in yw {
                    for &(x, wx) in xw {
                        let p = self.get(x, y);
                        let alpha = p[3] as f64 * wx * wy;
                        for c in 0..3 {
                            sum[c] += p[c] as f64 * alpha;
                        }
                        sum[3] += alpha;
                    }
                }
                let p = std::array::from_fn::<_, 4, _>(|c| {
                    if c == 3 {
                        sum[3] as f32
                    } else if sum[3] > 0. {
                        (sum[c] / sum[3]) as f32
                    } else {
                        0.
                    }
                });
                bits.extend(
                    p.into_iter()
                        .enumerate()
                        .map(|(c, v)| encode(v, self.depth, c == 3)),
                );
            }
        }
        Self {
            width,
            height,
            depth: self.depth,
            base: Arc::new(bits),
            tiles: BTreeMap::new(),
        }
    }
}

/// Exact area weights while reducing; two linear taps while enlarging.
fn resample_weights(source: u32, destination: u32) -> Vec<Vec<(u32, f64)>> {
    let scale = source as f64 / destination as f64;
    (0..destination)
        .map(|index| {
            if scale > 1. {
                let start = index as f64 * scale;
                let end = (index + 1) as f64 * scale;
                (start.floor() as u32..(end.ceil() as u32).min(source))
                    .map(|p| (p, (end.min(p as f64 + 1.) - start.max(p as f64)) / scale))
                    .collect()
            } else {
                let center = ((index as f64 + 0.5) * scale - 0.5).clamp(0., (source - 1) as f64);
                let low = center.floor() as u32;
                let t = center - low as f64;
                if t > 0. {
                    vec![(low, 1. - t), ((low + 1).min(source - 1), t)]
                } else {
                    vec![(low, 1.)]
                }
            }
        })
        .collect()
}
impl DeepRaster {
    /// Three separable box passes approximate a Gaussian in floating-point
    /// premultiplied space. It avoids repeatedly quantizing smooth gradients
    /// through byte buffers and retains HDR highlights and transparent edges.
    pub fn gaussian_blur(&self, radius: f64) -> Self {
        if !radius.is_finite() || radius <= 0.01 {
            return self.clone();
        }
        let sigma = radius.min(1000.);
        let ideal = (4. * sigma * sigma + 1.).sqrt();
        let mut lower = ideal.floor() as usize;
        if lower.is_multiple_of(2) {
            lower = lower.saturating_sub(1);
        }
        lower = lower.max(1);
        let upper = lower + 2;
        let m = ((12. * sigma * sigma - 3. * (lower * lower) as f64 - 12. * lower as f64 - 9.)
            / (-4. * lower as f64 - 4.))
            .round()
            .clamp(0., 3.) as usize;
        let mut data = self.to_f32();
        for p in data.as_chunks_mut::<4>().0 {
            for c in 0..3 {
                p[c] *= p[3];
            }
        }
        let width = self.width as usize;
        let height = self.height as usize;
        for pass in 0..3 {
            let r = (if pass < m { lower } else { upper }) / 2;
            if r == 0 {
                continue;
            }
            let mut temp = vec![0.; data.len()];
            for y in 0..height {
                box_line(
                    &data[y * width * 4..(y + 1) * width * 4],
                    &mut temp[y * width * 4..(y + 1) * width * 4],
                    width,
                    1,
                    r,
                );
            }
            for x in 0..width {
                box_line(&temp[x * 4..], &mut data[x * 4..], height, width, r);
            }
        }
        let mut bits = Vec::with_capacity(data.len());
        for p in data.as_chunks::<4>().0 {
            for &v in &p[..3] {
                bits.push(encode(
                    if p[3] > 0. { v / p[3] } else { 0. },
                    self.depth,
                    false,
                ));
            }
            bits.push(encode(p[3], self.depth, true));
        }
        Self {
            width: self.width,
            height: self.height,
            depth: self.depth,
            base: Arc::new(bits),
            tiles: BTreeMap::new(),
        }
    }
    pub fn sample_nearest(&self, x: f64, y: f64) -> [f32; 4] {
        if !x.is_finite() || !y.is_finite() {
            return [0.; 4];
        }
        let (x, y) = ((x + 0.5).floor() as i64, (y + 0.5).floor() as i64);
        if x < 0 || y < 0 {
            return [0.; 4];
        }
        self.get(x as u32, y as u32)
    }
    /// TL/TR/BR/BL destinations in pixel-edge coordinates, without a byte intermediate.
    pub fn perspective_warp(
        &self,
        width: u32,
        height: u32,
        corners: [[f64; 2]; 4],
    ) -> Result<Self, String> {
        let source = [
            [0., 0.],
            [self.width as f64, 0.],
            [self.width as f64, self.height as f64],
            [0., self.height as f64],
        ];
        let mut matrix = [[0.; 9]; 8];
        for i in 0..4 {
            let [x, y] = corners[i];
            let [u, v] = source[i];
            matrix[i * 2] = [x, y, 1., 0., 0., 0., -u * x, -u * y, u];
            matrix[i * 2 + 1] = [0., 0., 0., x, y, 1., -v * x, -v * y, v];
        }
        for c in 0..8 {
            let pivot = (c..8)
                .max_by(|&a, &b| matrix[a][c].abs().total_cmp(&matrix[b][c].abs()))
                .unwrap();
            matrix.swap(c, pivot);
            let divisor = matrix[c][c];
            if divisor.abs() < 1e-10 {
                return Err("变形角点退化".into());
            }
            for v in &mut matrix[c][c..] {
                *v /= divisor;
            }
            let row = matrix[c];
            for (r, other) in matrix.iter_mut().enumerate() {
                if r != c {
                    let f = other[c];
                    for (v, &p) in other[c..].iter_mut().zip(&row[c..]) {
                        *v -= p * f;
                    }
                }
            }
        }
        let a = std::array::from_fn::<_, 8, _>(|i| matrix[i][8]);
        let mut out = Self::blank(width, height, self.depth);
        for y in 0..height {
            for x in 0..width {
                let px = x as f64 + 0.5;
                let py = y as f64 + 0.5;
                let denominator = a[6] * px + a[7] * py + 1.;
                if denominator.abs() < 1e-10 {
                    continue;
                }
                let sx = (a[0] * px + a[1] * py + a[2]) / denominator - 0.5;
                let sy = (a[3] * px + a[4] * py + a[5]) / denominator - 0.5;
                out.set(x, y, self.sample(sx, sy));
            }
        }
        Ok(out)
    }
    pub fn sample_cubic(&self, x: f64, y: f64) -> [f32; 4] {
        if !x.is_finite() || !y.is_finite() {
            return [0.; 4];
        }
        let cubic = |distance: f64| {
            let t = distance.abs();
            if t <= 1. {
                ((1.5 * t - 2.5) * t) * t + 1.
            } else if t < 2. {
                ((-0.5 * t + 2.5) * t - 4.) * t + 2.
            } else {
                0.
            }
        };
        let (ix, iy) = (x.floor() as i64, y.floor() as i64);
        let mut out = [0f64; 4];
        for dy in -1..=2 {
            for dx in -1..=2 {
                let (xx, yy) = (ix + dx, iy + dy);
                if xx < 0 || yy < 0 || xx >= self.width as i64 || yy >= self.height as i64 {
                    continue;
                }
                let weight =
                    cubic(dx as f64 - (x - x.floor())) * cubic(dy as f64 - (y - y.floor()));
                let p = self.get(xx as u32, yy as u32);
                for c in 0..3 {
                    out[c] += p[c] as f64 * p[3] as f64 * weight;
                }
                out[3] += p[3] as f64 * weight;
            }
        }
        let alpha = out[3].clamp(0., 1.);
        let mut result = [0.; 4];
        if out[3] > 0. {
            for c in 0..3 {
                result[c] = (out[c] / out[3]) as f32;
            }
        }
        result[3] = alpha as f32;
        result
    }
    /// Run a deterministic byte operation with the original 4/10-run precision
    /// reconstruction. The callback consumes and returns packed premultiplied
    /// RGBA8 of identical dimensions; it must use the same settings each run.
    pub fn run_8bit(
        &self,
        mixes_channels: bool,
        mut operation: impl FnMut(Vec<u8>) -> Result<Vec<u8>, String>,
    ) -> Result<Self, String> {
        let source = self.to_bits();
        let mut normal = vec![0u16; source.len()];
        let mut scales = Vec::new();
        if self.depth == Depth::Hdr {
            scales.reserve(source.len() / 4);
        }
        for (input, output) in source
            .as_chunks::<4>()
            .0
            .iter()
            .zip(normal.as_chunks_mut::<4>().0)
        {
            let p = input.map(|v| decode(v, self.depth));
            let scale = if self.depth == Depth::Hdr {
                p[0].max(p[1]).max(p[2]).max(1.)
            } else {
                1.
            };
            if self.depth == Depth::Hdr {
                scales.push(scale);
            }
            for c in 0..3 {
                output[c] = (p[c] / scale)
                    .clamp(0., 1.)
                    .mul_add(p[3], 0.)
                    .mul_add(65535., 0.)
                    .round() as u16;
            }
            output[3] = (p[3] * 65535.).round() as u16;
        }
        let mut base = split_premultiplied(&normal, self.width, self.height, 0, None)?;
        let unprocessed = base.clone();
        base = operation(base)?;
        let mut unchanged = base == unprocessed;
        let count = normal.len();
        if base.len() != count {
            return Err("高位深滤镜改变了像素尺寸".into());
        }
        let per = if mixes_channels { 3 } else { 1 };
        let mut results: Vec<Vec<u8>> = Vec::with_capacity(per * 3);
        for step in [1, 2, 4] {
            for channel in 0..per {
                let input = split_premultiplied(
                    &normal,
                    self.width,
                    self.height,
                    step,
                    if mixes_channels { Some(channel) } else { None },
                )?;
                let input_compare = input.clone();
                let result = operation(input)?;
                if result.len() != count {
                    return Err("高位深滤镜改变了像素尺寸".into());
                }
                unchanged &= result == input_compare;
                results.push(result);
            }
        }
        if unchanged {
            return Ok(self.clone());
        }
        let runs = |offset: usize| -> [&[u8]; 3] {
            std::array::from_fn(|c| {
                results[offset * per + if mixes_channels { c } else { 0 }].as_slice()
            })
        };
        let merged = merge_premultiplied(
            &normal,
            &base,
            runs(0),
            runs(1),
            runs(2),
            self.width,
            self.height,
            mixes_channels,
        )?;
        let mut bits = Vec::with_capacity(source.len());
        for (i, p) in merged.as_chunks::<4>().0.iter().enumerate() {
            let alpha = p[3] as f32 / 65535.;
            // Carry full source coverage when the byte operation left coverage
            // unchanged, instead of quantizing it merely by adapting the kernel.
            let original_alpha = decode(source[i * 4 + 3], self.depth);
            let output_alpha = if base[i * 4 + 3] == unprocessed[i * 4 + 3] {
                original_alpha
            } else {
                alpha
            };
            let scale = if self.depth == Depth::Hdr {
                scales[i]
            } else {
                1.
            };
            for &channel in &p[..3] {
                let straight = if alpha > 0. {
                    channel as f32 / 65535. / alpha * scale
                } else {
                    0.
                };
                bits.push(encode(straight, self.depth, false));
            }
            bits.push(encode(output_alpha, self.depth, true));
        }
        Ok(Self {
            width: self.width,
            height: self.height,
            depth: self.depth,
            base: Arc::new(bits),
            tiles: BTreeMap::new(),
        })
    }
    /// A measured point-color cube with red varying fastest. HDR highlights are
    /// normalized per pixel and restored after the lookup, matching the upstream
    /// rule for byte-limited creative color operations.
    pub fn apply_cube(&self, cube: &[[f32; 3]], dimension: usize) -> Result<Self, String> {
        if !(2..=256).contains(&dimension)
            || cube.len() != dimension * dimension * dimension
            || cube.iter().flatten().any(|v| !v.is_finite())
        {
            return Err("颜色查找表尺寸或数值无效".into());
        }
        Ok(self.map_colors(|color| {
            let scale = if self.depth == Depth::Hdr {
                color[0].max(color[1]).max(color[2]).max(1.)
            } else {
                1.
            };
            let position = color.map(|v| (v / scale).clamp(0., 1.) * (dimension - 1) as f32);
            let low = position.map(|v| (v as usize).min(dimension - 2));
            let t = std::array::from_fn::<_, 3, _>(|c| position[c] - low[c] as f32);
            let mut out = [0.; 3];
            for corner in 0..8 {
                let d = [corner & 1, (corner >> 1) & 1, corner >> 2];
                let weight = (if d[0] == 0 { 1. - t[0] } else { t[0] })
                    * (if d[1] == 0 { 1. - t[1] } else { t[1] })
                    * (if d[2] == 0 { 1. - t[2] } else { t[2] });
                let at = (low[0] + d[0])
                    + (low[1] + d[1]) * dimension
                    + (low[2] + d[2]) * dimension * dimension;
                for c in 0..3 {
                    out[c] += cube[at][c] * weight;
                }
            }
            out.map(|v| v.clamp(0., 1.) * scale)
        }))
    }
}

fn box_line(input: &[f32], output: &mut [f32], length: usize, stride: usize, radius: usize) {
    if length == 0 {
        return;
    }
    let stride = stride * 4;
    let window = (radius * 2 + 1) as f64;
    let mut sum = [0f64; 4];
    for (c, s) in sum.iter_mut().enumerate() {
        *s = input[c] as f64 * (radius + 1) as f64;
        for k in 1..=radius {
            *s += input[k.min(length - 1) * stride + c] as f64;
        }
    }
    for at in 0..length {
        for c in 0..4 {
            output[at * stride + c] = (sum[c] / window) as f32;
            sum[c] += input[(at + radius + 1).min(length - 1) * stride + c] as f64
                - input[at.saturating_sub(radius) * stride + c] as f64;
        }
    }
}

fn low(value: u16, alpha: i32, x: u32, y: u32, channel: usize) -> i32 {
    ((value as f32 * (255f32 / 65535.) + noise(x, y, channel as u32 + 1)).floor() as i32 - 1)
        .clamp(0, alpha)
}
fn change(l: f32, s1: f32, s2: f32, s4: f32, past: f32, room: i32) -> (f32, Option<f32>) {
    if room <= 0 {
        return (0., None);
    }
    let stepped = if room == 1 || past < 1. {
        (s1 - l) * past.min(1.)
    } else {
        (s1 - l) + (s2 - s1) * (past - 1.)
    };
    if room < 4 {
        return (stepped, None);
    }
    let mean = (l + s1 + s2 + s4) / 4.;
    let slope = (-1.75 * l - 0.75 * s1 + 0.25 * s2 + 2.25 * s4) / 8.75;
    let start = mean - slope * 1.75;
    let off = (l - start)
        .abs()
        .max((s1 - start - slope).abs())
        .max((s2 - start - 2. * slope).abs())
        .max((s4 - start - 4. * slope).abs());
    if off > 1. {
        (stepped, None)
    } else {
        (slope * past, Some(start - l))
    }
}
/// Low-level reference kernel. Unlike DeepRaster, these buffers are premultiplied
/// native-endian RGBA16. Exposed for reproducible C parity and byte-kernel reuse.
pub fn split_premultiplied(
    deep: &[u16],
    width: u32,
    height: u32,
    step: i32,
    channel: Option<usize>,
) -> Result<Vec<u8>, String> {
    if deep.len() != width as usize * height as usize * 4
        || !(0..=4).contains(&step)
        || channel.is_some_and(|v| v > 2)
    {
        return Err("高位深分离参数无效".into());
    }
    let mut out = vec![0; deep.len()];
    for (i, (p, o)) in deep
        .as_chunks::<4>()
        .0
        .iter()
        .zip(out.as_chunks_mut::<4>().0)
        .enumerate()
    {
        let alpha = (p[3] as f32 * (255f32 / 65535.)).round() as i32;
        for c in 0..3 {
            o[c] = (low(p[c], alpha, i as u32 % width, i as u32 / width, c)
                + if channel.is_none_or(|v| v == c) {
                    step
                } else {
                    0
                })
            .min(alpha) as u8;
        }
        o[3] = alpha as u8;
    }
    Ok(out)
}
#[allow(clippy::too_many_arguments)]
pub fn merge_premultiplied(
    deep: &[u16],
    base: &[u8],
    near: [&[u8]; 3],
    mid: [&[u8]; 3],
    far: [&[u8]; 3],
    width: u32,
    height: u32,
    mixes: bool,
) -> Result<Vec<u16>, String> {
    let length = width as usize * height as usize * 4;
    if deep.len() != length
        || base.len() != length
        || near
            .iter()
            .chain(mid.iter())
            .chain(far.iter())
            .any(|v| v.len() != length)
    {
        return Err("高位深合并参数无效".into());
    }
    let mut out = vec![0; length];
    for (i, (p, o)) in deep
        .as_chunks::<4>()
        .0
        .iter()
        .zip(out.as_chunks_mut::<4>().0)
        .enumerate()
    {
        let alpha = (p[3] as f32 * (255f32 / 65535.)).round() as i32;
        let out_alpha = base[i * 4 + 3] as f32 * 257.;
        let mut past = [0.; 3];
        let mut room = [0; 3];
        for c in 0..3 {
            let lo = low(p[c], alpha, i as u32 % width, i as u32 / width, c);
            past[c] = (p[c] as f32 * (255f32 / 65535.) - lo as f32).clamp(0., 2.);
            room[c] = alpha - lo;
        }
        for (c, output) in o[..3].iter_mut().enumerate() {
            let at = i * 4 + c;
            let l = base[at] as f32;
            let (mut value, mut offset, mut fits) = (l, 0., 0);
            for k in if mixes { 0..3 } else { c..c + 1 } {
                let r = if mixes { k } else { 0 };
                let (delta, fit) = change(
                    l,
                    near[r][at] as f32,
                    mid[r][at] as f32,
                    far[r][at] as f32,
                    past[k],
                    room[k],
                );
                value += delta;
                if let Some(fit) = fit {
                    offset += fit;
                    fits += 1;
                }
            }
            if fits > 0 {
                value += offset / fits as f32;
            }
            *output = (value * 257.).clamp(0., out_alpha).round() as u16;
        }
        o[3] = out_alpha.round() as u16;
    }
    Ok(out)
}

impl PartialEq for DeepRaster {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.depth == other.depth
            && (self.shares(other)
                || (0..self.height)
                    .all(|y| (0..self.width).all(|x| self.get_bits(x, y) == other.get_bits(x, y))))
    }
}
impl Eq for DeepRaster {}

fn encode(value: f32, depth: Depth, alpha: bool) -> u16 {
    let value = if value.is_nan() { 0. } else { value };
    if alpha || depth == Depth::U16 {
        let value = value.clamp(0., 1.);
        if depth == Depth::U16 {
            (value * 65535.).round() as u16
        } else {
            float_to_half(value)
        }
    } else {
        float_to_half(value.clamp(-HALF_MAX, HALF_MAX))
    }
}
fn decode(value: u16, depth: Depth) -> f32 {
    if depth == Depth::U16 {
        value as f32 / 65535.
    } else {
        half_to_float(value)
    }
}
pub fn over(base: [f32; 4], paint: [f32; 4], amount: f32) -> [f32; 4] {
    let alpha = paint[3] * amount.clamp(0., 1.);
    let out_alpha = alpha + base[3] * (1. - alpha);
    std::array::from_fn(|c| {
        if c == 3 {
            out_alpha
        } else if out_alpha > 0. {
            (paint[c] * alpha + base[c] * base[3] * (1. - alpha)) / out_alpha
        } else {
            0.
        }
    })
}

pub fn linear(encoded: f32) -> f32 {
    let magnitude = encoded.abs();
    let result = if magnitude <= 0.04045 {
        magnitude / 12.92
    } else {
        ((magnitude + 0.055) / 1.055).powf(2.4)
    };
    result.copysign(encoded)
}
pub fn encoded(linear: f32) -> f32 {
    let magnitude = linear.abs();
    let result = if magnitude <= 0.0031308 {
        magnitude * 12.92
    } else {
        1.055 * magnitude.powf(1. / 2.4) - 0.055
    };
    result.copysign(linear)
}
pub fn half_to_float(bits: u16) -> f32 {
    let sign = ((bits as u32) >> 15) << 31;
    let exponent = (bits >> 10) & 31;
    let mantissa = bits & 1023;
    if exponent == 0 {
        let value = mantissa as f32 / 16777216.;
        if sign != 0 { -value } else { value }
    } else {
        f32::from_bits(
            sign | if exponent == 31 {
                0x7f800000 | ((mantissa as u32) << 13)
            } else {
                ((exponent as u32 + 112) << 23) | ((mantissa as u32) << 13)
            },
        )
    }
}
pub fn float_to_half(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = (bits >> 16) & 0x8000;
    let exponent = (bits >> 23) & 255;
    let mut mantissa = bits & 0x7fffff;
    if exponent == 255 {
        return (sign | 0x7c00 | if mantissa != 0 { 0x200 } else { 0 }) as u16;
    }
    let e = exponent as i32 - 127 + 15;
    if e >= 31 {
        return (sign | 0x7c00) as u16;
    }
    if e <= 0 {
        if e < -10 {
            return sign as u16;
        }
        mantissa |= 0x800000;
        let shift = (14 - e) as u32;
        let mut half = mantissa >> shift;
        let rest = mantissa & ((1 << shift) - 1);
        let halfway = 1 << (shift - 1);
        if rest > halfway || (rest == halfway && half & 1 != 0) {
            half += 1;
        }
        (sign | half) as u16
    } else {
        let mut half = sign | ((e as u32) << 10) | (mantissa >> 13);
        let rest = mantissa & 0x1fff;
        if rest > 0x1000 || (rest == 0x1000 && half & 1 != 0) {
            half += 1;
        }
        half as u16
    }
}
fn noise(x: u32, y: u32, draw: u32) -> f32 {
    let mut h =
        x.wrapping_mul(0x9e3779b1) ^ y.wrapping_mul(0x85ebca77) ^ draw.wrapping_mul(0xc2b2ae3d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a2d39);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_merge_matches_original_c_with_alpha_and_mixed_channels() {
        let mut lines = include_str!("../tests/reference/deep_pixels_1_5_3.txt")
            .lines()
            .filter(|line| !line.starts_with('#'));
        let dimensions: Vec<usize> = lines
            .next()
            .unwrap()
            .split_whitespace()
            .skip(1)
            .map(|v| v.parse().unwrap())
            .collect();
        let (width, height) = (dimensions[0] as u32, dimensions[1] as u32);
        let read = |line: &str| -> Vec<u16> {
            line.split_whitespace()
                .map(|v| v.parse().unwrap())
                .collect()
        };
        let source = read(lines.next().unwrap());
        let operation = |mut pixels: Vec<u8>, mode: u32| {
            for p in pixels.as_chunks_mut::<4>().0 {
                let [r, g, b, a] = p.map(u32::from);
                let rgb = match mode {
                    0 => [a - r, a - g, a - b],
                    1 => [r, g, b].map(|v| (v as i32 * 3 - a as i32 / 2).clamp(0, a as i32) as u32),
                    _ => [
                        (r + 2 * g + b + 2) / 4,
                        (3 * r + g + 2) / 4,
                        (g + 3 * b + 2) / 4,
                    ],
                };
                for c in 0..3 {
                    p[c] = rgb[c] as u8;
                }
            }
            pixels
        };
        while let Some(header) = lines.next() {
            let expected = read(lines.next().unwrap());
            if header == "split0" {
                assert_eq!(
                    split_premultiplied(&source, width, height, 0, None)
                        .unwrap()
                        .into_iter()
                        .map(u16::from)
                        .collect::<Vec<_>>(),
                    expected
                );
                continue;
            }
            let mode: u32 = header.split_whitespace().nth(1).unwrap().parse().unwrap();
            let mixes = mode == 2;
            let per = if mixes { 3 } else { 1 };
            let base = operation(
                split_premultiplied(&source, width, height, 0, None).unwrap(),
                mode,
            );
            let mut results = Vec::new();
            for step in [1, 2, 4] {
                for c in 0..per {
                    results.push(operation(
                        split_premultiplied(
                            &source,
                            width,
                            height,
                            step,
                            if mixes { Some(c) } else { None },
                        )
                        .unwrap(),
                        mode,
                    ));
                }
            }
            let runs = |offset: usize| -> [&[u8]; 3] {
                std::array::from_fn(|c| {
                    results[offset * per + if mixes { c } else { 0 }].as_slice()
                })
            };
            let actual = merge_premultiplied(
                &source,
                &base,
                runs(0),
                runs(1),
                runs(2),
                width,
                height,
                mixes,
            )
            .unwrap();
            let different: Vec<_> = actual
                .iter()
                .zip(&expected)
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .take(10)
                .collect();
            assert!(different.is_empty(), "mode {mode}: {different:?}");
        }
    }
    #[test]
    fn byte_filter_adapter_preserves_sixteen_bit_gradient_and_hdr_scale() {
        let source = DeepRaster::from_u16(
            1024,
            1,
            (0..1024)
                .flat_map(|i| [20000 + i, 30000 + i, 40000 + i, 65535])
                .collect(),
        )
        .unwrap();
        let inverted = source
            .run_8bit(false, |mut p| {
                for p in p.as_chunks_mut::<4>().0 {
                    let alpha = p[3];
                    for value in &mut p[..3] {
                        *value = alpha - *value;
                    }
                }
                Ok(p)
            })
            .unwrap();
        for x in 0..1024 {
            for c in 0..3 {
                assert!(inverted.get_bits(x, 0)[c].abs_diff(65535 - source.get_bits(x, 0)[c]) <= 1);
            }
        }
        let hdr = DeepRaster::from_f32(1, 1, Depth::Hdr, &[8., 4., 2., 1.]).unwrap();
        let half = hdr
            .run_8bit(false, |mut p| {
                for p in p.as_chunks_mut::<4>().0 {
                    for value in &mut p[..3] {
                        *value /= 2;
                    }
                }
                Ok(p)
            })
            .unwrap();
        assert!((half.get(0, 0)[0] - 4.).abs() < 0.04);
        assert!(half.peak() > 1.);
    }
    #[test]
    fn native_blur_keeps_smooth_ramp_fine_bits_and_hdr_energy() {
        let source = DeepRaster::from_u16(
            256,
            9,
            (0..256 * 9)
                .flat_map(|i| {
                    let v = 25000 + (i % 256) * 4;
                    [v, v, v, 65535]
                })
                .collect(),
        )
        .unwrap();
        let blurred = source.gaussian_blur(2.);
        for x in 16..240 {
            assert!(blurred.get_bits(x, 4)[0].abs_diff(source.get_bits(x, 4)[0]) <= 1);
        }
        let hdr = DeepRaster::solid(15, 11, Depth::Hdr, [4., 2., 1.5, 0.5]);
        let blur = hdr.gaussian_blur(3.);
        assert_eq!(blur.get(7, 5), hdr.get(7, 5));
    }
    #[test]
    fn all_finite_half_patterns_round_trip() {
        for bits in 0..=u16::MAX {
            if bits & 0x7c00 != 0x7c00 {
                assert_eq!(float_to_half(half_to_float(bits)), bits);
            }
        }
        assert_eq!(float_to_half(1. + 1. / 2048.), 0x3c00);
        assert_eq!(float_to_half(1. + 3. / 2048.), 0x3c02);
    }
    #[test]
    fn reduced_previews_average_thin_lines_without_aliasing_and_keep_hdr_and_alpha() {
        let source = DeepRaster::from_u16(
            8,
            1,
            (0..8)
                .flat_map(|i| {
                    if i % 4 == 0 {
                        [65535; 4]
                    } else {
                        [0, 0, 0, 65535]
                    }
                })
                .collect(),
        )
        .unwrap();
        let small = source.resized(2, 1);
        assert_eq!(small.get_bits(0, 0), [16384, 16384, 16384, 65535]);
        assert_eq!(small.get_bits(1, 0), [16384, 16384, 16384, 65535]);
        let hdr = DeepRaster::from_f32(
            4,
            1,
            Depth::Hdr,
            &[
                8., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1.,
            ],
        )
        .unwrap()
        .resized(1, 1);
        assert_eq!(hdr.get(0, 0), [2., 0., 0., 1.]);
        let transparent = DeepRaster::from_f32(2, 1, Depth::Hdr, &[1., 0., 1., 0., 1., 0., 0., 1.])
            .unwrap()
            .resized(1, 1);
        assert_eq!(transparent.get(0, 0), [1., 0., 0., 0.5]);
    }
    #[test]
    fn edits_copy_one_tile_and_preserve_low_bits_in_history() {
        let pixels: Vec<u16> = (0..257 * 129 * 4).map(|i| (i % 65536) as u16).collect();
        let source = DeepRaster::from_u16(257, 129, pixels.clone()).unwrap();
        let mut edited = source.clone();
        edited.set(130, 7, [0.2, 0.3, 0.4, 1.]);
        assert!(Arc::ptr_eq(&source.base, &edited.base));
        assert_eq!(edited.tiles.len(), 1);
        assert!(source.tiles.is_empty());
        assert_eq!(source.to_bits(), pixels);
        assert_eq!(edited.get_bits(129, 7), source.get_bits(129, 7));
        assert_eq!(edited.materialized(), edited);
    }
    #[test]
    fn u16_gradient_survives_edit_resize_and_conversion() {
        let pixels: Vec<_> = (0..4096)
            .flat_map(|i| [16384 + i, 32768 + i, 49152 + i, 65535])
            .collect();
        let source = DeepRaster::from_u16(4096, 1, pixels.clone()).unwrap();
        assert_eq!(source.resized(4096, 1).to_bits(), pixels);
        let inverted = source.map_colors(|v| v.map(|v| 1. - v));
        assert_eq!(inverted.get_bits(17, 0)[0], 65535 - 16401);
        assert_eq!(source.flipped(true, false).flipped(true, false), source);
        assert_eq!(source.rotated_quarter(true).rotated_quarter(false), source);
    }
    #[test]
    fn painted_preview_leaves_other_pixels_and_hdr_highlights_untouched() {
        let mut source = DeepRaster::from_f32(
            3,
            1,
            Depth::Hdr,
            &[4., 2., 1., 1., 0.1234, 0.2513, 0.75, 1., 2., 3., 4., 0.5],
        )
        .unwrap();
        let before = source.clone();
        let base = source.preview_rgba8();
        let mut painted = base.clone();
        painted[4] = 71;
        source.take_painted(&painted, &base).unwrap();
        assert_eq!(source.get_bits(0, 0), before.get_bits(0, 0));
        assert_eq!(source.get_bits(2, 0), before.get_bits(2, 0));
        assert_eq!(
            source.get(1, 0)[0],
            half_to_float(float_to_half(71. / 255.))
        );
    }
    #[test]
    fn exposure_recovers_hdr_highlights_and_resampling_is_alpha_correct() {
        let source =
            DeepRaster::from_f32(2, 1, Depth::Hdr, &[4., 2., 1., 1., 99., 12., 7., 0.]).unwrap();
        let dark = source.exposure(-4., 0., 1.);
        assert!(dark.get(0, 0)[0] < 1.5 && dark.get(0, 0)[0] > 1.);
        let roundtrip = dark.exposure(4., 0., 1.);
        assert!((roundtrip.get(0, 0)[0] - 4.).abs() < 0.01);
        let midpoint = source.sample(0.5, 0.);
        assert_eq!(midpoint, [4., 2., 1., 0.5]);
        assert!(source.peak() >= 4.);
    }
    #[test]
    fn rejects_invalid_samples_and_hdr_alpha() {
        assert!(DeepRaster::from_u16(2, 1, vec![0; 4]).is_err());
        assert!(DeepRaster::from_f32(1, 1, Depth::Hdr, &[f32::INFINITY, 0., 0., 1.]).is_err());
        assert!(DeepRaster::from_bits(1, 1, Depth::Hdr, vec![0, 0, 0, float_to_half(2.)]).is_err());
    }
}
