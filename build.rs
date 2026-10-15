use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/app-icon.ico");
    for name in [
        "RC",
        "WindowsSdkDir",
        "WindowsSdkVerBinPath",
        "ProgramFiles(x86)",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    assert_eq!(
        env::var("CARGO_CFG_TARGET_ENV").as_deref(),
        Ok("msvc"),
        "The Windows release uses the MSVC toolchain and Windows SDK resource compiler."
    );
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo project directory"));
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo build output directory"));
    let icon = rc_path(&root.join("assets/app-icon.ico"));
    let version = env::var("CARGO_PKG_VERSION").expect("Cargo package version");
    let numeric_version = [
        "CARGO_PKG_VERSION_MAJOR",
        "CARGO_PKG_VERSION_MINOR",
        "CARGO_PKG_VERSION_PATCH",
    ]
    .map(|name| {
        env::var(name)
            .expect("Cargo version component")
            .parse::<u16>()
            .expect("Windows file version components must fit in 16 bits")
            .to_string()
    })
    .join(",");
    let source = output.join("compositor.rc");
    let resource = output.join("compositor.res");
    let manifest = output.join("compositor.manifest");
    let manifest_path = rc_path(&manifest);
    // Keep the application's OS compatibility declaration in the executable.
    // Without this resource Windows 10 reports legacy version information to
    // libraries that still use version helpers, and DPI/window behavior can
    // differ between Windows 10 and Windows 11.  The GUID is the documented
    // Windows 10/11 compatibility identifier; the older entries preserve the
    // correct version reporting on supported legacy hosts while the installer
    // enforces the Windows 10 x64 minimum.
    fs::write(
        &manifest,
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity version="@APP_VERSION@.0" processorArchitecture="amd64" name="Compositor.Windows" type="win32"/>
  <description>Compositor Windows image editor</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
      <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
      <supportedOS Id="{4a2f28e3-53b9-4441-ba9c-d69d4a4a6e38}"/>
      <supportedOS Id="{35138b9a-5d96-4fbd-8e2d-a2440225f93a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
"#
        .replace("@APP_VERSION@", &version),
    )
    .expect("Write Windows application manifest");
    fs::write(
        &source,
        format!(
            r#"#pragma code_page(65001)
1 ICON "{icon}"
1 24 "{manifest_path}"
1 VERSIONINFO
FILEVERSION {numeric_version},0
PRODUCTVERSION {numeric_version},0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "Compositor Windows image editor\0"
      VALUE "FileVersion", "{version}\0"
      VALUE "InternalName", "compositor\0"
      VALUE "LegalCopyright", "See Help > Open-source software licenses (开源软件许可证).\0"
      VALUE "OriginalFilename", "compositor.exe\0"
      VALUE "ProductName", "Compositor\0"
      VALUE "ProductVersion", "{version}\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#
        ),
    )
    .expect("Write Windows resources");
    let compiler = resource_compiler();
    let result = Command::new(&compiler)
        .arg("/nologo")
        .arg("/c65001")
        .arg("/fo")
        .arg(&resource)
        .arg(&source)
        .output()
        .unwrap_or_else(|error| panic!("Run Windows SDK {}: {error}", compiler.display()));
    assert!(
        result.status.success(),
        "Windows resource compiler failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    println!("cargo:rustc-link-arg-bin=compositor={}", resource.display());
}

fn rc_path(path: &Path) -> String {
    path.to_str()
        .expect("Windows resource path must be Unicode")
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn resource_compiler() -> PathBuf {
    if let Some(path) = env::var_os("RC") {
        return path.into();
    }
    if let Some(path) = env::var_os("PATH").and_then(|value| {
        env::split_paths(&value)
            .map(|directory| directory.join("rc.exe"))
            .find(|candidate| candidate.is_file())
    }) {
        return path;
    }
    let architecture = match env::var("HOST").unwrap_or_default().split('-').next() {
        Some("aarch64") => "arm64",
        Some("i686") => "x86",
        _ => "x64",
    };
    if let Some(directory) = env::var_os("WindowsSdkVerBinPath") {
        let path = PathBuf::from(directory).join(architecture).join("rc.exe");
        if path.is_file() {
            return path;
        }
    }
    let sdk = env::var_os("WindowsSdkDir")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("ProgramFiles(x86)").map(|path| PathBuf::from(path).join("Windows Kits/10"))
        })
        .expect("Windows SDK is required: install its tools or set RC to rc.exe");
    let mut versions: Vec<_> = fs::read_dir(sdk.join("bin"))
        .expect("Read Windows SDK tools directory")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let parts: Option<Vec<u32>> = name.split('.').map(|part| part.parse().ok()).collect();
            let path = entry.path().join(architecture).join("rc.exe");
            parts.filter(|_| path.is_file()).map(|parts| (parts, path))
        })
        .collect();
    versions.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    versions
        .pop()
        .expect("Windows SDK rc.exe is missing: install its tools or set RC")
        .1
}
