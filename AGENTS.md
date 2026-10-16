# Notes for AI agents

Standalone Rust/WGSL Windows port of Compositor. Use the root Cargo project.

- Build: `project.ps1 Build -Action All -Offline -Jobs 1`; on this host set `RUST_TEST_THREADS=1` and use the installed Cargo/Rustup under `D:\DevTools\Rust` if they are absent from PATH. Run `project.ps1 Help` for the unified developer commands.
- Application: `src/`; assets: `assets/`; tests: `tests/`; benchmarks: `examples/`. Generated files belong in `build/`, releases in `dist/`.
- Preserve both model assets and `examples/round2_legacy/`; tests, model conversion and benchmarks directly use them.
- `examples/prepare-model.rs` is an optional Cargo example sharing the main lock file, not an application dependency.
- Save `.comp` as one file (container version 1, embedded manifest version 11); continue reading legacy directory packages with manifest versions 1–11. Legacy directories are read-only targets: save to a new file path. See `docs/PROJECT_FORMAT.md`; bump the format only when saved semantics require it.
- UI is Simplified Chinese; persisted canonical values stay unchanged. Match surrounding naming and comments; use American spelling in code.
- The main application uses Apache-2.0 (`LICENSE.md`). Original project and third-party copyright and license originals remain in `LICENSES.md`. Preserve their original bytes and component attribution; do not relabel third-party licenses.
- `project.ps1` is the sole developer script. Audit a new Release with `AuditRuntime`, then `Build -Action Package` and `VerifyPackage`; derive the release guide from the marked user section of `README.md`. Packaging sources live in `packaging/`, documentation in `docs/`, and Agent skills in `skills/`.
- `build.rs` embeds the Windows icon and version with the Windows SDK resource compiler; `project.ps1 GenerateIcon` rebuilds the ICO from the original artwork and needs Pillow only during development.
- Build the optional installer with `project.ps1 PackageInstaller` and official NSIS 3.11 after validating the portable payload. Keep its fixed current-user path and application ID and explicit owned-file removal; never add recursive directory cleanup, user-data removal or broad registry deletion. Preserve user projects, preferences, modified files and unrelated shortcuts.
- Installer versions come from Cargo and must match the packaged EXE. Verify genuine old-to-new installation upgrades, refusal before replacement when any owned file is busy, rollback on commit failure, and preservation of user data. Keep the installation path and application ID stable; an obsolete uninstaller must refuse a newer installation.
- Production builds do not compile or invoke the original Swift/C app. Optional C reference regeneration accepts an explicit original-project path.
- Earlier migration/optimization evidence is archived outside the project. Preserve its recorded artifact identities and historical paths.
