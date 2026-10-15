//! Windows-installed font discovery beyond fontdb's fixed directory scan.
use cosmic_text::fontdb::{Database, Source};
use std::{
    collections::HashSet,
    ffi::{OsString, c_void},
    os::windows::{ffi::OsStringExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

type Hkey = *mut c_void;

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        key: Hkey,
        subkey: *const u16,
        options: u32,
        access: u32,
        out: *mut Hkey,
    ) -> i32;
    fn RegEnumValueW(
        key: Hkey,
        index: u32,
        name: *mut u16,
        name_length: *mut u32,
        reserved: *const u32,
        kind: *mut u32,
        data: *mut u8,
        data_length: *mut u32,
    ) -> i32;
    fn RegCloseKey(key: Hkey) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn ExpandEnvironmentStringsW(source: *const u16, target: *mut u16, capacity: u32) -> u32;
}

struct RegistryKey(Hkey);
impl Drop for RegistryKey {
    fn drop(&mut self) {
        // This guard owns only handles returned successfully by RegOpenKeyExW.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn expanded(value: &[u16]) -> Option<OsString> {
    let source = value.iter().copied().chain(Some(0)).collect::<Vec<_>>();
    // The source is terminated UTF-16; the size probe receives no output buffer.
    let length = unsafe { ExpandEnvironmentStringsW(source.as_ptr(), std::ptr::null_mut(), 0) };
    if length == 0 || length > 32_768 {
        return None;
    }
    let mut target = vec![0; length as usize];
    // Windows writes at most the probed number of u16 units and retains no pointer.
    let written =
        unsafe { ExpandEnvironmentStringsW(source.as_ptr(), target.as_mut_ptr(), length) };
    if written == 0 || written > length {
        return None;
    }
    target.truncate(written.saturating_sub(1) as usize);
    Some(OsString::from_wide(&target))
}

fn registered_paths(root: Hkey) -> Vec<PathBuf> {
    const KEY_READ: u32 = 0x20019;
    const ERROR_NO_MORE_ITEMS: i32 = 259;
    const REG_SZ: u32 = 1;
    const REG_EXPAND_SZ: u32 = 2;
    let subkey = r"Software\Microsoft\Windows NT\CurrentVersion\Fonts"
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut key = std::ptr::null_mut();
    // The terminated subkey and out-handle remain valid for the synchronous call.
    if unsafe { RegOpenKeyExW(root, subkey.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
        return Vec::new();
    }
    let key = RegistryKey(key);
    // Registry value names are limited to 16,383 UTF-16 units; a Windows file
    // path is limited to 32,767 units. Keep both API buffers bounded.
    let mut name = vec![0_u16; 16_384];
    let mut data = vec![0_u8; 65_536];
    let mut paths = Vec::new();
    for index in 0..100_000 {
        let mut name_length = name.len() as u32;
        let mut data_length = data.len() as u32;
        let mut kind = 0;
        // Both buffers match the supplied in/out capacities; the reserved
        // argument is null as required by Win32. Neither buffer is retained.
        let status = unsafe {
            RegEnumValueW(
                key.0,
                index,
                name.as_mut_ptr(),
                &mut name_length,
                std::ptr::null(),
                &mut kind,
                data.as_mut_ptr(),
                &mut data_length,
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != 0
            || !matches!(kind, REG_SZ | REG_EXPAND_SZ)
            || data_length as usize > data.len()
            || !data_length.is_multiple_of(2)
        {
            continue;
        }
        let mut value = data[..data_length as usize]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect::<Vec<_>>();
        if let Some(end) = value.iter().position(|unit| *unit == 0) {
            value.truncate(end);
        }
        if value.is_empty() {
            continue;
        }
        let path = if kind == REG_EXPAND_SZ {
            expanded(&value)
        } else {
            Some(OsString::from_wide(&value))
        };
        if let Some(path) = path {
            paths.push(PathBuf::from(path));
        }
    }
    paths
}

fn normalized(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["ttf", "ttc", "otf", "otc"]
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

fn load_file(database: &mut Database, path: &Path, known: &mut HashSet<PathBuf>) {
    if !supported(path) || !path.is_file() || !known.insert(normalized(path)) {
        return;
    }
    if let Err(error) = database.load_font_file(path) {
        log::warn!("Cannot load installed font {}: {error}", path.display());
    }
}

fn load_directory(database: &mut Database, root: &Path, known: &mut HashSet<PathBuf>) {
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                // Do not recurse into Windows junctions or directory links.
                if metadata.file_attributes() & 0x400 == 0 {
                    directories.push(path);
                }
            } else {
                load_file(database, &path, known);
            }
        }
    }
}

pub(super) fn load(database: &mut Database) {
    let mut known = database
        .faces()
        .filter_map(|face| match &face.source {
            Source::File(path) | Source::SharedFile(path, _) => Some(normalized(path)),
            Source::Binary(_) => None,
        })
        .collect::<HashSet<_>>();
    let system_fonts =
        PathBuf::from(std::env::var_os("SYSTEMROOT").unwrap_or_else(|| r"C:\Windows".into()))
            .join("Fonts");
    let mut directories = vec![system_fonts.clone()];
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let profile = PathBuf::from(profile);
        directories.push(profile.join(r"AppData\Local\Microsoft\Windows\Fonts"));
        directories.push(profile.join(r"AppData\Roaming\Microsoft\Windows\Fonts"));
    }
    // Respect redirected profile locations as well as fontdb's conventional
    // USERPROFILE paths. All filesystem paths remain native UTF-16 OsStrings.
    for variable in ["LOCALAPPDATA", "APPDATA"] {
        if let Some(directory) = std::env::var_os(variable) {
            directories.push(PathBuf::from(directory).join(r"Microsoft\Windows\Fonts"));
        }
    }
    for directory in &directories {
        load_directory(database, directory, &mut known);
    }
    for root in [
        0x8000_0001_u32 as i32 as isize,
        0x8000_0002_u32 as i32 as isize,
    ] {
        for path in registered_paths(root as Hkey) {
            if path.is_absolute() {
                load_file(database, &path, &mut known);
            } else {
                // Most registrations contain just a filename in Windows Fonts.
                for directory in &directories {
                    let candidate = directory.join(&path);
                    if candidate.is_file() {
                        load_file(database, &candidate, &mut known);
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_font_outside_default_directories_is_discovered() {
        let path = Path::new(r"C:\ProgramData\Microsoft Office\Windows\Fonts\FFont-NameHu.ttf");
        if !path.is_file()
            || ![
                0x8000_0001_u32 as i32 as isize,
                0x8000_0002_u32 as i32 as isize,
            ]
            .into_iter()
            .flat_map(|root| registered_paths(root as Hkey))
            .any(|registered| normalized(&registered) == normalized(path))
        {
            eprintln!("No registered external Office font is available on this test machine");
            return;
        }
        let mut original = Database::new();
        original.load_system_fonts();
        if original.faces().any(|face| {
            face.post_script_name == "FFont-Name"
                || face.families.iter().any(|(name, _)| name == "FFont-Family")
        }) {
            eprintln!("Office test font is already present in a conventional directory");
            return;
        }
        let before = original.len();
        load(&mut original);
        assert!(original.len() > before);
        assert!(original.faces().any(|face| {
            face.post_script_name == "FFont-Name"
                && face.families.iter().any(|(name, _)| name == "FFont-Family")
        }));
        assert!(
            crate::text::font_names()
                .iter()
                .any(|name| name == "FFont-Family")
        );
        assert!(crate::text::font_data("FFont-Family").is_some());
        assert!(crate::text::font_data("FFont-Name").is_some());
    }

    #[test]
    fn mixed_case_extensions_and_unicode_directories_load_every_collection_face() {
        let original = crate::text::font_data("Microsoft YaHei").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("中文 字体目录 💡");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("中文 字体.TtC");
        std::fs::write(&path, &original.0).unwrap();
        let mut expected = Database::new();
        expected.load_font_file(&path).unwrap();
        let mut actual = Database::new();
        let mut known = HashSet::new();
        load_directory(&mut actual, &root, &mut known);
        assert_eq!(actual.len(), expected.len());
        assert!(!actual.is_empty());
        assert_eq!(
            actual.faces().map(|face| face.index).collect::<Vec<_>>(),
            expected.faces().map(|face| face.index).collect::<Vec<_>>()
        );
        let before = actual.len();
        load_directory(&mut actual, &root, &mut known);
        assert_eq!(actual.len(), before, "Do not load a registered file twice");
    }

    #[test]
    fn expanded_registry_paths_preserve_unicode_and_expand_native_environment() {
        let units = r"%SYSTEMROOT%\Fonts\中文 字体.TtF"
            .encode_utf16()
            .collect::<Vec<_>>();
        let expanded = expanded(&units).unwrap();
        let expected =
            PathBuf::from(std::env::var_os("SYSTEMROOT").unwrap()).join(r"Fonts\中文 字体.TtF");
        assert_eq!(PathBuf::from(expanded), expected);
    }
}
