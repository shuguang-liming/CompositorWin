//! RGB editing with user-selected CMYK output profiles, conversion and soft proofing.
use crate::model::{Document, Raster};
use anyhow::{Context, Result, ensure};
use moxcms::{
    ColorProfile, DataColorSpace, Layout, ParsingOptions, ProfileClass, ProfileText,
    RenderingIntent, Transform8BitExecutor, TransformOptions,
};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

pub const MAX_PROFILE_BYTES: usize = 16 * 1024 * 1024;
pub const PROOF_DIMENSION: usize = 33;

fn integer(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap())
}

/// Validate the directory before asking the CMS to parse LUTs. Shared identical tag
/// payloads are permitted by ICC; partially overlapping payloads and duplicate tags are not.
fn validate_header(data: &[u8]) -> Result<()> {
    ensure!(
        (132..MAX_PROFILE_BYTES).contains(&data.len()),
        "ICC 文件大小无效或超过 16 MB"
    );
    ensure!(
        integer(data, 0) as usize == data.len(),
        "ICC 声明大小与实际文件长度不一致"
    );
    ensure!(&data[36..40] == b"acsp", "文件不是有效的 ICC 色彩配置");
    ensure!(&data[12..16] == b"prtr", "请选择印刷输出类别的 ICC 配置");
    ensure!(&data[16..20] == b"CMYK", "请选择 CMYK 色彩配置");
    ensure!(
        &data[20..24] == b"Lab " || &data[20..24] == b"XYZ ",
        "CMYK ICC 连接色彩空间无效"
    );
    ensure!((2..=4).contains(&data[8]), "仅支持 ICC v2 至 v4 色彩配置");
    ensure!(integer(data, 64) <= 3, "ICC 渲染意图无效");
    let count = integer(data, 128) as usize;
    ensure!(count > 0 && count <= 4096, "ICC 标签数量无效");
    let end = 132 + count * 12;
    ensure!(end <= data.len(), "ICC 标签目录被截断");
    let mut tags = HashSet::new();
    let mut ranges = Vec::with_capacity(count);
    for tag in data[132..end].as_chunks::<12>().0 {
        ensure!(tags.insert(tag[..4].to_vec()), "ICC 包含重复标签");
        let offset = integer(tag, 4) as usize;
        let length = integer(tag, 8) as usize;
        ensure!(
            offset >= end && offset.is_multiple_of(4) && length >= 8,
            "ICC 标签位置或长度无效"
        );
        let limit = offset.checked_add(length).context("ICC 标签长度溢出")?;
        ensure!(limit <= data.len(), "ICC 标签数据被截断");
        ranges.push((offset, limit));
    }
    ranges.sort_unstable();
    for pair in ranges.windows(2) {
        ensure!(
            pair[0] == pair[1] || pair[1].0 >= pair[0].1,
            "ICC 标签数据部分重叠"
        );
    }
    ensure!(
        tags.contains(b"A2B0".as_slice()) || tags.contains(b"A2B1".as_slice()),
        "CMYK ICC 缺少墨色到屏幕颜色的转换表"
    );
    ensure!(
        tags.contains(b"B2A0".as_slice()) || tags.contains(b"B2A1".as_slice()),
        "CMYK ICC 缺少屏幕颜色到墨色的转换表"
    );
    Ok(())
}

fn allocation(length: usize) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    data.try_reserve_exact(length)
        .context("CMYK 色彩转换所需内存不足")?;
    data.resize(length, 0);
    Ok(data)
}

fn dimensions(width: u32, height: u32) -> Result<usize> {
    ensure!(
        Document::valid_size(width, height),
        "CMYK 图像尺寸超出支持范围"
    );
    (width as usize)
        .checked_mul(height as usize)
        .context("CMYK 图像尺寸溢出")
}

fn profile_description(profile: &ColorProfile) -> Option<&str> {
    match profile.description.as_ref()? {
        ProfileText::PlainString(value) => Some(value),
        ProfileText::Description(value) => Some(if value.unicode_string.is_empty() {
            &value.ascii_string
        } else {
            &value.unicode_string
        }),
        ProfileText::Localizable(values) => values
            .iter()
            .find(|v| v.language == "en")
            .or_else(|| values.first())
            .map(|v| v.value.as_str()),
    }
}

#[derive(Clone)]
pub struct CmykProfile {
    name: String,
    path: Option<PathBuf>,
    data: Arc<Vec<u8>>,
    fingerprint: String,
    to_ink: Arc<Transform8BitExecutor>,
    to_rgb: Arc<Transform8BitExecutor>,
    proof_cache: Arc<OnceLock<Arc<ProofLookups>>>,
}

impl std::fmt::Debug for CmykProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CmykProfile")
            .field("name", &self.name)
            .field("fingerprint", &self.fingerprint)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// Eight-bit ink coverage: 0 means no ink and 255 means full ink, in C/M/Y/K order.
#[derive(Clone, Debug)]
pub struct CmykRaster {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub profile: CmykProfile,
}

impl CmykProfile {
    pub fn load(path: &Path) -> Result<Self> {
        let length = std::fs::metadata(path)
            .context("读取 ICC 文件信息失败")?
            .len();
        ensure!(length < MAX_PROFILE_BYTES as u64, "ICC 文件超过 16 MB");
        let mut data = Vec::new();
        std::fs::File::open(path)
            .context("打开 ICC 文件失败")?
            .take(MAX_PROFILE_BYTES as u64 + 1)
            .read_to_end(&mut data)
            .context("读取 ICC 文件失败")?;
        let mut profile = Self::from_bytes(
            path.file_stem().and_then(|v| v.to_str()).unwrap_or("CMYK"),
            data,
        )?;
        profile.path = Some(path.canonicalize().unwrap_or_else(|_| path.to_owned()));
        Ok(profile)
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        Self::load(path)
    }

    pub fn from_bytes(name: impl Into<String>, data: Vec<u8>) -> Result<Self> {
        validate_header(&data)?;
        let parsed = std::panic::catch_unwind(|| {
            ColorProfile::new_from_slice_with_options(
                &data,
                ParsingOptions {
                    max_profile_size: MAX_PROFILE_BYTES,
                    max_allowed_clut_size: 8 * 1024 * 1024,
                    ..Default::default()
                },
            )
        })
        .map_err(|_| anyhow::anyhow!("ICC 色彩配置损坏"))?
        .context("解析 ICC 色彩配置失败")?;
        ensure!(
            parsed.color_space == DataColorSpace::Cmyk
                && parsed.profile_class == ProfileClass::OutputDevice,
            "请选择 CMYK 印刷输出配置"
        );
        let options = TransformOptions {
            rendering_intent: RenderingIntent::RelativeColorimetric,
            prefer_fixed_point: false,
            ..Default::default()
        };
        // moxcms 0.8 names four-channel CMYK Layout::Rgba; Cmyka is five channels.
        let made = std::panic::catch_unwind(|| {
            let srgb = ColorProfile::new_srgb();
            let to_ink = srgb.create_transform_8bit(Layout::Rgb, &parsed, Layout::Rgba, options)?;
            let to_rgb = parsed.create_transform_8bit(Layout::Rgba, &srgb, Layout::Rgb, options)?;
            Ok::<_, moxcms::CmsError>((to_ink, to_rgb))
        })
        .map_err(|_| anyhow::anyhow!("ICC 转换表损坏"))?
        .context("该 CMYK ICC 的双向转换表暂不受支持")?;
        let mut name = name.into();
        if name.trim().is_empty() {
            name = profile_description(&parsed)
                .filter(|v| !v.trim().is_empty())
                .unwrap_or("CMYK")
                .to_owned();
        }
        let fingerprint = format!("{:x}", Sha256::digest(&data));
        Ok(Self {
            name,
            path: None,
            data: Arc::new(data),
            fingerprint,
            to_ink: made.0,
            to_rgb: made.1,
            proof_cache: Arc::new(OnceLock::new()),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
    pub fn data(&self) -> &[u8] {
        &self.data
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Flatten in sRGB first; alpha is never treated as the black-ink channel.
    pub fn convert(&self, image: &Raster, matte: [u8; 3]) -> Result<CmykRaster> {
        let count = dimensions(image.width, image.height)?;
        let source = if image.tiles.is_empty() && image.pixels.len() == count * 4 {
            Cow::Borrowed(image.pixels.as_slice())
        } else {
            Cow::Owned(image.to_rgba())
        };
        ensure!(source.len() == count * 4, "CMYK 源图像数据长度不正确");
        let mut pixels = allocation(count * 4)?;
        // Bound scratch storage to 12 KiB per Rayon job, instead of a full RGB copy.
        source
            .par_chunks(4 * 4096)
            .zip(pixels.par_chunks_mut(4 * 4096))
            .try_for_each_init(
                || vec![0; 3 * 4096],
                |scratch, (src, dst)| {
                    let rgb = &mut scratch[..src.len() / 4 * 3];
                    for (rgb, pixel) in rgb
                        .as_chunks_mut::<3>()
                        .0
                        .iter_mut()
                        .zip(src.as_chunks::<4>().0)
                    {
                        let alpha = u32::from(pixel[3]);
                        for c in 0..3 {
                            rgb[c] = ((u32::from(pixel[c]) * alpha
                                + u32::from(matte[c]) * (255 - alpha)
                                + 127)
                                / 255) as u8;
                        }
                    }
                    self.to_ink.transform(rgb, dst)
                },
            )
            .context("RGB 转 CMYK 失败")?;
        Ok(CmykRaster {
            width: image.width,
            height: image.height,
            pixels,
            profile: self.clone(),
        })
    }

    pub fn to_srgb(&self, width: u32, height: u32, inks: &[u8]) -> Result<Raster> {
        let count = dimensions(width, height)?;
        ensure!(inks.len() == count * 4, "CMYK 图像数据长度不正确");
        let mut rgba = allocation(count * 4)?;
        inks.par_chunks(4 * 4096)
            .zip(rgba.par_chunks_mut(4 * 4096))
            .try_for_each_init(
                || vec![0; 3 * 4096],
                |scratch, (src, dst)| {
                    let rgb = &mut scratch[..src.len() / 4 * 3];
                    self.to_rgb.transform(src, rgb)?;
                    for (pixel, color) in dst
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(rgb.as_chunks::<3>().0)
                    {
                        pixel[..3].copy_from_slice(color);
                        pixel[3] = 255;
                    }
                    Ok::<_, moxcms::CmsError>(())
                },
            )
            .context("CMYK 转 RGB 失败")?;
        Ok(Raster::from_rgba(width, height, rgba))
    }

    pub fn proof_lookups(&self) -> Result<Arc<ProofLookups>> {
        if let Some(known) = self.proof_cache.get() {
            return Ok(known.clone());
        }
        let n = PROOF_DIMENSION;
        let mut lattice = allocation(n * n * n * 3)?;
        for (i, pixel) in lattice.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let level = |v| ((v * 255 + (n - 1) / 2) / (n - 1)) as u8;
            pixel.copy_from_slice(&[level(i % n), level(i / n % n), level(i / n / n)]);
        }
        let mut inks = allocation(n * n * n * 4)?;
        self.to_ink
            .transform(&lattice, &mut inks)
            .context("构建 CMYK 打样转换表失败")?;
        let mut back = allocation(lattice.len())?;
        self.to_rgb
            .transform(&inks, &mut back)
            .context("构建 CMYK 打样颜色失败")?;
        let proof = back.as_chunks::<3>().0.to_vec();
        let outside = lattice
            .as_chunks::<3>()
            .0
            .iter()
            .zip(&proof)
            .map(|(a, b)| a.iter().zip(b).any(|(&a, &b)| a.abs_diff(b) > 10))
            .collect();
        let lookups = Arc::new(ProofLookups { proof, outside });
        let _ = self.proof_cache.set(lookups);
        Ok(self.proof_cache.get().unwrap().clone())
    }

    /// Display operation only: edit pixels and alpha stay in their original RGB space.
    pub fn proof(&self, image: &Raster, proof_colors: bool, gamut_warning: bool) -> Result<Raster> {
        if !proof_colors && !gamut_warning {
            return Ok(image.clone());
        }
        let lookups = self.proof_lookups()?;
        let mut rgba = image.to_rgba();
        rgba.par_chunks_exact_mut(4).for_each(|p| {
            let rgb = lookups.sample([p[0], p[1], p[2]], proof_colors, gamut_warning);
            p[..3].copy_from_slice(&rgb);
        });
        Ok(Raster::from_rgba(image.width, image.height, rgba))
    }
}

/// Compact 33³ proof table. Warning follows the original's >10 code-value roundtrip test.
pub struct ProofLookups {
    proof: Vec<[u8; 3]>,
    outside: Vec<bool>,
}

impl ProofLookups {
    pub fn sample(&self, rgb: [u8; 3], proof_colors: bool, gamut_warning: bool) -> [u8; 3] {
        let n = PROOF_DIMENSION;
        let position = rgb.map(|v| f64::from(v) * (n - 1) as f64 / 255.);
        let lower = position.map(|v| v.floor() as usize);
        let fraction = std::array::from_fn::<_, 3, _>(|c| position[c] - lower[c] as f64);
        let mut result = [0.; 3];
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    let bits = [r, g, b];
                    let point = std::array::from_fn::<_, 3, _>(|c| (lower[c] + bits[c]).min(n - 1));
                    let weight = (0..3)
                        .map(|c| {
                            if bits[c] == 0 {
                                1. - fraction[c]
                            } else {
                                fraction[c]
                            }
                        })
                        .product::<f64>();
                    let index = (point[2] * n + point[1]) * n + point[0];
                    for (c, value) in result.iter_mut().enumerate() {
                        let node = if gamut_warning && self.outside[index] {
                            127.5
                        } else if proof_colors {
                            f64::from(self.proof[index][c])
                        } else {
                            point[c] as f64 * 255. / (n - 1) as f64
                        };
                        *value += node * weight;
                    }
                }
            }
        }
        result.map(|v| v.round().clamp(0., 255.) as u8)
    }
}

/// System and optionally application-owned profiles; this does not copy or modify any files.
pub fn installed_profiles(extra: Option<&Path>) -> Vec<CmykProfile> {
    let mut folders = Vec::new();
    #[cfg(windows)]
    if let Some(windows) = std::env::var_os("SystemRoot") {
        folders.push(std::path::PathBuf::from(windows).join("System32/spool/drivers/color"));
    }
    if let Some(extra) = extra {
        folders.push(extra.to_path_buf());
    }
    let mut profiles = Vec::new();
    let mut seen = HashSet::new();
    for folder in folders {
        let Ok(files) = std::fs::read_dir(folder) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if !path
                .extension()
                .and_then(|v| v.to_str())
                .is_some_and(|v| v.eq_ignore_ascii_case("icc") || v.eq_ignore_ascii_case("icm"))
            {
                continue;
            }
            if let Ok(profile) = CmykProfile::from_path(&path)
                && seen.insert(profile.fingerprint().to_owned())
            {
                profiles.push(profile);
            }
        }
    }
    profiles.sort_by(|a, b| a.name().cmp(b.name()));
    profiles
}

#[cfg(test)]
pub(crate) fn test_profile() -> Option<CmykProfile> {
    if let Some(path) = std::env::var_os("COMPOSITOR_CMYK_TEST_PROFILE") {
        return Some(
            CmykProfile::from_path(Path::new(&path))
                .expect("test ICC must be a valid CMYK output profile"),
        );
    }
    installed_profiles(None).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_non_cmyk_and_malformed_sizes_directories_classes_and_lut_tags() {
        assert!(
            CmykProfile::from_bytes("RGB", ColorProfile::new_srgb().encode().unwrap()).is_err()
        );
        assert!(CmykProfile::from_bytes("short", vec![0; 131]).is_err());
        let Some(profile) = test_profile() else {
            eprintln!("No system CMYK test profile available");
            return;
        };
        for edit in [
            "size",
            "class",
            "space",
            "pcs",
            "signature",
            "version",
            "intent",
            "count",
            "offset",
            "unaligned",
            "missing_forward",
            "missing_reverse",
            "duplicate",
            "overlap",
            "truncated",
        ] {
            let mut data = profile.data().to_vec();
            match edit {
                "size" => data[0..4].copy_from_slice(&133u32.to_be_bytes()),
                "class" => data[12..16].copy_from_slice(b"mntr"),
                "space" => data[16..20].copy_from_slice(b"RGB "),
                "pcs" => data[20..24].copy_from_slice(b"CMYK"),
                "signature" => data[36..40].copy_from_slice(b"fake"),
                "version" => data[8] = 9,
                "intent" => data[64..68].copy_from_slice(&4u32.to_be_bytes()),
                "count" => data[128..132].copy_from_slice(&u32::MAX.to_be_bytes()),
                "offset" => data[136..140].copy_from_slice(&u32::MAX.to_be_bytes()),
                "unaligned" => {
                    let offset = integer(&data, 136) + 1;
                    data[136..140].copy_from_slice(&offset.to_be_bytes());
                }
                "missing_forward" | "missing_reverse" => {
                    let prefix = if edit == "missing_forward" {
                        b"A2B"
                    } else {
                        b"B2A"
                    };
                    let end = 132 + integer(&data, 128) as usize * 12;
                    for (i, tag) in data[132..end]
                        .as_chunks_mut::<12>()
                        .0
                        .iter_mut()
                        .enumerate()
                    {
                        if &tag[..3] == prefix {
                            tag[..3].copy_from_slice(b"zzz");
                            tag[3] = i as u8;
                        }
                    }
                }
                "duplicate" => {
                    let first = data[132..136].to_vec();
                    data[144..148].copy_from_slice(&first);
                }
                "overlap" => {
                    let first = integer(&data, 136) + 4;
                    data[148..152].copy_from_slice(&first.to_be_bytes());
                }
                _ => {
                    data.pop();
                }
            }
            assert!(CmykProfile::from_bytes(edit, data).is_err(), "{edit}");
        }
    }
    #[test]
    fn real_icc_converts_ink_and_alpha_over_paper_and_proofs_the_same_profile() {
        let Some(profile) = test_profile() else {
            eprintln!("No system CMYK test profile available");
            return;
        };
        let source = Raster::from_rgba(
            3,
            1,
            vec![0, 0, 255, 255, 20, 30, 40, 0, 128, 128, 128, 128],
        );
        let ink = profile.convert(&source, [255; 3]).unwrap();
        let white = profile
            .convert(&Raster::solid(1, 1, [255; 4]), [255; 3])
            .unwrap();
        assert_eq!(&ink.pixels[4..8], &white.pixels);
        let half = profile
            .convert(&Raster::solid(1, 1, [191, 191, 191, 255]), [255; 3])
            .unwrap();
        assert_eq!(&ink.pixels[8..12], &half.pixels);
        let restored = profile.to_srgb(3, 1, &ink.pixels).unwrap();
        assert_eq!(restored.get(0, 0)[3], 255);
        let proof = profile.proof(&source, true, false).unwrap();
        assert_eq!(proof.get(0, 0)[3], 255);
        assert_eq!(proof.get(1, 0)[3], 0);
        assert_eq!(proof.get(2, 0)[3], 128);
        assert_ne!(proof.get(0, 0), source.get(0, 0));
        let warning = profile.proof(&source, false, true).unwrap();
        assert_eq!(warning.get(0, 0), [128, 128, 128, 255]);
        assert!(Arc::ptr_eq(
            &profile.proof_lookups().unwrap(),
            &profile.proof_lookups().unwrap()
        ));
        assert!(profile.to_srgb(1, 1, &[]).is_err());
        assert_eq!(source.get(0, 0), [0, 0, 255, 255]);
    }

    #[test]
    fn sparse_tiles_and_same_name_profile_instances_keep_their_own_colors_and_luts() {
        let Some(profile) = test_profile() else {
            eprintln!("No system CMYK test profile available");
            return;
        };
        let mut image = Raster::solid(65, 65, [20, 40, 200, 255]);
        image.set(64, 64, [200, 30, 10, 128]);
        let flattened = Raster::from_rgba(65, 65, image.to_rgba());
        assert_eq!(
            profile.convert(&image, [255; 3]).unwrap().pixels,
            profile.convert(&flattened, [255; 3]).unwrap().pixels
        );
        let mut data = profile.data().to_owned();
        data[52] ^= 1;
        let other = CmykProfile::from_bytes(profile.name(), data).unwrap();
        assert_eq!(profile.name(), other.name());
        assert_ne!(profile.fingerprint(), other.fingerprint());
        assert!(!Arc::ptr_eq(
            &profile.proof_lookups().unwrap(),
            &other.proof_lookups().unwrap()
        ));
    }
}
