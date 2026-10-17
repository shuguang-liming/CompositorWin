use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, sync::Arc};
use uuid::Uuid;

pub const TILE_SIZE: u32 = 128;
pub const MAX_SIDE: u32 = 30_000;
pub const MAX_PIXELS: u64 = 200_000_000;

/// Pixel budget for a fresh codec decode. Decoders temporarily hold the
/// compressed source, a native image buffer, and the RGBA document at once;
/// reserving roughly 1/32 of physical RAM for pixels leaves room for those
/// transient copies and the rest of the editor on small Windows machines.
/// High-memory systems retain the normal 200 MP application ceiling.
pub fn import_pixel_budget() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 && status.ullTotalPhys > 0 {
            return (status.ullTotalPhys / 32).clamp(32 * 1024 * 1024, MAX_PIXELS);
        }
    }
    MAX_PIXELS
}

pub fn document_pixel_budget() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // Windows fills exactly the sized MEMORYSTATUSEX value and retains no pointer.
        if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 && status.ullTotalPhys > 0 {
            return document_budget_for_memory(Some(status.ullTotalPhys));
        }
    }
    document_budget_for_memory(None)
}

fn document_budget_for_memory(physical_bytes: Option<u64>) -> u64 {
    // Images and masks each receive this aggregate pixel budget. Decoded
    // surfaces, edited tiles, history and GPU uploads can coexist, so keep
    // their base RGBA storage within roughly one quarter of installed RAM.
    // Previously a 4 GiB host allowed 268 MP for images plus another 268 MP for
    // masks (2 GiB of base storage before scratch allocations). Keep the 800
    // MP ceiling for large hosts while rejecting oversized projects normally
    // on small Win10 machines, before their decoders exhaust system commit.
    physical_bytes
        .map(|bytes| bytes / 32)
        .unwrap_or(MAX_PIXELS)
        .clamp(32 * 1024 * 1024, 800_000_000)
}

/// Straight-alpha sRGB RGBA8, top-to-bottom. Base data is immutable; edits copy only touched tiles.
#[derive(Clone, Debug)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<Vec<u8>>,
    pub tiles: BTreeMap<(u32, u32), Arc<Vec<u8>>>,
    /// Authoritative high-precision pixels. Byte buffers remain derived views
    /// for tools that work at screen precision; untouched low bits stay here.
    pub deep: Option<Arc<crate::deep_pixels::DeepRaster>>,
}
impl Raster {
    pub fn blank(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: Arc::new(Vec::new()),
            tiles: BTreeMap::new(),
            deep: None,
        }
    }
    pub fn from_rgba(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        assert_eq!(pixels.len(), width as usize * height as usize * 4);
        Self {
            width,
            height,
            pixels: Arc::new(pixels),
            tiles: BTreeMap::new(),
            deep: None,
        }
    }
    pub fn solid(width: u32, height: u32, color: [u8; 4]) -> Self {
        Self {
            width,
            height,
            pixels: Arc::new(color.to_vec()),
            tiles: BTreeMap::new(),
            deep: None,
        }
    }
    pub fn from_deep(deep: crate::deep_pixels::DeepRaster) -> Self {
        Self {
            width: deep.width,
            height: deep.height,
            pixels: Arc::new(deep.preview_rgba8()),
            tiles: BTreeMap::new(),
            deep: Some(Arc::new(deep)),
        }
    }
    pub fn get_f32(&self, x: u32, y: u32) -> [f32; 4] {
        self.deep.as_ref().map_or_else(
            || self.get(x, y).map(|v| v as f32 / 255.),
            |deep| deep.get(x, y),
        )
    }
    pub fn set_f32(&mut self, x: u32, y: u32, color: [f32; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        if let Some(mut deep) = self.deep.take() {
            Arc::make_mut(&mut deep).set(x, y, color);
            self.set(x, y, deep.preview_pixel(x, y));
            self.deep = Some(deep);
        } else {
            self.set(x, y, color.map(|v| (v.clamp(0., 1.) * 255.).round() as u8));
        }
    }
    pub fn sample_f32(&self, x: f64, y: f64, sampling: Sampling) -> [f32; 4] {
        if let Some(deep) = &self.deep {
            match sampling {
                Sampling::Nearest => deep.sample_nearest(x, y),
                Sampling::High => deep.sample_cubic(x, y),
                _ => deep.sample(x, y),
            }
        } else if sampling == Sampling::High {
            let pixel = crate::render::sample_premultiplied(self, x, y, sampling);
            if pixel[3] > 0. {
                [
                    (pixel[0] / pixel[3]) as f32,
                    (pixel[1] / pixel[3]) as f32,
                    (pixel[2] / pixel[3]) as f32,
                    pixel[3] as f32,
                ]
            } else {
                [0.; 4]
            }
        } else {
            let pixel = if sampling == Sampling::Nearest {
                if x < -0.5 || y < -0.5 {
                    [0; 4]
                } else {
                    self.get(x.round() as u32, y.round() as u32)
                }
            } else {
                crate::brush::sample(self, x, y)
            };
            pixel.map(|v| v as f32 / 255.)
        }
    }
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        let (data, index) = if let Some(tile) = self.tiles.get(&(x / TILE_SIZE, y / TILE_SIZE)) {
            (
                tile.as_ref(),
                (((y % TILE_SIZE) * TILE_SIZE + x % TILE_SIZE) * 4) as usize,
            )
        } else {
            (
                self.pixels.as_ref(),
                if self.pixels.len() == 4 {
                    0
                } else {
                    ((y * self.width + x) * 4) as usize
                },
            )
        };
        if data.is_empty() {
            [0; 4]
        } else {
            data[index..index + 4].try_into().unwrap()
        }
    }
    pub fn set(&mut self, x: u32, y: u32, color: [u8; 4]) {
        if x >= self.width || y >= self.height || self.get(x, y) == color {
            return;
        }
        if let Some(deep) = self.deep.as_mut() {
            Arc::make_mut(deep).set(x, y, color.map(|v| v as f32 / 255.));
        }
        let key = (x / TILE_SIZE, y / TILE_SIZE);
        let tile = self
            .tiles
            .entry(key)
            .or_insert_with(|| Arc::new(base_tile(self.width, self.height, &self.pixels, key)));
        let tile = Arc::make_mut(tile);
        let i = (((y % TILE_SIZE) * TILE_SIZE + x % TILE_SIZE) * 4) as usize;
        tile[i..i + 4].copy_from_slice(&color);
    }
    /// Update ordered pixels in one tile row, copying shared storage only if a byte changes.
    pub(crate) fn set_tile_row(&mut self, y: u32, pixels: &[(u32, [u8; 4])]) {
        let Some(&(x, _)) = pixels.first() else {
            return;
        };
        let key = (x / TILE_SIZE, y / TILE_SIZE);
        debug_assert!(y < self.height);
        debug_assert!(
            pixels
                .iter()
                .all(|(x, _)| *x < self.width && *x / TILE_SIZE == key.0)
        );
        let deep_changes = self.deep.as_ref().map(|_| {
            pixels
                .iter()
                .filter(|&&(x, color)| self.get(x, y) != color)
                .copied()
                .collect::<Vec<_>>()
        });
        let row = ((y % TILE_SIZE) * TILE_SIZE * 4) as usize;
        let tile = match self.tiles.entry(key) {
            std::collections::btree_map::Entry::Occupied(entry) => {
                if pixels.iter().all(|(x, color)| {
                    let i = row + (x % TILE_SIZE) as usize * 4;
                    entry.get()[i..i + 4] == *color
                }) {
                    return;
                }
                entry.into_mut()
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                if pixels.iter().all(|(x, color)| match self.pixels.len() {
                    0 => *color == [0; 4],
                    4 => self.pixels[..] == *color,
                    _ => {
                        let i = ((y * self.width + x) * 4) as usize;
                        self.pixels[i..i + 4] == *color
                    }
                }) {
                    return;
                }
                entry.insert(Arc::new(base_tile(
                    self.width,
                    self.height,
                    &self.pixels,
                    key,
                )))
            }
        };
        let tile = Arc::make_mut(tile);
        for &(x, color) in pixels {
            let i = row + (x % TILE_SIZE) as usize * 4;
            tile[i..i + 4].copy_from_slice(&color);
        }
        if let Some(changes) = deep_changes
            && let Some(deep) = self.deep.as_mut()
        {
            let deep = Arc::make_mut(deep);
            for (x, color) in changes {
                deep.set(x, y, color.map(|v| v as f32 / 255.));
            }
        }
    }
    pub fn to_rgba(&self) -> Vec<u8> {
        if self.tiles.is_empty() && self.pixels.len() == 4 {
            return self
                .pixels
                .repeat(self.width as usize * self.height as usize);
        }
        if self.tiles.is_empty() && !self.pixels.is_empty() {
            return self.pixels.as_ref().clone();
        }
        let mut data = if self.pixels.len() == 4 {
            self.pixels
                .repeat(self.width as usize * self.height as usize)
        } else if self.pixels.is_empty() {
            vec![0; self.width as usize * self.height as usize * 4]
        } else {
            self.pixels.as_ref().clone()
        };
        for (&(tx, ty), tile) in &self.tiles {
            let x = tx.saturating_mul(TILE_SIZE);
            let y = ty.saturating_mul(TILE_SIZE);
            let count = TILE_SIZE.min(self.width.saturating_sub(x)) as usize * 4;
            if count == 0 {
                continue;
            }
            for row in 0..TILE_SIZE.min(self.height.saturating_sub(y)) {
                let source = (row * TILE_SIZE * 4) as usize;
                let target = (((y + row) * self.width + x) * 4) as usize;
                data[target..target + count].copy_from_slice(&tile[source..source + count]);
            }
        }
        data
    }
    pub fn materialized(&self) -> Self {
        let mut out = Self::from_rgba(self.width, self.height, self.to_rgba());
        out.deep = self.deep.clone();
        out
    }
    pub fn byte_len(&self) -> usize {
        self.pixels.len()
            + self.tiles.values().map(|x| x.len()).sum::<usize>()
            + self.deep.as_ref().map_or(0, |deep| deep.byte_len())
    }
}

fn base_tile(width: u32, height: u32, pixels: &[u8], key: (u32, u32)) -> Vec<u8> {
    let mut tile = vec![0; (TILE_SIZE * TILE_SIZE * 4) as usize];
    if pixels.is_empty() {
        return tile;
    }
    let x = key.0 * TILE_SIZE;
    let y = key.1 * TILE_SIZE;
    let columns = TILE_SIZE.min(width.saturating_sub(x)) as usize * 4;
    let rows = TILE_SIZE.min(height.saturating_sub(y)) as usize;
    for row in 0..rows {
        let target = row * TILE_SIZE as usize * 4;
        let output = &mut tile[target..target + columns];
        if pixels.len() == 4 {
            for pixel in output.as_chunks_mut::<4>().0 {
                pixel.copy_from_slice(pixels);
            }
        } else {
            let source = ((y as usize + row) * width as usize + x as usize) * 4;
            output.copy_from_slice(&pixels[source..source + columns]);
        }
    }
    tile
}

impl Eq for Raster {}
impl PartialEq for Raster {
    fn eq(&self, other: &Self) -> bool {
        if self.width != other.width || self.height != other.height {
            return false;
        }
        if self.deep != other.deep {
            return false;
        }
        if self.pixels == other.pixels && self.tiles == other.tiles {
            return true;
        }
        if Arc::ptr_eq(&self.pixels, &other.pixels) {
            let keys: std::collections::BTreeSet<_> = self
                .tiles
                .keys()
                .chain(other.tiles.keys())
                .copied()
                .collect();
            for (tx, ty) in keys {
                if self.tiles.get(&(tx, ty)) == other.tiles.get(&(tx, ty)) {
                    continue;
                }
                for y in ty * TILE_SIZE..((ty + 1) * TILE_SIZE).min(self.height) {
                    for x in tx * TILE_SIZE..((tx + 1) * TILE_SIZE).min(self.width) {
                        if self.get(x, y) != other.get(x, y) {
                            return false;
                        }
                    }
                }
            }
            return true;
        }
        for y in 0..self.height {
            for x in 0..self.width {
                if self.get(x, y) != other.get(x, y) {
                    return false;
                }
            }
        }
        true
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Transform {
    pub origin: [f64; 2],
    pub size: [f64; 2],
    #[serde(default)]
    pub rotation: f64,
    #[serde(default)]
    pub flip_x: bool,
    #[serde(default)]
    pub flip_y: bool,
    #[serde(default)]
    pub sampling: Sampling,
}
impl Transform {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self {
            origin: [x, y],
            size: [w, h],
            rotation: 0.,
            flip_x: false,
            flip_y: false,
            sampling: Sampling::High,
        }
    }
    pub fn to_document(&self, p: [f64; 2]) -> [f64; 2] {
        let mut x = p[0] - 0.5;
        let mut y = p[1] - 0.5;
        if self.flip_x {
            x = -x;
        }
        if self.flip_y {
            y = -y;
        }
        x *= self.size[0];
        y *= self.size[1];
        let (s, c) = self.rotation.to_radians().sin_cos();
        [
            self.origin[0] + self.size[0] / 2. + c * x - s * y,
            self.origin[1] + self.size[1] / 2. + s * x + c * y,
        ]
    }
    pub fn from_document(&self, p: [f64; 2]) -> [f64; 2] {
        let x = p[0] - self.origin[0] - self.size[0] / 2.;
        let y = p[1] - self.origin[1] - self.size[1] / 2.;
        let (s, c) = self.rotation.to_radians().sin_cos();
        let mut u = (c * x + s * y) / self.size[0];
        let mut v = (-s * x + c * y) / self.size[1];
        if self.flip_x {
            u = -u;
        }
        if self.flip_y {
            v = -v;
        }
        [u + 0.5, v + 0.5]
    }
    pub fn corners(&self) -> [[f64; 2]; 4] {
        [[0., 0.], [1., 0.], [1., 1.], [0., 1.]].map(|p| self.to_document(p))
    }
    pub fn is_valid(&self) -> bool {
        self.origin
            .iter()
            .all(|x| x.is_finite() && x.abs() <= 1_000_000.)
            && self
                .size
                .iter()
                .all(|x| x.is_finite() && *x > 0. && *x <= 1_000_000.)
            && self.rotation.is_finite()
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Default)]
pub enum Sampling {
    #[serde(rename = "Nearest")]
    Nearest,
    #[serde(rename = "Smooth")]
    Smooth,
    #[default]
    #[serde(rename = "High quality")]
    High,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Darken,
    Multiply,
    #[serde(rename = "Color Burn")]
    ColorBurn,
    #[serde(rename = "Linear Burn")]
    LinearBurn,
    Lighten,
    Screen,
    #[serde(rename = "Color Dodge")]
    ColorDodge,
    #[serde(rename = "Linear Dodge (Add)")]
    LinearDodge,
    Overlay,
    #[serde(rename = "Soft Light")]
    SoftLight,
    #[serde(rename = "Hard Light")]
    HardLight,
    #[serde(rename = "Vivid Light")]
    VividLight,
    #[serde(rename = "Linear Light")]
    LinearLight,
    #[serde(rename = "Pin Light")]
    PinLight,
    #[serde(rename = "Hard Mix")]
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
}
impl BlendMode {
    pub const ALL: [Self; 24] = [
        Self::Normal,
        Self::Darken,
        Self::Multiply,
        Self::ColorBurn,
        Self::LinearBurn,
        Self::Lighten,
        Self::Screen,
        Self::ColorDodge,
        Self::LinearDodge,
        Self::Overlay,
        Self::SoftLight,
        Self::HardLight,
        Self::VividLight,
        Self::LinearLight,
        Self::PinLight,
        Self::HardMix,
        Self::Difference,
        Self::Exclusion,
        Self::Subtract,
        Self::Divide,
        Self::Hue,
        Self::Saturation,
        Self::Color,
        Self::Luminosity,
    ];
    pub fn label(self) -> String {
        serde_json::to_value(self)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub raster: Arc<Raster>,
    pub enabled: bool,
    pub linked: bool,
    pub placement: Option<Transform>,
}
impl Mask {
    pub fn white() -> Self {
        Self {
            raster: Arc::new(Raster::solid(1, 1, [255; 4])),
            enabled: true,
            linked: true,
            placement: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub id: Uuid,
    pub name: String,
    pub visible: bool,
    pub parent_id: Option<Uuid>,
    pub is_group: bool,
    pub opacity: f64,
    pub blend_mode: BlendMode,
    pub transform: Transform,
    pub image: Option<Arc<Raster>>,
    pub mask: Option<Mask>,
    pub mask_source_id: Option<Uuid>,
    pub adjustment: Option<Value>,
    pub effects: Option<Value>,
    pub text: Option<Value>,
    pub shape: Option<Value>,
    pub extra: Map<String, Value>,
}
impl Layer {
    pub fn new(name: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            visible: true,
            parent_id: None,
            is_group: false,
            opacity: 1.,
            blend_mode: BlendMode::Normal,
            transform: Transform::new(0., 0., width as f64, height as f64),
            image: None,
            mask: None,
            mask_source_id: None,
            adjustment: None,
            effects: None,
            text: None,
            shape: None,
            extra: Map::new(),
        }
    }
    pub fn raster(name: impl Into<String>, image: Raster) -> Self {
        let mut layer = Self::new(name, image.width, image.height);
        layer.image = Some(Arc::new(image));
        layer
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Guide {
    pub id: Uuid,
    pub axis: String,
    pub position: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectionRegion {
    pub origin: [i64; 2],
    pub size: [u32; 2],
    pub coverage: Arc<Vec<u8>>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    /// Canvas dimensions. The immutable coverage grids may extend beyond the canvas.
    pub width: u32,
    pub height: u32,
    pub coverage: Arc<Vec<u8>>,
    pub coverage_size: [u32; 2],
    pub origin: [i64; 2],
    /// Separate grids keep adding far-away outlines from allocating the empty gap.
    pub additional: Vec<SelectionRegion>,
    pub feather: f64,
    /// The outline before softening. Repeated feather edits add variance without rounding it again.
    pub feather_base: Option<Arc<Selection>>,
}
impl Selection {
    pub fn from_coverage(width: u32, height: u32, coverage: impl Into<Arc<Vec<u8>>>) -> Self {
        let coverage = coverage.into();
        assert_eq!(coverage.len(), width as usize * height as usize);
        Self {
            width,
            height,
            coverage,
            coverage_size: [width, height],
            origin: [0, 0],
            additional: Vec::new(),
            feather: 0.,
            feather_base: None,
        }
    }
    pub fn empty(width: u32, height: u32) -> Self {
        Self::from_coverage(width, height, vec![0; width as usize * height as usize])
    }
    pub fn full(width: u32, height: u32) -> Self {
        Self::from_coverage(width, height, vec![255; width as usize * height as usize])
    }
    pub fn regions(&self) -> impl Iterator<Item = ([i64; 2], [u32; 2], &Arc<Vec<u8>>)> {
        std::iter::once((self.origin, self.coverage_size, &self.coverage)).chain(
            self.additional
                .iter()
                .map(|r| (r.origin, r.size, &r.coverage)),
        )
    }
    pub fn from_regions(width: u32, height: u32, regions: Vec<SelectionRegion>) -> Self {
        let mut regions = regions.into_iter();
        let first = regions.next().unwrap_or(SelectionRegion {
            origin: [0, 0],
            size: [0, 0],
            coverage: Arc::new(Vec::new()),
        });
        Self {
            width,
            height,
            coverage: first.coverage,
            coverage_size: first.size,
            origin: first.origin,
            additional: regions.collect(),
            feather: 0.,
            feather_base: None,
        }
    }
    pub fn allocations(&self) -> Vec<&Arc<Vec<u8>>> {
        let mut allocations = self.regions().map(|(_, _, data)| data).collect::<Vec<_>>();
        if let Some(base) = &self.feather_base {
            allocations.extend(base.allocations());
        }
        allocations
    }
    pub fn same_coverage(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.regions().count() == other.regions().count()
            && self
                .regions()
                .zip(other.regions())
                .all(|(a, b)| a.0 == b.0 && a.1 == b.1 && Arc::ptr_eq(a.2, b.2))
    }
    /// Coverage in document coordinates, including a retained off-canvas outline.
    pub fn get_unclipped(&self, x: i64, y: i64) -> u8 {
        self.regions()
            .filter_map(|(origin, size, coverage)| {
                let sx = x.checked_sub(origin[0])?;
                let sy = y.checked_sub(origin[1])?;
                (sx >= 0 && sy >= 0 && sx < size[0] as i64 && sy < size[1] as i64)
                    .then(|| coverage[(sy as u64 * size[0] as u64 + sx as u64) as usize])
            })
            .max()
            .unwrap_or(0)
    }
    pub fn get(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            0
        } else {
            self.get_unclipped(x as i64, y as i64)
        }
    }
    pub fn canvas_coverage(&self) -> std::borrow::Cow<'_, [u8]> {
        if self.origin == [0, 0]
            && self.coverage_size == [self.width, self.height]
            && self.additional.is_empty()
        {
            return std::borrow::Cow::Borrowed(&self.coverage);
        }
        std::borrow::Cow::Owned(
            (0..self.height)
                .flat_map(|y| (0..self.width).map(move |x| self.get(x, y)))
                .collect(),
        )
    }
    /// Geometric emptiness differs from a nonempty outline temporarily outside the canvas.
    pub fn is_empty(&self) -> bool {
        if let Some(base) = &self.feather_base {
            return base.is_empty();
        }
        self.regions()
            .all(|(_, _, data)| data.iter().all(|v| *v == 0))
    }
    pub fn outline(&self) -> &Self {
        self.feather_base.as_deref().unwrap_or(self)
    }
    pub fn set(&mut self, x: u32, y: u32, v: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        self.feather = 0.;
        self.feather_base = None;
        let mut written = false;
        let set = |origin: [i64; 2], size: [u32; 2], data: &mut Arc<Vec<u8>>| {
            let sx = (x as i64).saturating_sub(origin[0]);
            let sy = (y as i64).saturating_sub(origin[1]);
            if sx >= 0 && sy >= 0 && sx < size[0] as i64 && sy < size[1] as i64 {
                Arc::make_mut(data)[(sy as u64 * size[0] as u64 + sx as u64) as usize] = v;
                true
            } else {
                false
            }
        };
        written |= set(self.origin, self.coverage_size, &mut self.coverage);
        for region in &mut self.additional {
            written |= set(region.origin, region.size, &mut region.coverage);
        }
        if !written && v != 0 {
            self.additional.push(SelectionRegion {
                origin: [x as i64, y as i64],
                size: [1, 1],
                coverage: Arc::new(vec![v]),
            });
        }
    }
    pub fn bounds(&self) -> Option<[u32; 4]> {
        let (mut x0, mut y0, mut x1, mut y1) = (self.width, self.height, 0, 0);
        for (origin, size, data) in self.regions() {
            let left = origin[0].max(0);
            let top = origin[1].max(0);
            let right = origin[0]
                .saturating_add(size[0] as i64)
                .min(self.width as i64);
            let bottom = origin[1]
                .saturating_add(size[1] as i64)
                .min(self.height as i64);
            if left >= right || top >= bottom {
                continue;
            }
            let sx = (left - origin[0]) as usize;
            let count = (right - left) as usize;
            for y in top..bottom {
                let start = (y - origin[1]) as usize * size[0] as usize + sx;
                let row = &data[start..start + count];
                if let Some(first) = row.iter().position(|&v| v != 0) {
                    let last = row.iter().rposition(|&v| v != 0).unwrap();
                    x0 = x0.min(left as u32 + first as u32);
                    x1 = x1.max(left as u32 + last as u32 + 1);
                    y0 = y0.min(y as u32);
                    y1 = y1.max(y as u32 + 1);
                }
            }
        }
        (x0 < x1 && y0 < y1).then_some([x0, y0, x1, y1])
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    pub id: Uuid,
    pub width: u32,
    pub height: u32,
    pub resolution: f64,
    pub active_layer_id: Option<Uuid>,
    pub layers: Vec<Layer>,
    pub guides: Vec<Guide>,
    pub selection: Option<Selection>,
    pub extra: Map<String, Value>,
}
impl Document {
    pub fn new(width: u32, height: u32) -> Self {
        let layer = Layer::new("图层 1", width, height);
        Self {
            id: Uuid::new_v4(),
            width,
            height,
            resolution: 72.,
            active_layer_id: Some(layer.id),
            layers: vec![layer],
            guides: vec![],
            selection: None,
            extra: Map::new(),
        }
    }
    pub fn layer(&self, id: Uuid) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }
    pub fn layer_mut(&mut self, id: Uuid) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }
    pub fn active(&self) -> Option<&Layer> {
        self.active_layer_id.and_then(|id| self.layer(id))
    }
    pub fn active_mut(&mut self) -> Option<&mut Layer> {
        self.active_layer_id.and_then(|id| self.layer_mut(id))
    }
    pub fn valid_size(width: u32, height: u32) -> bool {
        width > 0
            && height > 0
            && width <= MAX_SIDE
            && height <= MAX_SIDE
            && (width as u64 * height as u64) <= MAX_PIXELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_compatibility_document_budget_reserves_masks_history_and_decode_scratch() {
        const GIB: u64 = 1024 * 1024 * 1024;
        for (memory, expected) in [
            (2 * GIB, 67_108_864),
            (4 * GIB, 134_217_728),
            (16 * GIB, 536_870_912),
            (64 * GIB, 800_000_000),
            (0, 32 * 1024 * 1024),
        ] {
            assert_eq!(document_budget_for_memory(Some(memory)), expected);
        }
        assert_eq!(document_budget_for_memory(None), MAX_PIXELS);
        assert!((32 * 1024 * 1024..=800_000_000).contains(&document_pixel_budget()));
    }
    #[test]
    fn new_tiles_preserve_base_rows_and_clear_partial_tile_padding() {
        let (width, height) = (259, 131);
        for mut image in [
            Raster::blank(width, height),
            Raster::solid(width, height, [71, 121, 191, 173]),
            Raster::from_rgba(
                width,
                height,
                (0..width * height)
                    .flat_map(|i| [i as u8, (i / 5) as u8, (i / 11) as u8, (i / 13) as u8])
                    .collect(),
            ),
        ] {
            let original = image.clone();
            let mut expected = image.to_rgba();
            for (x, y) in [(127, 127), (128, 128), (258, 130), (256, 0)] {
                image.set(x, y, [23, 47, 89, 113]);
                let i = ((y * width + x) * 4) as usize;
                expected[i..i + 4].copy_from_slice(&[23, 47, 89, 113]);
            }
            assert_eq!(image.to_rgba(), expected);
            assert!(original.tiles.is_empty());
            assert!(Arc::ptr_eq(&original.pixels, &image.pixels));
            let edge = &image.tiles[&(2, 1)];
            for y in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    if x >= 3 || y >= 3 {
                        let i = ((y * TILE_SIZE + x) * 4) as usize;
                        assert_eq!(&edge[i..i + 4], &[0; 4]);
                    }
                }
            }
        }
    }
    #[test]
    fn tile_row_edits_match_scalar_writes_and_preserve_unchanged_snapshots() {
        let (width, height) = (259, 131);
        for mut raster in [
            Raster::blank(width, height),
            Raster::solid(width, height, [70, 120, 190, 170]),
            Raster::from_rgba(
                width,
                height,
                (0..width * height)
                    .flat_map(|i| [i as u8, (i / 3) as u8, (i / 7) as u8, (i / 9) as u8])
                    .collect(),
            ),
        ] {
            raster.set(2, 4, [1, 2, 3, 4]);
            let untouched = raster.tiles[&(0, 0)].clone();
            let snapshot = raster.clone();
            let snapshot_bytes = snapshot.to_rgba();
            let mut scalar = raster.clone();
            raster.set_tile_row(129, &[(256, raster.get(256, 129))]);
            assert!(!raster.tiles.contains_key(&(2, 1)));
            raster.set_tile_row(4, &[(2, [1, 2, 3, 4])]);
            assert!(Arc::ptr_eq(&untouched, &raster.tiles[&(0, 0)]));
            for (y, edits) in [
                (
                    127,
                    vec![(128, [20, 30, 40, 50]), (131, [0; 4]), (255, [255; 4])],
                ),
                (130, vec![(256, [70, 80, 90, 100]), (258, [0; 4])]),
            ] {
                raster.set_tile_row(y, &edits);
                for &(x, color) in &edits {
                    if scalar.get(x, y) != color {
                        scalar.set(x, y, color);
                    }
                }
            }
            assert_eq!(raster.to_rgba(), scalar.to_rgba());
            assert_eq!(
                raster.tiles.keys().collect::<Vec<_>>(),
                scalar.tiles.keys().collect::<Vec<_>>()
            );
            assert_eq!(snapshot.to_rgba(), snapshot_bytes);
            assert!(Arc::ptr_eq(&untouched, &raster.tiles[&(0, 0)]));
            let earlier = raster.clone();
            raster.set_tile_row(127, &[(128, [77; 4])]);
            assert_eq!(earlier.get(128, 127), [20, 30, 40, 50]);
            assert!(!Arc::ptr_eq(
                &earlier.tiles[&(1, 0)],
                &raster.tiles[&(1, 0)]
            ));
        }
    }
    #[test]
    fn materialization_matches_pixel_reads_for_all_bases_and_partial_edge_tiles() {
        let (width, height) = (259, 131);
        for mut image in [
            Raster::blank(width, height),
            Raster::solid(width, height, [53, 91, 207, 153]),
            Raster::from_rgba(
                width,
                height,
                (0..width * height)
                    .flat_map(|i| [i as u8, (i / 3) as u8, (i / 17) as u8, (i / 7) as u8])
                    .collect(),
            ),
        ] {
            for (x, y, color) in [
                (127, 127, [1, 2, 3, 4]),
                (128, 128, [5, 6, 7, 8]),
                (258, 130, [9, 10, 11, 12]),
                (256, 1, [0; 4]),
            ] {
                image.set(x, y, color);
            }
            let mut expected = Vec::new();
            for y in 0..height {
                for x in 0..width {
                    expected.extend_from_slice(&image.get(x, y));
                }
            }
            assert_eq!(image.to_rgba(), expected);
        }
    }
    #[test]
    fn sparse_edits_share_base_and_untouched_tiles() {
        let mut a = Raster::blank(4096, 4096);
        a.set(20, 20, [1, 2, 3, 4]);
        let mut b = a.clone();
        b.set(150, 150, [4, 3, 2, 1]);
        assert!(Arc::ptr_eq(&a.pixels, &b.pixels));
        assert!(Arc::ptr_eq(&a.tiles[&(0, 0)], &b.tiles[&(0, 0)]));
        assert_eq!(a.get(150, 150), [0; 4]);
        assert_eq!(b.tiles.len(), 2);
    }
    #[test]
    fn inverse_transform() {
        let mut t = Transform::new(12., 23., 200., 150.);
        t.rotation = 37.;
        t.flip_x = true;
        let p = [0.23, 0.86];
        let q = t.from_document(t.to_document(p));
        assert!((q[0] - p[0]).abs() < 1e-9 && (q[1] - p[1]).abs() < 1e-9);
    }
    #[test]
    fn deep_raster_edits_keep_untouched_low_bits_in_the_same_tile() {
        let deep = crate::deep_pixels::DeepRaster::from_u16(
            2,
            1,
            vec![30001, 30002, 30003, 40001, 31001, 31002, 31003, 41001],
        )
        .unwrap();
        let mut raster = Raster::from_deep(deep);
        let before = raster.clone();
        raster.set(0, 0, [255, 0, 0, 255]);
        assert_eq!(
            raster.deep.as_ref().unwrap().get_bits(1, 0),
            [31001, 31002, 31003, 41001]
        );
        assert_eq!(
            before.deep.as_ref().unwrap().get_bits(0, 0),
            [30001, 30002, 30003, 40001]
        );
        raster.set_f32(0, 0, [0.50001, 0.5, 0.5, 1.]);
        assert_ne!(raster.deep.as_ref().unwrap().get_bits(0, 0)[0], 32896);
        assert_eq!(raster.materialized().deep, raster.deep);
    }
}
