//! Bounded PSD/PSB reader for the original application's 8-bit RGB and CMYK subset.
use crate::model::{Document, Layer, Mask, Raster, Transform, import_pixel_budget};
use anyhow::{Context, Result, bail, ensure};
use moxcms::{
    ColorProfile, DataColorSpace, Layout, ParsingOptions, RenderingIntent, Transform8BitExecutor,
    TransformOptions,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::{fs::File, io::Read, path::Path, sync::Arc};
use uuid::Uuid;

const MAX_PSD_SOURCE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_PSD_METADATA_BYTES: usize = 64 * 1024 * 1024;

fn source_limit() -> u64 {
    // Keep the compressed source and decoded surfaces within a predictable
    // envelope. This is deliberately based on the same RAM-aware pixel
    // budget used by the importer and capped for high-memory machines.
    import_pixel_budget()
        .saturating_mul(8)
        .saturating_add(64 * 1024 * 1024)
        .min(MAX_PSD_SOURCE_BYTES)
}

fn read_source(path: &Path) -> Result<Vec<u8>> {
    read_source_with_limit(path, source_limit())
}

fn read_source_with_limit(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let metadata = std::fs::metadata(path)?;
    ensure!(metadata.is_file(), "Photoshop 路径必须是普通文件");
    let length = metadata.len();
    ensure!(
        length <= limit,
        "Photoshop 文件超过当前电脑的安全导入大小限制（{} MiB）",
        limit / (1024 * 1024)
    );
    let length = usize::try_from(length).context("Photoshop 文件超过当前平台地址空间")?;
    let mut data = Vec::new();
    data.try_reserve_exact(length)
        .context("Photoshop 文件内存分配失败")?;
    data.resize(length, 0);
    let mut file = File::open(path)?;
    file.read_exact(&mut data)
        .context("Photoshop 文件在读取时被截断")?;
    ensure!(
        file.metadata()?.len() == length as u64,
        "Photoshop 文件在读取时发生变化"
    );
    Ok(data)
}

fn zeroed_bytes(length: u64, what: &str) -> Result<Vec<u8>> {
    let length = usize::try_from(length).context("PSD 缓冲区超过当前平台地址空间")?;
    let mut data = Vec::new();
    data.try_reserve_exact(length)
        .with_context(|| format!("{what}内存分配失败"))?;
    data.resize(length, 0);
    Ok(data)
}

fn copied_bytes(source: &[u8], what: &str) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    data.try_reserve_exact(source.len())
        .with_context(|| format!("{what}内存分配失败"))?;
    data.extend_from_slice(source);
    Ok(data)
}
#[cfg(test)]
#[path = "psd_fixtures.rs"]
mod fixtures;
#[path = "psd_metadata.rs"]
mod metadata;

pub fn import_document(path: &Path) -> Result<crate::image_io::ImportResult> {
    let data = read_source(path)?;
    let psd = decode(
        &data,
        PsdLimits {
            document_pixels: import_pixel_budget(),
            ..PsdLimits::default()
        },
    )?;
    let mut document = Document::new(psd.width, psd.height);
    document.layers.clear();
    document.resolution = psd.resolution;
    let mut conversions = psd.conversions;
    let mut mask_pixels = 0_u64;
    for source in psd.layers {
        let (w, h) = extent(source.bounds);
        let mut layer = Layer::new(&source.name, psd.width, psd.height);
        layer.id = source.id;
        layer.parent_id = source.parent_id;
        layer.is_group = source.is_group;
        layer.visible = source.visible;
        layer.opacity = source.opacity;
        layer.blend_mode = serde_json::from_value(Value::String(source.blend))?;
        layer.mask_source_id = source.clipping_base;
        layer.adjustment = source
            .adjustment
            .as_ref()
            .map(crate::project::canonical_adjustment)
            .transpose()?;
        layer.text = source.text;
        layer.shape = source.shape;
        layer.transform = if w > 0 && h > 0 {
            Transform::new(
                f64::from(source.bounds[1]),
                f64::from(source.bounds[0]),
                f64::from(w),
                f64::from(h),
            )
        } else {
            Transform::new(0.0, 0.0, f64::from(psd.width), f64::from(psd.height))
        };
        layer.image = source
            .pixels
            .map(|p| Arc::new(Raster::from_rgba(p.width, p.height, p.data)));
        if let (Some(style), Some(placement)) = (&layer.text, &source.text_placement) {
            let style = crate::text::TextStyle::from_value(style)?;
            let (raster, anchor) = crate::text::rasterize_anchored(&style)?;
            let radians = placement.rotation.to_radians();
            let sign = if placement.flip_y { -1.0 } else { 1.0 };
            let delta = [
                anchor[0] - f64::from(raster.width) / 2.0,
                (anchor[1] - f64::from(raster.height) / 2.0) * sign,
            ];
            let rotated = [
                delta[0] * radians.cos() - delta[1] * radians.sin(),
                delta[0] * radians.sin() + delta[1] * radians.cos(),
            ];
            layer.transform = Transform::new(
                placement.document_anchor[0] - rotated[0] - f64::from(raster.width) / 2.0,
                placement.document_anchor[1] - rotated[1] - f64::from(raster.height) / 2.0,
                f64::from(raster.width),
                f64::from(raster.height),
            );
            layer.transform.rotation = placement.rotation;
            layer.transform.flip_y = placement.flip_y;
            layer.image = Some(Arc::new(raster));
            conversions.push(format!(
                "{}：可编辑文字使用 Windows 字体重新渲染，字体度量可能与 Photoshop 不同",
                layer.name
            ));
        }
        if let Some(patch) = source.mask {
            let (mw, mh) = layer
                .image
                .as_ref()
                .map_or((psd.width, psd.height), |r| (r.width, r.height));
            ensure!(
                u64::from(mw) * u64::from(mh) <= crate::model::MAX_PIXELS,
                "PSD 蒙版网格超出像素表面限制"
            );
            mask_pixels = mask_pixels
                .checked_add(u64::from(mw) * u64::from(mh))
                .context("PSD 蒙版像素数量溢出")?;
            ensure!(
                mask_pixels <= import_pixel_budget(),
                "PSD 蒙版总像素超出当前电脑的安全导入限制"
            );
            let mut pixels = zeroed_bytes(
                u64::from(mw)
                    .checked_mul(u64::from(mh))
                    .and_then(|n| n.checked_mul(4))
                    .context("PSD 蒙版缓冲区大小溢出")?,
                "PSD 蒙版",
            )?;
            for y in 0..mh {
                for x in 0..mw {
                    let point = layer.transform.to_document([
                        (f64::from(x) + 0.5) / f64::from(mw),
                        (f64::from(y) + 0.5) / f64::from(mh),
                    ]);
                    let px = (point[0] - f64::from(source.mask_bounds[1])).floor() as i64;
                    let py = (point[1] - f64::from(source.mask_bounds[0])).floor() as i64;
                    let gray = if px >= 0
                        && py >= 0
                        && px < i64::from(patch.width)
                        && py < i64::from(patch.height)
                    {
                        patch.data[py as usize * patch.width as usize + px as usize]
                    } else {
                        source.mask_default
                    };
                    let index = (y as usize * mw as usize + x as usize) * 4;
                    pixels[index..index + 4].copy_from_slice(&[gray, gray, gray, 255]);
                }
            }
            layer.mask = Some(Mask {
                raster: Arc::new(Raster::from_rgba(mw, mh, pixels)),
                enabled: source.mask_enabled,
                linked: source.mask_linked,
                placement: if source.mask_linked {
                    None
                } else {
                    Some(layer.transform)
                },
            });
        }
        document.layers.push(layer);
    }
    if let Some(merged) = psd.merged {
        document.layers.push(Layer::raster(
            "Background",
            Raster::from_rgba(merged.width, merged.height, merged.data),
        ));
    }
    document.active_layer_id = document.layers.last().map(|l| l.id);
    crate::project::validate_document(&document)?;
    Ok(crate::image_io::ImportResult {
        document,
        conversions,
    })
}

#[derive(Clone, Debug)]
pub struct PsdRaster {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct PsdLayer {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub is_group: bool,
    pub visible: bool,
    pub opacity: f64,
    pub blend: String,
    pub bounds: [i32; 4],
    pub pixels: Option<PsdRaster>,
    pub mask: Option<PsdRaster>,
    pub mask_bounds: [i32; 4],
    pub mask_default: u8,
    pub mask_enabled: bool,
    pub mask_linked: bool,
    pub clipping_base: Option<Uuid>,
    pub adjustment: Option<Value>,
    pub text: Option<Value>,
    pub text_placement: Option<metadata::TextPlacement>,
    pub shape: Option<Value>,
    pub extra: BTreeMap<String, Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct PsdFile {
    pub width: u32,
    pub height: u32,
    pub resolution: f64,
    pub layers: Vec<PsdLayer>,
    pub merged: Option<PsdRaster>,
    pub conversions: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct PsdLimits {
    pub max_side: u32,
    pub max_surface_pixels: u64,
    pub document_pixels: u64,
}
impl Default for PsdLimits {
    fn default() -> Self {
        Self {
            max_side: 30_000,
            max_surface_pixels: 200_000_000,
            document_pixels: 800_000_000,
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.offset.checked_add(n).context("PSD 数据长度溢出")?;
        ensure!(
            end <= self.bytes.len(),
            "Photoshop 数据在字节 {} 处被截断",
            self.offset
        );
        let result = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(result)
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n)?;
        Ok(())
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into()?))
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into()?))
    }
    fn length(&mut self, large: bool) -> Result<usize> {
        usize::try_from(if large {
            self.u64()?
        } else {
            u64::from(self.u32()?)
        })
        .context("PSD 数据段过大")
    }
    fn string(&mut self, n: usize) -> Result<String> {
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn section(&mut self, n: usize) -> Result<Cursor<'a>> {
        Ok(Cursor::new(self.take(n)?))
    }
}

#[derive(Default)]
struct Record {
    bounds: [i32; 4],
    mask_bounds: [i32; 4],
    opacity: u8,
    fill: u8,
    blend: String,
    clipping: bool,
    hidden: bool,
    channels: Vec<(i16, usize)>,
    name: String,
    section: Option<u32>,
    mask_default: u8,
    mask_enabled: bool,
    mask_linked: bool,
    mask_from_render: bool,
    has_mask: bool,
    extra: BTreeMap<String, Vec<u8>>,
}

fn extent(bounds: [i32; 4]) -> (u32, u32) {
    (
        (i64::from(bounds[3]) - i64::from(bounds[1])).max(0) as u32,
        (i64::from(bounds[2]) - i64::from(bounds[0])).max(0) as u32,
    )
}
fn clipped(bounds: [i32; 4], w: u32, h: u32) -> [i32; 4] {
    [
        bounds[0].clamp(0, h as i32),
        bounds[1].clamp(0, w as i32),
        bounds[2].clamp(0, h as i32),
        bounds[3].clamp(0, w as i32),
    ]
}
fn area(bounds: [i32; 4]) -> u64 {
    let (w, h) = extent(bounds);
    u64::from(w) * u64::from(h)
}

fn record(c: &mut Cursor<'_>, large: bool) -> Result<Record> {
    let mut r = Record {
        bounds: [c.i32()?, c.i32()?, c.i32()?, c.i32()?],
        opacity: 255,
        fill: 255,
        mask_default: 255,
        mask_enabled: true,
        mask_linked: true,
        ..Record::default()
    };
    let channels = c.u16()?;
    ensure!(channels <= 56, "PSD 通道数量过多");
    for _ in 0..channels {
        r.channels.push((c.i16()?, c.length(large)?));
    }
    ensure!(c.take(4)? == b"8BIM", "图层混合标识无效");
    r.blend = c.string(4)?;
    r.opacity = c.u8()?;
    r.clipping = c.u8()? != 0;
    r.hidden = c.u8()? & 2 != 0;
    c.skip(1)?;
    let length = c.u32()? as usize;
    let mut e = c.section(length)?;
    let mask_length = e.u32()? as usize;
    let mut mask = e.section(mask_length)?;
    if mask_length >= 20 {
        r.has_mask = true;
        r.mask_bounds = [mask.i32()?, mask.i32()?, mask.i32()?, mask.i32()?];
        r.mask_default = mask.u8()?;
        let flags = mask.u8()?;
        r.mask_enabled = flags & 2 == 0;
        r.mask_linked = flags & 1 == 0;
        r.mask_from_render = flags & 8 != 0;
    }
    let ranges = e.u32()? as usize;
    e.skip(ranges)?;
    let n = e.u8()? as usize;
    r.name = e.take(n)?.iter().map(|b| char::from(*b)).collect();
    e.skip((4 - (n + 1) % 4) % 4)?;
    const LARGE_KEYS: [&str; 13] = [
        "LMsk", "Lr16", "Lr32", "Layr", "Mt16", "Mt32", "Mtrn", "Alph", "FMsk", "lnk2", "FEid",
        "FXid", "PxSD",
    ];
    while e.bytes.len() - e.offset >= 12 {
        let sig = e.string(4)?;
        ensure!(sig == "8BIM" || sig == "8B64", "附加图层标识无效");
        let key = e.string(4)?;
        let size = e.length(sig == "8B64" || large && LARGE_KEYS.contains(&key.as_str()))?;
        // Keep only payloads that the importer actually parses. Marker-only
        // resources (smart objects, effects, and unknown blocks) can contain
        // very large embedded data; retaining their bytes multiplies peak
        // memory without changing the imported document.
        let bytes = e.take(size)?;
        if size % 2 != 0 && e.offset < e.bytes.len() {
            e.skip(1)?;
        }
        match key.as_str() {
            "luni" => {
                let mut u = Cursor::new(bytes);
                let count = u.u32()? as usize;
                ensure!(count <= 8_000_000, "PSD 名称过长");
                let mut units = Vec::new();
                units
                    .try_reserve_exact(count)
                    .context("PSD 图层名称内存分配失败")?;
                for _ in 0..count {
                    units.push(u.u16()?);
                }
                r.name = String::from_utf16_lossy(&units)
                    .trim_matches('\0')
                    .to_owned();
            }
            "lsct" | "lsdk" if bytes.len() >= 4 => {
                r.section = Some(u32::from_be_bytes(bytes[..4].try_into()?))
            }
            "iOpa" if !bytes.is_empty() => r.fill = bytes[0],
            _ => {}
        }
        if matches!(
            key.as_str(),
            "levl"
                | "curv"
                | "hue2"
                | "hue "
                | "TySh"
                | "tySh"
                | "SoCo"
                | "vstk"
                | "vogk"
                | "vmsk"
                | "vsms"
        ) {
            ensure!(
                bytes.len() <= MAX_PSD_METADATA_BYTES,
                "PSD 图层元数据块过大"
            );
            r.extra.insert(key, copied_bytes(bytes, "PSD 图层元数据")?);
        } else {
            // Preserve marker presence for `contains_key`/`keys` checks while
            // dropping an uninterpreted resource body.
            r.extra.insert(key, Vec::new());
        }
    }
    Ok(r)
}

/// Decode only the requested rectangle. Oversized source channels do not allocate source-size buffers.
fn channel(
    payload: &[u8],
    compression: u16,
    source: (u32, u32),
    crop: [u32; 4],
    large: bool,
) -> Result<Vec<u8>> {
    let (w, h) = source;
    let [x, y, cw, ch] = crop;
    ensure!(
        u64::from(x) + u64::from(cw) <= u64::from(w)
            && u64::from(y) + u64::from(ch) <= u64::from(h),
        "PSD 裁剪范围越界"
    );
    if compression == 0 {
        ensure!(
            (payload.len() as u64) >= u64::from(w) * u64::from(h),
            "PSD 未压缩通道数据被截断"
        );
        let mut out = zeroed_bytes(u64::from(cw) * u64::from(ch), "PSD 通道")?;
        for row in 0..ch as usize {
            let start = (row + y as usize) * w as usize + x as usize;
            out[row * cw as usize..(row + 1) * cw as usize]
                .copy_from_slice(&payload[start..start + cw as usize]);
        }
        return Ok(out);
    }
    ensure!(
        compression == 1,
        "不支持 Photoshop 压缩方式 {compression}；仅支持未压缩和 PackBits"
    );
    ensure!(
        u64::from(h) * if large { 4 } else { 2 } <= payload.len() as u64,
        "PackBits 行表被截断"
    );
    let row_size = if large { 4 } else { 2 };
    let table_size =
        usize::try_from(u64::from(h) * row_size).context("PSD 通道行表超过当前平台地址空间")?;
    let (table, encoded) = payload.split_at(table_size);
    let mut lengths = Cursor::new(table);
    let mut total = 0_usize;
    for _ in 0..h {
        total = total
            .checked_add(if large {
                lengths.u32()? as usize
            } else {
                lengths.u16()? as usize
            })
            .context("PSD 通道数据长度溢出")?;
    }
    ensure!(total <= encoded.len(), "PSD 通道的 PackBits 数据被截断");
    // Read the row table in-place instead of allocating one usize per source
    // row. Cropping a layer can retain a tiny rectangle from a very tall PSB.
    let mut lengths = Cursor::new(table);
    let mut c = Cursor::new(encoded);
    let mut out = zeroed_bytes(u64::from(cw) * u64::from(ch), "PSD 通道")?;
    for row in 0..h {
        let length = if large {
            lengths.u32()? as usize
        } else {
            lengths.u16()? as usize
        };
        let encoded = c.take(length)?;
        if row < y || row >= y + ch {
            continue;
        }
        let mut r = Cursor::new(encoded);
        let mut written = 0_u32;
        while written < w {
            let control = r.u8()? as i8;
            if control >= 0 {
                let n = control as u32 + 1;
                ensure!(
                    written.checked_add(n).is_some_and(|v| v <= w),
                    "PackBits 行长度无效"
                );
                let values = r.take(n as usize)?;
                let left = written.max(x);
                let right = (written + n).min(x + cw);
                if right > left {
                    let target = ((row - y) * cw + left - x) as usize;
                    out[target..target + (right - left) as usize].copy_from_slice(
                        &values[(left - written) as usize..(right - written) as usize],
                    );
                }
                written += n;
            } else if control != -128 {
                let n = (1 - i32::from(control)) as u32;
                ensure!(
                    written.checked_add(n).is_some_and(|v| v <= w),
                    "PackBits 行长度无效"
                );
                let value = r.u8()?;
                let left = written.max(x);
                let right = (written + n).min(x + cw);
                if right > left {
                    let target = ((row - y) * cw + left - x) as usize;
                    out[target..target + (right - left) as usize].fill(value);
                }
                written += n;
            }
        }
    }
    Ok(out)
}

enum PixelColor {
    Rgb,
    Cmyk(Option<Arc<Transform8BitExecutor>>),
}

impl PixelColor {
    fn new(mode: u16, profile: Option<&[u8]>, notes: &mut Vec<String>) -> Self {
        if mode == 3 {
            return Self::Rgb;
        }
        let transform = profile.map(Self::embedded_cmyk);
        match transform {
            Some(Ok(transform)) => {
                notes.push(
                    "CMYK Photoshop：已按嵌入 ICC 相对比色转换为 sRGB；继续使用 RGB 编辑".into(),
                );
                Self::Cmyk(Some(transform))
            }
            unavailable => {
                let reason = match unavailable {
                    Some(Err(error)) => format!("嵌入 ICC 不受支持（{error:#}）"),
                    _ => "文件没有嵌入 CMYK ICC".into(),
                };
                notes.push(format!(
                    "CMYK Photoshop：{reason}，使用 (1−墨量)×(1−黑墨量) 的设备 CMYK 近似转为 sRGB；颜色可能与印刷配置不同"
                ));
                Self::Cmyk(None)
            }
        }
    }

    fn embedded_cmyk(bytes: &[u8]) -> Result<Arc<Transform8BitExecutor>> {
        const MAX_ICC: usize = 16 * 1024 * 1024;
        ensure!(
            (132..=MAX_ICC).contains(&bytes.len()),
            "ICC 长度无效或超过 16 MiB"
        );
        ensure!(
            u32::from_be_bytes(bytes[..4].try_into()?) as usize == bytes.len(),
            "ICC 声明大小与实际长度不一致"
        );
        ensure!(&bytes[36..40] == b"acsp", "ICC 标识无效");
        ensure!(&bytes[16..20] == b"CMYK", "嵌入 ICC 不是 CMYK 色彩空间");
        // Import needs only the forward A2B transform. Output profiles used for
        // printing have a separate, stricter bidirectional validation path.
        std::panic::catch_unwind(|| {
            let profile = ColorProfile::new_from_slice_with_options(
                bytes,
                ParsingOptions {
                    max_profile_size: MAX_ICC,
                    max_allowed_clut_size: 8 * 1024 * 1024,
                    ..Default::default()
                },
            )?;
            ensure!(
                profile.color_space == DataColorSpace::Cmyk,
                "ICC 不是 CMYK 色彩空间"
            );
            profile
                .create_transform_8bit(
                    Layout::Rgba,
                    &ColorProfile::new_srgb(),
                    Layout::Rgb,
                    TransformOptions {
                        rendering_intent: RenderingIntent::RelativeColorimetric,
                        prefer_fixed_point: false,
                        ..Default::default()
                    },
                )
                .context("ICC 缺少可用的 CMYK 到 RGB 转换表")
        })
        .map_err(|_| anyhow::anyhow!("ICC 转换表损坏"))?
    }

    fn color_channels(&self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Cmyk(_) => 4,
        }
    }

    fn rgba(
        &self,
        count: usize,
        planes: &[Option<&[u8]>; 4],
        alpha: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        ensure!(
            planes.iter().flatten().all(|p| p.len() >= count)
                && alpha.is_none_or(|p| p.len() >= count),
            "Photoshop 像素通道被截断"
        );
        let mut rgba = zeroed_bytes(
            (count as u64)
                .checked_mul(4)
                .context("PSD 像素缓冲区大小溢出")?,
            "PSD 像素",
        )?;
        // Reuse bounded scratch for ICC conversion instead of copying every
        // plane into full-size interleaved CMYK and RGB buffers.
        let mut inks = [0_u8; 4096 * 4];
        let mut rgb = [0_u8; 4096 * 3];
        for (block, output) in rgba.chunks_mut(4096 * 4).enumerate() {
            let start = block * 4096;
            let length = output.len() / 4;
            if let Self::Cmyk(Some(transform)) = self {
                for (index, ink) in inks[..length * 4]
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .enumerate()
                {
                    for c in 0..4 {
                        // Photoshop encodes 255 as no ink, unlike ICC's 0.
                        ink[c] = 255 - planes[c].map_or(255, |p| p[start + index]);
                    }
                }
                transform.transform(&inks[..length * 4], &mut rgb[..length * 3])?;
            }
            for (index, pixel) in output.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let at = start + index;
                for c in 0..3 {
                    pixel[c] = match self {
                        Self::Rgb => planes[c].map_or(0, |p| p[at]),
                        Self::Cmyk(Some(_)) => rgb[index * 3 + c],
                        Self::Cmyk(None) => {
                            let ink = u16::from(planes[c].map_or(255, |p| p[at]));
                            let black = u16::from(planes[3].map_or(255, |p| p[at]));
                            ((ink * black + 127) / 255) as u8
                        }
                    };
                }
                // Raster uses straight alpha. Black ink is never alpha.
                pixel[3] = alpha.map_or(255, |p| p[at]);
            }
        }
        Ok(rgba)
    }
}

pub fn decode(bytes: &[u8], limits: PsdLimits) -> Result<PsdFile> {
    let mut c = Cursor::new(bytes);
    ensure!(c.take(4)? == b"8BPS", "这不是 Photoshop 文件");
    let version = c.u16()?;
    ensure!(
        version == 1 || version == 2,
        "不支持 Photoshop 版本 {version}"
    );
    let large = version == 2;
    c.skip(6)?;
    let channels = c.u16()?;
    ensure!((1..=56).contains(&channels), "Photoshop 通道数量无效");
    let height = c.u32()?;
    let width = c.u32()?;
    let depth = c.u16()?;
    let mode = c.u16()?;
    ensure!(
        width > 0
            && height > 0
            && width <= limits.max_side
            && height <= limits.max_side
            && u64::from(width) * u64::from(height) <= limits.max_surface_pixels,
        "Photoshop 画布超出内存限制"
    );
    ensure!(depth == 8, "仅支持 8-bit Photoshop 文件");
    ensure!(mode == 3 || mode == 4, "仅支持 RGB 或 CMYK Photoshop 文件");
    ensure!(
        channels >= if mode == 4 { 4 } else { 3 },
        "Photoshop 颜色通道数量不足"
    );
    let color_length = c.u32()? as usize;
    c.skip(color_length)?;
    let resources_length = c.u32()? as usize;
    let mut resources = c.section(resources_length)?;
    let mut resolution = 72.0;
    let mut icc = None;
    while resources.bytes.len() - resources.offset >= 12 {
        ensure!(resources.take(4)? == b"8BIM", "Photoshop 资源标识无效");
        let id = resources.u16()?;
        let n = resources.u8()? as usize;
        resources.skip(n)?;
        if !(n + 1).is_multiple_of(2) {
            resources.skip(1)?;
        }
        let len = resources.u32()? as usize;
        let data = resources.take(len)?;
        if !len.is_multiple_of(2) {
            resources.skip(1)?;
        }
        if id == 1005 && data.len() >= 4 {
            resolution =
                (u32::from_be_bytes(data[..4].try_into()?) as f64 / 65536.0).clamp(1.0, 9600.0);
        }
        if id == 1039 && !data.is_empty() {
            icc = Some(data);
        }
    }
    let mut notes = Vec::new();
    let color = PixelColor::new(mode, icc, &mut notes);
    let section_length = c.length(large)?;
    let mut section = c.section(section_length)?;
    let mut records = Vec::new();
    if section_length > 0 {
        let info_length = section.length(large)?;
        let mut info = section.section(info_length)?;
        if info_length > 0 {
            let count = i32::from(info.i16()?).unsigned_abs();
            ensure!(count <= 10_000, "Photoshop 图层数量过多");
            for _ in 0..count {
                records.push(record(&mut info, large)?);
            }
            let must_crop = records
                .iter()
                .fold(0_u64, |sum, r| sum.saturating_add(area(r.bounds)))
                > limits.document_pixels
                || records.iter().any(|r| {
                    let (w, h) = extent(r.bounds);
                    w > limits.max_side || h > limits.max_side
                });
            let mut used = 0_u64;
            let mut mask_used = 0_u64;
            let mut layers = Vec::new();
            let mut groups = Vec::new();
            let mut bases = BTreeMap::<Option<Uuid>, Uuid>::new();
            for r in records {
                let mut bounds = if must_crop {
                    clipped(r.bounds, width, height)
                } else {
                    r.bounds
                };
                let mask_bounds = if must_crop {
                    clipped(r.mask_bounds, width, height)
                } else {
                    r.mask_bounds
                };
                let (w, h) = extent(bounds);
                let (mw, mh) = extent(mask_bounds);
                ensure!(
                    w <= limits.max_side
                        && h <= limits.max_side
                        && mw <= limits.max_side
                        && mh <= limits.max_side,
                    "PSD 图层边长超出限制"
                );
                let count = u64::from(w) * u64::from(h);
                used = used.checked_add(count).context("PSD 像素数量溢出")?;
                ensure!(used <= limits.document_pixels, "PSD 图层像素超出内存限制");
                if !r.mask_from_render && r.channels.iter().any(|(id, _)| *id == -2) {
                    mask_used = mask_used
                        .checked_add(u64::from(mw) * u64::from(mh))
                        .context("PSD 蒙版像素数量溢出")?;
                    ensure!(
                        mask_used <= limits.document_pixels,
                        "PSD 蒙版总像素超出内存限制"
                    );
                }
                let mut planes = BTreeMap::new();
                for (id, len) in &r.channels {
                    let data = info.take(*len)?;
                    if !(*id == -2 || *id == -1 || (0..color.color_channels() as i16).contains(id))
                        || *len < 2
                    {
                        continue;
                    }
                    let mask = *id == -2;
                    // Photoshop marks some mask channels as an already
                    // rendered result. They are intentionally discarded
                    // below, so consume their bytes without decoding a
                    // potentially huge surface that is not in our budget.
                    if mask && r.mask_from_render {
                        continue;
                    }
                    let target = if mask { mask_bounds } else { bounds };
                    let src = if mask { r.mask_bounds } else { r.bounds };
                    let (tw, th) = extent(target);
                    if tw == 0 || th == 0 {
                        continue;
                    }
                    let compression = u16::from_be_bytes(data[..2].try_into()?);
                    let crop = [
                        (i64::from(target[1]) - i64::from(src[1])) as u32,
                        (i64::from(target[0]) - i64::from(src[0])) as u32,
                        tw,
                        th,
                    ];
                    planes.insert(
                        *id,
                        channel(&data[2..], compression, extent(src), crop, large)?,
                    );
                }
                if r.section == Some(3) {
                    groups.push(Uuid::new_v4());
                    continue;
                }
                let is_group = matches!(r.section, Some(1 | 2));
                let id = if is_group {
                    groups.pop().unwrap_or_else(Uuid::new_v4)
                } else {
                    Uuid::new_v4()
                };
                let parent_id = groups.last().copied();
                let name = if r.name.trim().is_empty() {
                    "Layer".to_owned()
                } else {
                    r.name
                };
                let mut pixels = if w > 0 && h > 0 && !is_group {
                    let channels =
                        std::array::from_fn(|c| planes.get(&(c as i16)).map(Vec::as_slice));
                    let data = color.rgba(
                        count as usize,
                        &channels,
                        planes.get(&-1).map(Vec::as_slice),
                    )?;
                    Some(PsdRaster {
                        width: w,
                        height: h,
                        data,
                    })
                } else {
                    None
                };
                let mask = if !r.mask_from_render {
                    planes.remove(&-2).map(|data| PsdRaster {
                        width: mw,
                        height: mh,
                        data,
                    })
                } else {
                    None
                };
                let adjustment = parse_adjustment(&r.extra);
                let mut metadata_notes = Vec::new();
                let parsed_text = metadata::text(&r.extra, &mut metadata_notes);
                let (text, text_placement) = parsed_text
                    .map_or((None, None), |(style, placement)| {
                        (Some(style), Some(placement))
                    });
                let mut shape = None;
                if text.is_none()
                    && !is_group
                    && let Some(vector) = metadata::vector(
                        &r.extra,
                        [width, height],
                        PsdLimits {
                            document_pixels: limits
                                .document_pixels
                                .saturating_sub(used)
                                .saturating_add(count),
                            ..limits
                        },
                    )?
                    && (vector.shape.is_some() || pixels.is_none())
                {
                    let vector_pixels =
                        u64::from(vector.raster.width) * u64::from(vector.raster.height);
                    used = used
                        .saturating_sub(
                            pixels
                                .as_ref()
                                .map_or(0, |p| u64::from(p.width) * u64::from(p.height)),
                        )
                        .saturating_add(vector_pixels);
                    ensure!(used <= limits.document_pixels, "PSD 矢量像素超出内存限制");
                    pixels = Some(vector.raster);
                    bounds = vector.bounds;
                    shape = vector.shape;
                    metadata_notes.extend(vector.notes);
                }
                for note in metadata_notes {
                    notes.push(format!("{name}: {note}"));
                }
                let unsupported_adjustment = r.extra.keys().any(|k| {
                    [
                        "levl", "curv", "hue2", "hue ", "expA", "grdm", "brit", "blnc", "nvrt",
                        "thrs", "post", "mixr", "selc", "blwh", "phfl", "vibA",
                    ]
                    .contains(&k.as_str())
                }) && adjustment.is_none();
                if unsupported_adjustment {
                    notes.push(format!("{name}：已跳过不支持的 Photoshop 调整"));
                    continue;
                }
                if r.extra.contains_key("SoLd") || r.extra.contains_key("SoLE") {
                    notes.push(format!("{name}：智能对象已按像素导入"));
                }
                if r.extra
                    .keys()
                    .any(|k| ["lfx2", "lrFX", "lmfx"].contains(&k.as_str()))
                {
                    notes.push(format!("{name}：Photoshop 图层效果已舍弃"));
                }
                if text.is_none()
                    && r.extra
                        .keys()
                        .any(|k| ["TySh", "tySh", "txt2"].contains(&k.as_str()))
                {
                    notes.push(format!("{name}：Photoshop 文字已按栅格像素导入"));
                }
                if shape.is_none()
                    && r.extra
                        .keys()
                        .any(|k| ["vmsk", "vsms", "vogk"].contains(&k.as_str()))
                {
                    notes.push(format!("{name}：Photoshop 矢量内容已按栅格像素导入"));
                }
                if must_crop && bounds != r.bounds {
                    notes.push(format!("{name}：为满足文档内存限制，已裁去画布外的像素"));
                }
                let blend = if is_group {
                    "Normal".to_owned()
                } else {
                    match blend_name(&r.blend) {
                        Some(v) => v.to_owned(),
                        None => {
                            notes.push(format!("{name}：混合模式 {} 已替换为正常模式", r.blend));
                            "Normal".to_owned()
                        }
                    }
                };
                let clipping_base = if r.clipping {
                    let base = bases.get(&parent_id).copied();
                    if base.is_none() {
                        notes.push(format!("{name}：剪贴蒙版没有可支持的基底图层"));
                    }
                    base
                } else {
                    if !is_group && adjustment.is_none() {
                        bases.insert(parent_id, id);
                    } else {
                        bases.remove(&parent_id);
                    }
                    None
                };
                let effects = r
                    .extra
                    .keys()
                    .any(|k| ["lfx2", "lrFX", "lmfx"].contains(&k.as_str()));
                let opacity = f64::from(r.opacity) / 255.0
                    * if effects {
                        1.0
                    } else {
                        f64::from(r.fill) / 255.0
                    };
                layers.push(PsdLayer {
                    id,
                    parent_id,
                    name,
                    is_group,
                    visible: !r.hidden,
                    opacity,
                    blend,
                    bounds: if is_group || adjustment.is_some() {
                        [0, 0, height as i32, width as i32]
                    } else {
                        bounds
                    },
                    pixels: if adjustment.is_some() { None } else { pixels },
                    mask,
                    mask_bounds,
                    mask_default: r.mask_default,
                    mask_enabled: r.mask_enabled,
                    mask_linked: r.mask_linked,
                    clipping_base,
                    adjustment,
                    text,
                    text_placement,
                    shape,
                    extra: r.extra,
                });
            }
            ensure!(groups.is_empty(), "Photoshop 分组标记不匹配");
            return Ok(PsdFile {
                width,
                height,
                resolution,
                layers,
                merged: None,
                conversions: notes,
            });
        }
    }
    let compression = c.u16()?;
    let data = c.take(c.bytes.len() - c.offset)?;
    let count = (u64::from(width) * u64::from(height)) as usize;
    ensure!(
        (count as u64) <= limits.document_pixels,
        "Photoshop 合成图像超出内存限制"
    );
    let mut planes = Vec::new();
    let retained_channels = color.color_channels() + 1;
    if compression == 0 {
        ensure!(
            data.len() >= count * channels as usize,
            "Photoshop 合成图像数据被截断"
        );
        // Photoshop may store spot/extra alpha channels here. The importer
        // retains RGB(A) or CMYK(A), skipping all remaining spot channels.
        for ch in 0..(channels as usize).min(retained_channels) {
            planes.push(copied_bytes(
                &data[ch * count..(ch + 1) * count],
                "PSD 合成通道",
            )?);
        }
    } else if compression == 1 {
        let row_size: usize = if large { 4 } else { 2 };
        let table_size = row_size
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(channels as usize))
            .context("PSD 合成图像行表长度溢出")?;
        ensure!(data.len() >= table_size, "合成图像的 PackBits 表被截断");
        let mut offset = table_size;
        for ch in 0..channels as usize {
            let table_start = ch
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(row_size))
                .context("PSD 合成图像行表偏移溢出")?;
            let table_end = table_start
                .checked_add(height as usize * row_size)
                .context("PSD 合成图像行表范围溢出")?;
            let mut t = Cursor::new(&data[table_start..table_end]);
            let mut total = 0_usize;
            for _ in 0..height {
                total = total
                    .checked_add(if large {
                        t.u32()? as usize
                    } else {
                        t.u16()? as usize
                    })
                    .context("PSD RLE 数据长度溢出")?;
            }
            ensure!(
                offset.checked_add(total).is_some_and(|n| n <= data.len()),
                "合成图像的 PackBits 数据被截断"
            );
            if ch < retained_channels {
                let mut payload = Vec::new();
                payload
                    .try_reserve_exact(
                        (table_end - table_start)
                            .checked_add(total)
                            .context("PSD 合成通道缓冲区大小溢出")?,
                    )
                    .context("PSD 合成通道内存分配失败")?;
                payload.extend_from_slice(&data[table_start..table_end]);
                payload.extend_from_slice(&data[offset..offset + total]);
                planes.push(channel(
                    &payload,
                    compression,
                    (width, height),
                    [0, 0, width, height],
                    large,
                )?);
            }
            offset += total;
        }
    } else {
        bail!("不支持 Photoshop 合成图像的压缩方式 {compression}");
    }
    ensure!(
        planes.len() >= color.color_channels(),
        "Photoshop 颜色通道数量不足"
    );
    let colors =
        std::array::from_fn(|c| (c < color.color_channels()).then(|| planes[c].as_slice()));
    let rgba = color.rgba(
        count,
        &colors,
        planes.get(color.color_channels()).map(Vec::as_slice),
    )?;
    Ok(PsdFile {
        width,
        height,
        resolution,
        layers: Vec::new(),
        merged: Some(PsdRaster {
            width,
            height,
            data: rgba,
        }),
        conversions: notes,
    })
}

fn blend_name(key: &str) -> Option<&'static str> {
    Some(match key {
        "norm" => "Normal",
        "mul " => "Multiply",
        "scrn" => "Screen",
        "over" => "Overlay",
        "sLit" => "Soft Light",
        "dark" => "Darken",
        "lite" => "Lighten",
        "diff" => "Difference",
        "div " => "Color Dodge",
        "idiv" => "Color Burn",
        "hue " => "Hue",
        "sat " => "Saturation",
        "colr" => "Color",
        "lum " => "Luminosity",
        "lbrn" => "Linear Burn",
        "lddg" => "Linear Dodge (Add)",
        "hLit" => "Hard Light",
        "vLit" => "Vivid Light",
        "lLit" => "Linear Light",
        "pLit" => "Pin Light",
        "hMix" => "Hard Mix",
        "smud" => "Exclusion",
        "fsub" => "Subtract",
        "fdiv" => "Divide",
        _ => return None,
    })
}

fn parse_adjustment(extra: &BTreeMap<String, Vec<u8>>) -> Option<Value> {
    if let Some(d) = extra.get("levl") {
        if d.len() < 292 {
            return None;
        }
        let mut c = Cursor::new(d);
        c.u16().ok()?;
        let mut ranges = Vec::new();
        for _ in 0..4 {
            let black = c.u16().ok()?.min(254);
            let white = c.u16().ok()?.clamp(black + 1, 255);
            let output_black = c.u16().ok()?.min(255);
            let output_white = c.u16().ok()?.min(255);
            let gamma = (f64::from(c.u16().ok()?) / 100.0).clamp(0.1, 9.99);
            ranges.push(json!({"black":black,"white":white,"outputBlack":output_black,"outputWhite":output_white,"gamma":gamma}));
        }
        return Some(json!({"kind":"Levels","levels":{"channel":"RGB","ranges":ranges}}));
    }
    if let Some(d) = extra.get("curv") {
        let mut c = Cursor::new(d);
        if d.first() == Some(&0) {
            c.skip(1).ok()?;
        }
        let version = c.u16().ok()?;
        if version != 1 && version != 4 {
            return None;
        }
        let count = c.u16().ok()?;
        let mut channels = vec![json!([{"x":0,"y":0},{"x":255,"y":255}]); 4];
        for ch in 0..count.min(4) {
            let n = c.u16().ok()?;
            if n > 4096 {
                return None;
            }
            let mut points = Vec::new();
            for _ in 0..n {
                let y = c.u16().ok()?.min(255);
                let x = c.u16().ok()?.min(255);
                points.push((x, y));
            }
            points.sort_by_key(|p| p.0);
            points.dedup_by_key(|p| p.0);
            if points.len() >= 2 {
                if points[0].0 != 0 {
                    points.insert(0, (0, points[0].1));
                }
                if points.last()?.0 != 255 {
                    points.push((255, points.last()?.1));
                }
                channels[ch as usize] = json!(
                    points
                        .iter()
                        .map(|&(x, y)| json!({"x":x,"y":y}))
                        .collect::<Vec<_>>()
                );
            }
        }
        return Some(json!({"kind":"Curves","curves":{"channel":"RGB","channels":channels}}));
    }
    if let Some(d) = extra.get("hue2").or_else(|| extra.get("hue ")) {
        if d.len() < 16 {
            return None;
        }
        let colorize = d[2] != 0;
        let mut c = Cursor::new(d);
        let values = |c: &mut Cursor<'_>| -> Option<Value> {
            Some(
                json!({"hue":c.i16().ok()?.clamp(-360,360),"saturation":c.i16().ok()?.clamp(-100,100),"lightness":c.i16().ok()?.clamp(-100,100)}),
            )
        };
        c.skip(if colorize { 4 } else { 10 }).ok()?;
        let master = values(&mut c)?;
        let mut adjustments = serde_json::Map::new();
        adjustments.insert("Master".to_owned(), master);
        let mut bands = serde_json::Map::new();
        bands.insert(
            "Master".to_owned(),
            json!({"falloffStart":0,"rangeStart":0,"rangeEnd":360,"falloffEnd":360}),
        );
        c.offset = 16;
        for (index, name) in ["Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas"]
            .iter()
            .enumerate()
        {
            let center = index as f64 * 60.0;
            let mut handles = [center - 45.0, center - 15.0, center + 15.0, center + 45.0]
                .map(|h| h.rem_euclid(360.0));
            if !colorize && d.len() - c.offset >= 14 {
                for handle in &mut handles {
                    *handle = f64::from(c.i16().ok()?).rem_euclid(360.0);
                }
                adjustments.insert((*name).to_owned(), values(&mut c)?);
            }
            bands.insert((*name).to_owned(), json!({"falloffStart":handles[0],"rangeStart":handles[1],"rangeEnd":handles[2],"falloffEnd":handles[3]}));
        }
        return Some(
            json!({"kind":"Hue/Saturation","hsvSettings":{"range":"Master","colorize":colorize,"invertRange":false,"adjustments":adjustments,"bands":bands}}),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_read_rejects_oversized_file_before_allocating_and_keeps_unicode_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("中文 项目 💡.psd");
        let original = b"8BPS source bytes";
        std::fs::write(&path, original).unwrap();
        assert!(read_source_with_limit(&path, original.len() as u64 - 1).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            read_source_with_limit(&path, original.len() as u64).unwrap(),
            original
        );
        assert!(read_source_with_limit(directory.path(), 1024).is_err());
    }
    #[test]
    fn allocation_failure_is_an_import_error() {
        assert!(zeroed_bytes(u64::MAX, "PSD 测试").is_err());
        assert_eq!(zeroed_bytes(6, "PSD 测试").unwrap(), [0; 6]);
    }
    #[test]
    fn packbits_literal_repeat_and_noop() {
        let d = [0, 6, 128, 1, 10, 20, 255, 30];
        assert_eq!(
            channel(&d, 1, (4, 1), [0, 0, 4, 1], false).unwrap(),
            [10, 20, 30, 30]
        );
    }
    #[test]
    fn packbits_crop_matches_source() {
        let d = [0, 5, 3, 10, 20, 30, 40];
        assert_eq!(
            channel(&d, 1, (4, 1), [1, 0, 2, 1], false).unwrap(),
            [20, 30]
        );
    }
    #[test]
    fn packbits_rejects_overflow() {
        assert!(channel(&[0, 2, 251, 3], 1, (4, 1), [0, 0, 4, 1], false).is_err());
    }
    #[test]
    fn merged_psd_and_psb() {
        for version in [1_u16, 2] {
            let mut b = b"8BPS".to_vec();
            b.extend(version.to_be_bytes());
            b.extend([0; 6]);
            b.extend(3_u16.to_be_bytes());
            b.extend(1_u32.to_be_bytes());
            b.extend(2_u32.to_be_bytes());
            b.extend(8_u16.to_be_bytes());
            b.extend(3_u16.to_be_bytes());
            b.extend(0_u32.to_be_bytes());
            b.extend(0_u32.to_be_bytes());
            if version == 2 {
                b.extend(0_u64.to_be_bytes());
            } else {
                b.extend(0_u32.to_be_bytes());
            }
            b.extend(0_u16.to_be_bytes());
            b.extend([255, 10, 20, 30, 40, 50]);
            let p = decode(&b, PsdLimits::default()).unwrap();
            assert_eq!(p.merged.unwrap().data, [255, 20, 40, 255, 10, 30, 50, 255]);
        }
    }
    #[test]
    fn truncated_header_is_error() {
        for len in 0..26 {
            assert!(decode(&vec![0; len], PsdLimits::default()).is_err());
        }
    }
}
