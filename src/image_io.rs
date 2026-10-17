//! Raster codecs, SVG rendering, camera sensor development and derivative exports.
use crate::model::{Document, Layer, MAX_PIXELS, MAX_SIDE, Raster, import_pixel_budget};
use anyhow::{Context, Result, ensure};
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageReader};
use rayon::prelude::*;
use std::{
    fs::{self, File},
    io::{BufReader, Cursor, Read, Seek, Write},
    path::Path,
};
use uuid::Uuid;

#[path = "raw_develop.rs"]
mod raw_develop;

pub struct ImportResult {
    pub document: Document,
    pub conversions: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RawDevelopSettings {
    pub exposure: f32,
    pub temperature: f32,
    pub tint: f32,
    pub boost: f32,
    pub noise_reduction: f32,
    pub lens_correction: bool,
    pub highlight_recovery: bool,
}
impl Default for RawDevelopSettings {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            temperature: 5000.0,
            tint: 0.0,
            boost: 1.0,
            noise_reduction: 0.0,
            lens_correction: false,
            highlight_recovery: false,
        }
    }
}
impl RawDevelopSettings {
    pub fn key(self) -> [u32; 7] {
        [
            self.exposure.to_bits(),
            self.temperature.to_bits(),
            self.tint.to_bits(),
            self.boost.to_bits(),
            self.noise_reduction.to_bits(),
            self.lens_correction as u32,
            self.highlight_recovery as u32,
        ]
    }

    /// rawloader 0.37 exposes no lens profile or correction opcodes.
    pub const fn supports_lens_correction() -> bool {
        false
    }

    pub(crate) fn validate(self) -> Result<()> {
        ensure!(
            self.exposure.is_finite()
                && (-20.0..=20.0).contains(&self.exposure)
                && self.temperature.is_finite()
                && (1000.0..=50000.0).contains(&self.temperature)
                && self.tint.is_finite()
                && (-150.0..=150.0).contains(&self.tint)
                && self.boost.is_finite()
                && (0.0..=2.0).contains(&self.boost)
                && self.noise_reduction.is_finite()
                && (0.0..=1.0).contains(&self.noise_reduction),
            "RAW 显影参数无效"
        );
        ensure!(
            !self.lens_correction,
            "当前 RAW 解码器未提供镜头配置数据，暂不支持按相机镜头配置自动校正"
        );
        Ok(())
    }
}

fn size(width: u32, height: u32) -> Result<()> {
    let pixels = u64::from(width) * u64::from(height);
    ensure!(
        width > 0
            && height > 0
            && width <= MAX_SIDE
            && height <= MAX_SIDE
            && pixels <= MAX_PIXELS
            && pixels <= import_pixel_budget(),
        "图像尺寸超出支持的内存限制"
    );
    Ok(())
}

/// Allocate a pixel buffer without turning an import-time allocation failure
/// into a process abort. `vec![0; n]` routes allocation failures through the
/// global OOM handler on Windows; a fallible reservation lets the caller show
/// the normal import error instead, which is especially important on Win10
/// machines with limited address space.
fn zeroed_bytes(width: u32, height: u32, channels: u64, what: &str) -> Result<Vec<u8>> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|n| n.checked_mul(channels))
        .context("图像缓冲区大小溢出")?;
    let length = usize::try_from(pixels).context("图像缓冲区超过当前平台地址空间")?;
    let mut data = Vec::new();
    data.try_reserve_exact(length)
        .with_context(|| format!("{what}内存分配失败"))?;
    data.resize(length, 0);
    Ok(data)
}
fn ext(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}
pub fn is_raw(path: &Path) -> bool {
    [
        "dng", "cr2", "crw", "nef", "nrw", "arw", "sr2", "srf", "orf", "rw2", "raf", "pef", "rwl",
        "3fr", "erf", "kdc", "dcr", "mos", "mrw", "mef", "raw", "srw", "iiq", "x3f",
    ]
    .contains(&ext(path).as_str())
}

pub fn import(path: &Path) -> Result<ImportResult> {
    if ext(path) == "pdf" {
        let info = crate::pdf_io::info(path)?;
        ensure!(
            info.pages.len() == 1,
            "这是多页 PDF，请在打开窗口选择页面，或使用 CLI 的 --page 指定页码"
        );
        let mut page = crate::pdf_io::render_pages(path, &[0], 300)?;
        let page = page.pop().context("PDF 没有可导入页面")?;
        let mut document = Document::new(page.raster.width, page.raster.height);
        document.resolution = 300.;
        let layer = Layer::raster(
            path.file_stem().unwrap_or_default().to_string_lossy(),
            page.raster,
        );
        document.active_layer_id = Some(layer.id);
        document.layers = vec![layer];
        return Ok(ImportResult {
            document,
            conversions: vec![
                "PDF 已按 300 DPI 转为图片；文字、链接和表单不再保留为 PDF 对象".into(),
            ],
        });
    }
    if path.is_dir() || ext(path) == "comp" {
        return Ok(ImportResult {
            document: crate::project::load(path)?,
            conversions: Vec::new(),
        });
    }
    let mut file = File::open(path)?;
    let mut magic = [0; 4];
    let length = file.read(&mut magic)?;
    if length == 4 && &magic == b"8BPS" {
        return crate::psd::import_document(path);
    }
    let raster = decode_image(path)?;
    let mut document = Document::new(raster.width, raster.height);
    let name = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let layer = Layer::raster(name, raster);
    document.active_layer_id = Some(layer.id);
    document.layers = vec![layer];
    Ok(ImportResult {
        document,
        conversions: if is_raw(path) {
            vec!["RAW 已使用 Rust 传感器解码、插值和相机色彩矩阵显影；渲染结果与 Apple Camera RAW 的色调曲线不同".to_owned()]
        } else {
            Vec::new()
        },
    })
}

pub fn decode_image(path: &Path) -> Result<Raster> {
    if is_raw(path) {
        return develop_raw(path, RawDevelopSettings::default(), None);
    }
    if ext(path) == "svg" {
        return decode_svg(path, None);
    }
    if ["heic", "heif", "hif"].contains(&ext(path).as_str()) {
        return decode_wic(path);
    }
    let mut reader = ImageReader::new(BufReader::new(File::open(path)?)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    // Keep decoder scratch space bounded. The previous 16 bytes/pixel limit
    // allowed a 200 MP image to request over 3 GiB before the application
    // could report an import error, which can exhaust commit on a low-memory
    // Win10 machine. Eight bytes/pixel still covers the
    // decoder's compressed/native/RGBA copies while leaving headroom for the
    // editor itself.
    limits.max_alloc = Some(
        import_pixel_budget()
            .saturating_mul(8)
            .saturating_add(64 * 1024 * 1024),
    );
    reader.limits(limits);
    let format = reader.format();
    let mut decoder = reader.into_decoder()?;
    let (w, h) = decoder.dimensions();
    size(w, h)?;
    let precision_cost = match decoder.color_type() {
        image::ColorType::Rgb32F | image::ColorType::Rgba32F => 6,
        image::ColorType::L16
        | image::ColorType::La16
        | image::ColorType::Rgb16
        | image::ColorType::Rgba16 => 3,
        _ => 1,
    };
    crate::deep_io::check_size(w, h, precision_cost)?;
    let orientation = decoder.orientation()?;
    let profile = decoder.icc_profile()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    if matches!(
        format,
        Some(image::ImageFormat::OpenExr | image::ImageFormat::Hdr)
    ) {
        return crate::deep_io::from_linear(image, format == Some(image::ImageFormat::OpenExr));
    }
    if profile.is_none() {
        image.apply_color_space(image::metadata::Cicp::SRGB, Default::default())?;
    }
    crate::deep_io::from_dynamic(image, profile.as_deref())
}

pub fn decode_svg(path: &Path, fitting: Option<[u32; 2]>) -> Result<Raster> {
    let path = path.to_path_buf();
    // usvg recursively converts nested groups and references. Keep parsing,
    // rendering and tree destruction off Windows' default 1 MiB caller stack.
    std::thread::Builder::new()
        .name("svg-decode".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || decode_svg_worker(&path, fitting))
        .context("启动 SVG 解码线程失败")?
        .join()
        .map_err(|_| anyhow::anyhow!("SVG 解码器处理图像时异常"))?
}

fn decode_svg_worker(path: &Path, fitting: Option<[u32; 2]>) -> Result<Raster> {
    ensure!(
        fs::metadata(path)?.len() <= 64 * 1024 * 1024,
        "SVG 文件超过 64 MiB"
    );
    let bytes = fs::read(path)?;
    let mut options = resvg::usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        ..Default::default()
    };
    options.fontdb_mut().load_system_fonts();
    let tree = resvg::usvg::Tree::from_data(&bytes, &options)?;
    let native = tree.size();
    let scale = fitting.map_or(1.0, |[w, h]| {
        (w as f32 / native.width()).min(h as f32 / native.height())
    });
    let width = (native.width() * scale).round().max(1.0) as u32;
    let height = (native.height() * scale).round().max(1.0) as u32;
    size(width, height)?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).context("SVG 栅格分配失败")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut bytes = pixmap.take();
    for p in bytes.as_chunks_mut::<4>().0 {
        if p[3] != 0 {
            for i in 0..3 {
                p[i] = ((u32::from(p[i]) * 255 + u32::from(p[3]) / 2) / u32::from(p[3])).min(255)
                    as u8;
            }
        }
    }
    Ok(Raster::from_rgba(width, height, bytes))
}

pub fn develop_raw(
    path: &Path,
    settings: RawDevelopSettings,
    limit: Option<u32>,
) -> Result<Raster> {
    let path = path.to_path_buf();
    // Several camera decoders use large stack-resident Huffman lookup tables.
    // Windows' default 1 MiB stack overflows on real Nikon NEF inputs.
    std::thread::Builder::new()
        .name("raw-decode".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || develop_raw_worker(&path, settings, limit))?
        .join()
        .map_err(|_| anyhow::anyhow!("RAW 解码器处理损坏的相机数据时异常"))?
}
fn develop_raw_worker(
    path: &Path,
    settings: RawDevelopSettings,
    limit: Option<u32>,
) -> Result<Raster> {
    settings.validate()?;
    let mut options = File::options();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Keep preflight and decoding on the same file, without a concurrent
        // writer or path replacement invalidating the checked sensor layout.
        options.share_mode(1); // FILE_SHARE_READ
    }
    let mut file = options.open(path)?;
    ensure!(
        file.metadata()?.len() <= 512 * 1024 * 1024,
        "RAW 编码文件超过 512 MiB"
    );
    let dummy = rawloader::decode_dummy(&mut file).context("不支持此 RAW 相机或压缩格式")?;
    // Unsupported component counts must be rejected before rawloader allocates
    // width * height * cpp samples; an allocation abort bypasses thread joining.
    raw_sample_count(&dummy)?;
    if limit.is_none() {
        crate::deep_io::check_size(u32::try_from(dummy.width)?, u32::try_from(dummy.height)?, 3)?;
    }
    file.rewind()?;
    let raw = rawloader::decode(&mut file)?;
    develop_sensor(&raw, settings, limit)
}

fn raw_sample_count(raw: &rawloader::RawImage) -> Result<usize> {
    size(u32::try_from(raw.width)?, u32::try_from(raw.height)?)?;
    ensure!(raw.cpp == 1 || raw.cpp == 3, "不支持此 RAW 分量数量");
    ensure!(
        raw.cpp == 3 || raw.cfa.is_valid(),
        "RAW 没有可支持的彩色滤光阵列"
    );
    raw.width
        .checked_mul(raw.height)
        .and_then(|n| n.checked_mul(raw.cpp))
        .context("RAW 尺寸溢出")
}

fn develop_sensor(
    raw: &rawloader::RawImage,
    settings: RawDevelopSettings,
    limit: Option<u32>,
) -> Result<Raster> {
    settings.validate()?;
    let expected = raw_sample_count(raw)?;
    ensure!(
        match &raw.data {
            rawloader::RawImageData::Integer(v) => v.len() >= expected,
            rawloader::RawImageData::Float(v) => v.len() >= expected,
        },
        "RAW 传感器数据被截断"
    );
    let [top, right, bottom, left] = raw.crops;
    let width = raw
        .width
        .checked_sub(left.checked_add(right).context("RAW 传感器裁剪范围溢出")?)
        .context("RAW 传感器裁剪范围无效")?;
    let height = raw
        .height
        .checked_sub(top.checked_add(bottom).context("RAW 传感器裁剪范围溢出")?)
        .context("RAW 传感器裁剪范围无效")?;
    ensure!(width > 0 && height > 0, "RAW 传感器裁剪范围为空");
    let scale = limit.map_or(1.0, |n| (n as f64 / width.max(height) as f64).min(1.0));
    let ow = (width as f64 * scale).round().max(1.0) as u32;
    let oh = (height as f64 * scale).round().max(1.0) as u32;
    if limit.is_none() {
        // Account for the authoritative 16-bit surface, its byte preview and
        // the temporary orientation/decoder buffers before processing pixels.
        crate::deep_io::check_size(ow, oh, 3)?;
    }
    let matrix = raw.cam_to_xyz_normalized();
    ensure!(
        matrix.iter().flatten().all(|v| v.is_finite()),
        "RAW 相机矩阵无效"
    );
    let sensor =
        raw_develop::Sensor::new(raw, settings.noise_reduction, settings.highlight_recovery)?;
    let neutral = raw.neutralwb();
    let mut wb = raw.wb_coeffs;
    for i in 0..4 {
        if !sensor.is_active(i) {
            wb[i] = 1.;
            continue;
        }
        if !wb[i].is_finite() || wb[i] <= 0.0 {
            wb[i] = neutral[i];
        }
        ensure!(wb[i] > 0. && wb[i].is_finite(), "RAW 白平衡无效");
    }
    let green = wb[1];
    ensure!(green > 0.0 && green.is_finite(), "RAW 白平衡无效");
    for x in &mut wb {
        *x /= green;
    }
    let warm = (settings.temperature / 5000.0).powf(0.45);
    wb[0] *= warm;
    wb[2] /= warm;
    wb[1] *= (1.0 - settings.tint / 300.0).max(0.1);
    let exposure = 2_f32.powf(settings.exposure);
    let sample = |x: usize, y: usize, color: usize| sensor.sample(x, y, color);
    let radius = raw.cfa.width.max(raw.cfa.height).max(2) as isize;
    let develop_pixel = |x: usize, row: usize| {
        let sy = top
            + ((row as f64 + 0.5) / scale)
                .floor()
                .min((height - 1) as f64) as usize;
        let sx = left + ((x as f64 + 0.5) / scale).floor().min((width - 1) as f64) as usize;
        let mut camera = [0.0; 4];
        for (color, c) in camera.iter_mut().enumerate() {
            if !sensor.is_active(color) {
                continue;
            }
            if raw.cpp == 3 {
                if color < 3 {
                    *c = sample(sx, sy, color);
                }
                continue;
            }
            if raw.cfa.color_at(sy, sx) == color {
                *c = sample(sx, sy, color);
                continue;
            }
            let mut weight = 0.0;
            let mut total = 0.0;
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = sx as isize + dx;
                    let ny = sy as isize + dy;
                    if nx < 0 || ny < 0 || nx >= raw.width as isize || ny >= raw.height as isize {
                        continue;
                    }
                    if raw.cfa.color_at(ny as usize, nx as usize) == color {
                        let w = 1.0 / (dx * dx + dy * dy) as f32;
                        weight += w;
                        total += sample(nx as usize, ny as usize, color) * w;
                    }
                }
            }
            *c = if weight > 0.0 { total / weight } else { 0.0 };
        }
        sensor.recover(sx, sy, &mut camera);
        for color in 0..4 {
            camera[color] *= wb[color];
        }
        let xyz = matrix.map(|m| m.iter().zip(camera).map(|(a, b)| a * b).sum::<f32>());
        let rgb = [
            3.2404542 * xyz[0] - 1.5371385 * xyz[1] - 0.4985314 * xyz[2],
            -0.969266 * xyz[0] + 1.8760108 * xyz[1] + 0.041556 * xyz[2],
            0.0556434 * xyz[0] - 0.2040259 * xyz[1] + 1.0572252 * xyz[2],
        ];
        let developed = rgb.map(|value| {
            let linear = (value * exposure).max(0.0);
            let shoulder = linear / (1.0 + linear);
            let mixed = linear + (shoulder - linear) * settings.boost * 0.5;
            if mixed <= 0.0031308 {
                mixed * 12.92
            } else {
                1.055 * mixed.powf(1.0 / 2.4) - 0.055
            }
        });
        (developed, sx, sy)
    };
    let mut image = if limit.is_none() {
        let length = (ow as usize)
            .checked_mul(oh as usize)
            .and_then(|n| n.checked_mul(4))
            .context("RAW 16 位图像尺寸溢出")?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(length)
            .context("RAW 16 位图像内存分配失败")?;
        pixels.resize(length, 0_u16);
        pixels
            .par_chunks_exact_mut(ow as usize * 4)
            .enumerate()
            .for_each(|(row, line)| {
                for (x, pixel) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let (developed, _, _) = develop_pixel(x, row);
                    for c in 0..3 {
                        // Preserve sensor-derived low bits before any 8-bit preview.
                        pixel[c] = (developed[c].clamp(0., 1.) * 65535.).round() as u16;
                    }
                    pixel[3] = u16::MAX;
                }
            });
        DynamicImage::ImageRgba16(
            image::ImageBuffer::from_raw(ow, oh, pixels).context("RAW 16 位图像分配大小不匹配")?,
        )
    } else {
        let mut pixels = zeroed_bytes(ow, oh, 4, "RAW 图像")?;
        pixels
            .par_chunks_exact_mut(ow as usize * 4)
            .enumerate()
            .for_each(|(row, line)| {
                for (x, pixel) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let (developed, sx, sy) = develop_pixel(x, row);
                    for c in 0..3 {
                        pixel[c] = raw_develop::quantize(developed[c], sx, sy, c);
                    }
                    pixel[3] = 255;
                }
            });
        DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(ow, oh, pixels).context("RAW 图像分配大小不匹配")?,
        )
    };
    if let Some(o) = image::metadata::Orientation::from_exif(raw.orientation.to_u16() as u8) {
        image.apply_orientation(o);
    }
    if let DynamicImage::ImageRgba16(image) = image {
        let (width, height) = image.dimensions();
        let deep = crate::deep_pixels::DeepRaster::from_u16(width, height, image.into_raw())
            .map_err(anyhow::Error::msg)?;
        let mut preview = zeroed_bytes(width, height, 4, "RAW 预览")?;
        preview
            .par_chunks_exact_mut(width as usize * 4)
            .enumerate()
            .for_each(|(y, line)| {
                for (x, pixel) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    *pixel = deep.preview_pixel(x as u32, y as u32);
                }
            });
        let mut raster = Raster::from_rgba(width, height, preview);
        raster.deep = Some(std::sync::Arc::new(deep));
        return Ok(raster);
    }
    let image = image.into_rgba8();
    Ok(Raster::from_rgba(
        image.width(),
        image.height(),
        image.into_raw(),
    ))
}

pub fn export_png(raster: &Raster, path: &Path, resolution: f64) -> Result<()> {
    size(raster.width, raster.height)?;
    ensure!(
        resolution.is_finite() && (1.0..=9600.0).contains(&resolution),
        "导出分辨率无效"
    );
    let mut encoded = if raster.deep.is_some() {
        crate::deep_io::encode_png(raster)?
    } else {
        let mut encoded = Vec::new();
        image::codecs::png::PngEncoder::new(&mut encoded).write_image(
            &raster.to_rgba(),
            raster.width,
            raster.height,
            image::ExtendedColorType::Rgba8,
        )?;
        encoded
    };
    let density = (resolution / 0.0254).round() as u32;
    let mut physical = Vec::new();
    physical.extend(density.to_be_bytes());
    physical.extend(density.to_be_bytes());
    physical.push(1);
    let chunk = png_chunk(b"pHYs", &physical);
    encoded.splice(33..33, chunk);
    encoded.splice(33..33, png_chunk(b"sRGB", &[0]));
    write_atomic(path, &encoded)
}
pub fn export_jpeg(
    raster: &Raster,
    path: &Path,
    resolution: f64,
    quality: u8,
    matte: [u8; 3],
) -> Result<()> {
    write_atomic(path, &encode_jpeg(raster, resolution, quality, matte)?)
}
pub fn encode_jpeg(
    raster: &Raster,
    resolution: f64,
    quality: u8,
    matte: [u8; 3],
) -> Result<Vec<u8>> {
    size(raster.width, raster.height)?;
    ensure!(
        resolution.is_finite() && (1.0..=9600.0).contains(&resolution),
        "导出分辨率无效"
    );
    ensure!((1..=100).contains(&quality), "JPEG 质量必须为 1–100");
    let mut rgb = vec![0; raster.width as usize * raster.height as usize * 3];
    rgb.par_chunks_exact_mut(raster.width as usize * 3)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, out) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                let p = raster.get(x as u32, y as u32);
                for c in 0..3 {
                    out[c] = ((u32::from(p[c]) * u32::from(p[3])
                        + u32::from(matte[c]) * (255 - u32::from(p[3]))
                        + 127)
                        / 255) as u8;
                }
            }
        });
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality);
    encoder.set_pixel_density(image::codecs::jpeg::PixelDensity::dpi(
        resolution.round() as u16
    ));
    encoder.encode(
        &rgb,
        raster.width,
        raster.height,
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(bytes)
}
pub fn jpeg_preview(raster: &Raster, quality: u8, matte: [u8; 3]) -> Result<(Raster, usize)> {
    let bytes = encode_jpeg(raster, 72.0, quality, matte)?;
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)?.into_rgba8();
    Ok((
        Raster::from_rgba(image.width(), image.height(), image.into_raw()),
        bytes.len(),
    ))
}
pub fn export_tiff(raster: &Raster, path: &Path) -> Result<()> {
    size(raster.width, raster.height)?;
    if raster.deep.is_some() {
        return write_atomic(path, &crate::deep_io::encode_tiff(raster)?);
    }
    let mut bytes = Cursor::new(Vec::new());
    image::codecs::tiff::TiffEncoder::new(&mut bytes).write_image(
        &raster.to_rgba(),
        raster.width,
        raster.height,
        image::ExtendedColorType::Rgba8,
    )?;
    write_atomic(path, bytes.get_ref())
}

/// Export a flattened raster using one of the formats exposed by the File > Export menu.
///
/// PNG, JPEG and TIFF keep their existing options. BMP and WebP use lossless
/// encoding. GIF uses a palette of up to 256 colors and exports one still frame.
pub fn export_bmp(raster: &Raster, path: &Path) -> Result<()> {
    size(raster.width, raster.height)?;
    let mut bytes = Vec::new();
    image::codecs::bmp::BmpEncoder::new(&mut bytes).write_image(
        &raster.to_rgba(),
        raster.width,
        raster.height,
        image::ExtendedColorType::Rgba8,
    )?;
    write_atomic(path, &bytes)
}

pub fn export_gif(raster: &Raster, path: &Path) -> Result<()> {
    size(raster.width, raster.height)?;
    let mut bytes = Vec::new();
    image::codecs::gif::GifEncoder::new(&mut bytes).write_image(
        &raster.to_rgba(),
        raster.width,
        raster.height,
        image::ExtendedColorType::Rgba8,
    )?;
    write_atomic(path, &bytes)
}

pub fn export_webp(raster: &Raster, path: &Path) -> Result<()> {
    size(raster.width, raster.height)?;
    let mut bytes = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut bytes).write_image(
        &raster.to_rgba(),
        raster.width,
        raster.height,
        image::ExtendedColorType::Rgba8,
    )?;
    write_atomic(path, &bytes)
}

/// Export using a normalized extension (`png`, `jpg`, `jpeg`, `tif`, `tiff`,
/// `bmp`, `gif`, `webp` or `exr`). This keeps GUI, CLI and Agent exports in sync.
pub fn export_raster(
    raster: &Raster,
    path: &Path,
    format: &str,
    resolution: f64,
    quality: u8,
    matte: [u8; 3],
) -> Result<()> {
    match format.to_ascii_lowercase().as_str() {
        "png" => export_png(raster, path, resolution),
        "jpg" | "jpeg" => export_jpeg(raster, path, resolution, quality, matte),
        "tif" | "tiff" => export_tiff(raster, path),
        "bmp" => export_bmp(raster, path),
        "gif" => export_gif(raster, path),
        "webp" => export_webp(raster, path),
        "exr" => {
            size(raster.width, raster.height)?;
            write_atomic(path, &crate::deep_io::encode_exr(raster)?)
        }
        "pdf" => crate::pdf_io::export_pages(
            path,
            std::iter::once(Ok((
                raster.clone(),
                crate::pdf_io::PdfPageSize {
                    width_points: f64::from(raster.width) * 72. / resolution,
                    height_points: f64::from(raster.height) * 72. / resolution,
                },
            ))),
        ),
        other => anyhow::bail!("不支持导出格式：{other}"),
    }
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend((data.len() as u32).to_be_bytes());
    out.extend(kind);
    out.extend(data);
    let mut crc = 0xffffffff_u32;
    for &b in &out[4..] {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb88320 } else { 0 };
        }
    }
    out.extend((!crc).to_be_bytes());
    out
}
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .context("导出路径没有文件名")?
        .to_string_lossy();
    let id = Uuid::new_v4();
    let stage = parent.join(format!(".{name}.{id}.tmp"));
    let written = (|| -> Result<()> {
        let mut f = File::create(&stage)?;
        f.write_all(data)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&stage);
        return Err(error);
    }
    let replaced = (|| -> Result<()> {
        if path.exists() {
            let metadata = fs::symlink_metadata(path)?;
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "导出目标必须是普通文件"
            );
        }
        replace_file(&stage, path)
    })();
    if let Err(error) = replaced {
        let _ = fs::remove_file(&stage);
        return Err(error);
    }
    Ok(())
}
#[cfg(windows)]
fn replace_file(stage: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::PCWSTR,
    };
    let stage = stage
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(stage.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}
#[cfg(not(windows))]
fn replace_file(stage: &Path, destination: &Path) -> Result<()> {
    Ok(fs::rename(stage, destination)?)
}

#[cfg(not(windows))]
fn decode_wic(_path: &Path) -> Result<Raster> {
    anyhow::bail!("导入 HEIC 需要 Windows Imaging Component 和已安装的 HEIF 编解码器")
}

#[cfg(windows)]
fn decode_wic(path: &Path) -> Result<Raster> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::{
            Foundation::{
                RPC_E_CHANGED_MODE, WINCODEC_ERR_INSUFFICIENTBUFFER, WINCODEC_ERR_PROPERTYNOTFOUND,
                WINCODEC_ERR_UNSUPPORTEDOPERATION,
            },
            Graphics::Imaging::*,
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
        },
        core::PCWSTR,
    };
    struct Apartment {
        owned: bool,
    }
    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.owned {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }
    // The worker owns a COM apartment; interfaces are released before the guard.
    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let owns_apartment = if initialized == RPC_E_CHANGED_MODE {
        // A caller may already own an STA (for example, an eframe/rfd callback).
        // COM allows WIC use from that apartment; do not tear down its lifetime.
        false
    } else {
        initialized.ok().context("初始化 Windows 图像解码器")?;
        true
    };
    let _apartment = Apartment {
        owned: owns_apartment,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let decoder = factory
            .CreateDecoderFromFilename(
                PCWSTR(wide.as_ptr()),
                None,
                windows::Win32::Foundation::GENERIC_READ,
                WICDecodeMetadataCacheOnLoad,
            )
            .context(
                "导入 HEIC 需要在 Windows 上安装 Microsoft HEIF 图像扩展及匹配的 HEVC 编解码器",
            )?;
        let frame = decoder.GetFrame(0)?;
        let (mut width, mut height) = (0, 0);
        frame.GetSize(&mut width, &mut height)?;
        size(width, height)?;
        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppRGBA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let mut data = zeroed_bytes(width, height, 4, "HEIC 图像")?;
        let mut context_count = 0;
        // WIC 10 can report INSUFFICIENTBUFFER for the probe with a zero-sized
        // array even though it has filled the required count. Built-in codecs
        // may instead report UNSUPPORTEDOPERATION or PROPERTYNOTFOUND when no
        // color context exists. All three mean that the converter should use
        // its normal pixel path; older WIC versions return S_OK here.
        if let Err(error) = frame.GetColorContexts(&mut [], &mut context_count) {
            ensure!(
                matches!(
                    error.code(),
                    WINCODEC_ERR_INSUFFICIENTBUFFER
                        | WINCODEC_ERR_UNSUPPORTEDOPERATION
                        | WINCODEC_ERR_PROPERTYNOTFOUND
                ),
                "读取 HEIC 色彩配置数量失败：{error}"
            );
        }
        ensure!(context_count <= 32, "HEIC 色彩配置数量过多");
        if context_count > 0 {
            let mut contexts = Vec::new();
            for _ in 0..context_count {
                contexts.push(Some(factory.CreateColorContext()?));
            }
            frame.GetColorContexts(&mut contexts, &mut context_count)?;
            let destination = factory.CreateColorContext()?;
            destination.InitializeFromExifColorSpace(1)?;
            let transform = factory.CreateColorTransformer()?;
            transform.Initialize(
                &frame,
                contexts[0].as_ref().context("缺少 HEIC 色彩配置")?,
                &destination,
                &GUID_WICPixelFormat32bppRGBA,
            )?;
            transform.CopyPixels(std::ptr::null(), width * 4, &mut data)?;
        } else {
            converter.CopyPixels(std::ptr::null(), width * 4, &mut data)?;
        }
        let mut image = DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(width, height, data).context("HEIC 解码尺寸不匹配")?,
        );
        if let Ok(reader) = frame.GetMetadataQueryReader() {
            use windows::Win32::System::{
                Com::StructuredStorage::{PROPVARIANT, PropVariantClear},
                Variant::VT_UI2,
            };
            for key in ["/app1/ifd/{ushort=274}", "/ifd/{ushort=274}"] {
                let wide: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
                let mut value = PROPVARIANT::default();
                if reader
                    .GetMetadataByName(PCWSTR(wide.as_ptr()), &mut value)
                    .is_ok()
                {
                    let orientation = if value.Anonymous.Anonymous.vt == VT_UI2 {
                        Some(value.Anonymous.Anonymous.Anonymous.uiVal as u8)
                    } else {
                        None
                    };
                    PropVariantClear(&mut value)?;
                    if let Some(orientation) =
                        orientation.and_then(image::metadata::Orientation::from_exif)
                    {
                        image.apply_orientation(orientation);
                        break;
                    }
                }
            }
        }
        let image = image.into_rgba8();
        Ok(Raster::from_rgba(
            image.width(),
            image.height(),
            image.into_raw(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_import_rejects_excess_components_before_sensor_allocation_and_recovers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("异常分量.dng");
        fs::write(
            &path,
            include_bytes!("../tests/fixtures/raw/unsupported-components.dng"),
        )
        .unwrap();
        let error = import(&path).err().unwrap().to_string();
        assert!(error.contains("RAW 分量数量"), "{error}");
        for fixture in [
            include_bytes!("../tests/fixtures/raw/bayer.dng").as_slice(),
            include_bytes!("../tests/fixtures/raw/linear-rgb.dng").as_slice(),
        ] {
            fs::write(&path, fixture).unwrap();
            let document = import(&path).unwrap().document;
            assert_eq!((document.width, document.height), (8, 8));
            let image = document.layers[0].image.as_ref().unwrap();
            assert_eq!(
                image.deep.as_ref().unwrap().depth,
                crate::deep_pixels::Depth::U16
            );
            assert_eq!(image.get(0, 0)[3], 255);
            assert!(image.get(0, 0)[..3].iter().any(|&v| v > 0));
        }
    }

    #[test]
    fn raw_new_parameters_run_through_bayer_and_linear_dng_decoders() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("合成传感器.dng");
        for fixture in [
            include_bytes!("../tests/fixtures/raw/bayer.dng").as_slice(),
            include_bytes!("../tests/fixtures/raw/linear-rgb.dng").as_slice(),
        ] {
            fs::write(&path, fixture).unwrap();
            for settings in [
                RawDevelopSettings::default(),
                RawDevelopSettings {
                    noise_reduction: 1.,
                    ..Default::default()
                },
                RawDevelopSettings {
                    highlight_recovery: true,
                    ..Default::default()
                },
                RawDevelopSettings {
                    noise_reduction: 0.5,
                    highlight_recovery: true,
                    ..Default::default()
                },
            ] {
                let actual = develop_raw(&path, settings, None).unwrap();
                assert_eq!((actual.width, actual.height), (8, 8));
                let deep = actual
                    .deep
                    .as_ref()
                    .expect("full RAW import keeps sensor precision");
                assert_eq!(deep.depth, crate::deep_pixels::Depth::U16);
                assert!(
                    deep.to_bits()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|pixel| pixel[..3].iter().any(|&value| value % 257 != 0)),
                    "RAW low bits must come directly from sensor development"
                );
                assert!(
                    actual
                        .to_rgba()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .all(|p| p[3] == 255)
                );
                assert!(actual.to_rgba().iter().any(|&v| v > 0 && v < 255));
                assert_eq!(develop_raw(&path, settings, None).unwrap(), actual);
                let preview = develop_raw(&path, settings, Some(4)).unwrap();
                assert_eq!((preview.width, preview.height), (4, 4));
                assert!(preview.deep.is_none());
            }
        }
    }

    #[test]
    fn raw_orientation_preserves_full_sixteen_bit_samples_for_all_exif_transforms() {
        for fixture in [
            include_bytes!("../tests/fixtures/raw/bayer.dng").as_slice(),
            include_bytes!("../tests/fixtures/raw/linear-rgb.dng").as_slice(),
        ] {
            let mut raw = rawloader::decode(&mut Cursor::new(fixture)).unwrap();
            raw.crops = [0, 1, 2, 0];
            raw.orientation = rawloader::Orientation::Normal;
            let normal = develop_sensor(&raw, RawDevelopSettings::default(), None).unwrap();
            assert_eq!((normal.width, normal.height), (7, 6));
            let source = normal.deep.as_ref().unwrap();
            assert!(source.to_bits().iter().any(|&sample| sample % 257 != 0));
            for code in 1..=8 {
                raw.orientation = rawloader::Orientation::from_u16(code);
                let actual = develop_sensor(&raw, RawDevelopSettings::default(), None).unwrap();
                let deep = actual.deep.as_ref().unwrap();
                assert_eq!(deep.depth, crate::deep_pixels::Depth::U16);
                let (transpose, horizontal, vertical) = raw.orientation.to_flips();
                assert_eq!(
                    (actual.width, actual.height),
                    if transpose { (6, 7) } else { (7, 6) }
                );
                for y in 0..normal.height {
                    for x in 0..normal.width {
                        let tx = if horizontal { normal.width - x - 1 } else { x };
                        let ty = if vertical { normal.height - y - 1 } else { y };
                        let (tx, ty) = if transpose { (ty, tx) } else { (tx, ty) };
                        assert_eq!(deep.get_bits(tx, ty), source.get_bits(x, y), "EXIF {code}");
                    }
                }
                let preview = develop_sensor(&raw, RawDevelopSettings::default(), Some(8)).unwrap();
                assert_eq!(
                    (preview.width, preview.height),
                    (actual.width, actual.height)
                );
                assert!(preview.deep.is_none());
            }
        }
    }

    #[test]
    fn raw_inactive_camera_channel_and_invalid_crop_do_not_corrupt_development() {
        let mut raw = rawloader::decode(&mut Cursor::new(include_bytes!(
            "../tests/fixtures/raw/bayer.dng"
        )))
        .unwrap();
        raw.wb_coeffs[3] = f32::NAN;
        let raster = develop_sensor(&raw, RawDevelopSettings::default(), None).unwrap();
        assert!(
            raster
                .to_rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[..3].iter().any(|&v| v > 0))
        );
        raw.crops = [0, usize::MAX, 0, usize::MAX];
        assert!(develop_sensor(&raw, RawDevelopSettings::default(), None).is_err());
    }

    #[test]
    fn raw_preview_keys_cover_every_setting_and_invalid_controls_fail_before_decoding() {
        let defaults = RawDevelopSettings::default();
        for settings in [
            RawDevelopSettings {
                exposure: 1.,
                ..defaults
            },
            RawDevelopSettings {
                temperature: 6000.,
                ..defaults
            },
            RawDevelopSettings {
                tint: 1.,
                ..defaults
            },
            RawDevelopSettings {
                boost: 0.5,
                ..defaults
            },
            RawDevelopSettings {
                noise_reduction: 0.5,
                ..defaults
            },
            RawDevelopSettings {
                lens_correction: true,
                ..defaults
            },
            RawDevelopSettings {
                highlight_recovery: true,
                ..defaults
            },
        ] {
            assert_ne!(settings.key(), defaults.key());
        }
        let missing = Path::new("missing-raw-control-validation.dng");
        for settings in [
            RawDevelopSettings {
                exposure: f32::NAN,
                ..defaults
            },
            RawDevelopSettings {
                temperature: f32::INFINITY,
                ..defaults
            },
            RawDevelopSettings {
                tint: 151.,
                ..defaults
            },
            RawDevelopSettings {
                boost: -0.1,
                ..defaults
            },
            RawDevelopSettings {
                noise_reduction: f32::NAN,
                ..defaults
            },
            RawDevelopSettings {
                noise_reduction: -0.01,
                ..defaults
            },
            RawDevelopSettings {
                noise_reduction: 1.01,
                ..defaults
            },
        ] {
            let error = develop_raw_worker(missing, settings, None)
                .unwrap_err()
                .to_string();
            assert_eq!(error, "RAW 显影参数无效");
        }
        assert!(!RawDevelopSettings::supports_lens_correction());
        let error = develop_raw_worker(
            missing,
            RawDevelopSettings {
                lens_correction: true,
                ..defaults
            },
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("未提供镜头配置数据"), "{error}");
    }

    #[test]
    fn high_precision_export_import_routes_preserve_depth_and_png_metadata() {
        use crate::deep_pixels::DeepRaster;
        let directory = tempfile::tempdir().unwrap();
        let bits = vec![25001, 32769, 50001, 65535, 12345, 23456, 34567, 32768];
        let raster = Raster::from_deep(DeepRaster::from_u16(2, 1, bits.clone()).unwrap());
        for format in ["png", "tiff"] {
            let path = directory.path().join(format!("低位精度.{format}"));
            export_raster(&raster, &path, format, 300., 90, [255; 3]).unwrap();
            let actual = decode_image(&path).unwrap();
            assert_eq!(actual.deep.as_ref().unwrap().to_bits(), bits);
            if format == "png" {
                let bytes = fs::read(path).unwrap();
                assert!(bytes.windows(4).any(|v| v == b"pHYs"));
                assert!(bytes.windows(4).any(|v| v == b"sRGB"));
            }
        }
        let path = directory.path().join("浮点亮部.exr");
        let raster = Raster::from_deep(
            DeepRaster::from_f32(1, 1, crate::deep_pixels::Depth::Hdr, &[2., 4., 8., 0.5]).unwrap(),
        );
        export_raster(&raster, &path, "exr", 72., 90, [255; 3]).unwrap();
        let actual = decode_image(&path).unwrap();
        assert_eq!(
            actual.deep.unwrap().to_bits(),
            raster.deep.unwrap().to_bits()
        );
    }

    #[test]
    fn pixel_allocation_rejects_overflow_without_aborting() {
        assert!(zeroed_bytes(u32::MAX, u32::MAX, u64::MAX, "图像测试").is_err());
        assert_eq!(zeroed_bytes(2, 3, 4, "图像测试").unwrap(), [0; 24]);
    }
    #[test]
    fn unicode_paths_import_export_and_replacement_preserve_pixels() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("中文素材 图层 💡");
        fs::create_dir(&folder).unwrap();
        let path = folder.join("画布 一.png");
        let first = Raster::solid(3, 2, [20, 40, 90, 128]);
        export_png(&first, &path, 72.0).unwrap();
        assert_eq!(decode_image(&path).unwrap(), first);
        let replacement = Raster::solid(3, 2, [90, 40, 20, 255]);
        export_png(&replacement, &path, 72.0).unwrap();
        let imported = import(&path).unwrap().document;
        assert_eq!(imported.layers[0].image.as_deref().unwrap(), &replacement);
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 1);
    }
    #[cfg(windows)]
    #[test]
    fn wic_unprofiled_builtin_decoders_preserve_pixels() {
        let temp = tempfile::tempdir().unwrap();
        let expected = [19, 71, 203, 255];
        for extension in ["png", "bmp"] {
            let path = temp.path().join(format!("unprofiled.{extension}"));
            image::RgbImage::from_pixel(3, 2, image::Rgb(expected[..3].try_into().unwrap()))
                .save(&path)
                .unwrap();
            let actual = std::thread::spawn(move || decode_wic(&path))
                .join()
                .unwrap()
                .unwrap();
            assert_eq!(actual, Raster::solid(3, 2, expected));
        }
    }

    #[cfg(windows)]
    #[test]
    fn wic_reuses_the_callers_sta_without_uninitializing_it() {
        use windows::Win32::{
            Foundation::RPC_E_CHANGED_MODE,
            System::Com::{
                COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize,
            },
        };
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sta.png");
        let expected = Raster::solid(2, 3, [10, 40, 90, 128]);
        export_png(&expected, &path, 72.0).unwrap();
        let (actual, still_sta) = std::thread::spawn(move || unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
            let actual = decode_wic(&path);
            let probe = CoInitializeEx(None, COINIT_MULTITHREADED);
            // Balance whichever initialization is still active. If decode_wic
            // accidentally tore down the caller's STA, this balances the probe
            // instead and lets the assertion report the ownership violation.
            CoUninitialize();
            (actual, probe == RPC_E_CHANGED_MODE)
        })
        .join()
        .unwrap();
        assert_eq!(actual.unwrap(), expected);
        assert!(
            still_sta,
            "The caller's COM apartment must remain initialized"
        );
    }

    #[cfg(windows)]
    #[test]
    fn wic_embedded_display_p3_converts_pixels_to_srgb() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wic-display-p3.png");
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder
            .set_icc_profile(moxcms::ColorProfile::new_display_p3().encode().unwrap())
            .unwrap();
        encoder
            .write_image(&[200, 100, 50, 123], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        fs::write(&path, bytes).unwrap();
        let actual = std::thread::spawn(move || decode_wic(&path))
            .join()
            .unwrap()
            .unwrap()
            .get(0, 0);
        for (actual, expected) in actual.into_iter().zip([215, 93, 31, 123]) {
            assert!(actual.abs_diff(expected) <= 2);
        }
        assert_eq!(actual[3], 123);
    }

    #[test]
    fn png_roundtrip_preserves_straight_alpha_and_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("alpha.png");
        let r = Raster::solid(2, 3, [10, 40, 90, 128]);
        export_png(&r, &path, 300.0).unwrap();
        assert_eq!(decode_image(&path).unwrap(), r);
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.windows(4).any(|x| x == b"pHYs"));
        assert!(bytes.windows(4).any(|x| x == b"sRGB"));
    }
    #[test]
    fn embedded_display_p3_profile_converts_real_png_pixels_to_srgb() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("display-p3.png");
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder
            .set_icc_profile(moxcms::ColorProfile::new_display_p3().encode().unwrap())
            .unwrap();
        encoder
            .write_image(&[200, 100, 50, 123], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        fs::write(&path, bytes).unwrap();
        let actual = decode_image(&path).unwrap().get(0, 0);
        // Independent reference: inverse sRGB EOTF, Display P3 -> sRGB D65 matrix, sRGB OETF.
        for (a, b) in actual.into_iter().zip([215, 93, 31, 123]) {
            assert!(a.abs_diff(b) <= 2);
        }
        assert_eq!(actual[3], 123);
    }
    #[test]
    fn gray_icc_conversion_preserves_alpha_and_applies_transfer_curve() {
        let profile = moxcms::ColorProfile::new_gray_with_gamma(1.0)
            .encode()
            .unwrap();
        let source = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([128, 128, 128, 45]),
        ));
        let pixels = crate::deep_io::from_dynamic(source.clone(), Some(&profile))
            .unwrap()
            .get(0, 0);
        assert!(pixels[..3].iter().all(|p| p.abs_diff(188) <= 2));
        assert_eq!(pixels[3], 45);
        assert!(crate::deep_io::from_dynamic(source, Some(b"broken-profile")).is_err());
    }
    #[test]
    fn jpeg_uses_selected_matte() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("matte.jpg");
        export_jpeg(
            &Raster::solid(8, 8, [255, 0, 0, 0]),
            &path,
            72.0,
            100,
            [10, 100, 220],
        )
        .unwrap();
        let p = decode_image(&path).unwrap().get(0, 0);
        assert!(p[0].abs_diff(10) < 5 && p[1].abs_diff(100) < 5 && p[2].abs_diff(220) < 5);
        assert_eq!(p[3], 255);
    }
    #[test]
    fn jpeg_preview_decodes_the_actual_encoded_quality_and_byte_count() {
        let r = Raster::from_rgba(
            32,
            32,
            (0..32 * 32)
                .flat_map(|i| {
                    [
                        (i % 2 * 255) as u8,
                        ((i / 32) % 2 * 255) as u8,
                        ((i * 19) % 256) as u8,
                        255,
                    ]
                })
                .collect(),
        );
        let (low, size_low) = jpeg_preview(&r, 5, [255; 3]).unwrap();
        let (high, size_high) = jpeg_preview(&r, 95, [255; 3]).unwrap();
        assert!(size_high > size_low);
        assert_ne!(low, high);
        let bytes = encode_jpeg(&r, 72.0, 95, [255; 3]).unwrap();
        assert_eq!(bytes.len(), size_high);
        let decoded = image::load_from_memory(&bytes).unwrap().into_rgba8();
        assert_eq!(high.to_rgba(), decoded.into_raw());
    }
    #[test]
    fn svg_rasterizes_and_unpremultiplies() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.svg");
        fs::write(&path,r##"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="#ff0000" fill-opacity="0.5"/></svg>"##).unwrap();
        let p = decode_svg(&path, None).unwrap().get(0, 0);
        assert_eq!(p[0], 255);
        assert!((127..=128).contains(&p[3]));
    }
    #[test]
    fn svg_nested_groups_import_on_a_small_windows_caller_stack() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested.svg");
        std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || {
                for depth in [384, 1000, 2048] {
                    let xml = format!(
                        r##"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2">{}<rect width="2" height="2" fill="#123456"/>{}</svg>"##,
                        "<g>".repeat(depth),
                        "</g>".repeat(depth)
                    );
                    fs::write(&path, xml).unwrap();
                    let imported = import(&path);
                    if depth <= 1000 {
                        let document = imported.unwrap().document;
                        let pixel = document.layers[0].image.as_ref().unwrap().get(0, 0);
                        assert_eq!(pixel, [0x12, 0x34, 0x56, 255]);
                    } else {
                        assert!(imported.is_err());
                    }
                    let normal = path.with_extension("png");
                    export_png(&Raster::solid(2, 2, [10, 20, 30, 255]), &normal, 72.0).unwrap();
                    assert_eq!(import(&normal).unwrap().document.width, 2);
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
    #[test]
    fn tiff_transparency_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.tiff");
        let r = Raster::solid(2, 2, [12, 45, 67, 123]);
        export_tiff(&r, &path).unwrap();
        assert_eq!(decode_image(&path).unwrap(), r);
    }
    #[test]
    fn additional_export_formats_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let r = Raster::from_rgba(
            3,
            2,
            vec![
                12, 45, 67, 255, 200, 100, 30, 255, 20, 40, 60, 0, 90, 80, 70, 255, 4, 5, 6, 255,
                240, 230, 220, 255,
            ],
        );
        for format in ["bmp", "gif", "webp"] {
            let path = temp.path().join(format!("export.{format}"));
            export_raster(&r, &path, format, 72.0, 90, [255; 3]).unwrap();
            let decoded = decode_image(&path).unwrap();
            assert_eq!((decoded.width, decoded.height), (3, 2));
            assert_eq!(decoded.get(0, 0), r.get(0, 0));
        }
    }
    #[test]
    fn derivative_replace_preserves_destination_on_failure_and_overwrites_successfully() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("export.png");
        write_atomic(&path, b"old bytes").unwrap();
        write_atomic(&path, b"new bytes").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new bytes");
        let folder = temp.path().join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("keep"), b"retained").unwrap();
        assert!(write_atomic(&folder, b"bad replacement").is_err());
        assert_eq!(fs::read(folder.join("keep")).unwrap(), b"retained");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
    }
}
