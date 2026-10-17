# Notes for AI agents

Standalone Rust/WGSL Windows port of Compositor. Use the root Cargo project.

- Build: `build.ps1 -Action All -Offline -Jobs 1`; on this host set `RUST_TEST_THREADS=1` and use the installed Cargo/Rustup under `D:\DevTools\Rust` if they are absent from PATH.
- Application: `src/`; assets: `assets/`; tests: `tests/`; benchmarks: `examples/`. Generated files belong in `build/`, releases in `dist/`.
- Preserve both model assets and `examples/round2_legacy/`; tests, model conversion and benchmarks directly use them.
- `prepare-model.rs` is an optional Cargo example sharing the main lock file, not an application dependency.
- Save `.comp` as one file (container version 1, embedded manifest version 11); continue reading legacy directory packages with manifest versions 1–11. Legacy directories are read-only targets: save to a new file path. See `PROJECT_FORMAT.md`; bump the format only when saved semantics require it.
- UI is Simplified Chinese; persisted canonical values stay unchanged. Match surrounding naming and comments; use American spelling in code.
- CompositorWin and new Windows-port code use Apache-2.0; the official text is in `LICENSE.md`. The original Compositor remains MIT. `LICENSES.md` aggregates the application and third-party originals; preserve their original bytes and component attribution, without relabeling upstream or third-party licenses.
- Build and packaging scripts live at the root. Audit a new Release, then package and run `verify-package.ps1`; derive the release guide from the marked user section of `README.md`.
- `build.rs` embeds the Windows icon and version with the Windows SDK resource compiler; `generate-icon.py` rebuilds the ICO from the original artwork and needs Pillow only during development.
- Build the optional installer with `package-installer.ps1` and official NSIS 3.11 after validating the portable payload. Keep its fixed current-user path and explicit owned-file removal; never add recursive directory cleanup, user-data removal or broad registry deletion. Preserve user projects, preferences, modified files and unrelated shortcuts.
- Installer versions come from Cargo and must match the packaged EXE. Verify genuine old-to-new installation upgrades, refusal before replacement when any owned file is busy, rollback on commit failure, and preservation of user data. Keep the installation path and application ID stable; an obsolete uninstaller must refuse a newer installation.
- Production builds do not compile or invoke the original Swift/C app. Optional C reference regeneration accepts an explicit original-project path.
- Earlier migration/optimization evidence is archived outside the project. Preserve its recorded artifact identities and historical paths.
