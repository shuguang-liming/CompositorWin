#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

type SharedEditor = Rc<RefCell<Option<compositor::app::EditorApp>>>;

#[derive(Default)]
struct GraphicsRecovery {
    lost: AtomicBool,
    device: Mutex<Option<wgpu::Device>>,
}

impl GraphicsRecovery {
    fn watch(self: &Arc<Self>, state: &egui_wgpu::RenderState, ctx: &eframe::egui::Context) {
        *self
            .device
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(state.device.clone());
        let recovery = Arc::downgrade(self);
        let context = ctx.clone();
        state
            .device
            .set_device_lost_callback(move |reason, message| {
                if let Some(recovery) = recovery.upgrade() {
                    log::error!("GUI graphics device lost: {reason:?}: {message}");
                    recovery.lost.store(true, Ordering::Release);
                    context.request_repaint();
                }
            });
    }

    fn poll(&self) {
        if self.lost.load(Ordering::Acquire) {
            return;
        }
        let device = self
            .device
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(device) = device {
            // Also delivers device-loss callbacks if the renderer failed before
            // its normal queue submission could poll the device.
            let _ = device.poll(wgpu::PollType::Poll);
        }
    }
}

struct RetainedEditor {
    editor: SharedEditor,
    graphics: Arc<GraphicsRecovery>,
}

impl eframe::App for RetainedEditor {
    fn update(&mut self, ctx: &eframe::egui::Context, frame: &mut eframe::Frame) {
        self.graphics.poll();
        if self.graphics.lost.load(Ordering::Acquire) {
            // Close only the failed renderer. The editor stays owned by main,
            // including pending file operations and unsaved document history.
            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
            return;
        }
        if let Some(editor) = self.editor.borrow_mut().as_mut() {
            editor.update(ctx, frame);
        }
    }

    fn raw_input_hook(&mut self, ctx: &eframe::egui::Context, input: &mut eframe::egui::RawInput) {
        if let Some(editor) = self.editor.borrow_mut().as_mut() {
            editor.raw_input_hook(ctx, input);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some(editor) = self.editor.borrow_mut().as_mut() {
            editor.save(storage);
        }
    }

    fn clear_color(&self, visuals: &eframe::egui::Visuals) -> [f32; 4] {
        self.editor.borrow().as_ref().map_or_else(
            || visuals.panel_fill.to_normalized_gamma_f32(),
            |editor| editor.clear_color(visuals),
        )
    }
}

fn main() -> eframe::Result {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().is_some_and(|argument| argument == "cli") {
        compositor::cli::attach_console();
        std::process::exit(compositor::cli::run(arguments.collect()));
    }
    compositor::diagnostics::initialize(&compositor::app::preferences_directory());
    let restore_maximized = std::rc::Rc::new(std::cell::Cell::new(false));
    let application_created = std::rc::Rc::new(std::cell::Cell::new(false));
    let editor = Rc::new(RefCell::new(None));
    let requested_renderer = std::env::var("COMPOSITOR_RENDERER").ok();
    let (renderer, result, graphics) = run_startup_candidates(
        compatible_wgpu_options(),
        requested_renderer.as_deref(),
        restore_maximized.clone(),
        application_created.clone(),
        editor.clone(),
    );
    if should_retry_with_glow(renderer, result.is_err(), application_created.get())
        || (matches!(renderer, eframe::Renderer::Wgpu) && graphics.lost.load(Ordering::Acquire))
    {
        // eframe 0.33 keeps a thread-local event loop when run_and_return is
        // enabled. Reusing it allows one startup retry without starting another
        // process. Device-loss recovery retains the entire existing editor;
        // unrelated errors in a live session still propagate.
        log::error!("WGPU stopped; retrying OpenGL with the retained editor: {result:?}");
        let retry = catch_recovery_renderer(|| {
            run_gui(
                eframe::Renderer::Glow,
                compatible_wgpu_options(),
                restore_maximized,
                application_created,
                editor.clone(),
                Arc::new(GraphicsRecovery::default()),
            )
        });
        if retry.is_err()
            && let Some(editor) = editor.borrow_mut().as_mut()
        {
            match editor.save_recovery_projects() {
                Ok(path) => {
                    let message = format!(
                        "显卡渲染恢复失败。项目恢复副本已保存到：\n{}",
                        path.display()
                    );
                    log::error!("{message}");
                    rfd::MessageDialog::new()
                        .set_title("Compositor 项目恢复")
                        .set_description(message)
                        .show();
                }
                Err(error) => {
                    log::error!("Saving renderer recovery copies failed: {error:#}");
                    rfd::MessageDialog::new()
                        .set_title("Compositor 项目恢复")
                        .set_description(format!(
                            "显卡渲染恢复失败。保存恢复副本时发生错误：\n{error:#}"
                        ))
                        .show();
                }
            }
        }
        return retry;
    }
    result
}

fn should_retry_with_glow(
    renderer: eframe::Renderer,
    failed: bool,
    application_created: bool,
) -> bool {
    matches!(renderer, eframe::Renderer::Wgpu) && failed && !application_created
}

fn run_gui(
    renderer: eframe::Renderer,
    wgpu_options: egui_wgpu::WgpuConfiguration,
    restore_maximized: std::rc::Rc<std::cell::Cell<bool>>,
    application_created: std::rc::Rc<std::cell::Cell<bool>>,
    editor: SharedEditor,
    graphics: Arc<GraphicsRecovery>,
) -> eframe::Result {
    catch_gui_device_failure(renderer, &graphics, || {
        catch_gui_startup(renderer, &application_created, || {
            run_gui_inner(
                renderer,
                wgpu_options,
                restore_maximized,
                application_created.clone(),
                editor,
                graphics.clone(),
            )
        })
    })
}

fn catch_gui_device_failure(
    renderer: eframe::Renderer,
    graphics: &GraphicsRecovery,
    run: impl FnOnce() -> eframe::Result,
) -> eframe::Result {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(result) => result,
        Err(payload) => {
            // Preserve the original panic if polling the failed driver also
            // panics. Once loss is confirmed, no further driver call is needed.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| graphics.poll()));
            if matches!(renderer, eframe::Renderer::Wgpu) && graphics.lost.load(Ordering::Acquire) {
                Err(eframe::Error::AppCreation(
                    std::io::Error::other("GUI graphics device lost").into(),
                ))
            } else {
                std::panic::resume_unwind(payload)
            }
        }
    }
}

fn catch_recovery_renderer(run: impl FnOnce() -> eframe::Result) -> eframe::Result {
    // Only the already-authorized recovery attempt uses this boundary. If GL
    // initialization also panics, return to main so it can back up the retained
    // documents before reporting the failure.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| {
        Err(eframe::Error::AppCreation(
            std::io::Error::other("The replacement renderer panicked").into(),
        ))
    })
}

fn catch_gui_startup(
    renderer: eframe::Renderer,
    application_created: &std::cell::Cell<bool>,
    run: impl FnOnce() -> eframe::Result,
) -> eframe::Result {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(result) => result,
        Err(payload) => {
            if should_retry_with_glow(renderer, true, application_created.get()) {
                let message = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("unknown panic payload");
                log::error!("WGPU initialization panicked before editor creation: {message}");
                Err(eframe::Error::AppCreation(
                    std::io::Error::other(format!("WGPU startup panic: {message}")).into(),
                ))
            } else {
                // A live editor may contain unsaved documents. Only startup
                // failures qualify for the existing one-time renderer retry.
                std::panic::resume_unwind(payload)
            }
        }
    }
}

fn run_gui_inner(
    renderer: eframe::Renderer,
    wgpu_options: egui_wgpu::WgpuConfiguration,
    restore_maximized: std::rc::Rc<std::cell::Cell<bool>>,
    application_created: std::rc::Rc<std::cell::Cell<bool>>,
    editor: SharedEditor,
    graphics: Arc<GraphicsRecovery>,
) -> eframe::Result {
    let requested_maximized = restore_maximized.clone();
    let options = eframe::NativeOptions {
        renderer,
        // eframe reuses this thread's event loop for the startup fallback.
        run_and_return: true,
        // egui-wgpu's default asks for an 8192x8192 texture even when the
        // adapter only advertises the Vulkan/DX12 minimum of 4096. Older
        // Windows 10 drivers reject that device request before the window is
        // usable. Clamp only the resolution limits to the selected adapter;
        // all other required limits remain unchanged.
        wgpu_options,
        persistence_path: Some(compositor::app::preferences_directory().join("window.ron")),
        window_builder: Some(Box::new(move |viewport| {
            defer_window_maximization(viewport, &requested_maximized)
        })),
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Compositor 图像编辑器")
            .with_icon(original_app_icon())
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([800.0, 520.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Compositor",
        options,
        Box::new(move |cc| {
            if let Some(state) = &cc.wgpu_render_state {
                graphics.watch(state, &cc.egui_ctx);
            }
            let mut retained = editor.borrow_mut();
            let app = if let Some(app) = retained.as_mut() {
                app.recover_renderer(cc);
                app
            } else {
                retained.insert(compositor::app::EditorApp::new(cc))
            };
            if restore_maximized.get() {
                app.defer_maximized_restore(&cc.egui_ctx);
            }
            application_created.set(true);
            drop(retained);
            Ok(Box::new(RetainedEditor {
                editor: editor.clone(),
                graphics: graphics.clone(),
            }))
        }),
    );
    if let Err(error) = &result {
        log::error!("GUI stopped with error: {error}");
    } else {
        log::info!("GUI closed normally");
    }
    result
}

// Prepare each device once and pass its exact resources to the native renderer.
fn run_startup_candidates(
    configuration: egui_wgpu::WgpuConfiguration,
    requested_renderer: Option<&str>,
    restore_maximized: Rc<std::cell::Cell<bool>>,
    application_created: Rc<std::cell::Cell<bool>>,
    editor: SharedEditor,
) -> (eframe::Renderer, eframe::Result, Arc<GraphicsRecovery>) {
    let mut graphics = Arc::new(GraphicsRecovery::default());
    let attempted = try_prepared_candidates(
        uses_wgpu_candidates(requested_renderer),
        || wgpu_startup_candidates(configuration.clone()),
        &application_created,
        |prepared| {
            // A failed startup may have installed a loss callback before app
            // construction failed. Never carry that old callback into another card.
            graphics = Arc::new(GraphicsRecovery::default());
            let result = run_gui(
                eframe::Renderer::Wgpu,
                prepared,
                restore_maximized.clone(),
                application_created.clone(),
                editor.clone(),
                graphics.clone(),
            );
            if should_retry_with_glow(
                eframe::Renderer::Wgpu,
                result.is_err(),
                application_created.get(),
            ) {
                log::warn!("WGPU candidate failed before editor creation; trying the next adapter");
            }
            result
        },
    );
    if let Some(result) = attempted {
        // If every prepared adapter failed before app creation, return the last
        // error to main's existing Glow fallback. A live result returns unchanged.
        return (eframe::Renderer::Wgpu, result, graphics);
    }
    log::info!("Using OpenGL: explicitly requested or no WGPU device could be prepared");
    let result = run_gui(
        eframe::Renderer::Glow,
        configuration,
        restore_maximized,
        application_created,
        editor,
        graphics.clone(),
    );
    (eframe::Renderer::Glow, result, graphics)
}

fn uses_wgpu_candidates(requested_renderer: Option<&str>) -> bool {
    !requested_renderer.is_some_and(|value| {
        value.eq_ignore_ascii_case("glow") || value.eq_ignore_ascii_case("opengl")
    })
}

fn try_prepared_candidates<C, E, I>(
    enabled: bool,
    prepare: impl FnOnce() -> I,
    application_created: &std::cell::Cell<bool>,
    mut start: impl FnMut(C) -> Result<(), E>,
) -> Option<Result<(), E>>
where
    I: IntoIterator<Item = C>,
{
    if !enabled {
        return None;
    }
    let mut last_failure = None;
    for prepared in prepare() {
        let result = start(prepared);
        if result.is_ok() || application_created.get() {
            return Some(result);
        }
        last_failure = Some(result);
    }
    last_failure
}

fn prepare_wgpu_candidate<T>(operation: impl FnOnce() -> Result<T, String>) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => {
            log::warn!("WGPU candidate rejected: {error}");
            None
        }
        Err(_) => {
            log::warn!("WGPU candidate panicked; continuing with the remaining adapters");
            None
        }
    }
}

fn adapter_priority(kind: wgpu::DeviceType, preference: wgpu::PowerPreference) -> u8 {
    if preference == wgpu::PowerPreference::None {
        return 0;
    }
    match (kind, preference) {
        (wgpu::DeviceType::IntegratedGpu, wgpu::PowerPreference::LowPower)
        | (wgpu::DeviceType::DiscreteGpu, wgpu::PowerPreference::HighPerformance) => 1,
        (wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu, _) => 2,
        (wgpu::DeviceType::Other, _) => 3,
        (wgpu::DeviceType::VirtualGpu, _) => 4,
        (wgpu::DeviceType::Cpu, _) => 5,
    }
}

fn wgpu_startup_candidates(
    configuration: egui_wgpu::WgpuConfiguration,
) -> Box<dyn Iterator<Item = egui_wgpu::WgpuConfiguration>> {
    let setup = match &configuration.wgpu_setup {
        egui_wgpu::WgpuSetup::Existing(_) => return Box::new(std::iter::once(configuration)),
        egui_wgpu::WgpuSetup::CreateNew(setup) => setup.clone(),
    };
    let Some((instance, adapters)) = prepare_wgpu_candidate(|| {
        let instance = pollster::block_on(configuration.wgpu_setup.new_instance());
        let adapters = instance.enumerate_adapters(setup.instance_descriptor.backends);
        Ok((instance, adapters))
    }) else {
        return Box::new(std::iter::empty());
    };
    let mut ordered: Vec<_> = adapters
        .into_iter()
        .filter_map(|adapter| {
            let info = prepare_wgpu_candidate(|| Ok(adapter.get_info()))?;
            Some((
                adapter_priority(info.device_type, setup.power_preference),
                info,
                adapter,
            ))
        })
        .collect();
    // Stable ordering keeps backend enumeration order within the same GPU class.
    ordered.sort_by_key(|(priority, _, _)| *priority);
    Box::new(ordered.into_iter().filter_map(move |(_, info, adapter)| {
        // This is lazy: never request a later device before the current candidate
        // was rejected by eframe's actual window surface or renderer initialization.
        let (device, queue) = prepare_wgpu_candidate(|| {
            let descriptor = (setup.device_descriptor)(&adapter);
            pollster::block_on(adapter.request_device(&descriptor))
                .map_err(|error| format!("{:?} {}: {error}", info.backend, info.name))
        })?;
        log::info!(
            "Prepared WGPU device for actual GUI use: {:?} {}",
            info.backend,
            info.name
        );
        let mut prepared = configuration.clone();
        prepared.wgpu_setup = egui_wgpu::WgpuSetup::Existing(egui_wgpu::WgpuSetupExisting {
            instance: instance.clone(),
            adapter,
            device,
            queue,
        });
        Some(prepared)
    }))
}

fn compatible_wgpu_options() -> egui_wgpu::WgpuConfiguration {
    let defaults = egui_wgpu::WgpuConfiguration::default();
    let default_on_surface_error = defaults.on_surface_error.clone();
    let mut configuration = egui_wgpu::WgpuConfiguration {
        on_surface_error: Arc::new(move |error| {
            if error == wgpu::SurfaceError::Lost {
                // Reconfigure the existing window without replacing the editor
                // or its unsaved documents. Preserve eframe's other policies.
                log::warn!("Window surface lost; reconfiguring the existing surface");
                egui_wgpu::SurfaceErrorAction::RecreateSurface
            } else {
                default_on_surface_error(error)
            }
        }),
        ..defaults
    };
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut configuration.wgpu_setup {
        let compiled_backends = wgpu::Backends::DX12 | wgpu::Backends::VULKAN;
        let requested_backends = setup.instance_descriptor.backends;
        let selected_backends = compiled_wgpu_backends(requested_backends);
        if (requested_backends & compiled_backends).is_empty() {
            log::warn!(
                "WGPU_BACKEND={requested_backends:?} is unavailable in this build; using DX12/Vulkan"
            );
        }
        setup.instance_descriptor.backends = selected_backends;
        let descriptor = setup.device_descriptor.clone();
        setup.device_descriptor = Arc::new(move |adapter| {
            let mut device = descriptor(adapter);
            let supported = adapter.limits();
            device.required_limits =
                clamp_resolution_limits(device.required_limits, supported.clone());
            let info = adapter.get_info();
            log::info!(
                "wgpu adapter backend={:?} name={} max_texture_dimension_2d={}",
                info.backend,
                info.name,
                supported.max_texture_dimension_2d
            );
            device
        });
    }
    configuration
}

fn clamp_resolution_limits(required: wgpu::Limits, supported: wgpu::Limits) -> wgpu::Limits {
    required.using_resolution(supported)
}

fn compiled_wgpu_backends(requested: wgpu::Backends) -> wgpu::Backends {
    let compiled = wgpu::Backends::DX12 | wgpu::Backends::VULKAN;
    let selected = requested & compiled;
    if selected.is_empty() {
        compiled
    } else {
        selected
    }
}

fn defer_window_maximization(
    mut viewport: eframe::egui::ViewportBuilder,
    restore_maximized: &std::cell::Cell<bool>,
) -> eframe::egui::ViewportBuilder {
    // winit calls ShowWindow(SW_MAXIMIZE) during Windows window creation, even while hidden.
    if cfg!(windows) && viewport.maximized == Some(true) {
        restore_maximized.set(true);
        viewport.maximized = Some(false);
    }
    viewport
}

fn original_app_icon() -> eframe::egui::IconData {
    let image = image::load_from_memory(include_bytes!("../assets/app-icon-128.png"))
        .expect("The embedded original Compositor icon is a valid PNG")
        .into_rgba8();
    let (width, height) = image.dimensions();
    eframe::egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_renderer_panic_returns_to_the_document_backup_path() {
        assert!(catch_recovery_renderer(|| panic!("No compatible GL configuration")).is_err());
        assert!(catch_recovery_renderer(|| Ok(())).is_ok());
    }

    #[test]
    fn only_confirmed_wgpu_device_loss_converts_a_live_renderer_panic_to_recovery() {
        let graphics = GraphicsRecovery::default();
        graphics.lost.store(true, Ordering::Release);
        assert!(
            catch_gui_device_failure(eframe::Renderer::Wgpu, &graphics, || panic!(
                "lost device paint"
            ))
            .is_err()
        );
        for (renderer, lost) in [
            (eframe::Renderer::Wgpu, false),
            (eframe::Renderer::Glow, true),
        ] {
            graphics.lost.store(lost, Ordering::Release);
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    catch_gui_device_failure(renderer, &graphics, || panic!("unrelated failure"))
                }))
                .is_err()
            );
        }
    }

    #[test]
    fn startup_panic_reaches_fallback_but_live_editor_and_glow_panics_propagate() {
        let created = std::cell::Cell::new(false);
        let result = catch_gui_startup(eframe::Renderer::Wgpu, &created, || {
            panic!("adapter startup")
        });
        assert!(matches!(result, Err(eframe::Error::AppCreation(_))));
        for (renderer, app_created) in [
            (eframe::Renderer::Wgpu, true),
            (eframe::Renderer::Glow, false),
        ] {
            created.set(app_created);
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                catch_gui_startup(renderer, &created, || panic!("preserved panic"))
            }));
            assert!(panic.is_err());
        }
    }

    #[test]
    fn graphics_fallback_only_retries_a_failed_wgpu_startup() {
        assert!(should_retry_with_glow(eframe::Renderer::Wgpu, true, false));
        assert!(!should_retry_with_glow(
            eframe::Renderer::Wgpu,
            false,
            false
        ));
        assert!(!should_retry_with_glow(eframe::Renderer::Wgpu, true, true));
        assert!(!should_retry_with_glow(eframe::Renderer::Glow, true, false));
    }

    #[test]
    fn lost_window_surface_requests_reconfiguration_without_restarting_the_editor() {
        let configuration = compatible_wgpu_options();
        for _ in 0..2 {
            assert!(matches!(
                (configuration.on_surface_error)(wgpu::SurfaceError::Lost),
                egui_wgpu::SurfaceErrorAction::RecreateSurface
            ));
        }
        for error in [wgpu::SurfaceError::Timeout, wgpu::SurfaceError::Outdated] {
            assert!(matches!(
                (configuration.on_surface_error)(error),
                egui_wgpu::SurfaceErrorAction::SkipFrame
            ));
        }
    }

    #[test]
    fn maximize_restore_preserves_saved_geometry_and_other_window_options() {
        let requested = std::cell::Cell::new(false);
        let viewport = eframe::egui::ViewportBuilder::default()
            .with_title("Compositor")
            .with_position([-8.0, -8.0])
            .with_inner_size([1920.0, 1057.0])
            .with_maximized(true)
            .with_fullscreen(false);
        let restored = defer_window_maximization(viewport.clone(), &requested);
        assert_eq!(requested.get(), cfg!(windows));
        assert_eq!(restored.maximized, Some(!cfg!(windows)));
        assert_eq!(restored.position, viewport.position);
        assert_eq!(restored.inner_size, viewport.inner_size);
        assert_eq!(restored.title, viewport.title);
        assert_eq!(restored.fullscreen, viewport.fullscreen);

        for maximized in [None, Some(false)] {
            let requested = std::cell::Cell::new(false);
            let restored = defer_window_maximization(
                eframe::egui::ViewportBuilder {
                    maximized,
                    ..Default::default()
                },
                &requested,
            );
            assert!(!requested.get());
            assert_eq!(restored.maximized, maximized);
        }
    }

    #[test]
    fn wgpu_resolution_requirements_follow_a_4096_adapter() {
        let required = wgpu::Limits {
            max_texture_dimension_2d: 8192,
            max_texture_dimension_1d: 8192,
            max_texture_dimension_3d: 2048,
            max_uniform_buffers_per_shader_stage: 12,
            ..Default::default()
        };
        let supported = wgpu::Limits {
            max_texture_dimension_2d: 4096,
            max_texture_dimension_1d: 4096,
            max_texture_dimension_3d: 1024,
            ..Default::default()
        };
        let clamped = clamp_resolution_limits(required.clone(), supported);
        assert_eq!(clamped.max_texture_dimension_2d, 4096);
        assert_eq!(clamped.max_texture_dimension_1d, 4096);
        assert_eq!(clamped.max_texture_dimension_3d, 1024);
        assert_eq!(
            clamped.max_uniform_buffers_per_shader_stage,
            required.max_uniform_buffers_per_shader_stage
        );
    }

    #[test]
    fn unsupported_backend_environment_cannot_disable_all_compiled_backends() {
        assert_eq!(
            compiled_wgpu_backends(wgpu::Backends::GL),
            wgpu::Backends::DX12 | wgpu::Backends::VULKAN
        );
        assert_eq!(
            compiled_wgpu_backends(wgpu::Backends::VULKAN),
            wgpu::Backends::VULKAN
        );
    }
}

#[cfg(test)]
mod startup_adapter_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    #[ignore = "Requires a real supported graphics adapter"]
    fn prepared_graphics_resources_are_exactly_reused_by_the_runtime_renderer() {
        let configuration = wgpu_startup_candidates(compatible_wgpu_options())
            .next()
            .expect("This explicit hardware QA needs a usable GPU");
        let egui_wgpu::WgpuSetup::Existing(existing) = configuration.wgpu_setup.clone() else {
            panic!("The prepared candidate lost its opened graphics resources");
        };
        let instance = pollster::block_on(configuration.wgpu_setup.new_instance());
        assert_eq!(instance, existing.instance);
        let state = pollster::block_on(egui_wgpu::RenderState::create(
            &configuration,
            &instance,
            None,
            egui_wgpu::RendererOptions::default(),
        ))
        .expect("The actual renderer must accept the prepared device");
        assert_eq!(state.adapter, existing.adapter);
        assert_eq!(state.device, existing.device);
        assert_eq!(state.queue, existing.queue);
    }

    #[test]
    fn forced_glow_never_enumerates_or_requests_any_adapter() {
        for value in ["glow", "OpenGL", "GLOW"] {
            let result: Option<Result<(), &str>> = try_prepared_candidates(
                uses_wgpu_candidates(Some(value)),
                || -> Vec<u8> { panic!("A forced Glow launch must not probe") },
                &Cell::new(false),
                |_| panic!("A forced Glow launch must not start WGPU"),
            );
            assert!(result.is_none());
        }
    }

    #[test]
    fn surface_startup_failure_tries_the_next_prepared_candidate_lazily() {
        let prepared = Rc::new(Cell::new(0));
        let observed = prepared.clone();
        let mut starts = Vec::new();
        let result = try_prepared_candidates(
            true,
            || {
                (1..=3).map(move |candidate| {
                    observed.set(observed.get() + 1);
                    (candidate, format!("opaque-device-{candidate}"))
                })
            },
            &Cell::new(false),
            |(candidate, device)| {
                assert_eq!(prepared.get(), candidate);
                assert_eq!(device, format!("opaque-device-{candidate}"));
                starts.push(candidate);
                if candidate == 1 {
                    Err("Incompatible actual surface")
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result, Some(Ok(())));
        assert_eq!(starts, [1, 2]);
        assert_eq!(
            prepared.get(),
            2,
            "The unused third adapter must never be requested"
        );
    }

    #[test]
    fn every_pre_editor_startup_failure_returns_the_last_error_to_existing_glow_fallback() {
        let result = try_prepared_candidates(true, || [1, 2], &Cell::new(false), Err);
        assert_eq!(result, Some(Err(2)));
        assert!(should_retry_with_glow(
            eframe::Renderer::Wgpu,
            result.unwrap().is_err(),
            false
        ));
    }

    #[test]
    fn live_editor_errors_never_restart_with_another_adapter() {
        let created = Cell::new(false);
        let mut starts = 0;
        let result = try_prepared_candidates(
            true,
            || [1, 2],
            &created,
            |_| {
                starts += 1;
                created.set(true);
                Err("Live error")
            },
        );
        assert_eq!(result, Some(Err("Live error")));
        assert_eq!(starts, 1);
        assert!(!should_retry_with_glow(
            eframe::Renderer::Wgpu,
            true,
            created.get()
        ));
    }

    #[test]
    fn live_panics_propagate_without_trying_another_adapter() {
        let created = Cell::new(true);
        let starts = Cell::new(0);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Option<Result<(), ()>> = try_prepared_candidates(
                true,
                || [1, 2],
                &created,
                |_| {
                    starts.set(starts.get() + 1);
                    panic!("Original runtime panic")
                },
            );
        }));
        assert!(result.is_err());
        assert_eq!(starts.get(), 1);
    }

    #[test]
    fn device_request_error_or_panic_cannot_hide_the_remaining_usable_adapter() {
        let opened: Vec<_> = [0, 1, 2]
            .into_iter()
            .filter_map(|candidate| {
                prepare_wgpu_candidate(|| match candidate {
                    0 => Err("Device rejected".into()),
                    1 => panic!("Driver panic in one adapter"),
                    _ => Ok((candidate, "the actual opened device")),
                })
            })
            .collect();
        assert_eq!(opened, [(2, "the actual opened device")]);
    }

    #[test]
    fn adapter_order_matches_the_configured_power_preference_and_is_stable() {
        use wgpu::{DeviceType::*, PowerPreference::*};
        let original = [
            Cpu,
            IntegratedGpu,
            DiscreteGpu,
            VirtualGpu,
            Other,
            DiscreteGpu,
        ];
        let mut high = original;
        high.sort_by_key(|kind| adapter_priority(*kind, HighPerformance));
        assert_eq!(
            high,
            [
                DiscreteGpu,
                DiscreteGpu,
                IntegratedGpu,
                Other,
                VirtualGpu,
                Cpu
            ]
        );
        let mut low = original;
        low.sort_by_key(|kind| adapter_priority(*kind, LowPower));
        assert_eq!(
            low,
            [
                IntegratedGpu,
                DiscreteGpu,
                DiscreteGpu,
                Other,
                VirtualGpu,
                Cpu
            ]
        );
        let mut none = original;
        none.sort_by_key(|kind| adapter_priority(*kind, wgpu::PowerPreference::None));
        assert_eq!(none, original);
    }
}
