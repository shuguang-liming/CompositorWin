//! High-precision codec boundaries. Documents use straight extended sRGB;
//! OpenEXR stores associated linear RGB, and integer PNG/TIFF store straight sRGB.
use crate::{
    deep_pixels::{DeepRaster, Depth},
    model::{MAX_SIDE, Raster, import_pixel_budget},
};
use anyhow::{Context, Result, ensure};
use image::{DynamicImage, ImageEncoder};
use std::io::Cursor;

pub fn from_dynamic(image: DynamicImage, profile: Option<&[u8]>) -> Result<Raster> {
    let (width, height) = (image.width(), image.height());
    let deep = matches!(
        image.color(),
        image::ColorType::L16
            | image::ColorType::La16
            | image::ColorType::Rgb16
            | image::ColorType::Rgba16
            | image::ColorType::Rgb32F
            | image::ColorType::Rgba32F
    );
    if !deep {
        let mut rgba = image.into_rgba8();
        if let Some(profile) = profile {
            convert_u8(profile, rgba.as_mut())?;
        }
        return Ok(Raster::from_rgba(width, height, rgba.into_raw()));
    }
    check_size(width, height, 3)?;
    if matches!(
        image.color(),
        image::ColorType::Rgb32F | image::ColorType::Rgba32F
    ) {
        let mut rgba = image.into_rgba32f().into_raw();
        if let Some(profile) = profile {
            ensure!(
                rgba.as_chunks::<4>().0.iter().all(|p| p[..3]
                    .iter()
                    .all(|v| v.is_finite() && (0. ..=1.).contains(v))),
                "带 ICC 的浮点图像包含超出 0–1 的颜色；当前 ICC 转换未支持该范围，请先转换为线性 sRGB OpenEXR"
            );
            let input = input_profile(profile)?;
            let transform = input.create_transform_f32(
                layout(&input)?,
                &moxcms::ColorProfile::new_srgb(),
                moxcms::Layout::Rgba,
                Default::default(),
            )?;
            let mut out = vec![0_f32; rgba.len()];
            if input.color_space == moxcms::DataColorSpace::Gray {
                let gray: Vec<_> = rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[3]])
                    .collect();
                transform.transform(&gray, &mut out)?;
            } else {
                transform.transform(&rgba, &mut out)?;
            }
            rgba = out;
        }
        return DeepRaster::from_f32(width, height, Depth::Hdr, &rgba)
            .map(Raster::from_deep)
            .map_err(anyhow::Error::msg);
    }
    let mut rgba = image.into_rgba16().into_raw();
    if let Some(profile) = profile {
        let input = input_profile(profile)?;
        let transform = input.create_transform_16bit(
            layout(&input)?,
            &moxcms::ColorProfile::new_srgb(),
            moxcms::Layout::Rgba,
            Default::default(),
        )?;
        let mut out = vec![0_u16; rgba.len()];
        if input.color_space == moxcms::DataColorSpace::Gray {
            let gray: Vec<_> = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[3]])
                .collect();
            transform.transform(&gray, &mut out)?;
        } else {
            transform.transform(&rgba, &mut out)?;
        }
        rgba = out;
    }
    DeepRaster::from_u16(width, height, rgba)
        .map(Raster::from_deep)
        .map_err(anyhow::Error::msg)
}

fn input_profile(bytes: &[u8]) -> Result<moxcms::ColorProfile> {
    moxcms::ColorProfile::new_from_slice(bytes).context("嵌入的 ICC 色彩配置无效")
}
fn layout(profile: &moxcms::ColorProfile) -> Result<moxcms::Layout> {
    match profile.color_space {
        moxcms::DataColorSpace::Rgb => Ok(moxcms::Layout::Rgba),
        moxcms::DataColorSpace::Gray => Ok(moxcms::Layout::GrayAlpha),
        _ => anyhow::bail!("高位深导入仅支持 RGB 或灰度 ICC 配置"),
    }
}
fn convert_u8(bytes: &[u8], rgba: &mut [u8]) -> Result<()> {
    let profile = input_profile(bytes)?;
    let transform = profile.create_transform_8bit(
        layout(&profile)?,
        &moxcms::ColorProfile::new_srgb(),
        moxcms::Layout::Rgba,
        Default::default(),
    )?;
    let mut out = vec![0_u8; 65536];
    for chunk in rgba.chunks_mut(65536) {
        if profile.color_space == moxcms::DataColorSpace::Gray {
            let gray: Vec<_> = chunk
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[3]])
                .collect();
            transform.transform(&gray, &mut out[..chunk.len()])?;
        } else {
            transform.transform(chunk, &mut out[..chunk.len()])?;
        }
        chunk.copy_from_slice(&out[..chunk.len()]);
    }
    Ok(())
}

pub(crate) fn check_size(width: u32, height: u32, cost: u64) -> Result<()> {
    ensure!(
        width > 0
            && height > 0
            && width <= MAX_SIDE
            && height <= MAX_SIDE
            && u64::from(width) * u64::from(height) <= import_pixel_budget() / cost,
        "高位深图像超出当前内存预算"
    );
    Ok(())
}

fn words(raster: &Raster) -> Vec<u16> {
    if let Some(deep) = &raster.deep
        && deep.depth == Depth::U16
    {
        return deep.to_bits();
    }
    (0..raster.height)
        .flat_map(|y| {
            (0..raster.width).flat_map(move |x| {
                raster
                    .get_f32(x, y)
                    .map(|v| (v.clamp(0., 1.) * 65535.).round() as u16)
            })
        })
        .collect()
}
pub fn encode_png(raster: &Raster) -> Result<Vec<u8>> {
    let words = words(raster);
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes).write_image(
        bytemuck::cast_slice(&words),
        raster.width,
        raster.height,
        image::ExtendedColorType::Rgba16,
    )?;
    Ok(bytes)
}
pub fn encode_tiff(raster: &Raster) -> Result<Vec<u8>> {
    let words = words(raster);
    let mut bytes = Cursor::new(Vec::new());
    image::codecs::tiff::TiffEncoder::new(&mut bytes).write_image(
        bytemuck::cast_slice(&words),
        raster.width,
        raster.height,
        image::ExtendedColorType::Rgba16,
    )?;
    Ok(bytes.into_inner())
}

fn linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn encoded(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1. / 2.4) - 0.055
    }
}

pub fn encode_exr(raster: &Raster) -> Result<Vec<u8>> {
    let mut values = Vec::with_capacity(raster.width as usize * raster.height as usize * 4);
    for y in 0..raster.height {
        for x in 0..raster.width {
            let p = raster.get_f32(x, y);
            values.extend([
                linear(p[0]) * p[3],
                linear(p[1]) * p[3],
                linear(p[2]) * p[3],
                p[3],
            ]);
        }
    }
    let mut bytes = Cursor::new(Vec::new());
    image::codecs::openexr::OpenExrEncoder::new(&mut bytes).write_image(
        bytemuck::cast_slice(&values),
        raster.width,
        raster.height,
        image::ExtendedColorType::Rgba32F,
    )?;
    Ok(bytes.into_inner())
}

/// `associated` follows OpenEXR's convention; Radiance RGBE has no alpha.
pub fn from_linear(image: DynamicImage, associated: bool) -> Result<Raster> {
    let (width, height) = (image.width(), image.height());
    check_size(width, height, 6)?;
    let mut values = image.into_rgba32f().into_raw();
    for p in values.as_chunks_mut::<4>().0 {
        ensure!(
            p.iter().all(|v| v.is_finite()) && (0. ..=1.).contains(&p[3]),
            "HDR 图像含非有限值或无效透明度"
        );
        for c in 0..3 {
            let v = if associated {
                if p[3] > 0. { p[c] / p[3] } else { 0. }
            } else {
                p[c]
            };
            p[c] = encoded(v);
        }
    }
    DeepRaster::from_f32(width, height, Depth::Hdr, &values)
        .map(Raster::from_deep)
        .map_err(anyhow::Error::msg)
}

pub fn decode_exr(bytes: &[u8]) -> Result<Raster> {
    decode_exr_budgeted(bytes, u64::MAX)
}
pub(crate) fn decode_exr_budgeted(bytes: &[u8], pixel_budget: u64) -> Result<Raster> {
    let mut reader =
        image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::OpenExr);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(import_pixel_budget() * 4);
    reader.limits(limits);
    let decoder = reader.into_decoder()?;
    let (width, height) = image::ImageDecoder::dimensions(&decoder);
    check_size(width, height, 6)?;
    ensure!(
        u64::from(width) * u64::from(height) <= pixel_budget,
        "项目超出栅格内存预算"
    );
    from_linear(DynamicImage::from_decoder(decoder)?, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn integer_png_and_tiff_retain_every_low_bit_and_alpha() {
        let bits: Vec<u16> = (0..256)
            .flat_map(|x| [25000 + x, 32768 + x, 50000 - x, 65535 - x])
            .collect();
        let raster = Raster::from_deep(DeepRaster::from_u16(256, 1, bits.clone()).unwrap());
        for bytes in [encode_png(&raster).unwrap(), encode_tiff(&raster).unwrap()] {
            let decoded = from_dynamic(image::load_from_memory(&bytes).unwrap(), None).unwrap();
            assert_eq!(decoded.deep.unwrap().to_bits(), bits);
        }
    }
    #[test]
    fn exr_preserves_bright_half_values_and_associated_alpha() {
        let source = DeepRaster::from_f32(
            3,
            1,
            Depth::Hdr,
            &[2., 4., 8., 1., 0.25, 1.5, 3., 0.5, -0.125, 0.125, 1., 1.],
        )
        .unwrap();
        let source_bits = source.to_bits();
        let bytes = encode_exr(&Raster::from_deep(source)).unwrap();
        let decoded = decode_exr(&bytes).unwrap();
        assert_eq!(decoded.deep.unwrap().to_bits(), source_bits);
    }
    #[test]
    fn nonfinite_hdr_and_oversized_buffers_are_rejected() {
        let image = image::Rgba32FImage::from_raw(1, 1, vec![f32::INFINITY, 0., 0., 1.]).unwrap();
        assert!(from_linear(DynamicImage::ImageRgba32F(image), false).is_err());
        assert!(check_size(MAX_SIDE, MAX_SIDE, 6).is_err());
    }
    #[test]
    fn float_icc_extended_values_are_rejected_before_silent_clipping() {
        let image = image::Rgba32FImage::from_raw(1, 1, vec![4., 2., -0.125, 0.5]).unwrap();
        // The range guard runs before profile decoding, without allocating a transform.
        let error = from_dynamic(DynamicImage::ImageRgba32F(image), Some(&[])).unwrap_err();
        assert!(error.to_string().contains("0–1"));
    }
}
