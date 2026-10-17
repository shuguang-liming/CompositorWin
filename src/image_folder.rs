//! Direct, naturally sorted folder imports; project packages remain single documents.
use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
};

pub(super) fn is_plain_folder(path: &Path) -> bool {
    path.is_dir()
        && !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("comp"))
        && !path.join("manifest.json").is_file()
}

pub(super) fn images(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut images = Vec::new();
    for entry in fs::read_dir(path).map_err(|e| format!("读取图像文件夹失败：{e}"))? {
        let entry = entry.map_err(|e| format!("读取文件夹内容失败：{e}"))?;
        let kind = entry
            .file_type()
            .map_err(|e| format!("读取文件类型失败：{e}"))?;
        if !kind.is_file() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            let metadata = entry
                .metadata()
                .map_err(|e| format!("读取文件属性失败：{e}"))?;
            // Hidden files and reparse points do not belong to the folder's image batch.
            if metadata.file_attributes() & (0x2 | 0x400) != 0 {
                continue;
            }
        }
        let file = entry.path();
        let extension = file
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !crate::image_io::is_raw(&file)
            && ![
                "png", "jpg", "jpeg", "jpe", "tif", "tiff", "bmp", "gif", "webp", "heic", "heif",
                "svg", "psd", "psb", "pdf", "exr", "hdr",
            ]
            .contains(&extension.as_str())
        {
            continue;
        }
        if images.len() == 10_000 {
            return Err("单次文件夹导入最多 10,000 张图像，请分批导入".into());
        }
        images.push((entry.file_name().to_string_lossy().to_lowercase(), file));
    }
    images.sort_by(|(a, ap), (b, bp)| natural_cmp(a, b).then_with(|| ap.cmp(bp)));
    if images.is_empty() {
        return Err("这个文件夹没有可导入的图像；只读取文件夹内的直接文件".into());
    }
    Ok(images.into_iter().map(|(_, path)| path).collect())
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut x, mut y) = (0, 0);
    while x < a.len() && y < b.len() {
        if a[x].is_ascii_digit() && b[y].is_ascii_digit() {
            let (start_a, start_b) = (x, y);
            while x < a.len() && a[x].is_ascii_digit() {
                x += 1;
            }
            while y < b.len() && b[y].is_ascii_digit() {
                y += 1;
            }
            let mut first_a = start_a;
            let mut first_b = start_b;
            while first_a + 1 < x && a[first_a] == b'0' {
                first_a += 1;
            }
            while first_b + 1 < y && b[first_b] == b'0' {
                first_b += 1;
            }
            let order = (x - first_a)
                .cmp(&(y - first_b))
                .then_with(|| a[first_a..x].cmp(&b[first_b..y]))
                .then_with(|| (x - start_a).cmp(&(y - start_b)));
            if order != Ordering::Equal {
                return order;
            }
        } else {
            let order = a[x].cmp(&b[y]);
            if order != Ordering::Equal {
                return order;
            }
            x += 1;
            y += 1;
        }
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_batch_is_direct_sorted_and_excludes_projects_hidden_and_unrelated_files() {
        let folder = tempfile::tempdir().unwrap();
        for file in [
            "照片10.PNG",
            "照片2.jpg",
            "照片02.jpg",
            ".hidden.png",
            "readme.md",
            "项目.comp",
            "照片1.psd",
        ] {
            fs::write(folder.path().join(file), b"decoder validates content later").unwrap();
        }
        fs::create_dir(folder.path().join("nested")).unwrap();
        fs::write(folder.path().join("nested/照片0.png"), b"not recursive").unwrap();
        assert!(is_plain_folder(folder.path()));
        let names: Vec<_> = images(folder.path())
            .unwrap()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["照片1.psd", "照片2.jpg", "照片02.jpg", "照片10.PNG"]
        );
        fs::write(folder.path().join("manifest.json"), b"{}").unwrap();
        assert!(!is_plain_folder(folder.path()));
    }

    #[test]
    fn long_numbers_do_not_overflow_and_empty_folders_are_reported() {
        assert_eq!(
            natural_cmp("a999999999999999999999.png", "a1000000000000000000000.png"),
            Ordering::Less
        );
        assert_eq!(natural_cmp("a2", "a20"), Ordering::Less);
        assert_eq!(natural_cmp("a0", "a00"), Ordering::Less);
        assert_eq!(natural_cmp("abc", "abc"), Ordering::Equal);
        let folder = tempfile::tempdir().unwrap();
        assert!(images(folder.path()).unwrap_err().contains("没有"));
        let package = folder.path().join("原项目.comp");
        fs::create_dir(&package).unwrap();
        assert!(!is_plain_folder(&package));
    }
}
