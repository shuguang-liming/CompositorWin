//! Single-file projects, legacy directory compatibility, and recoverable replacement.
use crate::model::{
    BlendMode, Document, Guide, Layer, MAX_PIXELS, MAX_SIDE, Mask, Raster, Transform,
    document_pixel_budget,
};
use anyhow::{Context, Result, ensure};
use image::{ImageEncoder, ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Cursor, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

const MANIFEST_LIMIT: u64 = 4 * 1024 * 1024;
const ASSET_LIMIT: u64 = 512 * 1024 * 1024;
const CURRENT_VERSION: u32 = 11;
const MAX_READ_VERSION: u32 = 14;
const CONTAINER_MAGIC: &[u8; 8] = b"COMP\r\n\x1a\n";
const CONTAINER_VERSION: u32 = 1;
const CONTAINER_HEADER_SIZE: u64 = 64;
const CONTAINER_INDEX_LIMIT: u64 = 8 * 1024 * 1024;
const CONTAINER_ENTRY_LIMIT: usize = 20_001;
// The aggregate limit follows the existing 10,000-layer and per-asset limits.
const CONTAINER_LIMIT: u64 = CONTAINER_HEADER_SIZE
    + MANIFEST_LIMIT
    + ASSET_LIMIT * (CONTAINER_ENTRY_LIMIT as u64 - 1)
    + CONTAINER_INDEX_LIMIT;

#[derive(Debug)]
pub struct ProjectAccessError {
    pub code: &'static str,
    message: String,
}
impl std::fmt::Display for ProjectAccessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for ProjectAccessError {}

fn access_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    ProjectAccessError {
        code,
        message: message.into(),
    }
    .into()
}

fn project_lock_key(path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut clean = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if clean.file_name().is_some() {
                    clean.pop();
                }
            }
            other => clean.push(other.as_os_str()),
        }
    }
    // Resolve the existing prefix so a new target and its later existing path share one key.
    let mut prefix = clean;
    let mut suffix = Vec::new();
    while !prefix.exists() {
        suffix.push(
            prefix
                .file_name()
                .context("项目锁路径没有有效父目录")?
                .to_owned(),
        );
        ensure!(prefix.pop(), "项目锁路径没有有效父目录");
    }
    let mut resolved = prefix.canonicalize()?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    #[cfg(windows)]
    let normalized = resolved.to_string_lossy().to_lowercase();
    #[cfg(not(windows))]
    let normalized = resolved.to_string_lossy().into_owned();
    let digest = Sha256::digest(normalized.as_bytes());
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn same_project_path(left: &Path, right: &Path) -> Result<bool> {
    Ok(project_lock_key(left)? == project_lock_key(right)?)
}

#[cfg(windows)]
mod project_mutex {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub(super) fn CreateMutexW(
            attributes: *const c_void,
            initial_owner: i32,
            name: *const u16,
        ) -> *mut c_void;
        pub(super) fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        pub(super) fn ReleaseMutex(handle: *mut c_void) -> i32;
        pub(super) fn CloseHandle(handle: *mut c_void) -> i32;
    }
}

#[cfg(windows)]
struct ProjectLock(*mut std::ffi::c_void);
#[cfg(windows)]
impl ProjectLock {
    fn acquire(path: &Path) -> Result<Self> {
        let name: Vec<u16> = format!("Local\\Compositor.Project.{}", project_lock_key(path)?)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // The OS owns this named object. An abandoned mutex is acquired and then recovered normally.
        let handle = unsafe { project_mutex::CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let status = unsafe { project_mutex::WaitForSingleObject(handle, 0) };
        match status {
            0 | 0x80 => Ok(Self(handle)),
            other => {
                let error = if other == 0x102 {
                    access_error(
                        "project_busy",
                        "项目正由其他线程或进程读取/保存，请稍后重试",
                    )
                } else {
                    std::io::Error::last_os_error().into()
                };
                unsafe {
                    project_mutex::CloseHandle(handle);
                }
                Err(error)
            }
        }
    }
}
#[cfg(windows)]
impl Drop for ProjectLock {
    fn drop(&mut self) {
        unsafe {
            project_mutex::ReleaseMutex(self.0);
            project_mutex::CloseHandle(self.0);
        }
    }
}

#[cfg(not(windows))]
struct ProjectLock {
    _file: File,
}
#[cfg(not(windows))]
impl ProjectLock {
    fn acquire(path: &Path) -> Result<Self> {
        let directory = std::env::temp_dir().join("compositor-project-locks");
        fs::create_dir_all(&directory)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(format!("{}.lock", project_lock_key(path)?)))?;
        file.try_lock().map_err(|error| {
            access_error("project_busy", format!("项目正由其他进程使用：{error}"))
        })?;
        Ok(Self { _file: file })
    }
}
fn default_opacity() -> f64 {
    1.0
}
fn default_resolution() -> f64 {
    72.0
}
fn default_visible() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    format: String,
    version: u32,
    color_space: String,
    #[serde(rename = "documentID", alias = "documentId")]
    document_id: Uuid,
    width: u32,
    height: u32,
    #[serde(default = "default_resolution")]
    resolution: f64,
    #[serde(
        rename = "activeLayerID",
        alias = "activeLayerId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    active_layer_id: Option<Uuid>,
    layers: Vec<LayerRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    guides: Vec<Guide>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayerRecord {
    id: Uuid,
    name: String,
    #[serde(default = "default_visible")]
    is_visible: bool,
    transform: Transform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_file: Option<String>,
    #[serde(
        rename = "parentID",
        alias = "parentId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    parent_id: Option<Uuid>,
    #[serde(default)]
    is_group: bool,
    #[serde(default = "default_opacity")]
    opacity: f64,
    #[serde(default)]
    blend_mode: BlendMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask_enabled: Option<bool>,
    #[serde(
        rename = "maskSourceID",
        alias = "maskSourceId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    mask_source_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask_placement: Option<Transform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask_linked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adjustment: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effects: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shape: Option<Value>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

fn id_name(id: Uuid, mask: bool) -> String {
    format!(
        "{}{}.png",
        id.to_string().to_uppercase(),
        if mask { ".mask" } else { "" }
    )
}
fn image_name(layer: &Layer) -> String {
    if layer
        .image
        .as_ref()
        .and_then(|r| r.deep.as_ref())
        .is_some_and(|r| r.depth == crate::deep_pixels::Depth::Hdr)
    {
        format!("{}.exr", layer.id.to_string().to_uppercase())
    } else {
        id_name(layer.id, false)
    }
}
fn is_plain_metadata(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        !metadata.file_type().is_symlink() && metadata.file_attributes() & 0x400 == 0
    }
    #[cfg(not(windows))]
    {
        !metadata.file_type().is_symlink()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContainerEntry {
    path: String,
    offset: u64,
    length: u64,
    sha256: [u8; 32],
}

struct ContainerReader {
    file: File,
    length: u64,
    entries: HashMap<String, ContainerEntry>,
}

impl ContainerReader {
    fn open(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_file() && is_plain_metadata(&metadata),
            "单文件项目必须是普通文件"
        );
        ensure!(
            (CONTAINER_HEADER_SIZE..=CONTAINER_LIMIT).contains(&metadata.len()),
            "单文件项目长度无效"
        );
        let mut file = File::open(path)?;
        let mut header = [0; CONTAINER_HEADER_SIZE as usize];
        file.read_exact(&mut header).context("单文件项目头被截断")?;
        ensure!(
            &header[..8] == CONTAINER_MAGIC,
            "这不是 Compositor 单文件项目"
        );
        let version = u32::from_le_bytes(header[8..12].try_into()?);
        ensure!(
            version == CONTAINER_VERSION,
            "不支持单文件容器版本 {version}；支持的版本为 {CONTAINER_VERSION}"
        );
        let count = u32::from_le_bytes(header[12..16].try_into()?) as usize;
        ensure!(
            (1..=CONTAINER_ENTRY_LIMIT).contains(&count),
            "项目容器条目数无效"
        );
        let index_offset = u64::from_le_bytes(header[16..24].try_into()?);
        let index_length = u64::from_le_bytes(header[24..32].try_into()?);
        ensure!(
            index_offset >= CONTAINER_HEADER_SIZE
                && (1..=CONTAINER_INDEX_LIMIT).contains(&index_length)
                && index_offset.checked_add(index_length) == Some(metadata.len()),
            "项目容器索引长度或偏移无效"
        );
        file.seek(SeekFrom::Start(index_offset))?;
        let mut index = vec![0; index_length as usize];
        file.read_exact(&mut index).context("项目容器索引被截断")?;
        let index_digest: [u8; 32] = Sha256::digest(&index).into();
        ensure!(
            index_digest.as_slice() == &header[32..64],
            "项目容器索引校验失败"
        );
        let records: Vec<ContainerEntry> =
            serde_json::from_slice(&index).context("项目容器索引已损坏")?;
        ensure!(records.len() == count, "项目容器条目数不匹配");
        let mut entries = HashMap::with_capacity(count);
        let mut offset = CONTAINER_HEADER_SIZE;
        for entry in records {
            let limit = if entry.path == "manifest.json" {
                MANIFEST_LIMIT
            } else {
                let name = entry
                    .path
                    .strip_prefix("images/")
                    .context("项目容器资产路径无效")?;
                let (id, mask) = if let Some(id) = name.strip_suffix(".mask.png") {
                    (id, true)
                } else {
                    (
                        name.strip_suffix(".png")
                            .or_else(|| name.strip_suffix(".exr"))
                            .context("项目容器资产名称无效")?,
                        false,
                    )
                };
                let id = Uuid::parse_str(id).context("项目容器资产 ID 无效")?;
                ensure!(
                    name == id_name(id, mask)
                        || (!mask && name == format!("{}.exr", id.to_string().to_uppercase())),
                    "项目容器资产名称无效"
                );
                ASSET_LIMIT
            };
            ensure!(
                entry.offset == offset && (1..=limit).contains(&entry.length),
                "项目容器资产重叠、有空洞或超出大小限制"
            );
            offset = offset
                .checked_add(entry.length)
                .context("项目容器资产长度溢出")?;
            ensure!(offset <= index_offset, "项目容器资产超出数据区域");
            ensure!(
                entries.insert(entry.path.clone(), entry).is_none(),
                "项目容器资产路径重复"
            );
        }
        ensure!(offset == index_offset, "项目容器含未索引的数据");
        ensure!(entries.contains_key("manifest.json"), "项目容器缺少清单");
        Ok(Self {
            file,
            length: metadata.len(),
            entries,
        })
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let entry = self.entries.get(path).context("项目容器缺少引用的资产")?;
        self.file.seek(SeekFrom::Start(entry.offset))?;
        let mut data = Vec::new();
        data.try_reserve_exact(entry.length as usize)
            .context("项目资产内存分配失败")?;
        data.resize(entry.length as usize, 0);
        self.file
            .read_exact(&mut data)
            .context("项目容器资产被截断")?;
        ensure!(
            <[u8; 32]>::from(Sha256::digest(&data)) == entry.sha256,
            "项目容器资产校验失败：{path}"
        );
        ensure!(
            self.file.metadata()?.len() == self.length,
            "读取时项目文件长度发生变化"
        );
        Ok(data)
    }

    fn validate_references(&self, manifest: &Manifest) -> Result<()> {
        let mut referenced = HashSet::from(["manifest.json".to_owned()]);
        for name in manifest.layers.iter().flat_map(|layer| {
            [layer.image_file.as_ref(), layer.mask_file.as_ref()]
                .into_iter()
                .flatten()
        }) {
            referenced.insert(format!("images/{name}"));
        }
        ensure!(
            referenced.len() == self.entries.len()
                && referenced
                    .iter()
                    .all(|name| self.entries.contains_key(name)),
            "项目容器资产与清单引用不一致"
        );
        Ok(())
    }
}

enum ProjectSource {
    Directory { root: PathBuf },
    File(ContainerReader),
}
impl ProjectSource {
    fn open(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(is_plain_metadata(&metadata), "项目路径不应为链接或重解析点");
        if metadata.is_dir() {
            Ok(Self::Directory {
                root: path.canonicalize()?,
            })
        } else {
            Ok(Self::File(ContainerReader::open(path)?))
        }
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        match self {
            Self::Directory { root } => checked_file(
                root,
                &root.join(path),
                if path == "manifest.json" {
                    MANIFEST_LIMIT
                } else {
                    ASSET_LIMIT
                },
            ),
            Self::File(reader) => reader.read(path),
        }
    }
}

fn checked_file(root: &Path, path: &Path, limit: u64) -> Result<Vec<u8>> {
    let meta =
        fs::symlink_metadata(path).with_context(|| format!("缺少项目包文件 {}", path.display()))?;
    ensure!(
        meta.is_file() && is_plain_metadata(&meta),
        "项目资产必须是普通文件"
    );
    ensure!(meta.len() <= limit, "项目包文件超出大小限制");
    ensure!(
        path.canonicalize()?.starts_with(root),
        "项目文件路径超出项目包目录"
    );
    let mut f = File::open(path)?;
    let mut data = Vec::with_capacity(meta.len() as usize);
    Read::by_ref(&mut f)
        .take(limit + 1)
        .read_to_end(&mut data)?;
    ensure!(data.len() as u64 <= limit, "项目包文件增大后超出大小限制");
    Ok(data)
}
fn decode_png(data: Vec<u8>, mask: bool, budget: &mut u64) -> Result<Raster> {
    ensure!(
        data.len() >= 33
            && data[..8] == [137, 80, 78, 71, 13, 10, 26, 10]
            && &data[12..16] == b"IHDR",
        "嵌入资产不是 PNG 文件"
    );
    let width = u32::from_be_bytes(data[16..20].try_into()?);
    let height = u32::from_be_bytes(data[20..24].try_into()?);
    ensure!(data[24] <= 16, "项目 PNG 位深超出 16-bit");
    ensure!(
        !mask || (data[24] == 8 && data[25] == 0),
        "图层蒙版必须是无 Alpha 通道的 8-bit 灰度 PNG"
    );
    ensure!(
        width > 0 && height > 0 && width <= MAX_SIDE && height <= MAX_SIDE,
        "嵌入图像尺寸无效"
    );
    let area = u64::from(width) * u64::from(height);
    let cost = if data[24] == 16 { 3 } else { 1 };
    ensure!(area * cost <= *budget, "项目超出栅格内存预算");
    *budget -= area * cost;
    // APNG frames would otherwise be silently discarded by the raster decoder.
    let mut offset = 8;
    while offset + 12 <= data.len() {
        let size = u32::from_be_bytes(data[offset..offset + 4].try_into()?) as usize;
        let end = offset
            .checked_add(size)
            .and_then(|x| x.checked_add(12))
            .context("PNG 数据块长度溢出")?;
        ensure!(end <= data.len(), "PNG 数据块被截断");
        ensure!(
            &data[offset + 4..offset + 8] != b"acTL",
            "不支持项目中的动画 PNG"
        );
        offset = end;
    }
    let mut reader = ImageReader::with_format(Cursor::new(data), ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(area * 16 + 1024 * 1024);
    reader.limits(limits);
    crate::deep_io::from_dynamic(reader.decode()?, None)
}

pub fn load(path: &Path) -> Result<Document> {
    let _lock = ProjectLock::acquire(path)?;
    load_unlocked(path)
}

fn load_unlocked(path: &Path) -> Result<Document> {
    recover_unlocked(path)?;
    load_contents(path)
}

fn load_contents(path: &Path) -> Result<Document> {
    let mut source = ProjectSource::open(path)?;
    let metadata = source.read("manifest.json")?;
    let header: Value = serde_json::from_slice(&metadata)?;
    ensure!(
        header["format"] == "com.compositor.project",
        "这不是 Compositor 项目"
    );
    let version = header["version"].as_u64().context("缺少项目版本")?;
    ensure!(
        (1..=u64::from(MAX_READ_VERSION)).contains(&version),
        "不支持项目格式版本 {version}；最高支持版本为 {MAX_READ_VERSION}"
    );
    let mut manifest: Manifest = serde_json::from_slice(&metadata).context("项目清单已损坏")?;
    for layer in &mut manifest.layers {
        if let Some(adjustment) = &mut layer.adjustment {
            normalize_hsv_maps(adjustment)?;
        }
    }
    validate_manifest(&manifest)?;
    if let ProjectSource::File(reader) = &source {
        reader.validate_references(&manifest)?;
    }
    let mut image_budget = document_pixel_budget();
    let mut mask_budget = document_pixel_budget();
    let mut layers = Vec::with_capacity(manifest.layers.len());
    for r in manifest.layers {
        let image = r
            .image_file
            .as_ref()
            .map(|name| {
                source
                    .read(&format!("images/{name}"))
                    .and_then(|data| {
                        if name.ends_with(".exr") {
                            let raster =
                                crate::deep_io::decode_exr_budgeted(&data, image_budget / 3)?;
                            let cost = u64::from(raster.width) * u64::from(raster.height) * 3;
                            ensure!(cost <= image_budget, "项目超出栅格内存预算");
                            image_budget -= cost;
                            Ok(raster)
                        } else {
                            ensure!(
                                manifest.version >= 12 || data.get(24) != Some(&16),
                                "16-bit 图层要求项目版本至少为 12"
                            );
                            decode_png(data, false, &mut image_budget)
                        }
                    })
                    .map(Arc::new)
            })
            .transpose()?;
        let mask = r
            .mask_file
            .as_ref()
            .map(|name| {
                source
                    .read(&format!("images/{name}"))
                    .and_then(|data| decode_png(data, true, &mut mask_budget))
                    .map(|raster| Mask {
                        raster: Arc::new(raster),
                        enabled: r.mask_enabled.unwrap_or(true),
                        linked: r.mask_linked.unwrap_or(true),
                        placement: r.mask_placement,
                    })
            })
            .transpose()?;
        layers.push(Layer {
            id: r.id,
            name: r.name,
            visible: r.is_visible,
            parent_id: r.parent_id,
            is_group: r.is_group,
            opacity: r.opacity,
            blend_mode: r.blend_mode,
            transform: r.transform,
            image,
            mask,
            mask_source_id: r.mask_source_id,
            adjustment: r.adjustment,
            effects: r.effects,
            text: r.text,
            shape: r.shape,
            extra: r.extra,
        });
    }
    Ok(Document {
        id: manifest.document_id,
        width: manifest.width,
        height: manifest.height,
        resolution: manifest.resolution,
        active_layer_id: manifest.active_layer_id,
        layers,
        guides: manifest.guides,
        selection: None,
        extra: manifest.extra,
    })
}
fn manifest(document: &Document) -> Manifest {
    Manifest {
        format: "com.compositor.project".to_owned(),
        version: if document.layers.iter().any(|layer| {
            layer
                .shape
                .as_ref()
                .is_some_and(|style| style["smoothing"] == true)
        }) {
            14
        } else {
            document
                .layers
                .iter()
                .filter_map(|layer| layer.image.as_ref()?.deep.as_ref())
                .map(|deep| {
                    if deep.depth == crate::deep_pixels::Depth::Hdr {
                        13
                    } else {
                        12
                    }
                })
                .max()
                .unwrap_or(CURRENT_VERSION)
        },
        color_space: "sRGB".to_owned(),
        document_id: document.id,
        width: document.width,
        height: document.height,
        resolution: document.resolution,
        active_layer_id: document.active_layer_id,
        guides: document.guides.clone(),
        extra: document.extra.clone(),
        layers: document
            .layers
            .iter()
            .map(|l| LayerRecord {
                id: l.id,
                name: l.name.clone(),
                is_visible: l.visible,
                transform: l.transform,
                image_file: l.image.as_ref().map(|_| image_name(l)),
                parent_id: l.parent_id,
                is_group: l.is_group,
                opacity: l.opacity,
                blend_mode: l.blend_mode,
                mask_file: l.mask.as_ref().map(|_| id_name(l.id, true)),
                mask_enabled: l.mask.as_ref().map(|m| m.enabled),
                mask_linked: l.mask.as_ref().map(|m| m.linked),
                mask_placement: l.mask.as_ref().and_then(|m| m.placement),
                mask_source_id: l.mask_source_id,
                adjustment: l.adjustment.as_ref().map(compatible_adjustment),
                effects: l.effects.clone(),
                text: l.text.clone(),
                shape: l.shape.clone(),
                extra: l.extra.clone(),
            })
            .collect(),
    }
}
pub fn validate_document(document: &Document) -> Result<()> {
    validate_manifest(&manifest(document))?;
    let mut images = 0;
    let mut masks = 0;
    for layer in &document.layers {
        for (is_mask, r) in [
            (false, layer.image.as_ref()),
            (true, layer.mask.as_ref().map(|m| &m.raster)),
        ] {
            if let Some(r) = r {
                if let Some(deep) = &r.deep {
                    ensure!(
                        deep.width == r.width && deep.height == r.height,
                        "高位深像素尺寸与预览不一致"
                    );
                    deep.validate().map_err(anyhow::Error::msg)?;
                }
                ensure!(
                    r.width > 0 && r.height > 0 && r.width <= MAX_SIDE && r.height <= MAX_SIDE,
                    "栅格尺寸无效"
                );
                let pixels = u64::from(r.width) * u64::from(r.height);
                let used = if is_mask { &mut masks } else { &mut images };
                *used += pixels * if r.deep.is_some() { 3 } else { 1 };
                ensure!(*used <= document_pixel_budget(), "超出文档像素预算");
                ensure!(
                    r.pixels.is_empty()
                        || r.pixels.len() == 4
                        || r.pixels.len() == pixels as usize * 4,
                    "栅格存储长度无效"
                );
                for (&(tx, ty), tile) in &r.tiles {
                    ensure!(
                        tx * crate::model::TILE_SIZE < r.width
                            && ty * crate::model::TILE_SIZE < r.height
                            && tile.len()
                                == (crate::model::TILE_SIZE * crate::model::TILE_SIZE * 4) as usize,
                        "栅格编辑块无效"
                    );
                }
                if is_mask {
                    ensure!(r.deep.is_none(), "图层蒙版只支持 8-bit");
                    for y in 0..r.height {
                        for x in 0..r.width {
                            let p = r.get(x, y);
                            ensure!(
                                p[0] == p[1] && p[1] == p[2] && p[3] == 255,
                                "蒙版像素必须是不透明灰度"
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn save(document: &Document, path: &Path) -> Result<()> {
    let _lock = ProjectLock::acquire(path)?;
    save_unlocked(document, path, || Ok(()))
}

struct HashingWriter<'a> {
    file: &'a mut File,
    hash: Sha256,
    length: u64,
    limit: u64,
}
impl Write for HashingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.length) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "编码项目资产超过大小限制",
            ));
        }
        let written = self.file.write(bytes)?;
        self.hash.update(&bytes[..written]);
        self.length += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn write_container_entry(
    file: &mut File,
    entries: &mut Vec<ContainerEntry>,
    path: String,
    limit: u64,
    write: impl FnOnce(&mut HashingWriter<'_>) -> Result<()>,
) -> Result<()> {
    let offset = file.stream_position()?;
    let mut writer = HashingWriter {
        file,
        hash: Sha256::new(),
        length: 0,
        limit,
    };
    write(&mut writer)?;
    ensure!(writer.length > 0, "项目容器资产为空");
    entries.push(ContainerEntry {
        path,
        offset,
        length: writer.length,
        sha256: writer.hash.finalize().into(),
    });
    Ok(())
}

fn write_container(document: &Document, metadata: &[u8], path: &Path) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&[0; CONTAINER_HEADER_SIZE as usize])?;
    let mut entries = Vec::with_capacity(document.layers.len() * 2 + 1);
    write_container_entry(
        &mut file,
        &mut entries,
        "manifest.json".into(),
        MANIFEST_LIMIT,
        |writer| {
            writer.write_all(metadata)?;
            Ok(())
        },
    )?;
    for layer in &document.layers {
        if let Some(image) = &layer.image {
            write_container_entry(
                &mut file,
                &mut entries,
                format!("images/{}", image_name(layer)),
                ASSET_LIMIT,
                |writer| {
                    if image
                        .deep
                        .as_ref()
                        .is_some_and(|deep| deep.depth == crate::deep_pixels::Depth::Hdr)
                    {
                        writer.write_all(&crate::deep_io::encode_exr(image)?)?;
                        Ok(())
                    } else {
                        encode_png(image, writer, false)
                    }
                },
            )?;
        }
        if let Some(mask) = &layer.mask {
            write_container_entry(
                &mut file,
                &mut entries,
                format!("images/{}", id_name(layer.id, true)),
                ASSET_LIMIT,
                |writer| encode_png(&mask.raster, writer, true),
            )?;
        }
    }
    let index = serde_json::to_vec(&entries)?;
    ensure!(
        index.len() as u64 <= CONTAINER_INDEX_LIMIT,
        "项目容器索引过大"
    );
    let index_offset = file.stream_position()?;
    file.write_all(&index)?;
    let mut header = Vec::with_capacity(CONTAINER_HEADER_SIZE as usize);
    header.extend_from_slice(CONTAINER_MAGIC);
    header.extend_from_slice(&CONTAINER_VERSION.to_le_bytes());
    header.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    header.extend_from_slice(&index_offset.to_le_bytes());
    header.extend_from_slice(&(index.len() as u64).to_le_bytes());
    header.extend_from_slice(&Sha256::digest(&index));
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header)?;
    file.sync_all()?;
    Ok(())
}

fn check_save_target(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(is_plain_metadata(&metadata), "保存目标不应为链接或重解析点");
            if metadata.is_dir() {
                return Err(access_error(
                    "directory_project_save_as",
                    "原项目是旧版目录包，请另存为新的 .comp 单文件；原目录会保留",
                ));
            }
            ensure!(metadata.is_file(), "保存目标不是普通文件");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Check the destination and replace it under the same cross-process lock.
/// The returned fingerprint belongs to this save, even if another writer starts next.
pub fn save_checked(
    document: &Document,
    path: &Path,
    expected_fingerprint: Option<&str>,
    overwrite: bool,
) -> Result<[u8; 32]> {
    save_checked_observed(document, path, expected_fingerprint, overwrite, || ())
        .map(|(digest, ())| digest)
}

pub(crate) fn save_checked_observed<T>(
    document: &Document,
    path: &Path,
    expected_fingerprint: Option<&str>,
    overwrite: bool,
    observe: impl FnOnce() -> T,
) -> Result<([u8; 32], T)> {
    let _lock = ProjectLock::acquire(path)?;
    recover_unlocked(path)?;
    check_save_preconditions(path, expected_fingerprint, overwrite)?;
    save_unlocked(document, path, || {
        check_save_preconditions(path, expected_fingerprint, overwrite)
    })?;
    Ok((fingerprint_unlocked(path)?, observe()))
}

fn check_save_preconditions(
    path: &Path,
    expected_fingerprint: Option<&str>,
    overwrite: bool,
) -> Result<()> {
    check_save_target(path)?;
    if path.exists() {
        if !overwrite {
            return Err(access_error(
                "already_exists",
                "保存目标已存在，请明确允许覆盖",
            ));
        }
        if let Some(expected) = expected_fingerprint {
            let actual = fingerprint_unlocked(path)?;
            let actual: String = actual.iter().map(|byte| format!("{byte:02x}")).collect();
            if !expected.eq_ignore_ascii_case(&actual) {
                return Err(access_error(
                    "conflict",
                    "目标项目已被其他操作修改，请重新检查后再保存",
                ));
            }
        }
    } else if expected_fingerprint.is_some() {
        return Err(access_error(
            "conflict",
            "预期项目已删除或指纹对应的路径不存在",
        ));
    }
    Ok(())
}

fn save_unlocked(
    document: &Document,
    path: &Path,
    before_commit: impl FnOnce() -> Result<()>,
) -> Result<()> {
    validate_document(document)?;
    let data = serde_json::to_vec_pretty(&manifest(document))?;
    ensure!(data.len() as u64 <= MANIFEST_LIMIT, "项目清单超过 4 MiB");
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    recover_unlocked(path)?;
    check_save_target(path)?;
    let name = path
        .file_name()
        .context("项目路径没有文件名")?
        .to_string_lossy();
    let token = Uuid::new_v4();
    let stage = parent.join(format!(".{name}.{token}.staging"));
    let backup = parent.join(format!(".{name}.{token}.backup"));
    if let Err(error) = write_container(document, &data, &stage) {
        let _ = fs::remove_file(&stage);
        return Err(error);
    }
    // A writer outside Compositor may have created the destination while encoding.
    if let Err(error) = check_save_target(path).and_then(|()| before_commit()) {
        let _ = fs::remove_file(&stage);
        return Err(error);
    }
    let journal = SaveJournal {
        target: name.to_string(),
        stage: stage.file_name().unwrap().to_string_lossy().into_owned(),
        backup: backup.file_name().unwrap().to_string_lossy().into_owned(),
        container_version: Some(CONTAINER_VERSION),
    };
    if let Err(error) = write_journal(path, &journal) {
        let _ = fs::remove_file(&stage);
        return Err(error);
    }
    let had_previous = path.exists();
    if had_previous && let Err(error) = fs::rename(path, &backup) {
        let _ = fs::remove_file(&stage);
        let _ = fs::remove_file(journal_path(path)?);
        return Err(error.into());
    }
    if let Err(error) = fs::rename(&stage, path) {
        if had_previous {
            fs::rename(&backup, path).with_context(|| {
                format!("保存失败且恢复失败；原项目保留在 {}", backup.display())
            })?;
        }
        let _ = fs::remove_file(&stage);
        let _ = fs::remove_file(journal_path(path)?);
        return Err(error.into());
    }
    if had_previous {
        fs::remove_file(&backup)
            .with_context(|| format!("项目已保存；旧备份仍保留在 {}", backup.display()))?;
    }
    fs::remove_file(journal_path(path)?)?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct SaveJournal {
    target: String,
    stage: String,
    backup: String,
    #[serde(
        default,
        rename = "containerVersion",
        skip_serializing_if = "Option::is_none"
    )]
    container_version: Option<u32>,
}
fn journal_path(path: &Path) -> Result<std::path::PathBuf> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .context("项目没有文件名")?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.compositor-save.json")))
}
fn write_journal(path: &Path, journal: &SaveJournal) -> Result<()> {
    let dest = journal_path(path)?;
    let temporary = dest.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let bytes = serde_json::to_vec(journal)?;
    let written = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &dest)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// Recover an interrupted replacement. If the old destination disappeared,
/// restore its backup; if replacement completed, keep it and remove obsolete staging data.
pub fn recover(path: &Path) -> Result<()> {
    let _lock = ProjectLock::acquire(path)?;
    recover_unlocked(path)
}

fn recover_unlocked(path: &Path) -> Result<()> {
    let journal_file = journal_path(path)?;
    match fs::symlink_metadata(&journal_file) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let root = parent.canonicalize()?;
    let bytes = checked_file(&root, &journal_file, 4096)?;
    let journal: SaveJournal = serde_json::from_slice(&bytes).context("项目恢复日志已损坏")?;
    ensure!(
        journal
            .container_version
            .is_none_or(|version| version == CONTAINER_VERSION),
        "项目恢复日志的容器版本不受支持"
    );
    let single_file = journal.container_version.is_some();
    let filename = path.file_name().context("恢复目标无效")?.to_string_lossy();
    ensure!(
        journal_component_matches(&journal.target, &filename),
        "恢复目标不匹配"
    );
    let prefix = format!(".{filename}.");
    for (name, suffix) in [(&journal.stage, ".staging"), (&journal.backup, ".backup")] {
        ensure!(
            Path::new(name).components().count() == 1
                && name
                    .get(..prefix.len())
                    .is_some_and(|start| journal_component_matches(start, &prefix))
                && name.ends_with(suffix),
            "恢复日志路径不安全"
        );
    }
    let stage = parent.join(journal.stage);
    let backup = parent.join(journal.backup);
    for candidate in [stage.as_path(), backup.as_path(), path] {
        match fs::symlink_metadata(candidate) {
            Ok(meta) => {
                ensure!(
                    (if single_file {
                        meta.is_file()
                    } else {
                        meta.is_dir()
                    }) && is_plain_metadata(&meta)
                        && candidate.canonicalize()?.starts_with(&root),
                    "恢复项目路径或类型不安全"
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if !path.exists() {
        if backup.exists() {
            load_contents(&backup).context("中断后的原项目备份不完整，保留恢复文件")?;
            fs::rename(&backup, path).context("中断后恢复原保存项目包")?;
        } else if stage.exists() {
            load_contents(&stage).context("暂存项目不完整，保留恢复文件")?;
            fs::rename(&stage, path).context("中断后完成首次项目保存")?;
        } else {
            anyhow::bail!("恢复日志存在，但原项目包和暂存项目包均已缺失");
        }
    }
    // Never discard the previous version until the replacement can actually be opened.
    load_contents(path).context("恢复目标已损坏，保留旧备份和日志")?;
    if stage.exists() {
        if single_file {
            fs::remove_file(stage)?;
        } else {
            fs::remove_dir_all(stage)?;
        }
    }
    if backup.exists() {
        if single_file {
            fs::remove_file(backup)?;
        } else {
            fs::remove_dir_all(backup)?;
        }
    }
    fs::remove_file(journal_file)?;
    Ok(())
}

fn journal_component_matches(left: &str, right: &str) -> bool {
    #[cfg(windows)]
    {
        left.eq_ignore_ascii_case(right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}
fn encode_png(r: &Raster, writer: impl Write, mask: bool) -> Result<()> {
    if !mask && r.deep.is_some() {
        let mut writer = writer;
        writer.write_all(&crate::deep_io::encode_png(r)?)?;
        return Ok(());
    }
    let encoder = image::codecs::png::PngEncoder::new(writer);
    if mask {
        let mut gray = Vec::with_capacity((u64::from(r.width) * u64::from(r.height)) as usize);
        for y in 0..r.height {
            for x in 0..r.width {
                gray.push(r.get(x, y)[0]);
            }
        }
        encoder.write_image(&gray, r.width, r.height, image::ExtendedColorType::L8)?;
    } else {
        encoder.write_image(
            &r.to_rgba(),
            r.width,
            r.height,
            image::ExtendedColorType::Rgba8,
        )?;
    }
    Ok(())
}

#[cfg(test)]
fn write_png(r: &Raster, path: &Path, mask: bool) -> Result<()> {
    let mut file = File::create(path)?;
    encode_png(r, &mut file, mask)?;
    file.sync_all()?;
    ensure!(
        file.metadata()?.len() <= ASSET_LIMIT,
        "编码 PNG 超过 512 MiB"
    );
    Ok(())
}

/// Full content fingerprint, including equal-size PNG edits. Run on the file worker.
pub fn fingerprint(path: &Path) -> Result<[u8; 32]> {
    let _lock = ProjectLock::acquire(path)?;
    recover_unlocked(path)?;
    fingerprint_unlocked(path)
}

fn fingerprint_unlocked(path: &Path) -> Result<[u8; 32]> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        is_plain_metadata(&metadata),
        "项目指纹路径不应为链接或重解析点"
    );
    if metadata.is_file() {
        ensure!(
            (CONTAINER_HEADER_SIZE..=CONTAINER_LIMIT).contains(&metadata.len()),
            "单文件项目长度无效"
        );
        let mut file = File::open(path)?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 65_536];
        let mut total = 0_u64;
        loop {
            let length = file.read(&mut buffer)?;
            if length == 0 {
                break;
            }
            total = total
                .checked_add(length as u64)
                .context("项目指纹长度溢出")?;
            ensure!(total <= CONTAINER_LIMIT, "项目文件增大后超出大小限制");
            hash.update(&buffer[..length]);
        }
        ensure!(total == metadata.len(), "计算指纹时项目文件长度发生变化");
        return Ok(hash.finalize().into());
    }
    ensure!(metadata.is_dir(), "项目指纹路径类型无效");
    let root = path.canonicalize()?;
    let bytes = checked_file(&root, &path.join("manifest.json"), MANIFEST_LIMIT)?;
    let m: Manifest = serde_json::from_slice(&bytes)?;
    validate_manifest(&m)?;
    let mut hash = Sha256::new();
    hash.update(bytes);
    let mut files: Vec<String> = m
        .layers
        .iter()
        .flat_map(|l| {
            [l.image_file.clone(), l.mask_file.clone()]
                .into_iter()
                .flatten()
        })
        .collect();
    files.sort();
    for name in files {
        hash.update(name.as_bytes());
        let file = path.join("images").join(&name);
        let meta = fs::symlink_metadata(&file)?;
        ensure!(
            meta.is_file()
                && is_plain_metadata(&meta)
                && meta.len() <= ASSET_LIMIT
                && file.canonicalize()?.starts_with(&root),
            "项目指纹资产无效"
        );
        let mut f = File::open(file)?;
        let mut buf = [0; 65536];
        let mut total = 0_u64;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            ensure!(total <= ASSET_LIMIT, "资产增大后超出大小限制");
            hash.update(&buf[..n]);
        }
    }
    Ok(hash.finalize().into())
}

fn validate_manifest(m: &Manifest) -> Result<()> {
    ensure!(
        !m.extra.keys().any(|k| [
            "format",
            "version",
            "colorSpace",
            "documentID",
            "documentId",
            "width",
            "height",
            "resolution",
            "activeLayerID",
            "activeLayerId",
            "layers",
            "guides"
        ]
        .contains(&k.as_str())),
        "扩展清单字段与必需元数据冲突"
    );
    ensure!(
        m.format == "com.compositor.project" && m.color_space == "sRGB",
        "项目标识符或色彩空间无效"
    );
    ensure!(
        (1..=MAX_READ_VERSION).contains(&m.version),
        "不支持此项目版本"
    );
    ensure!(
        Document::valid_size(m.width, m.height),
        "项目画布无效或超出尺寸限制"
    );
    ensure!(m.layers.len() <= 10_000, "图层数量过多");
    ensure!(
        m.resolution.is_finite() && (1.0..=9600.0).contains(&m.resolution),
        "项目分辨率无效"
    );
    let mut ids = HashMap::new();
    for l in &m.layers {
        ensure!(
            !l.extra.keys().any(|k| [
                "id",
                "name",
                "isVisible",
                "transform",
                "imageFile",
                "parentID",
                "parentId",
                "isGroup",
                "opacity",
                "blendMode",
                "maskFile",
                "maskEnabled",
                "maskSourceID",
                "maskSourceId",
                "maskPlacement",
                "maskLinked",
                "adjustment",
                "effects",
                "text",
                "shape"
            ]
            .contains(&k.as_str())),
            "扩展图层字段与必需元数据冲突"
        );
        ensure!(ids.insert(l.id, l).is_none(), "图层 ID 重复");
        ensure!(
            !l.name.trim().is_empty() && l.name.len() <= 16_384,
            "图层名称无效"
        );
        validate_transform(l.transform)?;
        ensure!(
            l.image_file
                .as_ref()
                .is_none_or(|f| *f == id_name(l.id, false)
                    || (m.version >= 13
                        && *f == format!("{}.exr", l.id.to_string().to_uppercase()))),
            "图像文件名无效"
        );
        ensure!(
            l.mask_file
                .as_ref()
                .is_none_or(|f| *f == id_name(l.id, true)),
            "蒙版文件名无效"
        );
        ensure!(
            l.opacity.is_finite() && (0.0..=1.0).contains(&l.opacity),
            "图层不透明度无效"
        );
        ensure!(
            m.version >= 2 || l.parent_id.is_none() && !l.is_group,
            "分组要求项目版本至少为 2"
        );
        ensure!(
            m.version >= 3 || l.opacity == 1.0 && l.blend_mode == BlendMode::Normal,
            "图层外观要求项目版本至少为 3"
        );
        ensure!(
            !l.is_group
                || l.image_file.is_none()
                    && l.blend_mode == BlendMode::Normal
                    && (m.version >= 8 || l.opacity == 1.0),
            "分组图像或外观无效"
        );
        ensure!(
            l.mask_file.is_none() || m.version >= if l.is_group { 6 } else { 4 },
            "蒙版要求更高的项目版本"
        );
        ensure!(
            l.mask_enabled.is_none() && l.mask_linked.is_none() && l.mask_placement.is_none()
                || l.mask_file.is_some(),
            "蒙版元数据没有对应的蒙版资产"
        );
        if let Some(t) = l.mask_placement {
            validate_transform(t)?;
        }
        ensure!(
            m.version >= 5 || l.mask_source_id.is_none(),
            "剪贴蒙版要求项目版本至少为 5"
        );
        if let Some(a) = &l.adjustment {
            ensure!(
                m.version >= 7 && !l.is_group && l.image_file.is_none() && l.text.is_none(),
                "调整图层无效"
            );
            validate_adjustment(a)?;
            if ["Gaussian Blur", "Motion Blur", "Add Noise"]
                .contains(&a["kind"].as_str().unwrap_or(""))
            {
                ensure!(m.version >= 9, "邻域采样调整要求项目版本至少为 9");
            }
        }
        if let Some(t) = &l.text {
            ensure!(
                l.image_file.is_some() && !l.is_group && l.adjustment.is_none(),
                "文字图层需要栅格回退资产"
            );
            validate_text(t, m.version)?;
        }
        if let Some(e) = &l.effects {
            validate_effects(e)?;
        }
        if let Some(s) = &l.shape {
            validate_shape(s)?;
            ensure!(
                m.version >= 14 || s["smoothing"] != true,
                "平滑圆角要求项目版本至少为 14"
            );
        }
    }
    ensure!(
        m.active_layer_id.is_none_or(|id| ids.contains_key(&id)),
        "缺少当前图层 ID"
    );
    for l in &m.layers {
        let mut seen = HashSet::new();
        let mut parent = l.parent_id;
        let mut depth = 0;
        while let Some(id) = parent {
            ensure!(seen.insert(id) && id != l.id, "图层分组引用形成循环");
            let p = ids.get(&id).context("父图层不存在")?;
            ensure!(p.is_group, "父图层不是分组");
            depth += 1;
            ensure!(depth <= 64, "分组嵌套超过 64 层");
            parent = p.parent_id;
        }
        let mut seen = HashSet::new();
        let mut source = l.mask_source_id;
        let mut depth = 0;
        while let Some(id) = source {
            ensure!(
                !l.is_group && seen.insert(id) && id != l.id,
                "剪贴蒙版存在循环或分组端点"
            );
            let s = ids.get(&id).context("剪贴源图层不存在")?;
            ensure!(!s.is_group, "剪贴源图层是分组");
            depth += 1;
            ensure!(depth <= 256, "剪贴链超过 256 个节点");
            source = s.mask_source_id;
        }
    }
    ensure!(m.guides.len() <= 1000, "参考线数量过多");
    ensure!(
        m.version >= 8 || m.guides.is_empty(),
        "参考线要求项目版本至少为 8"
    );
    let mut guide_ids = HashSet::new();
    for g in &m.guides {
        ensure!(
            guide_ids.insert(g.id)
                && ["horizontal", "vertical"].contains(&g.axis.as_str())
                && g.position.is_finite()
                && g.position.abs() <= 1_000_000.0,
            "对齐参考线无效"
        );
    }
    Ok(())
}
fn validate_transform(t: Transform) -> Result<()> {
    ensure!(
        t.is_valid() && t.size.iter().all(|v| (1.0..=300_000.0).contains(v)),
        "图层变换无效"
    );
    Ok(())
}
fn range(v: &Value, key: &str, low: f64, high: f64) -> Result<()> {
    if let Some(value) = v.get(key).filter(|v| !v.is_null()) {
        let number = value
            .as_f64()
            .with_context(|| format!("{key} 必须为数值"))?;
        ensure!(
            number.is_finite() && (low..=high).contains(&number),
            "{key} 无效"
        );
    }
    Ok(())
}
fn colors(v: &Value) -> Result<()> {
    for key in ["red", "green", "blue"] {
        range(v, key, 0.0, 1.0)?;
    }
    Ok(())
}
fn validate_text(t: &Value, version: u32) -> Result<()> {
    ensure!(t.is_object(), "文字元数据必须是对象");
    let content = t["content"].as_str().context("缺少文字内容")?;
    let utf16_len = content.encode_utf16().count();
    ensure!(utf16_len <= 100_000, "文字内容过长");
    range(t, "fontSize", 1.0, 2000.0)?;
    range(t, "tracking", -100.0, 1000.0)?;
    range(t, "leading", 0.0, 5000.0)?;
    colors(t)?;
    if let Some(a) = t.get("alignment") {
        ensure!(
            ["Left", "Center", "Right"].contains(&a.as_str().unwrap_or("")),
            "文字对齐方式无效"
        );
    }
    if let Some(size) = t.get("boxSize").filter(|v| !v.is_null()) {
        let a = size.as_array().context("文字框无效")?;
        ensure!(a.len() == 2, "文字框无效");
        let w = a[0].as_f64().context("文字框宽度无效")?;
        let h = a[1].as_f64().context("文字框高度无效")?;
        ensure!(
            (16.0..=MAX_SIDE as f64).contains(&w)
                && (16.0..=MAX_SIDE as f64).contains(&h)
                && w * h <= MAX_PIXELS as f64,
            "文字框超出范围"
        );
    }
    for (key, min_version) in [("colorRuns", 10), ("fontRuns", 11)] {
        if let Some(runs) = t.get(key).filter(|v| !v.is_null()) {
            ensure!(
                version >= min_version,
                "字符样式区段要求项目版本至少为 {min_version}"
            );
            let runs = runs.as_array().context("字符样式区段无效")?;
            ensure!(!runs.is_empty(), "字符样式区段不得为空");
            let mut end = 0_u64;
            for run in runs {
                let start = run["location"].as_u64().context("字符样式区段位置无效")?;
                let length = run["length"].as_u64().context("字符样式区段长度无效")?;
                ensure!(start >= end && length > 0, "字符样式区段重叠或为空");
                end = start.checked_add(length).context("字符样式区段长度溢出")?;
                ensure!(end <= utf16_len as u64, "字符样式区段超出文字内容");
                if key == "colorRuns" {
                    colors(run)?;
                } else {
                    let font = run["fontName"].as_str().context("缺少字符样式区段字体")?;
                    ensure!(
                        !font.is_empty()
                            && font.chars().count() <= 200
                            && !font.contains(['\n', '\r']),
                        "字符样式区段字体无效"
                    );
                }
            }
        }
    }
    Ok(())
}
fn validate_shape(s: &Value) -> Result<()> {
    ensure!(
        s.is_object()
            && ["Rectangle", "Ellipse", "Line"].contains(&s["kind"].as_str().unwrap_or("")),
        "形状类型无效"
    );
    colors(s)?;
    if let Some(smoothing) = s.get("smoothing").filter(|v| !v.is_null()) {
        ensure!(smoothing.is_boolean(), "平滑圆角必须为布尔值");
    }
    range(s, "cornerRadius", 0.0, 300_000.0)?;
    range(s, "lineWidth", 0.01, 300_000.0)?;
    for key in ["start", "end"] {
        if let Some(p) = s.get(key).filter(|v| !v.is_null()) {
            let a = p.as_array().context("直线端点无效")?;
            ensure!(
                a.len() == 2
                    && a.iter()
                        .all(|v| v.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n))),
                "直线端点无效"
            );
        }
    }
    Ok(())
}
fn validate_effects(e: &Value) -> Result<()> {
    ensure!(e.is_object(), "效果元数据必须是对象");
    for key in [
        "stroke",
        "shadow",
        "colorOverlay",
        "innerShadow",
        "outerGlow",
        "innerGlow",
    ] {
        if let Some(effect) = e.get(key).filter(|v| !v.is_null()) {
            ensure!(effect.is_object(), "效果无效");
            colors(effect)?;
            range(effect, "opacity", 0.0, 1.0)?;
            range(effect, "size", 0.0, 500.0)?;
            range(effect, "angle", -360.0, 360.0)?;
            range(effect, "distance", 0.0, 5000.0)?;
            range(effect, "blur", 0.0, 500.0)?;
            if let Some(v) = effect.get("enabled").filter(|v| !v.is_null()) {
                ensure!(v.is_boolean(), "效果可见性无效");
            }
        }
    }
    Ok(())
}
fn validate_adjustment(a: &Value) -> Result<()> {
    ensure!(a.is_object(), "调整元数据无效");
    let kind = a["kind"].as_str().context("缺少调整类型")?;
    ensure!(
        [
            "Hue/Saturation",
            "Levels",
            "Curves",
            "Exposure",
            "Gradient Map",
            "Grain",
            "Invert",
            "Black & White",
            "Color Balance",
            "Gaussian Blur",
            "Motion Blur",
            "Add Noise"
        ]
        .contains(&kind),
        "不支持此调整类型"
    );
    for (key, lo, hi) in [
        ("hue", -360.0, 360.0),
        ("saturation", -100.0, 100.0),
        ("lightness", -100.0, 100.0),
        ("blurRadius", 0.1, 250.0),
        ("motionAngle", -90.0, 90.0),
        ("motionDistance", 1.0, 2000.0),
        ("noiseAmount", 0.1, 400.0),
    ] {
        range(a, key, lo, hi)?;
    }
    if let Some(l) = a.get("levels") {
        let ranges = l["ranges"].as_array().context("缺少色阶范围")?;
        ensure!(ranges.len() == 4, "色阶需要四个通道");
        for r in ranges {
            range(r, "black", 0.0, 254.0)?;
            range(r, "white", 1.0, 255.0)?;
            range(r, "gamma", 0.1, 9.99)?;
            range(r, "outputBlack", 0.0, 255.0)?;
            range(r, "outputWhite", 0.0, 255.0)?;
            ensure!(
                r["white"].as_f64().unwrap_or(255.0) >= r["black"].as_f64().unwrap_or(0.0) + 1.0,
                "色阶输入范围无效"
            );
        }
    }
    if let Some(c) = a.get("curves") {
        let channels = c["channels"].as_array().context("缺少曲线通道")?;
        ensure!(channels.len() == 4, "曲线需要四个通道");
        for channel in channels {
            let points = channel.as_array().context("曲线无效")?;
            ensure!((2..=32).contains(&points.len()), "曲线点数量无效");
            let mut last = -1.0;
            for point in points {
                range(point, "x", 0.0, 255.0)?;
                range(point, "y", 0.0, 255.0)?;
                let x = point["x"].as_f64().context("缺少曲线点的 x 坐标")?;
                ensure!(x > last, "曲线点必须按 x 坐标递增");
                last = x;
            }
            ensure!(
                points[0]["x"].as_f64() == Some(0.0)
                    && points.last().unwrap()["x"].as_f64() == Some(255.0),
                "曲线必须覆盖 0–255"
            );
        }
    }
    if let Some(hsv) = a.get("hsvSettings").filter(|v| !v.is_null()) {
        ensure!(hsv.is_object(), "色相/饱和度参数无效");
        let mut normalized = a.clone();
        normalize_hsv_maps(&mut normalized)?;
        let hsv = &normalized["hsvSettings"];
        if let Some(name) = hsv.get("range") {
            ensure!(
                name.as_str().is_some_and(|n| HUE_RANGES.contains(&n)),
                "色相范围无效"
            );
        }
        for key in ["colorize", "invertRange"] {
            if let Some(value) = hsv.get(key) {
                ensure!(value.is_boolean(), "色相开关无效");
            }
        }
        if let Some(adjustments) = hsv.get("adjustments") {
            for (name, value) in adjustments.as_object().context("色相调整集合无效")? {
                ensure!(
                    HUE_RANGES.contains(&name.as_str()) && value.is_object(),
                    "色相调整无效"
                );
                range(value, "hue", -360.0, 360.0)?;
                range(value, "saturation", -100.0, 100.0)?;
                range(value, "lightness", -100.0, 100.0)?;
            }
        }
        if let Some(bands) = hsv.get("bands") {
            for (name, band) in bands.as_object().context("色相区间集合无效")? {
                ensure!(
                    HUE_RANGES.contains(&name.as_str()) && band.is_object(),
                    "色相区间无效"
                );
                for key in ["falloffStart", "rangeStart", "rangeEnd", "falloffEnd"] {
                    ensure!(band.get(key).is_some(), "缺少色相区间控制点");
                    range(band, key, 0.0, 360.0)?;
                }
            }
        }
    }
    // All settings remain serialized, including fields added by newer applications.
    validate_settings(a)?;
    Ok(())
}
/// Swift's synthesized Codable requires these identity blocks even when another kind is active.
pub fn canonical_adjustment(value: &Value) -> Result<Value> {
    let mut value = compatible_adjustment(value);
    normalize_hsv_maps(&mut value)?;
    validate_adjustment(&value)?;
    Ok(value)
}
fn compatible_adjustment(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        for (key, default) in [
            ("hue", serde_json::json!(0.0)),
            ("saturation", serde_json::json!(0.0)),
            ("lightness", serde_json::json!(0.0)),
            ("colorize", Value::Bool(false)),
            (
                "levels",
                serde_json::json!({"channel":"RGB","ranges":vec![serde_json::json!({"black":0.0,"white":255.0,"gamma":1.0,"outputBlack":0.0,"outputWhite":255.0});4]}),
            ),
            (
                "curves",
                serde_json::json!({"channel":"RGB","channels":vec![serde_json::json!([{"x":0.0,"y":0.0},{"x":255.0,"y":255.0}]);4]}),
            ),
        ] {
            object.entry(key.to_owned()).or_insert(default);
        }
        if let Some(hsv) = object.get_mut("hsvSettings").and_then(Value::as_object_mut) {
            for (key, default) in [
                ("range", Value::from("Master")),
                ("colorize", Value::Bool(false)),
                ("invertRange", Value::Bool(false)),
                ("adjustments", Value::Object(Map::new())),
                ("bands", default_hue_bands()),
            ] {
                hsv.entry(key.to_owned()).or_insert(default);
            }
            // Codable encodes dictionaries whose keys are enums as [key,value,...].
            for key in ["adjustments", "bands"] {
                if let Some(map) = hsv.get(key).and_then(Value::as_object) {
                    let pairs = map
                        .iter()
                        .flat_map(|(key, value)| [Value::from(key.clone()), value.clone()])
                        .collect();
                    hsv.insert(key.to_owned(), Value::Array(pairs));
                }
            }
        }
    }
    value
}

const HUE_RANGES: [&str; 7] = [
    "Master", "Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas",
];
fn default_hue_bands() -> Value {
    let mut bands = Map::new();
    for (index, name) in HUE_RANGES.iter().enumerate() {
        let handles = if index == 0 {
            [0.0, 0.0, 360.0, 360.0]
        } else {
            let center = ((index - 1) * 60) as f64;
            [center - 45.0, center - 15.0, center + 15.0, center + 45.0]
                .map(|v| v.rem_euclid(360.0))
        };
        bands.insert((*name).to_owned(), serde_json::json!({"falloffStart":handles[0],"rangeStart":handles[1],"rangeEnd":handles[2],"falloffEnd":handles[3]}));
    }
    Value::Object(bands)
}
fn normalize_hsv_maps(adjustment: &mut Value) -> Result<()> {
    let Some(hsv) = adjustment
        .get_mut("hsvSettings")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    for key in ["adjustments", "bands"] {
        if let Some(array) = hsv.get(key).and_then(Value::as_array) {
            ensure!(array.len().is_multiple_of(2), "以枚举为键的色相字典不完整");
            let mut map = Map::new();
            for pair in array.as_chunks::<2>().0 {
                let name = pair[0].as_str().context("以枚举为键的色相字典无效")?;
                ensure!(
                    map.insert(name.to_owned(), pair[1].clone()).is_none(),
                    "色相字典键重复"
                );
            }
            hsv.insert(key.to_owned(), Value::Object(map));
        }
    }
    Ok(())
}
fn validate_settings(v: &Value) -> Result<()> {
    if let Value::Object(o) = v {
        for (key, value) in o {
            let bounds = match key.as_str() {
                "exposure" => Some((-20.0, 20.0)),
                "offset" => Some((-0.5, 0.5)),
                "gamma" => Some((0.01, 9.99)),
                "shadowCyanRed"
                | "shadowMagentaGreen"
                | "shadowYellowBlue"
                | "midCyanRed"
                | "midMagentaGreen"
                | "midYellowBlue"
                | "highlightCyanRed"
                | "highlightMagentaGreen"
                | "highlightYellowBlue" => Some((-100.0, 100.0)),
                _ => None,
            };
            if let Some((lo, hi)) = bounds {
                range(v, key, lo, hi)?;
            }
            validate_settings(value)?;
        }
    } else if let Value::Array(a) = v {
        for item in a {
            validate_settings(item)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_directory_entry_count(directory: &Path, expected: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            let entries = fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<std::io::Result<Vec<_>>>()
                .unwrap();
            if entries.len() == expected {
                return;
            }
            // A transient directory entry must disappear within this bounded interval.
            if std::time::Instant::now() >= deadline {
                assert_eq!(entries.len(), expected, "Remaining entries: {entries:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn read_manifest(path: &Path) -> Value {
        serde_json::from_slice(
            &ProjectSource::open(path)
                .unwrap()
                .read("manifest.json")
                .unwrap(),
        )
        .unwrap()
    }

    fn write_legacy_fixture(document: &Document, path: &Path, version: u32) {
        fs::create_dir_all(path.join("images")).unwrap();
        for layer in &document.layers {
            if let Some(image) = &layer.image {
                write_png(
                    image,
                    &path.join("images").join(id_name(layer.id, false)),
                    false,
                )
                .unwrap();
            }
            if let Some(mask) = &layer.mask {
                write_png(
                    &mask.raster,
                    &path.join("images").join(id_name(layer.id, true)),
                    true,
                )
                .unwrap();
            }
        }
        let mut metadata = manifest(document);
        metadata.version = version;
        fs::write(
            path.join("manifest.json"),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();
    }

    fn rewrite_index(path: &Path, mutate: impl FnOnce(&mut Vec<ContainerEntry>)) {
        let mut bytes = fs::read(path).unwrap();
        let offset = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
        let mut entries = serde_json::from_slice(&bytes[offset..]).unwrap();
        mutate(&mut entries);
        let index = serde_json::to_vec(&entries).unwrap();
        bytes.truncate(offset);
        bytes[12..16].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        bytes[24..32].copy_from_slice(&(index.len() as u64).to_le_bytes());
        bytes[32..64].copy_from_slice(&Sha256::digest(&index));
        bytes.extend_from_slice(&index);
        fs::write(path, bytes).unwrap();
    }

    fn assert_access_error(error: anyhow::Error, expected: &str) {
        assert_eq!(
            error.downcast_ref::<ProjectAccessError>().unwrap().code,
            expected
        );
    }

    #[test]
    fn concurrent_project_access_returns_busy_without_disturbing_saved_data() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("locked.comp");
        let document = Document::new(2, 2);
        save(&document, &path).unwrap();
        let saved = fingerprint(&path).unwrap();
        let (ready, acquired) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let worker_path = path.clone();
        let worker = std::thread::spawn(move || {
            let _lock = ProjectLock::acquire(&worker_path).unwrap();
            ready.send(()).unwrap();
            released
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
        });
        acquired
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert_access_error(load(&path).unwrap_err(), "project_busy");
        assert_access_error(save(&document, &path).unwrap_err(), "project_busy");
        assert_access_error(fingerprint(&path).unwrap_err(), "project_busy");
        assert_access_error(recover(&path).unwrap_err(), "project_busy");
        release.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(fingerprint(&path).unwrap(), saved);
        assert_eq!(load(&path).unwrap(), document);
        assert!(!journal_path(&path).unwrap().exists());
    }

    #[test]
    fn checked_save_preserves_newer_writes_and_respects_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("checked.comp");
        let document = Document::new(2, 2);
        let saved = save_checked(&document, &path, None, false).unwrap();
        let expected: String = saved.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut newer = document.clone();
        newer.layers[0].name = "newer external write".into();
        let latest = save_checked(&newer, &path, Some(&expected), true).unwrap();
        assert_access_error(
            save_checked(&document, &path, Some(&expected), true).unwrap_err(),
            "conflict",
        );
        assert_access_error(
            save_checked(&document, &path, None, false).unwrap_err(),
            "already_exists",
        );
        assert_eq!(fingerprint(&path).unwrap(), latest);
        assert_eq!(load(&path).unwrap(), newer);
        assert_access_error(
            save_checked(
                &document,
                &temp.path().join("missing.comp"),
                Some(&expected),
                true,
            )
            .unwrap_err(),
            "conflict",
        );
    }

    #[test]
    fn checked_save_rechecks_external_creations_and_edits_after_encoding() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("protected.comp");
        let document = Document::new(2, 2);
        // The precommit hook deterministically represents another program writing while PNGs encode.
        check_save_preconditions(&path, None, false).unwrap();
        let error = save_unlocked(&document, &path, || {
            fs::write(&path, b"external file created during encoding")?;
            check_save_preconditions(&path, None, false)
        })
        .unwrap_err();
        assert_access_error(error, "already_exists");
        assert_eq!(
            fs::read(&path).unwrap(),
            b"external file created during encoding"
        );
        assert_directory_entry_count(temp.path(), 1);

        save(&document, &path).unwrap();
        let expected: String = fingerprint(&path)
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let other = tempfile::tempdir().unwrap();
        let newer_path = other.path().join("newer.comp");
        let mut newer = document.clone();
        newer.layers[0].name = "external newer version".into();
        save(&newer, &newer_path).unwrap();
        let newer_bytes = fs::read(newer_path).unwrap();
        check_save_preconditions(&path, Some(&expected), true).unwrap();
        let error = save_unlocked(&document, &path, || {
            fs::write(&path, &newer_bytes)?;
            check_save_preconditions(&path, Some(&expected), true)
        })
        .unwrap_err();
        assert_access_error(error, "conflict");
        assert_eq!(load(&path).unwrap(), newer);
        assert_directory_entry_count(temp.path(), 1);
    }

    #[test]
    fn competing_checked_writers_cannot_both_commit_the_same_expected_version() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("race.comp");
        let document = Document::new(2, 2);
        let saved = save_checked(&document, &path, None, false).unwrap();
        let expected: String = saved.iter().map(|byte| format!("{byte:02x}")).collect();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let workers: Vec<_> = (0..2)
            .map(|index| {
                let mut document = document.clone();
                document.layers[0].name = format!("writer {index}");
                let path = path.clone();
                let expected = expected.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    save_checked(&document, &path, Some(&expected), true)
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let failure = results.into_iter().find_map(Result::err).unwrap();
        assert!(matches!(
            failure.downcast_ref::<ProjectAccessError>().unwrap().code,
            "project_busy" | "conflict"
        ));
        assert!(load(&path).unwrap().layers[0].name.starts_with("writer "));
        assert!(!journal_path(&path).unwrap().exists());
    }

    #[test]
    fn lock_key_survives_creation_and_windows_canonical_path_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("new-parent/New.comp");
        let before = project_lock_key(&path).unwrap();
        save(&Document::new(1, 1), &path).unwrap();
        assert_eq!(before, project_lock_key(&path).unwrap());
        assert!(same_project_path(&path, &path.canonicalize().unwrap()).unwrap());
        assert!(
            same_project_path(
                &path,
                &temp.path().join("new-parent/../new-parent/New.comp")
            )
            .unwrap()
        );
        #[cfg(windows)]
        assert_eq!(
            before,
            project_lock_key(&PathBuf::from(path.to_string_lossy().to_uppercase())).unwrap()
        );
    }

    #[cfg(windows)]
    #[test]
    fn project_lock_subprocess_helper() {
        let Some(path) = std::env::var_os("COMPOSITOR_TEST_LOCK_PATH") else {
            return;
        };
        let ready = PathBuf::from(std::env::var_os("COMPOSITOR_TEST_LOCK_READY").unwrap());
        let release = PathBuf::from(std::env::var_os("COMPOSITOR_TEST_LOCK_RELEASE").unwrap());
        let _lock = ProjectLock::acquire(Path::new(&path)).unwrap();
        fs::write(ready, b"ready").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !release.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "parent did not release child"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Exit without running Drop to exercise the Windows abandoned-owner path.
        std::process::exit(0);
    }

    #[cfg(windows)]
    #[test]
    fn project_lock_excludes_other_processes_and_recovers_an_abandoned_owner() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("process.comp");
        let ready = temp.path().join("ready");
        let release = temp.path().join("release");
        let document = Document::new(1, 1);
        save(&document, &path).unwrap();
        let name: Vec<u16> = format!(
            "Local\\Compositor.Project.{}",
            project_lock_key(&path).unwrap()
        )
        .encode_utf16()
        .chain(Some(0))
        .collect();
        // Keep an unowned handle open so the named mutex survives the child's exit.
        let handle = unsafe { project_mutex::CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        assert!(!handle.is_null());
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "project::tests::project_lock_subprocess_helper",
                "--nocapture",
            ])
            .env("COMPOSITOR_TEST_LOCK_PATH", &path)
            .env("COMPOSITOR_TEST_LOCK_READY", &ready)
            .env("COMPOSITOR_TEST_LOCK_RELEASE", &release)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !ready.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "project lock helper exited before acquiring mutex"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "project lock helper timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_access_error(load(&path).unwrap_err(), "project_busy");
        assert_access_error(save(&document, &path).unwrap_err(), "project_busy");
        fs::write(&release, b"release").unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(load(&path).unwrap(), document);
        unsafe {
            project_mutex::CloseHandle(handle);
        }
        save(&document, &path).unwrap();
    }

    #[test]
    fn documented_swift_manifest_names_load_and_save_with_uppercase_id_suffixes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("swift.comp");
        fs::create_dir_all(path.join("images")).unwrap();
        // Independent fixture uses the exact keys in ProjectStore.swift and writing-comp-files.md.
        let bytes = br#"{
          "format":"com.compositor.project","version":11,"colorSpace":"sRGB",
          "documentID":"0C5E7A91-3B2D-4F6A-8E1C-9D0B7A6F5E4D","width":1,"height":1,
          "activeLayerID":"6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F",
          "layers":[
            {"id":"00000000-0000-0000-0000-000000000001","name":"Folder","isGroup":true,
             "transform":{"origin":[0,0],"size":[1,1],"rotation":0,"flipX":false,"flipY":false,"sampling":"High quality"}},
            {"id":"00000000-0000-0000-0000-000000000002","name":"Base","parentID":"00000000-0000-0000-0000-000000000001",
             "transform":{"origin":[0,0],"size":[1,1],"rotation":0,"flipX":false,"flipY":false,"sampling":"High quality"}},
            {"id":"6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F","name":"Background",
             "parentID":"00000000-0000-0000-0000-000000000001","maskSourceID":"00000000-0000-0000-0000-000000000002",
             "imageFile":"6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F.png",
             "transform":{"origin":[0,0],"size":[1,1],"rotation":0,"flipX":false,"flipY":false,"sampling":"High quality"}}
          ]}"#;
        fs::write(path.join("manifest.json"), bytes).unwrap();
        write_png(
            &Raster::solid(1, 1, [42, 71, 19, 255]),
            &path.join("images/6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F.png"),
            false,
        )
        .unwrap();
        let document = load(&path).unwrap();
        assert_eq!(
            document.id.to_string(),
            "0c5e7a91-3b2d-4f6a-8e1c-9d0b7a6f5e4d"
        );
        assert_eq!(document.active_layer_id, Some(document.layers[2].id));
        assert_eq!(document.layers[2].parent_id, Some(document.layers[0].id));
        assert_eq!(
            document.layers[2].mask_source_id,
            Some(document.layers[1].id)
        );
        assert_eq!(
            document.layers[2].image.as_ref().unwrap().get(0, 0),
            [42, 71, 19, 255]
        );
        let destination = temp.path().join("swift-single.comp");
        save(&document, &destination).unwrap();
        let saved = read_manifest(&destination);
        for key in ["documentID", "activeLayerID"] {
            assert!(saved.get(key).is_some());
        }
        for key in ["documentId", "activeLayerId"] {
            assert!(saved.get(key).is_none());
        }
        assert!(saved["layers"][2].get("parentID").is_some());
        assert!(saved["layers"][2].get("maskSourceID").is_some());
        assert!(saved["layers"][2].get("parentId").is_none());
        assert!(saved["layers"][2].get("maskSourceId").is_none());
    }
    #[test]
    fn swift_enum_key_hue_dictionary_is_readable_and_written_in_codable_shape() {
        let mut document = Document::new(1, 1);
        document.layers[0].adjustment = Some(
            serde_json::json!({"kind":"Hue/Saturation","hsvSettings":{
                "range":"Reds","colorize":false,"invertRange":true,
                "adjustments":["Master",{"hue":10,"saturation":0,"lightness":0},"Reds",{"hue":-25,"saturation":40,"lightness":5}],
                "bands":["Reds",{"falloffStart":315,"rangeStart":345,"rangeEnd":15,"falloffEnd":45}],
                "futureHueField":{"kept":true}
            }}),
        );
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hsv.comp");
        save(&document, &path).unwrap();
        let read = load(&path).unwrap();
        let adjustment = read.layers[0].adjustment.as_ref().unwrap();
        assert_eq!(
            adjustment["hsvSettings"]["adjustments"]["Reds"]["saturation"],
            40
        );
        assert_eq!(adjustment["hsvSettings"]["bands"]["Reds"]["rangeEnd"], 15);
        assert_eq!(adjustment["hsvSettings"]["futureHueField"]["kept"], true);
        save(&read, &path).unwrap();
        let stored = read_manifest(&path);
        assert!(stored["layers"][0]["adjustment"]["hsvSettings"]["adjustments"].is_array());
        assert!(stored["layers"][0]["adjustment"]["hsvSettings"]["bands"].is_array());
        assert_eq!(load(&path).unwrap(), read);
        let mut malformed = adjustment.clone();
        malformed["hsvSettings"]["adjustments"] = serde_json::json!(["Reds"]);
        assert!(validate_adjustment(&malformed).is_err());
    }
    #[test]
    fn interrupted_replacement_restores_previous_saved_package() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("old.comp");
        let old = Document::new(1, 1);
        save(&old, &path).unwrap();
        let stage = temp.path().join(".old.comp.test.staging");
        let backup = temp.path().join(".old.comp.test.backup");
        let mut edited = old.clone();
        edited.layers[0].name = "Unsaved replacement".to_owned();
        save(&edited, &stage).unwrap();
        write_journal(
            &path,
            &SaveJournal {
                target: "old.comp".to_owned(),
                stage: stage.file_name().unwrap().to_string_lossy().into_owned(),
                backup: backup.file_name().unwrap().to_string_lossy().into_owned(),
                container_version: Some(CONTAINER_VERSION),
            },
        )
        .unwrap();
        fs::rename(&path, &backup).unwrap();
        assert_eq!(load(&path).unwrap(), old);
        assert!(!stage.exists() && !backup.exists() && !journal_path(&path).unwrap().exists());
    }
    #[test]
    fn completed_replacement_recovery_keeps_new_package() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("new.comp");
        let old = Document::new(1, 1);
        save(&old, &path).unwrap();
        let backup = temp.path().join(".new.comp.test.backup");
        fs::rename(&path, &backup).unwrap();
        let mut new = old.clone();
        new.layers[0].name = "New version".to_owned();
        save(&new, &path).unwrap();
        write_journal(
            &path,
            &SaveJournal {
                target: "new.comp".to_owned(),
                stage: ".new.comp.test.staging".to_owned(),
                backup: ".new.comp.test.backup".to_owned(),
                container_version: Some(CONTAINER_VERSION),
            },
        )
        .unwrap();
        assert_eq!(load(&path).unwrap(), new);
        assert!(!backup.exists());
    }
    #[test]
    fn recovery_rejects_paths_outside_package_parent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("evil.comp");
        write_journal(
            &path,
            &SaveJournal {
                target: "evil.comp".to_owned(),
                stage: "../outside.staging".to_owned(),
                backup: ".evil.comp.test.backup".to_owned(),
                container_version: Some(CONTAINER_VERSION),
            },
        )
        .unwrap();
        assert!(recover(&path).is_err());
    }
    #[test]
    fn package_roundtrip_preserves_pixels_metadata_masks_and_unknown_fields() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("round.comp");
        let mut d = Document::new(2, 2);
        let l = &mut d.layers[0];
        l.image = Some(Arc::new(Raster::solid(2, 2, [20, 40, 60, 128])));
        l.mask = Some(Mask::white());
        l.transform.rotation = 32.0;
        l.extra
            .insert("futureLayerField".to_owned(), serde_json::json!({"a":12}));
        d.extra
            .insert("futureDocumentField".to_owned(), Value::Bool(true));
        save(&d, &path).unwrap();
        let read = load(&path).unwrap();
        assert_eq!(d, read);
        assert!(path.is_file());
        assert_directory_entry_count(temp.path(), 1);
    }

    #[test]
    fn single_file_fingerprint_is_the_full_file_sha256_and_detects_equal_size_edits() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("指纹 test.comp");
        save(&Document::new(2, 2), &path).unwrap();
        let mut bytes = fs::read(&path).unwrap();
        assert_eq!(
            fingerprint(&path).unwrap(),
            <[u8; 32]>::from(Sha256::digest(&bytes))
        );
        let saved = fingerprint(&path).unwrap();
        bytes[CONTAINER_HEADER_SIZE as usize + 1] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert_ne!(fingerprint(&path).unwrap(), saved);
        assert!(load(&path).is_err());
    }

    #[test]
    fn newer_8bit_projects_and_smoothing_have_explicit_version_semantics() {
        let temp = tempfile::tempdir().unwrap();
        let mut document = Document::new(8, 8);
        for version in 12..=14 {
            let path = temp.path().join(format!("upstream-{version}.comp"));
            write_legacy_fixture(&document, &path, version);
            assert_eq!(load(&path).unwrap(), document);
        }
        crate::edit::add_shape_with_smoothing(
            &mut document,
            crate::edit::ShapeKind::Rectangle,
            [0., 0., 8., 8.],
            [100, 180, 230, 255],
            2.,
            1.,
            None,
            true,
        )
        .unwrap();
        let metadata = manifest(&document);
        assert_eq!(metadata.version, 14);
        validate_manifest(&metadata).unwrap();
        for version in 1..=13 {
            let mut older = manifest(&document);
            older.version = version;
            assert!(validate_manifest(&older).is_err());
        }
        let path = temp.path().join("smoothed.comp");
        save(&document, &path).unwrap();
        assert_eq!(read_manifest(&path)["version"], 14);
        assert_eq!(load(&path).unwrap(), document);
    }

    #[test]
    fn high_precision_assets_keep_low_bits_and_reject_older_manifest_claims() {
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                bytemuck::cast_slice(&[10_001u16, 22_222, 33_333, 65_535]),
                1,
                1,
                image::ExtendedColorType::Rgba16,
            )
            .unwrap();
        let mut budget = 100;
        let decoded = decode_png(bytes, false, &mut budget).unwrap();
        assert_eq!(
            decoded.deep.as_ref().unwrap().get_bits(0, 0),
            [10_001, 22_222, 33_333, 65_535]
        );
        assert_eq!(budget, 97);
        let mut document = Document::new(8, 8);
        document.layers[0].shape = Some(serde_json::json!({
            "kind": "Rectangle", "smoothing": "true"
        }));
        assert!(validate_document(&document).is_err());
        let mut metadata = manifest(&Document::new(8, 8));
        metadata.version = 12;
        metadata.layers[0].image_file = Some(format!(
            "{}.exr",
            metadata.layers[0].id.to_string().to_uppercase()
        ));
        assert!(validate_manifest(&metadata).is_err());
    }

    #[test]
    fn deep_and_hdr_single_file_projects_round_trip_edits_and_manifest_versions() {
        use crate::deep_pixels::{DeepRaster, Depth};
        let temp = tempfile::tempdir().unwrap();
        for depth in [Depth::U16, Depth::Hdr] {
            let mut doc = Document::new(4, 2);
            let pixels = DeepRaster::solid(
                4,
                2,
                depth,
                [
                    0.1234,
                    0.5678,
                    if depth == Depth::Hdr { 4. } else { 0.9 },
                    1.,
                ],
            );
            let mut raster = Raster::from_deep(pixels);
            raster.set(1, 1, [80, 90, 100, 255]);
            let expected = raster.deep.as_ref().unwrap().to_bits();
            doc.layers[0].image = Some(Arc::new(raster));
            let path = temp.path().join(format!("{depth:?}.comp"));
            save(&doc, &path).unwrap();
            assert!(path.is_file());
            assert_eq!(
                read_manifest(&path)["version"],
                if depth == Depth::Hdr { 13 } else { 12 }
            );
            let loaded = load(&path).unwrap();
            assert_eq!(
                loaded.layers[0]
                    .image
                    .as_ref()
                    .unwrap()
                    .deep
                    .as_ref()
                    .unwrap()
                    .to_bits(),
                expected
            );
        }
    }

    #[test]
    fn legacy_directories_versions_one_through_eleven_load_and_save_to_a_new_file() {
        let temp = tempfile::tempdir().unwrap();
        let document = Document::new(2, 2);
        for version in 1..=CURRENT_VERSION {
            let directory = temp.path().join(format!("legacy-v{version}.comp"));
            write_legacy_fixture(&document, &directory, version);
            let original = fingerprint(&directory).unwrap();
            assert_eq!(load(&directory).unwrap(), document);
            assert_access_error(
                save(&document, &directory).unwrap_err(),
                "directory_project_save_as",
            );
            assert_access_error(
                save_checked(&document, &directory, None, true).unwrap_err(),
                "directory_project_save_as",
            );
            assert_eq!(fingerprint(&directory).unwrap(), original);
            let output = temp.path().join(format!("converted-v{version}.comp"));
            save(&load(&directory).unwrap(), &output).unwrap();
            assert!(output.is_file());
            assert_eq!(read_manifest(&output)["version"], CURRENT_VERSION);
            assert_eq!(load(&output).unwrap(), document);
            assert!(directory.is_dir());
        }
    }

    #[test]
    fn single_file_load_rejects_corrupt_headers_indexes_assets_and_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("corrupt.comp");
        let mut document = Document::new(2, 2);
        document.layers[0].image = Some(Arc::new(Raster::solid(2, 2, [20, 40, 60, 255])));
        save(&document, &path).unwrap();
        let original = fs::read(&path).unwrap();
        for offset in [0, 8, 12, 16, 24, 32, 64, original.len() - 1] {
            let mut edited = original.clone();
            edited[offset] ^= 0xff;
            fs::write(&path, edited).unwrap();
            assert!(load(&path).is_err(), "corruption at {offset} was accepted");
        }
        for length in [0, 8, 63, 64, original.len() - 1] {
            fs::write(&path, &original[..length]).unwrap();
            assert!(load(&path).is_err(), "truncation to {length} was accepted");
        }
        let mut trailing = original.clone();
        trailing.push(0);
        fs::write(&path, trailing).unwrap();
        assert!(load(&path).is_err());
        fs::write(&path, original).unwrap();
        assert!(load(&path).is_ok());
    }

    #[test]
    fn single_file_index_rejects_unsafe_paths_duplicate_entries_and_invalid_bounds() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("unsafe.comp");
        let mut document = Document::new(2, 2);
        document.layers[0].image = Some(Arc::new(Raster::solid(2, 2, [20, 40, 60, 255])));
        save(&document, &path).unwrap();
        let original = fs::read(&path).unwrap();
        for unsafe_path in [
            "../outside.png",
            "images/../outside.png",
            "C:\\outside.png",
            "images/evil.png",
        ] {
            fs::write(&path, &original).unwrap();
            rewrite_index(&path, |entries| entries[1].path = unsafe_path.into());
            assert!(load(&path).is_err());
        }
        for mutation in 0..6 {
            fs::write(&path, &original).unwrap();
            rewrite_index(&path, |entries| match mutation {
                0 => entries[1].offset = entries[0].offset,
                1 => entries[1].offset += 1,
                2 => entries[1].length = u64::MAX,
                3 => entries[1].length = ASSET_LIMIT + 1,
                4 => entries[1].path = entries[0].path.clone(),
                _ => entries[0].length = MANIFEST_LIMIT + 1,
            });
            assert!(
                load(&path).is_err(),
                "index mutation {mutation} was accepted"
            );
        }
        assert!(!temp.path().join("outside.png").exists());
    }

    #[test]
    fn single_file_load_rejects_missing_or_unreferenced_assets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("references.comp");
        let mut document = Document::new(2, 2);
        document.layers[0].image = Some(Arc::new(Raster::solid(2, 2, [20, 40, 60, 255])));
        save(&document, &path).unwrap();
        rewrite_index(&path, |entries| {
            entries[1].path = format!("images/{}", id_name(Uuid::new_v4(), false))
        });
        assert!(load(&path).is_err());
    }

    #[test]
    fn first_single_file_save_recovers_only_a_complete_staged_project() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("first.comp");
        let stage = temp.path().join(".first.comp.test.staging");
        let document = Document::new(2, 2);
        save(&document, &stage).unwrap();
        write_journal(
            &path,
            &SaveJournal {
                target: "first.comp".into(),
                stage: ".first.comp.test.staging".into(),
                backup: ".first.comp.test.backup".into(),
                container_version: Some(CONTAINER_VERSION),
            },
        )
        .unwrap();
        let original = fs::read(&stage).unwrap();
        fs::write(&stage, &original[..64]).unwrap();
        assert!(recover(&path).is_err());
        assert!(stage.exists() && journal_path(&path).unwrap().exists());
        assert!(!path.exists());
        fs::write(&stage, original).unwrap();
        assert_eq!(load(&path).unwrap(), document);
        assert_directory_entry_count(temp.path(), 1);
    }

    #[test]
    fn corrupt_replacement_recovery_preserves_valid_previous_backup() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("new.comp");
        let backup = temp.path().join(".new.comp.test.backup");
        let document = Document::new(2, 2);
        save(&document, &path).unwrap();
        fs::rename(&path, &backup).unwrap();
        fs::write(&path, b"corrupt replacement").unwrap();
        write_journal(
            &path,
            &SaveJournal {
                target: "new.comp".into(),
                stage: ".new.comp.test.staging".into(),
                backup: ".new.comp.test.backup".into(),
                container_version: Some(CONTAINER_VERSION),
            },
        )
        .unwrap();
        assert!(recover(&path).is_err());
        assert!(backup.is_file() && journal_path(&path).unwrap().is_file());
        assert_eq!(load(&backup).unwrap(), document);
    }

    #[test]
    fn old_directory_journal_recovers_without_converting_or_deleting_the_directory() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("old.comp");
        let stage = temp.path().join(".old.comp.test.staging");
        let backup = temp.path().join(".old.comp.test.backup");
        let document = Document::new(2, 2);
        write_legacy_fixture(&document, &backup, CURRENT_VERSION);
        write_legacy_fixture(&document, &stage, CURRENT_VERSION);
        // Match the historical three-field journal exactly; the container field did not exist.
        fs::write(journal_path(&path).unwrap(), br#"{"target":"old.comp","stage":".old.comp.test.staging","backup":".old.comp.test.backup"}"#).unwrap();
        assert_eq!(load(&path).unwrap(), document);
        assert!(path.is_dir());
        assert!(!stage.exists() && !backup.exists());
        assert_directory_entry_count(temp.path(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn interrupted_replacement_recovers_through_a_windows_case_alias() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mixed-case.comp");
        let alias = temp.path().join("MIXED-CASE.COMP");
        let backup = temp.path().join(".mixed-case.comp.test.backup");
        let document = Document::new(2, 2);
        save(&document, &path).unwrap();
        fs::rename(&path, &backup).unwrap();
        write_journal(
            &path,
            &SaveJournal {
                target: "mixed-case.comp".into(),
                stage: ".mixed-case.comp.test.staging".into(),
                backup: ".mixed-case.comp.test.backup".into(),
                container_version: Some(CONTAINER_VERSION),
            },
        )
        .unwrap();
        assert_eq!(load(&alias).unwrap(), document);
        assert!(path.is_file());
        assert_directory_entry_count(temp.path(), 1);
    }
    #[test]
    fn failed_save_preserves_package_and_assets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("keep.comp");
        let mut d = Document::new(1, 1);
        save(&d, &path).unwrap();
        let before = fingerprint(&path).unwrap();
        d.layers[0].opacity = 2.0;
        assert!(save(&d, &path).is_err());
        assert_eq!(before, fingerprint(&path).unwrap());
    }
    #[test]
    fn old_version_rejects_new_semantics() {
        let d = Document::new(1, 1);
        let mut m = manifest(&d);
        m.version = 1;
        m.layers[0].opacity = 0.5;
        assert!(validate_manifest(&m).is_err());
    }
    #[test]
    fn graph_cycles_and_escape_paths_rejected() {
        let mut d = Document::new(1, 1);
        d.layers[0].mask_source_id = Some(d.layers[0].id);
        assert!(validate_document(&d).is_err());
        let mut m = manifest(&Document::new(1, 1));
        m.layers[0].image_file = Some("../outside.png".to_owned());
        assert!(validate_manifest(&m).is_err());
    }
    #[test]
    fn utf16_text_runs_do_not_use_utf8_byte_offsets() {
        let t = serde_json::json!({"content":"A🦀B","fontName":"Segoe UI","fontSize":20,"colorRuns":[{"location":1,"length":2,"red":1,"green":0,"blue":0}]});
        assert!(validate_text(&t, 11).is_ok());
        let mut invalid = t;
        invalid["colorRuns"][0]["length"] = Value::from(4);
        assert!(validate_text(&invalid, 11).is_err());
    }
}
