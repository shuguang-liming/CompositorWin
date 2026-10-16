#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

fn main() -> eframe::Result {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().is_some_and(|argument| argument == "cli") {
        compositor::cli::attach_console();
        std::process::exit(compositor::cli::run(arguments.collect()));
    }
    compositor::diagnostics::initialize(&compositor::app::preferences_directory());
    let restore_maximized = std::rc::Rc::new(std::cell::Cell::new(false));
    let application_created = std::rc::Rc::new(std::cell::Cell::new(false));
    let wgpu_options = compatible_wgpu_options();
    let renderer = select_renderer(&wgpu_options);
    let result = run_gui(
        renderer,
        wgpu_options,
        restore_maximized.clone(),
        application_created.clone(),
    );
    if should_retry_with_glow(renderer, result.is_err(), application_created.get()) {
        // eframe 0.33 keeps a thread-local event loop when run_and_return is
        // enabled. Reusing it allows one startup retry without starting another
        // process or losing the original file-opening arguments. Never retry
        // after EditorApp has been created: a later error may belong to a live,
        // unsaved editing session.
        log::error!(
            "WGPU window initialization failed; retrying with OpenGL: {}",
            result.as_ref().unwrap_err()
        );
        return run_gui(
            eframe::Renderer::Glow,
            compatible_wgpu_options(),
            restore_maximized,
            application_created,
        );
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
) -> eframe::Result {
    catch_gui_startup(renderer, &application_created, || {
        run_gui_inner(
            renderer,
            wgpu_options,
            restore_maximized,
            application_created.clone(),
        )
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
            let mut app = compositor::app::EditorApp::new(cc);
            if restore_maximized.get() {
                app.defer_maximized_restore(&cc.egui_ctx);
            }
            application_created.set(true);
            Ok(Box::new(app))
        }),
    );
    if let Err(error) = &result {
        log::error!("GUI stopped with error: {error}");
    } else {
        log::info!("GUI closed normally");
    }
    result
}

fn select_renderer(configuration: &egui_wgpu::WgpuConfiguration) -> eframe::Renderer {
    match std::env::var("COMPOSITOR_RENDERER")
        .ok()
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("glow") | Some("opengl") => {
            log::info!("renderer override: OpenGL");
            return eframe::Renderer::Glow;
        }
        Some("wgpu") => return eframe::Renderer::Wgpu,
        _ => {}
    }
    if probe_wgpu(configuration) {
        eframe::Renderer::Wgpu
    } else {
        log::warn!("DX12/Vulkan device probe failed; falling back to OpenGL renderer");
        eframe::Renderer::Glow
    }
}

/// Probe every compiled adapter before eframe creates a visible window. Older
/// Windows 10 drivers can expose a Vulkan adapter that rejects the default
/// device request; eframe otherwise tries one adapter and exits immediately.
/// The probe lets the application keep its CPU compositing path through the
/// OpenGL renderer in that case.
fn probe_wgpu(configuration: &egui_wgpu::WgpuConfiguration) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        probe_wgpu_inner(configuration)
    })) {
        Ok(result) => result,
        Err(_) => {
            log::error!("WGPU adapter probe panicked; using OpenGL renderer");
            false
        }
    }
}

fn probe_wgpu_inner(configuration: &egui_wgpu::WgpuConfiguration) -> bool {
    let egui_wgpu::WgpuSetup::CreateNew(setup) = &configuration.wgpu_setup else {
        return true;
    };
    let instance = pollster::block_on(configuration.wgpu_setup.new_instance());
    let adapters = instance.enumerate_adapters(setup.instance_descriptor.backends);
    for adapter in adapters {
        let info = adapter.get_info();
        let descriptor = (setup.device_descriptor)(&adapter);
        match pollster::block_on(adapter.request_device(&descriptor)) {
            Ok((_device, _queue)) => {
                log::info!(
                    "wgpu device probe succeeded: {:?} {}",
                    info.backend,
                    info.name
                );
                return true;
            }
            Err(error) => log::warn!(
                "wgpu device probe rejected {:?} {}: {error}",
                info.backend,
                info.name
            ),
        }
    }
    false
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
