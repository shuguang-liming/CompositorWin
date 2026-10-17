//! PDF pages are rasterized by Windows and exported as lossless image-only PDFs.
use crate::model::{Document, Raster};
use anyhow::{Context, Result, ensure};
use flate2::{Compression, write::ZlibEncoder};
use std::{io::Write, path::Path};

const MAX_PAGES: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfPageSize {
    pub width_points: f64,
    pub height_points: f64,
}

#[derive(Clone, Debug)]
pub struct PdfInfo {
    pub pages: Vec<PdfPageSize>,
}

#[derive(Clone, Debug)]
pub struct PdfPage {
    pub raster: Raster,
    pub size: PdfPageSize,
    pub index: u32,
}

fn validate_page_size(size: PdfPageSize) -> Result<()> {
    ensure!(
        size.width_points.is_finite()
            && size.height_points.is_finite()
            && size.width_points > 0.
            && size.height_points > 0.
            && size.width_points <= 14_400.
            && size.height_points <= 14_400.,
        "PDF 页面尺寸无效或超过 200 英寸"
    );
    Ok(())
}

fn raster_dimensions(size: PdfPageSize, dpi: u32) -> Result<(u32, u32)> {
    validate_page_size(size)?;
    ensure!((72..=600).contains(&dpi), "PDF 清晰度范围为 72–600 DPI");
    let width = (size.width_points * f64::from(dpi) / 72.).ceil();
    let height = (size.height_points * f64::from(dpi) / 72.).ceil();
    ensure!(
        width <= f64::from(crate::model::MAX_SIDE) && height <= f64::from(crate::model::MAX_SIDE),
        "PDF 页面像素边长超过支持范围，请降低 DPI"
    );
    let dimensions = (width as u32, height as u32);
    ensure!(
        Document::valid_size(dimensions.0, dimensions.1),
        "PDF 页面像素尺寸过大，请降低 DPI"
    );
    Ok(dimensions)
}

fn file_byte_budget() -> usize {
    (crate::workspace_limits::workspace_budget_bytes() / 2)
        .clamp(64 * 1024 * 1024, 1024 * 1024 * 1024) as usize
}

/// Read the display bounds in PDF points (72 points per inch).
pub fn info(path: &Path) -> Result<PdfInfo> {
    #[cfg(windows)]
    {
        let path = path.to_path_buf();
        native::run(move || {
            let document = native::open(&path)?;
            native::document_info(&document)
        })
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        anyhow::bail!("PDF 页面导入需要 Windows 10 或 Windows 11")
    }
}

/// Render a caller-selected page order; page indices are zero based.
pub fn render_pages(path: &Path, indices: &[u32], dpi: u32) -> Result<Vec<PdfPage>> {
    ensure!(!indices.is_empty(), "请至少选择一页 PDF");
    ensure!(indices.len() <= MAX_PAGES, "一次选择的 PDF 页面过多");
    ensure!((72..=600).contains(&dpi), "PDF 清晰度范围为 72–600 DPI");
    #[cfg(windows)]
    {
        let path = path.to_path_buf();
        let indices = indices.to_vec();
        native::run(move || {
            let document = native::open(&path)?;
            let information = native::document_info(&document)?;
            let mut dimensions = Vec::new();
            let mut total_pixels = 0_u64;
            // Reject the entire selection before Windows creates any surfaces.
            for &index in &indices {
                let size = *information
                    .pages
                    .get(index as usize)
                    .context("PDF 页码超出范围")?;
                let (width, height) = raster_dimensions(size, dpi)?;
                total_pixels = total_pixels
                    .checked_add(u64::from(width) * u64::from(height))
                    .context("PDF 页面像素数量溢出")?;
                ensure!(
                    total_pixels <= crate::model::import_pixel_budget(),
                    "选中的 PDF 页面过大，请降低 DPI 或分批导入"
                );
                dimensions.push((index, size, width, height));
            }
            let mut pages = Vec::new();
            pages
                .try_reserve_exact(dimensions.len())
                .context("分配 PDF 页面列表失败")?;
            for (index, size, width, height) in dimensions {
                let raster = native::render(&document, index, width, height)
                    .with_context(|| format!("读取 PDF 第 {} 页", index + 1))?;
                pages.push(PdfPage {
                    raster,
                    size,
                    index,
                });
            }
            Ok(pages)
        })
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        anyhow::bail!("PDF 页面导入需要 Windows 10 或 Windows 11")
    }
}

struct Bytes {
    data: Vec<u8>,
    limit: usize,
}

impl Bytes {
    fn new(limit: usize) -> Self {
        Self {
            data: Vec::new(),
            limit,
        }
    }
}

impl Write for Bytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let length = self
            .data
            .len()
            .checked_add(bytes.len())
            .filter(|&n| n <= self.limit)
            .ok_or_else(|| std::io::Error::other("PDF 导出数据超出内存限制，请分批导出"))?;
        self.data
            .try_reserve(length - self.data.len())
            .map_err(std::io::Error::other)?;
        self.data.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn object(output: &mut Bytes, offsets: &mut Vec<usize>, id: usize, bytes: &[u8]) -> Result<()> {
    if offsets.len() <= id {
        offsets.resize(id + 1, 0);
    }
    offsets[id] = output.data.len();
    writeln!(output, "{id} 0 obj")?;
    output.write_all(bytes)?;
    output.write_all(b"\nendobj\n")?;
    Ok(())
}

/// Export supplied flattened pages in order; transparent pixels are composited on white.
/// Only the current page's scanline and compressed stream are retained during encoding.
pub fn export_pages(
    path: &Path,
    pages: impl IntoIterator<Item = Result<(Raster, PdfPageSize)>>,
) -> Result<()> {
    let limit = file_byte_budget();
    let mut output = Bytes::new(limit);
    output.write_all(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")?;
    let mut offsets = vec![0; 3];
    object(
        &mut output,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>",
    )?;
    let mut page_ids = Vec::new();
    for (index, page) in pages.into_iter().enumerate() {
        ensure!(index < MAX_PAGES, "导出的 PDF 页面过多");
        let (raster, size) = page?;
        validate_page_size(size)?;
        ensure!(
            Document::valid_size(raster.width, raster.height),
            "PDF 导出图像尺寸无效"
        );
        let id = 3 + index * 3;
        let content_id = id + 1;
        let image_id = id + 2;
        let mut compressed = ZlibEncoder::new(
            Bytes::new(limit.saturating_sub(output.data.len())),
            Compression::default(),
        );
        let mut row = Vec::new();
        row.try_reserve_exact(raster.width as usize * 3)
            .context("分配 PDF 导出行缓冲区失败")?;
        row.resize(raster.width as usize * 3, 0);
        for y in 0..raster.height {
            for x in 0..raster.width {
                let pixel = raster.get(x, y);
                let alpha = u32::from(pixel[3]);
                let start = x as usize * 3;
                for channel in 0..3 {
                    row[start + channel] =
                        ((u32::from(pixel[channel]) * alpha + 255 * (255 - alpha) + 127) / 255)
                            as u8;
                }
            }
            compressed.write_all(&row)?;
        }
        let compressed = compressed.finish()?.data;
        let page_dictionary = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.6} {:.6}] /Resources << /XObject << /Image {image_id} 0 R >> >> /Contents {content_id} 0 R >>",
            size.width_points, size.height_points
        );
        object(&mut output, &mut offsets, id, page_dictionary.as_bytes())?;
        let commands = format!(
            "q\n{:.6} 0 0 {:.6} 0 0 cm\n/Image Do\nQ\n",
            size.width_points, size.height_points
        );
        let content = format!(
            "<< /Length {} >>\nstream\n{}endstream",
            commands.len(),
            commands
        );
        object(&mut output, &mut offsets, content_id, content.as_bytes())?;
        offsets.resize(image_id + 1, 0);
        offsets[image_id] = output.data.len();
        write!(
            output,
            "{image_id} 0 obj\n<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Interpolate false /Filter /FlateDecode /Length {} >>\nstream\n",
            raster.width,
            raster.height,
            compressed.len()
        )?;
        output.write_all(&compressed)?;
        output.write_all(b"\nendstream\nendobj\n")?;
        page_ids.push(id);
    }
    ensure!(!page_ids.is_empty(), "没有可导出的 PDF 页面");
    let kids = page_ids
        .iter()
        .map(|id| format!("{id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    object(
        &mut output,
        &mut offsets,
        2,
        format!(
            "<< /Type /Pages /Count {} /Kids [{kids}] >>",
            page_ids.len()
        )
        .as_bytes(),
    )?;
    let xref = output.data.len();
    write!(output, "xref\n0 {}\n0000000000 65535 f \n", offsets.len())?;
    for offset in offsets.iter().skip(1) {
        writeln!(output, "{offset:010} 00000 n ")?;
    }
    write!(
        output,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        offsets.len()
    )?;
    crate::image_io::write_atomic(path, &output.data)
}

#[cfg(windows)]
mod native {
    use super::*;
    use image::ImageDecoder;
    use std::io::Cursor;
    use windows::{
        Data::Pdf::{PdfDocument, PdfPageRenderOptions},
        Graphics::Imaging::BitmapEncoder,
        Storage::{
            StorageFile,
            Streams::{DataReader, InMemoryRandomAccessStream},
        },
        Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
        core::HSTRING,
    };

    pub(super) fn run<T: Send + 'static>(
        operation: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        std::thread::Builder::new()
            .name("pdf-decode".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                struct Apartment;
                impl Drop for Apartment {
                    fn drop(&mut self) {
                        unsafe {
                            RoUninitialize();
                        }
                    }
                }
                // All WinRT interfaces are released before this private MTA is uninitialized.
                unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
                    .context("初始化 Windows PDF 服务失败")?;
                let _apartment = Apartment;
                operation()
            })
            .context("启动 PDF 解码线程失败")?
            .join()
            .map_err(|_| anyhow::anyhow!("Windows PDF 解码器处理文件时异常"))?
    }

    pub(super) fn open(path: &Path) -> Result<PdfDocument> {
        let metadata = std::fs::metadata(path).context("读取 PDF 文件失败")?;
        ensure!(
            metadata.is_file() && metadata.len() > 0,
            "PDF 文件为空或不是普通文件"
        );
        ensure!(
            metadata.len() <= file_byte_budget() as u64,
            "PDF 文件过大，请拆分后再打开"
        );
        // Canonicalized and temporary paths may contain the `\\?\` extended
        // prefix. WinRT's StorageFile path APIs reject that prefix
        // with the misleading `UNABLE_TO_MASK_PATH` error, so retain a normal
        // absolute DOS path for the WinRT boundary.
        let path = std::fs::canonicalize(path).context("解析 PDF 绝对路径失败")?;
        let raw_path = path
            .to_str()
            .context("PDF 路径包含无法传给 Windows PDF 服务的字符")?;
        let normalized = raw_path.replace('/', "\\");
        let raw_path = normalized.as_str();
        let path_text = raw_path
            .strip_prefix(r"\\?\UNC\")
            .map(|unc| format!(r"\\{unc}"))
            .or_else(|| raw_path.strip_prefix(r"\\?\").map(str::to_owned))
            .unwrap_or_else(|| raw_path.to_owned());
        let hpath = HSTRING::from(path_text.as_str());
        ensure!(
            hpath.to_string_lossy() == path_text,
            "PDF 路径编码失败：{path_text}"
        );
        let file = StorageFile::GetFileFromPathAsync(&hpath)
            .with_context(|| format!("打开 PDF 文件失败：{path_text}"))?
            .join()
            .with_context(|| format!("读取 PDF 文件失败：{path_text}"))?;
        PdfDocument::LoadFromFileAsync(&file)
            .context("创建 Windows PDF 读取任务失败")?
            .join()
            .context("PDF 打开失败：文件可能损坏、受密码保护或系统 PDF 服务不可用")
    }

    pub(super) fn document_info(document: &PdfDocument) -> Result<PdfInfo> {
        let count = document.PageCount().context("读取 PDF 页数失败")?;
        ensure!(
            count > 0 && count as usize <= MAX_PAGES,
            "PDF 没有页面或页数超过支持范围"
        );
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(count as usize)
            .context("分配 PDF 页面信息失败")?;
        for index in 0..count {
            let page = document.GetPage(index).context("读取 PDF 页面信息失败")?;
            let result = page.Size();
            let _ = page.Close();
            let size = result.context("读取 PDF 页面尺寸失败")?;
            // Windows.Data.Pdf reports display pixels at 96 DPI, not PDF points.
            let size = PdfPageSize {
                width_points: f64::from(size.Width) * 72. / 96.,
                height_points: f64::from(size.Height) * 72. / 96.,
            };
            validate_page_size(size)?;
            pages.push(size);
        }
        Ok(PdfInfo { pages })
    }

    pub(super) fn render(
        document: &PdfDocument,
        index: u32,
        width: u32,
        height: u32,
    ) -> Result<Raster> {
        let page = document.GetPage(index)?;
        let result = (|| -> Result<Raster> {
            let options = PdfPageRenderOptions::new()?;
            options.SetDestinationWidth(width)?;
            options.SetDestinationHeight(height)?;
            options.SetBackgroundColor(windows::UI::Color {
                A: 255,
                R: 255,
                G: 255,
                B: 255,
            })?;
            options.SetIsIgnoringHighContrast(true)?;
            options.SetBitmapEncoderId(BitmapEncoder::PngEncoderId()?)?;
            let stream = InMemoryRandomAccessStream::new()?;
            page.RenderWithOptionsToStreamAsync(&stream, &options)?
                .join()?;
            let length = stream.Size()?;
            ensure!(
                length > 0 && length <= file_byte_budget() as u64 && length <= u64::from(u32::MAX),
                "PDF 渲染图像过大或为空"
            );
            let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
            let count = reader.LoadAsync(length as u32)?.join()?;
            ensure!(u64::from(count) == length, "PDF 页面图像读取不完整");
            let mut encoded = Vec::new();
            encoded
                .try_reserve_exact(count as usize)
                .context("分配 PDF 页面图像缓冲区失败")?;
            encoded.resize(count as usize, 0);
            reader.ReadBytes(&mut encoded)?;
            let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(encoded))?;
            ensure!(
                decoder.dimensions() == (width, height),
                "PDF 渲染尺寸与请求尺寸不一致"
            );
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(width);
            limits.max_image_height = Some(height);
            limits.max_alloc = Some(u64::from(width) * u64::from(height) * 8);
            decoder.set_limits(limits)?;
            let image = image::DynamicImage::from_decoder(decoder)?.into_rgba8();
            Ok(Raster::from_rgba(width, height, image.into_raw()))
        })();
        let _ = page.Close();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn page_dimensions_validate_dpi_and_memory_bounds() {
        let size = PdfPageSize {
            width_points: 612.,
            height_points: 792.,
        };
        assert_eq!(raster_dimensions(size, 72).unwrap(), (612, 792));
        assert_eq!(raster_dimensions(size, 300).unwrap(), (2550, 3300));
        assert!(raster_dimensions(size, 0).is_err());
        assert!(
            raster_dimensions(
                PdfPageSize {
                    width_points: f64::NAN,
                    ..size
                },
                300
            )
            .is_err()
        );
        assert!(
            raster_dimensions(
                PdfPageSize {
                    width_points: 14_400.,
                    ..size
                },
                600
            )
            .is_err()
        );
    }

    #[test]
    fn lossless_pdf_stream_and_xref_keep_page_order_and_alpha_matte() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("多页 导出.pdf");
        let size = PdfPageSize {
            width_points: 144.,
            height_points: 72.,
        };
        let raster = Raster::from_rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 128]);
        export_pages(
            &path,
            [
                Ok((raster, size)),
                Ok((Raster::solid(1, 1, [0, 255, 0, 255]), size)),
            ],
        )
        .unwrap();
        let bytes = std::fs::read(path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Count 2 /Kids [3 0 R 6 0 R]"));
        assert!(text.contains("/MediaBox [0 0 144.000000 72.000000]"));
        let marker = b"/Filter /FlateDecode /Length ";
        let position = bytes
            .windows(marker.len())
            .position(|part| part == marker)
            .unwrap();
        let stream = position
            + bytes[position..]
                .windows(7)
                .position(|part| part == b"stream\n")
                .unwrap()
            + 7;
        let end = stream
            + bytes[stream..]
                .windows(10)
                .position(|part| part == b"\nendstream")
                .unwrap();
        let mut decoded = Vec::new();
        flate2::read::ZlibDecoder::new(&bytes[stream..end])
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, [255, 0, 0, 127, 127, 127]);
        let xref = bytes
            .windows(6)
            .position(|part| part == b"\nxref\n")
            .unwrap()
            + 1;
        assert!(text.contains(&format!("startxref\n{xref}\n")));
        let xref_text = std::str::from_utf8(&bytes[xref..]).unwrap();
        for (id, line) in xref_text.lines().skip(3).take(8).enumerate() {
            let offset: usize = line[..10].parse().unwrap();
            assert!(bytes[offset..].starts_with(format!("{} 0 obj\n", id + 1).as_bytes()));
        }
    }

    #[test]
    fn invalid_export_preserves_existing_target() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing.pdf");
        std::fs::write(&path, b"preserve").unwrap();
        assert!(export_pages(&path, std::iter::empty()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"preserve");
        let size = PdfPageSize {
            width_points: 72.,
            height_points: 72.,
        };
        assert!(
            export_pages(
                &path,
                [
                    Ok((Raster::solid(1, 1, [255; 4]), size)),
                    Err(anyhow::anyhow!("page failed"))
                ]
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), b"preserve");
    }

    #[cfg(windows)]
    #[test]
    fn windows_pdf_opens_unicode_spaces_forward_and_extended_paths() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("中文 PDF 路径");
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("颜色 页面.pdf");
        let size = PdfPageSize {
            width_points: 144.,
            height_points: 72.,
        };
        export_pages(&path, [Ok((Raster::solid(2, 1, [255, 0, 0, 255]), size))]).unwrap();
        let forward = std::path::PathBuf::from(path.to_string_lossy().replace('\\', "/"));
        let extended = path.canonicalize().unwrap();
        for candidate in [&path, &forward, &extended] {
            let info = info(candidate).unwrap_or_else(|error| panic!("{candidate:?}: {error:#}"));
            assert_eq!(info.pages.len(), 1);
            assert!((info.pages[0].width_points - 144.).abs() < 0.01);
            let pages = render_pages(candidate, &[0], 72).unwrap();
            assert_eq!((pages[0].raster.width, pages[0].raster.height), (144, 72));
            let pixel = pages[0].raster.get(72, 36);
            assert!(
                pixel[0] > 240 && pixel[1] < 15 && pixel[2] < 15,
                "{candidate:?}: {pixel:?}"
            );
        }
    }
}
