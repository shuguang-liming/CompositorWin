//! Profile-tagged, four-channel CMYK output. Editing and compositing remain RGB.
use crate::{
    color_management::{CmykProfile, CmykRaster},
    model::Raster,
};
use anyhow::{Context, Result, bail, ensure};
use flate2::{Compression, write::ZlibEncoder};
use std::{
    borrow::Cow,
    io::{Cursor, Write},
    path::Path,
};

/// Print exports flatten transparency onto white in RGB before the ICC conversion.
pub fn export(
    image: &Raster,
    path: &Path,
    format: &str,
    profile: &CmykProfile,
    resolution: f64,
    quality: u8,
) -> Result<()> {
    crate::image_io::write_atomic(path, &encode(image, format, profile, resolution, quality)?)
}

pub fn encode(
    image: &Raster,
    format: &str,
    profile: &CmykProfile,
    resolution: f64,
    quality: u8,
) -> Result<Vec<u8>> {
    ensure!(
        resolution.is_finite() && (1.0..=65535.0).contains(&resolution),
        "印刷分辨率需在 1 至 65535 DPI 之间"
    );
    let format = format.trim_start_matches('.').to_ascii_lowercase();
    ensure!(
        matches!(format.as_str(), "jpg" | "jpeg" | "tif" | "tiff" | "pdf"),
        "CMYK 印刷导出仅支持 JPEG、TIFF 和 PDF"
    );
    if matches!(format.as_str(), "jpg" | "jpeg") {
        ensure!((1..=100).contains(&quality), "JPEG 质量需在 1 至 100 之间");
        ensure!(
            image.width <= u16::MAX as u32 && image.height <= u16::MAX as u32,
            "JPEG 图像宽高超过 65535 像素"
        );
    }
    if format == "pdf" {
        ensure!(
            f64::from(image.width) * 72. / resolution <= 14_400.
                && f64::from(image.height) * 72. / resolution <= 14_400.,
            "PDF 页面超过 200 英寸，请提高导出 DPI"
        );
    }
    let inks = profile.convert(image, [255; 3])?;
    match format.as_str() {
        "jpg" | "jpeg" => jpeg(&inks, resolution, quality),
        "tif" | "tiff" => tiff(&inks, resolution),
        "pdf" => pdf(&inks, resolution),
        _ => bail!("不支持的 CMYK 印刷格式"),
    }
}

fn jpeg(inks: &CmykRaster, resolution: f64, quality: u8) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut bytes, quality);
    encoder.set_density(jpeg_encoder::PixelDensity::dpi(resolution.round() as u16));
    encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_1_1);
    encoder
        .add_icc_profile(inks.profile.data())
        .context("JPEG 嵌入 CMYK ICC 失败")?;
    // This encoder performs the Adobe CMYK inversion and writes APP14 itself.
    encoder
        .encode(
            &inks.pixels,
            inks.width as u16,
            inks.height as u16,
            jpeg_encoder::ColorType::Cmyk,
        )
        .context("编码 CMYK JPEG 失败")?;
    Ok(bytes)
}

struct IccTag<'a>(&'a [u8]);
impl tiff::encoder::TiffValue for IccTag<'_> {
    const BYTE_LEN: u8 = 1;
    const FIELD_TYPE: tiff::tags::Type = tiff::tags::Type::UNDEFINED;
    fn count(&self) -> usize {
        self.0.len()
    }
    fn data(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self.0)
    }
}

fn tiff(inks: &CmykRaster, resolution: f64) -> Result<Vec<u8>> {
    use tiff::{
        encoder::{Compression, DeflateLevel, Rational, TiffEncoder, colortype::CMYK8},
        tags::{ResolutionUnit, Tag},
    };
    let mut bytes = Vec::new();
    let mut encoder = TiffEncoder::new(Cursor::new(&mut bytes))?
        .with_compression(Compression::Deflate(DeflateLevel::Balanced));
    let mut image = encoder.new_image::<CMYK8>(inks.width, inks.height)?;
    image.resolution(
        ResolutionUnit::Inch,
        Rational {
            n: (resolution * 1000.).round() as u32,
            d: 1000,
        },
    );
    image
        .encoder()
        .write_tag(Tag::Unknown(34675), IccTag(inks.profile.data()))?;
    image.encoder().write_tag(Tag::Unknown(332), 1u16)?;
    image.encoder().write_tag(Tag::Unknown(334), 4u16)?;
    image
        .write_data(&inks.pixels)
        .context("编码 CMYK TIFF 失败")?;
    Ok(bytes)
}

fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

/// A single raster PDF page with an explicit four-component ICCBased color space.
/// This is an ordinary PDF, not a PDF/X conformance claim.
fn pdf(inks: &CmykRaster, resolution: f64) -> Result<Vec<u8>> {
    let width = f64::from(inks.width) * 72. / resolution;
    let height = f64::from(inks.height) * 72. / resolution;
    let pixels = deflate(&inks.pixels)?;
    let profile = deflate(inks.profile.data())?;
    let content = format!("q\n{width:.8} 0 0 {height:.8} 0 0 cm\n/Im0 Do\nQ\n");
    let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = vec![0];
    fn object(out: &mut Vec<u8>, offsets: &mut Vec<usize>, data: &[u8]) -> Result<()> {
        offsets.push(out.len());
        writeln!(out, "{} 0 obj", offsets.len() - 1)?;
        out.extend_from_slice(data);
        out.extend_from_slice(b"\nendobj\n");
        Ok(())
    }
    fn stream(
        out: &mut Vec<u8>,
        offsets: &mut Vec<usize>,
        attributes: &str,
        data: &[u8],
    ) -> Result<()> {
        offsets.push(out.len());
        writeln!(
            out,
            "{} 0 obj\n<< {attributes} /Length {} >>\nstream",
            offsets.len() - 1,
            data.len()
        )?;
        out.extend_from_slice(data);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        Ok(())
    }
    object(&mut out, &mut offsets, b"<< /Type /Catalog /Pages 2 0 R >>")?;
    object(
        &mut out,
        &mut offsets,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    )?;
    object(&mut out,&mut offsets,format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width:.8} {height:.8}] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 6 0 R >>").as_bytes())?;
    stream(
        &mut out,
        &mut offsets,
        &format!(
            "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace [/ICCBased 5 0 R] /Intent /RelativeColorimetric /BitsPerComponent 8 /Filter /FlateDecode /Decode [0 1 0 1 0 1 0 1]",
            inks.width, inks.height
        ),
        &pixels,
    )?;
    stream(
        &mut out,
        &mut offsets,
        "/N 4 /Alternate /DeviceCMYK /Filter /FlateDecode",
        &profile,
    )?;
    stream(&mut out, &mut offsets, "", content.as_bytes())?;
    let xref = out.len();
    writeln!(out, "xref\n0 {}\n0000000000 65535 f ", offsets.len())?;
    for offset in &offsets[1..] {
        writeln!(out, "{offset:010} 00000 n ")?;
    }
    write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        offsets.len()
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_management::test_profile;
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    fn source() -> Raster {
        Raster::from_rgba(
            4,
            1,
            vec![
                0, 0, 255, 255, 255, 0, 0, 255, 255, 255, 255, 255, 20, 30, 40, 0,
            ],
        )
    }

    #[test]
    fn cmyk_tiff_is_four_inks_with_exact_profile_resolution_and_paper_white() {
        use tiff::{
            decoder::{Decoder, DecodingResult},
            tags::Tag,
        };
        let Some(profile) = test_profile() else {
            eprintln!("No system CMYK test profile available");
            return;
        };
        let image = source();
        let data = encode(&image, "tiff", &profile, 300., 95).unwrap();
        let mut reader = Decoder::new(Cursor::new(&data)).unwrap();
        assert_eq!(reader.dimensions().unwrap(), (4, 1));
        assert_eq!(
            reader.get_tag_u32(Tag::PhotometricInterpretation).unwrap(),
            5
        );
        assert_eq!(reader.get_tag_u32(Tag::SamplesPerPixel).unwrap(), 4);
        assert_eq!(
            reader.get_tag_u8_vec(Tag::Unknown(34675)).unwrap(),
            profile.data()
        );
        assert_eq!(reader.get_tag_u32(Tag::Unknown(332)).unwrap(), 1);
        assert_eq!(reader.get_tag_u32(Tag::Unknown(334)).unwrap(), 4);
        assert!(reader.get_tag(Tag::ExtraSamples).is_err());
        let DecodingResult::U8(actual) = reader.read_image().unwrap() else {
            panic!("TIFF must contain eight-bit inks")
        };
        let expected = profile.convert(&image, [255; 3]).unwrap();
        assert_eq!(actual, expected.pixels);
        assert_eq!(&actual[8..12], &actual[12..16]);
        let ifd = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let count = u16::from_le_bytes(data[ifd..ifd + 2].try_into().unwrap()) as usize;
        let tag = data[ifd + 2..ifd + 2 + 12 * count]
            .as_chunks::<12>()
            .0
            .iter()
            .find(|v| u16::from_le_bytes(v[..2].try_into().unwrap()) == 34675)
            .unwrap();
        assert_eq!(
            u16::from_le_bytes(tag[2..4].try_into().unwrap()),
            7,
            "ICC must be UNDEFINED"
        );
    }

    #[test]
    fn cmyk_jpeg_has_four_components_and_reassembles_the_original_icc() {
        let Some(profile) = test_profile() else {
            eprintln!("No system CMYK test profile available");
            return;
        };
        let data = encode(&source(), "jpeg", &profile, 300., 98).unwrap();
        assert_eq!(&data[..2], b"\xff\xd8");
        let mut offset = 2;
        let mut chunks = Vec::new();
        let mut sof = false;
        let mut adobe = false;
        while offset + 4 <= data.len() {
            assert_eq!(data[offset], 255);
            let marker = data[offset + 1];
            let length =
                u16::from_be_bytes(data[offset + 2..offset + 4].try_into().unwrap()) as usize;
            let payload = &data[offset + 4..offset + 2 + length];
            if marker == 0xe2 && payload.starts_with(b"ICC_PROFILE\0") {
                chunks.push((payload[12], payload[13], payload[14..].to_vec()));
            }
            if marker == 0xee && payload.starts_with(b"Adobe") {
                adobe = true;
                assert_eq!(payload[11], 0);
            }
            if marker == 0xc0 {
                sof = true;
                assert_eq!(payload[5], 4);
            }
            if marker == 0xda {
                break;
            }
            offset += length + 2;
        }
        chunks.sort_by_key(|v| v.0);
        assert!(!chunks.is_empty());
        for (index, (sequence, total, _)) in chunks.iter().enumerate() {
            assert_eq!(*sequence as usize, index + 1);
            assert_eq!(*total as usize, chunks.len());
        }
        assert_eq!(
            chunks.into_iter().flat_map(|v| v.2).collect::<Vec<_>>(),
            profile.data()
        );
        assert!(sof && adobe);
    }

    #[test]
    fn cmyk_pdf_embeds_four_component_icc_and_true_inks_in_a_valid_cross_reference() {
        let Some(profile) = test_profile() else {
            eprintln!("No system CMYK test profile available");
            return;
        };
        let data = encode(&source(), "pdf", &profile, 300., 95).unwrap();
        let text = String::from_utf8_lossy(&data);
        assert!(text.contains("/ColorSpace [/ICCBased 5 0 R]"));
        assert!(text.contains("/N 4 /Alternate /DeviceCMYK"));
        assert!(text.contains("/MediaBox [0 0 0.96000000 0.24000000]"));
        fn stream(data: &[u8], object: usize) -> Vec<u8> {
            let prefix = format!("{object} 0 obj\n");
            let start = data
                .windows(prefix.len())
                .position(|v| v == prefix.as_bytes())
                .unwrap();
            let length_start = start
                + data[start..]
                    .windows(8)
                    .position(|v| v == b"/Length ")
                    .unwrap()
                + 8;
            let length_end = length_start
                + data[length_start..]
                    .iter()
                    .position(|v| !v.is_ascii_digit())
                    .unwrap();
            let length = std::str::from_utf8(&data[length_start..length_end])
                .unwrap()
                .parse::<usize>()
                .unwrap();
            let payload = start
                + data[start..]
                    .windows(7)
                    .position(|v| v == b"stream\n")
                    .unwrap()
                + 7;
            let mut output = Vec::new();
            ZlibDecoder::new(&data[payload..payload + length])
                .read_to_end(&mut output)
                .unwrap();
            output
        }
        assert_eq!(stream(&data, 5), profile.data());
        assert_eq!(
            stream(&data, 4),
            profile.convert(&source(), [255; 3]).unwrap().pixels
        );
        let start = text.rfind("startxref\n").unwrap() + 10;
        let xref = text[start..]
            .lines()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert_eq!(&data[xref..xref + 5], b"xref\n");
    }

    #[test]
    fn rejects_invalid_print_settings_before_export() {
        let Some(profile) = test_profile() else {
            eprintln!("No system CMYK test profile available");
            return;
        };
        for resolution in [f64::NAN, f64::INFINITY, 0., -1., 65536.] {
            assert!(encode(&source(), "tiff", &profile, resolution, 95).is_err());
        }
        assert!(encode(&source(), "png", &profile, 300., 95).is_err());
        assert!(encode(&source(), "jpeg", &profile, 300., 0).is_err());
    }
}
