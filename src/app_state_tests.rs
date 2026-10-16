use super::*;

#[test]
fn window_close_keeps_dirty_and_busy_documents_before_any_animation() {
    for busy in [false, true] {
        let mut app = headless_app();
        app.install_canvas_session(Session::new(Document::new(32, 24), None));
        app.window_motion = crate::window_motion::WindowMotion::with_animations(true);
        if busy {
            app.session_mut().edit_busy = true;
        } else {
            app.session_mut().edit("Opacity", |doc| {
                doc.active_mut().unwrap().opacity = 0.25;
            });
        }
        let before = app.session().doc.clone();
        app.exit_pending = true;
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.handle_window_close(ctx)
        });
        assert_eq!(app.close_pending, Some(app.session().id));
        assert!(!app.window_motion.is_closing());
        assert!(!app.allow_exit);
        assert_eq!(app.session().doc, before);
    }
}

#[test]
fn window_close_waits_for_an_open_dialog_and_cancel_preserves_the_document() {
    let mut app = headless_app();
    app.window_motion = crate::window_motion::WindowMotion::with_animations(true);
    app.dialog = Some(Dialog::Jpeg);
    app.exit_pending = true;
    let ctx = egui::Context::default();
    let before = app.session().doc.clone();
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        app.handle_window_close(ctx)
    });
    assert!(!app.window_motion.is_closing());
    assert!(!app.allow_exit);
    app.dialog = None;
    app.exit_pending = false;
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        app.handle_window_close(ctx)
    });
    assert!(!app.window_motion.is_closing());
    assert_eq!(app.session().doc, before);
}

#[test]
fn clean_window_close_blocks_editing_and_defers_native_close_until_motion_finishes() {
    let mut app = headless_app();
    app.window_motion = crate::window_motion::WindowMotion::with_animations(true);
    app.exit_pending = true;
    let ctx = egui::Context::default();
    let before = app.session().doc.clone();
    let output = ctx.run(
        egui::RawInput {
            events: vec![egui::Event::Text("Must not edit during close".into())],
            dropped_files: vec![egui::DroppedFile {
                path: Some(PathBuf::from("must-not-import.png")),
                ..Default::default()
            }],
            ..Default::default()
        },
        |ctx| app.handle_window_close(ctx),
    );
    assert!(app.window_motion.is_closing());
    assert!(!app.allow_exit);
    assert!(ctx.input(|i| i.events.is_empty() && i.raw.dropped_files.is_empty()));
    assert!(
        !output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .any(|cmd| matches!(cmd, egui::ViewportCommand::Close))
    );
    assert_eq!(app.session().doc, before);
}

#[test]
fn load_alpha_runs_in_background_and_preserves_pixels_and_undo() {
    let mut app = headless_app();
    let mut doc = Document::new(32, 24);
    doc.active_mut().unwrap().image = Some(Arc::new(Raster::from_rgba(
        32,
        24,
        (0..32 * 24)
            .flat_map(|i| [40, 80, 160, (i % 256) as u8])
            .collect(),
    )));
    let expected = selection::alpha(&render::render_document(&doc), false);
    app.install_canvas_session(Session::new(doc.clone(), None));
    app.command(Command::LoadAlpha);
    assert!(app.session().edit_busy);
    assert_eq!(app.session().doc, doc);
    let worker = app.receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    send(&mut app, &egui::Context::default(), worker);
    assert!(!app.session().edit_busy);
    assert_eq!(app.session().doc.selection, Some(expected));
    assert_eq!(app.session().doc.layers, doc.layers);
    app.command(Command::Undo);
    assert_eq!(app.session().doc, doc);
}

#[test]
fn raw_input_hook_keeps_cancel_available_but_blocks_approved_close_frame_clicks() {
    let ctx = egui::Context::default();
    let mut app = headless_app();
    app.exit_pending = true;
    app.dialog = Some(Dialog::Jpeg);
    let mut input = egui::RawInput {
        events: vec![egui::Event::Text("keep cancel available".into())],
        ..Default::default()
    };
    eframe::App::raw_input_hook(&mut app, &ctx, &mut input);
    assert_eq!(input.events.len(), 1);
    app.dialog = None;
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .events
        .push(egui::ViewportEvent::Close);
    eframe::App::raw_input_hook(&mut app, &ctx, &mut input);
    assert!(input.events.is_empty());
    assert!(input.viewports[&egui::ViewportId::ROOT].close_requested());
}

#[test]
fn window_close_keeps_an_uncommitted_property_drag_until_release_or_cancel() {
    let mut app = headless_app();
    app.install_canvas_session(Session::new(Document::new(32, 24), None));
    let before = app.session().doc.clone();
    app.property_edit = Some((app.session().id, before));
    app.session_mut().doc.active_mut().unwrap().opacity = 0.25;
    app.exit_pending = true;
    assert!(!app.window_close_ready());
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        app.handle_window_close(ctx)
    });
    assert_eq!(app.close_pending, Some(app.session().id));
    assert!(!app.window_motion.is_closing());
    assert_eq!(app.session().doc.active().unwrap().opacity, 0.25);
}

#[derive(Default)]
struct ThemeSettingsStorage(BTreeMap<String, String>);

impl eframe::Storage for ThemeSettingsStorage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }

    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.into(), value);
    }

    fn flush(&mut self) {}
}

#[test]
fn custom_theme_import_is_atomic_and_survives_restart_without_the_source_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("theme.json");
    let invalid = directory.path().join("invalid.json");
    let ctx = egui::Context::default();
    let mut app = headless_app();
    let document = app.session().doc.clone();
    let revision = app.session().history.revision();
    let mut theme = crate::custom_theme::ThemeFile::builtin();
    theme.name = "松林主题".into();
    theme.write(&path).unwrap();
    app.theme_preference = egui::ThemePreference::Dark;
    app.import_custom_theme(&ctx, &path).unwrap();
    assert!(app.custom_theme_enabled && app.theme_settings_dirty);
    assert_eq!(app.custom_theme.as_ref().unwrap().name, "松林主题");
    let style = ctx.style();
    std::fs::write(&invalid, br#"{"version":99}"#).unwrap();
    assert!(app.import_custom_theme(&ctx, &invalid).is_err());
    assert_eq!(app.custom_theme.as_ref().unwrap().name, "松林主题");
    assert_eq!(ctx.style().visuals.panel_fill, style.visuals.panel_fill);
    assert_eq!(app.session().doc, document);
    assert_eq!(app.session().history.revision(), revision);

    let mut storage = ThemeSettingsStorage::default();
    eframe::App::save(&mut app, &mut storage);
    std::fs::remove_file(&path).unwrap();
    let mut restored = EditorApp::create(None, Some(&storage));
    assert!(restored.custom_theme_enabled);
    assert_eq!(restored.custom_theme.as_ref().unwrap().name, "松林主题");
    assert_eq!(restored.theme_preference, egui::ThemePreference::Dark);
    let exported = directory.path().join("exported.json");
    restored.export_custom_theme(&exported).unwrap();
    assert_eq!(
        crate::custom_theme::ThemeFile::load(&exported)
            .unwrap()
            .name,
        "松林主题"
    );
    restored.custom_theme_enabled = false;
    restored.apply_editor_theme(&ctx);
    restored.export_custom_theme(&exported).unwrap();
    assert_eq!(
        crate::custom_theme::ThemeFile::load(&exported)
            .unwrap()
            .name,
        crate::custom_theme::ThemeFile::builtin().name
    );
    eframe::App::save(&mut restored, &mut storage);
    let disabled = EditorApp::create(None, Some(&storage));
    assert!(!disabled.custom_theme_enabled);
    assert_eq!(disabled.custom_theme.as_ref().unwrap().name, "松林主题");
}

#[test]
fn invalid_persisted_custom_theme_restores_readable_default_and_old_settings() {
    let mut storage = ThemeSettingsStorage::default();
    eframe::set_value(
        &mut storage,
        "editor",
        &json!({
            "themePreference": egui::ThemePreference::Light,
            "customThemeEnabled": true,
            "customTheme": {"version": 77, "name": "broken"},
            "jpegQuality": 81,
            "rulers": true,
        }),
    );
    let app = EditorApp::create(None, Some(&storage));
    assert!(!app.custom_theme_enabled);
    assert!(app.custom_theme.is_none());
    assert!(app.theme_settings_message.is_some());
    assert_eq!(app.theme_preference, egui::ThemePreference::Light);
    assert_eq!(app.jpeg_quality, 81);
    assert!(app.rulers);
}

#[test]
fn maximized_window_restore_waits_for_a_completed_frame_even_with_multiple_passes() {
    let mut app = EditorApp::create(None, None);
    let ctx = egui::Context::default();
    app.defer_maximized_restore(&ctx);
    let first = ctx.run(egui::RawInput::default(), |ctx| {
        app.restore_window_after_first_frame(ctx);
        if ctx.current_pass_index() == 0 {
            ctx.request_discard("Exercise a startup layout retry");
        }
    });
    assert_eq!(first.platform_output.num_completed_passes, 2);
    assert!(app.restore_maximized_after_frame.is_some());
    assert!(
        !first.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .any(|command| matches!(command, egui::ViewportCommand::Maximized(_)))
    );
    assert_eq!(
        first.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
        Duration::ZERO
    );

    let restored = ctx.run(egui::RawInput::default(), |ctx| {
        app.restore_window_after_first_frame(ctx);
    });
    assert_eq!(app.restore_maximized_after_frame, None);
    assert_eq!(
        restored.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .filter(|command| matches!(command, egui::ViewportCommand::Maximized(true)))
            .count(),
        1
    );
    let stable = ctx.run(egui::RawInput::default(), |ctx| {
        app.restore_window_after_first_frame(ctx);
    });
    assert!(
        !stable.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .any(|command| matches!(command, egui::ViewportCommand::Maximized(_)))
    );
}

#[test]
fn theme_preference_defaults_to_system_for_new_legacy_and_invalid_settings() {
    assert_eq!(
        EditorApp::create(None, None).theme_preference,
        egui::ThemePreference::System
    );
    for saved in [
        json!({"rulers": true}),
        json!({"rulers": true, "themePreference": "unknown"}),
        json!({"rulers": true, "themePreference": null}),
    ] {
        let mut storage = ThemeSettingsStorage::default();
        eframe::set_value(&mut storage, "editor", &saved);
        let app = EditorApp::create(None, Some(&storage));
        assert_eq!(app.theme_preference, egui::ThemePreference::System);
        assert!(app.rulers, "Old settings must survive the theme upgrade");
    }
}

#[test]
fn theme_preference_survives_save_and_reopen_with_other_editor_settings() {
    for preference in [
        egui::ThemePreference::System,
        egui::ThemePreference::Light,
        egui::ThemePreference::Dark,
    ] {
        let mut storage = ThemeSettingsStorage::default();
        let mut app = EditorApp::create(None, None);
        app.theme_preference = preference;
        app.rulers = true;
        app.jpeg_quality = 73;
        eframe::App::save(&mut app, &mut storage);
        let reopened = EditorApp::create(None, Some(&storage));
        assert_eq!(reopened.theme_preference, preference);
        assert!(reopened.rulers);
        assert_eq!(reopened.jpeg_quality, 73);
    }
}

#[test]
fn worker_completion_wakes_a_sleeping_editor_and_applies_the_result() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let ctx = egui::Context::default();
    for _ in 0..3 {
        let _ = ctx.run(egui::RawInput::default(), |_| {});
    }
    let (wake, woke) = mpsc::channel();
    ctx.set_request_repaint_callback(move |info| {
        wake.send(info.delay).unwrap();
    });
    app.sender.context.set(ctx.clone()).unwrap();
    let sender = app.sender.clone();
    std::thread::spawn(move || {
        sender
            .send(Worker::Export(Ok(PathBuf::from("completed.png"))))
            .unwrap();
    })
    .join()
    .unwrap();
    assert_eq!(
        woke.recv_timeout(Duration::from_secs(1)).unwrap(),
        Duration::ZERO
    );
    app.poll(&ctx);
    assert_eq!(app.message.as_deref(), Some("已导出 completed.png"));
}

#[test]
fn disk_watch_does_not_repaint_and_selection_tracks_real_phase_boundaries() {
    let mut app = editor(vec![Document::new(8, 8)]);
    app.smoke = false;
    assert_eq!(app.repaint_delay(0.), None);
    app.session_mut().path = Some(PathBuf::from("watched.comp"));
    assert_eq!(app.repaint_delay(0.), None);
    app.session_mut().doc.selection = Some(Selection::empty(8, 8));
    assert_eq!(app.repaint_delay(0.), Some(Duration::from_millis(250)));
    assert_eq!(app.repaint_delay(0.125), Some(Duration::from_millis(125)));
    assert_eq!(app.repaint_delay(0.25), Some(Duration::from_millis(250)));
    assert_eq!(app.repaint_delay(12.375), Some(Duration::from_millis(125)));
    app.session_mut().doc.selection = None;
    app.external_pending = Some((app.session().id, 1));
    assert_eq!(app.repaint_delay(0.), None);
    app.session_mut().path = None;
    app.external_pending = None;
    app.smoke = true;
    app.smoke_done = false;
    app.started = Instant::now();
    let delay = app.repaint_delay(0.).unwrap();
    assert!(delay > Duration::from_secs(1) && delay <= Duration::from_secs(2));
}

#[test]
fn background_watch_reports_only_changes_and_stops_when_the_editor_closes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("watched.comp");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("manifest.json"), b"first").unwrap();
    let target = WatchTarget {
        id: Uuid::new_v4(),
        path: path.clone(),
        stamp: package_stamp(&path),
    };
    let (sender, receiver) = mpsc::channel();
    let mut watcher = PackageWatcher::new(WorkerSender {
        sender,
        context: Arc::new(OnceLock::new()),
    });
    watcher.sync(vec![target.clone()]);
    assert!(matches!(
        receiver.recv_timeout(Duration::from_millis(650)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    image_io::write_atomic(&path.join("manifest.json"), b"replaced manifest").unwrap();
    let stamp = package_stamp(&path);
    match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
        Worker::DiskChanged {
            target: reported,
            stamp: observed,
        } => {
            assert_eq!(reported.id, target.id);
            assert_eq!(reported.path, path);
            assert_eq!(observed, stamp);
        }
        _ => panic!("Expected an external disk change"),
    }
    assert!(matches!(
        receiver.recv_timeout(Duration::from_millis(650)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    watcher.sync(Vec::new());
    drop(watcher);
    assert!(matches!(
        receiver.recv_timeout(Duration::from_secs(2)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
}

#[test]
fn package_stamp_detects_image_addition_modification_and_deletion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    std::fs::create_dir(path.join("images")).unwrap();
    std::fs::write(path.join("manifest.json"), b"manifest").unwrap();
    let before = package_stamp(path);
    let image = path.join("images").join("layer.png");
    std::fs::write(&image, b"image").unwrap();
    let added = package_stamp(path);
    assert_ne!(before, added);
    std::fs::write(&image, b"changed image bytes").unwrap();
    assert_ne!(added, package_stamp(path));
    std::fs::remove_file(image).unwrap();
    assert_eq!(package_stamp(path), before);
}

#[test]
fn single_file_projects_keep_their_saved_path_and_detect_external_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("单文件 工程.comp");
    let original = Document::new(8, 8);
    project::save(&original, &path).unwrap();
    assert!(path.is_file());
    let mut app = editor(vec![Document::new(2, 2)]);
    app.install_import(path.clone(), image_io::import(&path).unwrap(), None);
    assert_eq!(app.session().path.as_ref(), Some(&path));
    assert!(!app.session().history.is_modified());
    let before = app.session().stamp;
    let mut changed = original;
    changed.layers[0].name = "externally changed layer with a longer name".into();
    project::save(&changed, &path).unwrap();
    let after = package_stamp(&path);
    assert_ne!(before, after);
    assert_eq!(project::load(&path).unwrap(), changed);
    std::fs::remove_file(&path).unwrap();
    assert_ne!(after, package_stamp(&path));
}

fn watched(app: &EditorApp) -> WatchTarget {
    WatchTarget {
        id: app.session().id,
        path: app.session().path.clone().unwrap(),
        stamp: app.session().stamp,
    }
}
fn watch_test_directory(app: &mut EditorApp) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("manifest.json"), b"original").unwrap();
    app.session_mut().path = Some(directory.path().to_path_buf());
    app.session_mut().stamp = package_stamp(directory.path());
    directory
}

#[test]
fn disk_change_survives_background_edit_and_opens_a_conflict_after_completion() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let directory = watch_test_directory(&mut app);
    app.session_mut().edit_busy = true;
    let target = watched(&app);
    std::fs::write(
        directory.path().join("manifest.json"),
        b"externally changed",
    )
    .unwrap();
    let stamp = package_stamp(directory.path());
    send(
        &mut app,
        &egui::Context::default(),
        Worker::DiskChanged { target, stamp },
    );
    app.watch();
    assert_eq!(app.disk_changes.len(), 1);
    assert!(app.external_pending.is_none());
    app.session_mut()
        .edit("Opacity", |doc| doc.layers[0].opacity = 0.4);
    app.session_mut().edit_busy = false;
    app.watch();
    assert!(app.disk_changes.is_empty());
    assert_eq!(app.external_pending, Some((app.session().id, stamp)));
    assert_eq!(app.session().doc.layers[0].opacity, 0.4);
}

#[test]
fn disk_changes_from_old_save_path_old_stamp_or_closed_tabs_are_discarded() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(8, 8)]);
    app.session_mut().path = Some(PathBuf::from("before.comp"));
    let target = watched(&app);
    app.session_mut().path = Some(PathBuf::from("after.comp"));
    send(
        &mut app,
        &egui::Context::default(),
        Worker::DiskChanged { target, stamp: 77 },
    );
    assert!(app.disk_changes.is_empty());
    let target = watched(&app);
    app.session_mut().stamp = 12;
    send(
        &mut app,
        &egui::Context::default(),
        Worker::DiskChanged { target, stamp: 78 },
    );
    assert!(app.disk_changes.is_empty());
    let target = watched(&app);
    app.close_tab(0);
    send(
        &mut app,
        &egui::Context::default(),
        Worker::DiskChanged { target, stamp: 79 },
    );
    assert!(app.disk_changes.is_empty());
    assert!(app.external_pending.is_none());
}

#[test]
fn disk_change_received_before_own_save_is_cleared_by_the_saved_stamp() {
    let mut app = editor(vec![Document::new(8, 8)]);
    app.session_mut().path = Some(PathBuf::from("watched.comp"));
    app.session_mut().save_busy = true;
    let target = watched(&app);
    send(
        &mut app,
        &egui::Context::default(),
        Worker::DiskChanged { target, stamp: 77 },
    );
    app.watch();
    assert_eq!(app.disk_changes.len(), 1);
    app.session_mut().stamp = 88;
    app.session_mut().save_busy = false;
    app.watch();
    assert!(app.disk_changes.is_empty());
    assert!(app.external_pending.is_none());
    assert!(!app.session().reload_busy);
}

#[test]
fn drag_defers_disk_changes_and_closing_the_tab_clears_the_conflict() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(8, 8)]);
    let directory = watch_test_directory(&mut app);
    let target = watched(&app);
    std::fs::write(
        directory.path().join("manifest.json"),
        b"externally changed",
    )
    .unwrap();
    let stamp = package_stamp(directory.path());
    app.drag = Some(distort_drag(&app.session().doc));
    send(
        &mut app,
        &egui::Context::default(),
        Worker::DiskChanged { target, stamp },
    );
    app.watch();
    assert_eq!(app.disk_changes.len(), 1);
    assert!(app.external_pending.is_none());
    app.drag = None;
    app.session_mut()
        .edit("Opacity", |doc| doc.layers[0].opacity = 0.4);
    app.watch();
    assert_eq!(app.external_pending, Some((app.session().id, stamp)));
    app.close_tab(0);
    assert!(app.external_pending.is_none());
    assert!(app.disk_changes.is_empty());
}

#[test]
fn delayed_disk_notification_is_rechecked_before_reporting_a_conflict() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let _directory = watch_test_directory(&mut app);
    let target = watched(&app);
    app.session_mut()
        .edit("Opacity", |doc| doc.layers[0].opacity = 0.4);
    send(
        &mut app,
        &egui::Context::default(),
        Worker::DiskChanged { target, stamp: 77 },
    );
    app.watch();
    assert!(app.disk_changes.is_empty());
    assert!(app.external_pending.is_none());
    assert_eq!(app.session().doc.layers[0].opacity, 0.4);
}

#[test]
fn explicit_qa_readiness_marker_is_written_once_and_labels_its_scope() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ready.json");
    app.qa_ready = Some((path.clone(), Instant::now()));
    app.record_qa_ready();
    let marker: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(marker["stage"], "first update completed");
    assert_eq!(marker["gpu_presentation_measured"], false);
    assert!(marker["elapsed_ms"].as_f64().unwrap() >= 0.);
    std::fs::write(&path, b"already measured").unwrap();
    app.record_qa_ready();
    assert_eq!(std::fs::read(&path).unwrap(), b"already measured");
}

#[test]
fn busy_feedback_has_a_deadline_without_polling_worker_completion() {
    let mut app = editor(vec![Document::new(8, 8)]);
    app.smoke = false;
    app.pending_io = 1;
    app.busy_since = Some(Instant::now());
    let delay = app.repaint_delay(0.).unwrap();
    assert!(delay > Duration::from_millis(150) && delay <= Duration::from_millis(250));
    app.busy_since = Some(Instant::now() - Duration::from_secs(1));
    assert_eq!(app.repaint_delay(0.), None);
}

#[test]
fn crossing_busy_deadline_after_painting_the_status_bar_wakes_the_next_frame() {
    let mut app = editor(vec![Document::new(8, 8)]);
    app.smoke = false;
    let ctx = egui::Context::default();
    for _ in 0..3 {
        let _ = ctx.run(egui::RawInput::default(), |_| {});
    }
    let (wake, woke) = mpsc::channel();
    ctx.set_request_repaint_callback(move |info| {
        wake.send(info.delay).unwrap();
    });
    app.pending_io = 1;
    app.busy_since = Some(Instant::now() - Duration::from_secs(1));
    assert_eq!(app.repaint_delay(0.), None);
    app.schedule_repaint(&ctx, true);
    assert!(
        woke.try_recv().is_err(),
        "Shown feedback needs no worker polling"
    );
    app.schedule_repaint(&ctx, false);
    assert_eq!(
        woke.recv_timeout(Duration::from_secs(1)).unwrap(),
        Duration::ZERO,
        "An expired deadline must repaint if this frame did not show feedback"
    );
}

fn editor(documents: Vec<Document>) -> EditorApp {
    let mut app = EditorApp::create(None, None);
    app.tabs = documents
        .into_iter()
        .map(|document| Session::new(document, None))
        .collect();
    app.current = 0;
    app.recent.clear();
    app
}

fn imported(name: &str, conversion: Option<&str>) -> image_io::ImportResult {
    let mut document = Document::new(8, 8);
    document.layers[0].name = name.into();
    image_io::ImportResult {
        document,
        conversions: conversion.into_iter().map(str::to_owned).collect(),
    }
}

fn send(app: &mut EditorApp, ctx: &egui::Context, worker: Worker) {
    app.sender.send(worker).expect("worker receiver is live");
    app.poll(ctx);
}

fn dialog_frame(
    app: &mut EditorApp,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280., 860.))),
            events,
            ..Default::default()
        },
        |ctx| app.dialogs(ctx),
    )
}

fn click_dialog_button(app: &mut EditorApp, ctx: &egui::Context, label: &str) {
    let translated_label = tr(label);
    let label = translated_label.as_ref();
    // The first pass establishes the window position before pointer input.
    dialog_frame(app, ctx, vec![]);
    let output = dialog_frame(app, ctx, vec![]);
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("dialog button {label:?} must be visible"));
    dialog_frame(
        app,
        ctx,
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ],
    );
    dialog_frame(
        app,
        ctx,
        vec![egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }],
    );
}

#[test]
fn save_completion_routes_to_session_with_duplicate_document_id() {
    let document = Document::new(8, 8);
    let mut app = editor(vec![document.clone(), document]);
    assert_ne!(app.tabs[0].id, app.tabs[1].id);
    for (index, session) in app.tabs.iter_mut().enumerate() {
        session.edit("Rename", |d| d.layers[0].name = format!("Tab {index}"));
        session.save_busy = true;
    }
    let target = app.tabs[1].id;
    let revision = app.tabs[1].history.revision();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("second.comp");
    send(
        &mut app,
        &egui::Context::default(),
        Worker::Save {
            id: target,
            revision,
            path: path.clone(),
            result: Ok(()),
        },
    );
    assert!(app.tabs[0].save_busy);
    assert!(app.tabs[0].history.is_modified());
    assert!(app.tabs[0].path.is_none());
    assert!(!app.tabs[1].save_busy);
    assert!(!app.tabs[1].history.is_modified());
    assert_eq!(app.tabs[1].path, Some(path));
    assert_eq!(app.save_count, 1);
}

#[test]
fn edit_completion_routes_to_session_with_duplicate_document_id() {
    let document = Document::new(8, 8);
    let mut app = editor(vec![document.clone(), document.clone()]);
    for session in &mut app.tabs {
        session.edit_busy = true;
    }
    let mut result = document.clone();
    result.layers[0].name = "Edited second tab".into();
    let worker = Worker::Edit {
        id: app.tabs[1].id,
        revision: app.tabs[1].history.revision(),
        serial: app.tabs[1].serial,
        name: "Background edit".into(),
        before: document.clone(),
        result: Ok(result.clone()),
    };
    send(&mut app, &egui::Context::default(), worker);
    assert_eq!(app.tabs[0].doc, document);
    assert!(app.tabs[0].edit_busy);
    assert!(!app.tabs[0].history.is_modified());
    assert_eq!(app.tabs[1].doc, result);
    assert!(!app.tabs[1].edit_busy);
    assert!(app.tabs[1].history.is_modified());
    assert_eq!(app.tabs[1].history.undo_name(), Some("Background edit"));
}

#[test]
fn preview_completion_routes_to_session_with_duplicate_document_id() {
    let document = Document::new(8, 8);
    let mut app = editor(vec![document.clone(), document]);
    app.tabs[0].preview_busy = true;
    app.tabs[1].preview_busy = true;
    let view = crate::preview::ViewRequest {
        region: [0, 0, 8, 8],
        width: 3,
        height: 2,
    };
    app.tabs[1].preview_view = Some(view);
    let worker = Worker::Preview {
        id: app.tabs[1].id,
        serial: app.tabs[1].serial,
        view,
        raster: Raster::solid(3, 2, [10, 20, 30, 255]),
    };
    send(&mut app, &egui::Context::default(), worker);
    assert!(app.tabs[0].preview_busy);
    assert!(app.tabs[0].texture.is_none());
    assert!(!app.tabs[1].preview_busy);
    assert_eq!(app.tabs[1].texture.as_ref().unwrap().size(), [3, 2]);
    assert_eq!(app.tabs[1].rendered, app.tabs[1].serial);
}

#[test]
fn import_layers_keeps_original_target_after_switching_tabs() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(16, 16)]);
    let target = app.tabs[0].id;
    let other = app.tabs[1].doc.clone();
    app.current = 1;
    let directory = tempfile::tempdir().unwrap();
    send(
        &mut app,
        &egui::Context::default(),
        Worker::Open {
            path: directory.path().join("layer.png"),
            result: Ok(imported("Imported into first", None)),
            target: Some(target),
        },
    );
    assert_eq!(app.current, 1);
    assert_eq!(app.tabs[1].doc, other);
    assert_eq!(app.tabs[0].doc.layers.len(), 2);
    assert_eq!(app.tabs[0].doc.layers[1].name, "Imported into first");
    assert!(app.tabs[0].history.is_modified());
}

#[test]
fn import_layers_does_not_fall_back_when_original_target_is_closed() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(16, 16)]);
    let target = app.tabs[0].id;
    app.close_tab(0);
    let remaining = app.session().doc.clone();
    let directory = tempfile::tempdir().unwrap();
    send(
        &mut app,
        &egui::Context::default(),
        Worker::Open {
            path: directory.path().join("layer.png"),
            result: Ok(imported("Closed destination", None)),
            target: Some(target),
        },
    );
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.session().doc, remaining);
    assert!(!app.session().history.is_modified());
    assert!(
        app.message
            .as_deref()
            .unwrap()
            .contains("destination project was closed")
    );
}

#[test]
fn conversion_reports_are_fifo_and_preserve_import_targets() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(16, 16)]);
    let target = app.tabs[0].id;
    let other = app.tabs[1].doc.clone();
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.psd");
    let second_path = directory.path().join("second.psd");
    let ctx = egui::Context::default();
    app.sender
        .send(Worker::Open {
            path: first_path.clone(),
            result: Ok(imported("First document", Some("First conversion"))),
            target: None,
        })
        .unwrap();
    app.sender
        .send(Worker::Open {
            path: second_path.clone(),
            result: Ok(imported("Second layer", Some("Second conversion"))),
            target: Some(target),
        })
        .unwrap();
    app.poll(&ctx);
    assert_eq!(app.pending_import.as_ref().unwrap().0, first_path);
    assert_eq!(app.report, ["First conversion"]);
    assert_eq!(app.import_queue.len(), 1);
    click_dialog_button(&mut app, &ctx, "Import");
    assert_eq!(app.tabs.len(), 3);
    assert_eq!(app.session().doc.layers[0].name, "First document");
    assert_eq!(app.pending_import.as_ref().unwrap().0, second_path);
    assert_eq!(app.pending_import.as_ref().unwrap().2, Some(target));
    assert_eq!(app.report, ["Second conversion"]);
    click_dialog_button(&mut app, &ctx, "Import");
    assert!(app.pending_import.is_none());
    assert!(app.import_queue.is_empty());
    assert!(app.report.is_empty());
    assert_eq!(app.current, 2);
    assert_eq!(app.tabs[0].doc.layers[1].name, "Second layer");
    assert_eq!(app.tabs[1].doc, other);
}

#[test]
fn close_confirmation_follows_session_after_tab_reordering() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(16, 16)]);
    app.tabs[0].edit("Rename", |d| d.layers[0].name = "Dirty".into());
    let closing = app.tabs[0].id;
    let remaining = app.tabs[1].id;
    app.request_close(0);
    assert_eq!(app.close_pending, Some(closing));
    app.tabs.swap(0, 1);
    app.current = 0;
    let ctx = egui::Context::default();
    click_dialog_button(&mut app, &ctx, "Discard");
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].id, remaining);
    assert!(app.close_pending.is_none());
}

#[test]
fn discard_closes_only_requested_tab_with_clean_saved_neighbor() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(16, 16)]);
    let directory = tempfile::tempdir().unwrap();
    app.tabs[1].path = Some(directory.path().join("neighbor.comp"));
    let remaining = app.tabs[1].id;
    app.tabs[0].edit("Rename", |d| d.layers[0].name = "Dirty".into());
    app.request_close(0);
    click_dialog_button(&mut app, &egui::Context::default(), "Discard");
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].id, remaining);
    assert_eq!(app.tabs[0].doc.width, 16);
}

#[test]
fn close_waits_for_clean_document_background_edit() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(16, 16)]);
    let closing = app.tabs[0].id;
    let directory = tempfile::tempdir().unwrap();
    app.tabs[0].path = Some(directory.path().join("processing.comp"));
    app.tabs[0].edit_busy = true;
    app.request_close(0);
    dialog_frame(&mut app, &egui::Context::default(), vec![]);
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.close_pending, Some(closing));
    let before = app.tabs[0].doc.clone();
    let mut result = before.clone();
    result.layers[0].opacity = 0.5;
    let worker = Worker::Edit {
        id: closing,
        revision: app.tabs[0].history.revision(),
        serial: app.tabs[0].serial,
        name: "Pending operation".into(),
        before,
        result: Ok(result.clone()),
    };
    let ctx = egui::Context::default();
    send(&mut app, &ctx, worker);
    dialog_frame(&mut app, &ctx, vec![]);
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.tabs[0].doc, result);
    assert!(app.tabs[0].history.is_modified());
    assert_eq!(app.close_pending, Some(closing));
}

#[test]
fn stale_edit_serial_preserves_uncommitted_live_changes() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let before = app.session().doc.clone();
    let id = app.session().id;
    let revision = app.session().history.revision();
    let serial = app.session().serial;
    let mut old_result = before.clone();
    old_result.layers[0].name = "Stale result".into();
    app.session_mut().doc.layers[0].transform.origin = [3., 4.];
    app.session_mut().changed();
    app.session_mut().edit_busy = true;
    let live = app.session().doc.clone();
    send(
        &mut app,
        &egui::Context::default(),
        Worker::Edit {
            id,
            revision,
            serial,
            name: "Outdated operation".into(),
            before,
            result: Ok(old_result),
        },
    );
    assert_eq!(app.session().doc, live);
    assert!(!app.session().edit_busy);
    assert_eq!(app.session().history.revision(), revision);
    assert!(!app.session().history.is_modified());
    assert!(app.message.as_deref().unwrap().contains("document changed"));
}

#[test]
fn reload_intersects_layers_preserves_active_and_clips_pixel_selection() {
    let mut old = Document::new(8, 8);
    let active = old.layers[0].id;
    let removed = Layer::new("Removed", 8, 8);
    let removed_id = removed.id;
    let surviving = Layer::new("Surviving", 8, 8);
    let surviving_id = surviving.id;
    old.layers.extend([removed, surviving.clone()]);
    let mut selected = Selection::empty(8, 8);
    selected.set(1, 1, 255);
    selected.set(7, 7, 255);
    old.selection = Some(selected);
    let mut app = editor(vec![old.clone()]);
    app.session_mut().selected = HashSet::from([active, removed_id]);
    app.session_mut().reload_busy = true;
    let mut replacement = Document::new(4, 3);
    replacement.layers = vec![old.layers[0].clone(), surviving];
    replacement.active_layer_id = Some(surviving_id);
    let replacement_id = replacement.id;
    let runtime_id = app.session().id;
    let worker = Worker::Reload {
        id: runtime_id,
        stamp: 99,
        revision: app.session().history.revision(),
        serial: app.session().serial,
        force: false,
        result: Ok(replacement),
    };
    send(&mut app, &egui::Context::default(), worker);
    assert_eq!(app.session().id, runtime_id);
    assert_eq!(app.session().doc.id, replacement_id);
    assert_eq!(app.session().doc.active_layer_id, Some(active));
    assert_eq!(app.session().selected, HashSet::from([active]));
    let selection = app.session().doc.selection.as_ref().unwrap();
    assert_eq!([selection.width, selection.height], [4, 3]);
    assert_eq!(selection.get(1, 1), 255);
    assert_eq!(selection.bounds(), Some([1, 1, 2, 2]));
    assert_eq!(app.session().stamp, 99);
    assert!(!app.session().reload_busy);
    assert!(!app.session().history.is_modified());
    assert_eq!(app.watch_count, 1);
}

#[test]
fn reload_falls_back_to_new_active_when_all_selected_layers_disappear() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let replacement = Document::new(8, 8);
    let active = replacement.active_layer_id.unwrap();
    let worker = Worker::Reload {
        id: app.session().id,
        stamp: 100,
        revision: app.session().history.revision(),
        serial: app.session().serial,
        force: false,
        result: Ok(replacement),
    };
    send(&mut app, &egui::Context::default(), worker);
    assert_eq!(app.session().doc.active_layer_id, Some(active));
    assert_eq!(app.session().selected, HashSet::from([active]));
}

#[test]
fn stale_reload_preserves_new_edits_and_requests_conflict_resolution() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let id = app.session().id;
    let worker = Worker::Reload {
        id,
        stamp: 101,
        revision: app.session().history.revision(),
        serial: app.session().serial,
        force: false,
        result: Ok(Document::new(16, 16)),
    };
    app.session_mut()
        .edit("New local edit", |d| d.layers[0].opacity = 0.3);
    app.session_mut().reload_busy = true;
    let live = app.session().doc.clone();
    send(&mut app, &egui::Context::default(), worker);
    assert_eq!(app.session().doc, live);
    assert!(app.session().history.is_modified());
    assert!(!app.session().reload_busy);
    assert_eq!(app.external_pending, Some((id, 101)));
    assert_eq!(app.watch_count, 0);
}

#[test]
fn reload_of_identical_saved_content_preserves_undo_history() {
    let mut app = editor(vec![Document::new(8, 8)]);
    app.session_mut()
        .edit("Saved opacity edit", |d| d.layers[0].opacity = 0.6);
    let revision = app.session().history.revision();
    app.session_mut().history.mark_saved(revision);
    let serial = app.session().serial;
    let document = app.session().doc.clone();
    let id = app.session().id;
    app.session_mut().stamp = 100;
    app.session_mut().reload_busy = true;
    send(
        &mut app,
        &egui::Context::default(),
        Worker::Reload {
            id,
            stamp: 102,
            revision,
            serial,
            force: false,
            result: Ok(document.clone()),
        },
    );
    assert_eq!(app.session().doc, document);
    assert_eq!(app.session().history.revision(), revision);
    assert_eq!(
        app.session().history.undo_name(),
        Some("Saved opacity edit")
    );
    assert!(!app.session().history.is_modified());
    assert_eq!(app.session().serial, serial);
    assert_eq!(app.session().stamp, 102);
    assert!(!app.session().reload_busy);
    assert!(app.external_pending.is_none());
    assert_eq!(app.watch_count, 0);
}

#[test]
fn raw_preview_rejects_old_parameters_and_accepts_current_parameters() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("camera.nef");
    app.dialog = Some(Dialog::Raw {
        path: path.clone(),
        target: None,
    });
    let old_key = raw_settings_key(app.raw_settings);
    app.raw_key = Some((path.clone(), old_key));
    app.raw_settings.exposure = 1.;
    app.raw_busy = true;
    let ctx = egui::Context::default();
    send(
        &mut app,
        &ctx,
        Worker::RawPreview {
            path: path.clone(),
            key: old_key,
            result: Ok(Raster::solid(3, 2, [255, 0, 0, 255])),
        },
    );
    assert!(!app.raw_busy);
    assert!(app.raw_texture.is_none());
    let current_key = raw_settings_key(app.raw_settings);
    assert_ne!(app.raw_key, Some((path.clone(), current_key)));
    app.raw_busy = true;
    send(
        &mut app,
        &ctx,
        Worker::RawPreview {
            path,
            key: current_key,
            result: Ok(Raster::solid(5, 4, [0, 255, 0, 255])),
        },
    );
    assert!(!app.raw_busy);
    assert_eq!(app.raw_texture.as_ref().unwrap().size(), [5, 4]);
}

#[test]
fn raw_preview_rejects_other_file_and_closed_dialog() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("current.nef");
    app.dialog = Some(Dialog::Raw {
        path: path.clone(),
        target: None,
    });
    let key = raw_settings_key(app.raw_settings);
    app.raw_busy = true;
    let ctx = egui::Context::default();
    send(
        &mut app,
        &ctx,
        Worker::RawPreview {
            path: directory.path().join("previous.nef"),
            key,
            result: Ok(Raster::solid(3, 2, [255; 4])),
        },
    );
    assert!(app.raw_texture.is_none());
    app.dialog = None;
    send(
        &mut app,
        &ctx,
        Worker::RawPreview {
            path,
            key,
            result: Ok(Raster::solid(3, 2, [255; 4])),
        },
    );
    assert!(app.raw_texture.is_none());
}

#[test]
fn canvas_size_dialog_keeps_original_target_after_switching_tabs() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(16, 16)]);
    let original = app.tabs[0].id;
    app.command(Command::CanvasSize);
    let ctx = egui::Context::default();
    dialog_frame(&mut app, &ctx, vec![]);
    assert_eq!(app.dialog_target.unwrap().0, original);
    app.new_size = [4, 5];
    app.current = 1;
    let other = app.tabs[1].doc.clone();
    click_dialog_button(&mut app, &ctx, "Apply");
    assert_eq!([app.tabs[0].doc.width, app.tabs[0].doc.height], [4, 5]);
    assert_eq!(app.tabs[1].doc, other);
    assert_eq!(app.current, 1);
    assert!(app.dialog.is_none());
    assert!(app.dialog_target.is_none());
}

#[test]
fn raw_preview_reschedules_latest_settings_after_old_request_completes() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("camera.nef");
    app.dialog = Some(Dialog::Raw {
        path: path.clone(),
        target: None,
    });
    let old_key = raw_settings_key(app.raw_settings);
    app.raw_key = Some((path.clone(), old_key));
    app.raw_busy = true;
    app.raw_settings.exposure = 2.;
    app.raw_settings.temperature = 6000.;
    let ctx = egui::Context::default();
    send(
        &mut app,
        &ctx,
        Worker::RawPreview {
            path: path.clone(),
            key: old_key,
            result: Ok(Raster::solid(3, 2, [255; 4])),
        },
    );
    dialog_frame(&mut app, &ctx, vec![]);
    assert!(app.raw_busy);
    assert!(app.raw_texture.is_none());
    assert_eq!(
        app.raw_key,
        Some((path, raw_settings_key(app.raw_settings)))
    );
}

#[test]
fn raw_dialog_queue_preserves_file_order_and_destination() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(16, 16)]);
    let target = app.tabs[0].id;
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.nef");
    let second = directory.path().join("second.nef");
    app.dialog = Some(Dialog::Grid);
    app.open_target(first.clone(), Some(target));
    app.open_target(second.clone(), None);
    assert_eq!(app.raw_queue.len(), 2);
    assert!(matches!(app.dialog, Some(Dialog::Grid)));
    app.current = 1;
    app.dialog = None;
    dialog_frame(&mut app, &egui::Context::default(), vec![]);
    assert!(matches!(
        &app.dialog,
        Some(Dialog::Raw { path, target: active_target })
            if *path == first && *active_target == Some(target)
    ));
    assert_eq!(app.raw_queue.front(), Some(&(second, None)));
    assert_eq!(app.current, 1);
}

#[test]
fn queued_imports_preserve_order_and_recover_after_a_bad_psd() {
    let mut app = headless_app();
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("第一张.png");
    let broken = directory.path().join("损坏文件.psd");
    let last = directory.path().join("最后一张.jpg");
    image_io::export_png(&Raster::solid(7, 5, [17, 29, 61, 255]), &first, 72.).unwrap();
    std::fs::write(&broken, b"8BPS").unwrap();
    image_io::export_jpeg(
        &Raster::solid(9, 6, [91, 120, 141, 255]),
        &last,
        72.,
        95,
        [255; 3],
    )
    .unwrap();
    for path in [&first, &broken, &last] {
        app.open_target(path.clone(), None);
    }
    assert_eq!(app.pending_io, 3);
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(30);
    while app.pending_io > 0 && Instant::now() < deadline {
        app.poll(&ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        app.pending_io, 0,
        "Every import must complete, including errors"
    );
    assert_eq!(app.tabs.len(), 2, "The bad PSD must not create a document");
    assert_eq!([app.tabs[0].doc.width, app.tabs[0].doc.height], [7, 5]);
    assert_eq!([app.tabs[1].doc.width, app.tabs[1].doc.height], [9, 6]);
    assert_eq!(
        app.tabs[0].doc.layers[0].image.as_ref().unwrap().to_rgba(),
        Raster::solid(7, 5, [17, 29, 61, 255]).to_rgba()
    );
    assert!(app.pending_import.is_none());
    assert!(app.import_queue.is_empty());
}

#[test]
fn canvas_size_relative_dimensions_commit_signed_pixel_delta() {
    let mut app = editor(vec![Document::new(100, 80)]);
    app.command(Command::CanvasSize);
    app.size_relative = true;
    app.size_delta = [-25, 20];
    click_dialog_button(&mut app, &egui::Context::default(), "Apply");
    assert_eq!(
        [app.session().doc.width, app.session().doc.height],
        [75, 100]
    );
    assert!(app.session().history.is_modified());
    assert_eq!(app.session().history.undo_name(), Some("Canvas size"));
    app.command(Command::Undo);
    assert_eq!(
        [app.session().doc.width, app.session().doc.height],
        [100, 80]
    );
}

#[test]
fn physical_size_units_and_percent_produce_expected_pixel_dimensions() {
    let cases = [
        ("Pixels", 150., 300., 400, 150.),
        ("Inches", 2., 300., 400, 600.),
        ("Centimeters", 2.54, 300., 400, 300.),
        ("Millimeters", 25.4, 300., 400, 300.),
        ("Percent", 50., 300., 400, 200.),
    ];
    for (unit, input, ppi, original, expected_pixels) in cases {
        let pixels = input * size_unit_factor(unit, ppi, original);
        assert!((pixels - expected_pixels).abs() < 1e-9, "{unit}: {pixels}");
    }
}

#[test]
fn single_click_brush_commits_preview_and_roundtrips_history() {
    let mut app = editor(vec![Document::new(32, 32)]);
    app.tool = Tool::Brush;
    app.diameter = 8.;
    app.hardness = 1.;
    app.opacity = 1.;
    app.foreground = [220, 30, 40, 255];
    let before = app.session().doc.clone();
    app.begin_brush([16.5, 16.5]);
    assert!(app.brush_stroke.is_some());
    assert_eq!(
        app.session()
            .doc
            .active()
            .unwrap()
            .image
            .as_ref()
            .unwrap()
            .get(16, 16),
        app.foreground
    );
    app.finish_brush(before.clone(), [16.5, 16.5]);
    assert!(app.message.is_none(), "{:?}", app.message);
    assert!(app.brush_stroke.is_none());
    assert_eq!(
        app.session()
            .doc
            .active()
            .unwrap()
            .image
            .as_ref()
            .unwrap()
            .get(16, 16),
        app.foreground
    );
    assert_eq!(app.session().history.undo_name(), Some("Brush stroke"));
    assert!(app.session().history.is_modified());
    let committed = app.session().doc.clone();
    app.command(Command::Undo);
    assert_eq!(app.session().doc, before);
    assert!(!app.session().history.is_modified());
    app.command(Command::Redo);
    assert_eq!(app.session().doc, committed);
    assert!(app.session().history.is_modified());
}

#[test]
fn dragged_brush_commits_expanded_preview_and_preserves_linked_mask_placement() {
    let mut document = Document::new(48, 48);
    let layer = &mut document.layers[0];
    layer.image = Some(Arc::new(Raster::blank(8, 8)));
    layer.transform = crate::model::Transform::new(16., 16., 8., 8.);
    layer.mask = Some(Mask::white());
    let original_placement = layer.transform;
    let mut app = editor(vec![document]);
    app.tool = Tool::Brush;
    app.diameter = 6.;
    app.hardness = 1.;
    app.opacity = 1.;
    app.foreground = [25, 170, 60, 255];
    let before = app.session().doc.clone();
    app.begin_brush([20.5, 20.5]);
    app.brush_stroke
        .as_mut()
        .unwrap()
        .append([28.5, 20.5])
        .unwrap();
    app.show_brush();
    app.brush_stroke
        .as_mut()
        .unwrap()
        .append([35.5, 20.5])
        .unwrap();
    app.show_brush();
    assert!(app.session().doc.active().unwrap().transform.size[0] > 8.);
    app.finish_brush(before.clone(), [35.5, 20.5]);
    assert!(app.message.is_none(), "{:?}", app.message);
    let layer = app.session().doc.active().unwrap();
    let raster = layer.image.as_ref().unwrap();
    let point = layer.transform.from_document([35.5, 20.5]);
    assert_eq!(
        raster.get(
            (point[0] * raster.width as f64).floor() as u32,
            (point[1] * raster.height as f64).floor() as u32
        ),
        app.foreground
    );
    let mask = layer.mask.as_ref().unwrap();
    assert!(mask.linked);
    assert_eq!(mask.placement, Some(original_placement));
    assert_eq!(
        mask.raster,
        before.active().unwrap().mask.as_ref().unwrap().raster
    );
    assert_eq!(
        render::render_document(&app.session().doc).get(35, 20),
        app.foreground
    );
    let committed = app.session().doc.clone();
    app.command(Command::Undo);
    assert_eq!(app.session().doc, before);
    app.command(Command::Redo);
    assert_eq!(app.session().doc, committed);
}

#[test]
fn escape_cancels_dialog_binding_before_opening_another_project_dialog() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(16, 16)]);
    app.command(Command::CanvasSize);
    let ctx = egui::Context::default();
    dialog_frame(&mut app, &ctx, vec![]);
    let generation = app.dialog_generation;
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280., 860.))),
            events: vec![egui::Event::Key {
                key: Key::Escape,
                physical_key: Some(Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        },
        |ctx| {
            app.shortcuts(ctx);
        },
    );
    assert!(app.dialog.is_none());
    assert!(app.dialog_target.is_none());
    assert!(app.dialog_generation > generation);
    let original = app.tabs[0].doc.clone();
    app.current = 1;
    app.command(Command::CanvasSize);
    app.new_size = [5, 6];
    click_dialog_button(&mut app, &ctx, "Apply");
    assert_eq!(app.tabs[0].doc, original);
    assert_eq!([app.tabs[1].doc.width, app.tabs[1].doc.height], [5, 6]);
    assert_eq!(app.current, 1);
}

#[test]
fn closing_an_earlier_inactive_tab_preserves_active_session() {
    let mut app = editor(vec![
        Document::new(8, 8),
        Document::new(16, 16),
        Document::new(32, 32),
    ]);
    app.current = 1;
    let active = app.session().id;
    app.request_close(0);
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.session().id, active);
    assert_eq!(app.session().doc.width, 16);
}

fn shortcut_key(app: &mut EditorApp, ctx: &egui::Context, key: Key, modifiers: egui::Modifiers) {
    for pressed in [true, false] {
        let _ = ctx.run(
            egui::RawInput {
                modifiers,
                events: vec![egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            },
            |ctx| {
                for command in app.shortcuts(ctx) {
                    app.command(command);
                }
            },
        );
    }
}

#[test]
fn remapped_brush_shortcut_and_tool_mode_keys_use_live_input() {
    let mut app = editor(vec![Document::new(8, 8)]);
    app.shortcuts.insert("Brush tool".into(), "P".into());
    app.tool = Tool::Move;
    let ctx = egui::Context::default();
    shortcut_key(&mut app, &ctx, Key::B, egui::Modifiers::NONE);
    assert_eq!(app.tool, Tool::Move);
    shortcut_key(&mut app, &ctx, Key::P, egui::Modifiers::NONE);
    assert_eq!(app.tool, Tool::Brush);
    shortcut_key(&mut app, &ctx, Key::M, egui::Modifiers::NONE);
    assert_eq!(app.tool, Tool::Rectangle);
    shortcut_key(&mut app, &ctx, Key::Tab, egui::Modifiers::NONE);
    assert_eq!(app.tool, Tool::Ellipse);
    shortcut_key(&mut app, &ctx, Key::M, egui::Modifiers::NONE);
    assert_eq!(app.tool, Tool::Rectangle);
    shortcut_key(&mut app, &ctx, Key::M, egui::Modifiers::SHIFT);
    assert_eq!(app.tool, Tool::Ellipse);
    let foreground = app.foreground;
    let background = app.background;
    shortcut_key(&mut app, &ctx, Key::X, egui::Modifiers::NONE);
    assert_eq!(app.foreground, background);
    assert_eq!(app.background, foreground);
    shortcut_key(&mut app, &ctx, Key::D, egui::Modifiers::NONE);
    assert_eq!(app.foreground, [0, 0, 0, 255]);
    assert_eq!(app.background, [255; 4]);
}

#[test]
fn focused_text_input_guards_tool_shortcuts_and_color_keys() {
    let mut app = editor(vec![Document::new(8, 8)]);
    app.tool = Tool::Move;
    app.foreground = [1, 2, 3, 255];
    let ctx = egui::Context::default();
    let field = egui::Id::new("shortcut-focus-guard");
    let mut text = String::new();
    for events in [
        vec![],
        vec![
            egui::Event::Key {
                key: Key::B,
                physical_key: Some(Key::B),
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
            egui::Event::Key {
                key: Key::D,
                physical_key: Some(Key::D),
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
            egui::Event::Text("bd".into()),
        ],
    ] {
        let _ = ctx.run(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut text).id(field));
                    ctx.memory_mut(|memory| memory.request_focus(field));
                });
                assert!(ctx.wants_keyboard_input());
                assert!(app.shortcuts(ctx).is_empty());
            },
        );
    }
    assert_eq!(text, "bd");
    assert_eq!(app.tool, Tool::Move);
    assert_eq!(app.foreground, [1, 2, 3, 255]);
}

#[test]
fn more_specific_shift_shortcuts_dispatch_save_as_and_redo() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let ctx = egui::Context::default();
    let modifiers = egui::Modifiers {
        ctrl: true,
        command: true,
        shift: true,
        ..Default::default()
    };
    for key in [Key::S, Key::Z] {
        let mut commands = Vec::new();
        let _ = ctx.run(
            egui::RawInput {
                modifiers,
                events: vec![egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            },
            |ctx| commands = app.shortcuts(ctx),
        );
        match key {
            Key::S => assert!(matches!(commands.as_slice(), [Command::SaveAs])),
            Key::Z => assert!(matches!(commands.as_slice(), [Command::Redo])),
            _ => unreachable!(),
        }
    }
}

fn clipboard_shortcut_frame(
    app: &mut EditorApp,
    ctx: &egui::Context,
    modifiers: egui::Modifiers,
    events: Vec<egui::Event>,
) -> Vec<Command> {
    let mut commands = Vec::new();
    let _ = ctx.run(
        egui::RawInput {
            modifiers,
            events,
            ..Default::default()
        },
        |ctx| {
            commands = app.shortcuts(ctx);
            assert!(app.shortcuts(ctx).is_empty(), "Input must be consumed once");
        },
    );
    commands
}

fn clipboard_key_event(key: Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers,
    }
}

#[test]
fn native_clipboard_events_dispatch_default_configured_commands() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let ctx = egui::Context::default();
    for (event, expected) in [
        (egui::Event::Cut, Command::Cut),
        (egui::Event::Copy, Command::Copy),
        (egui::Event::Paste("中文🙂".into()), Command::Paste),
    ] {
        // Native events do not carry modifiers, and Ctrl may already be released.
        let commands = clipboard_shortcut_frame(&mut app, &ctx, egui::Modifiers::NONE, vec![event]);
        assert_eq!(commands.len(), 1);
        assert_eq!(
            std::mem::discriminant(&commands[0]),
            std::mem::discriminant(&expected)
        );
    }
}

#[test]
fn native_clipboard_events_respect_disabled_remapped_and_reassigned_bindings() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let ctx = egui::Context::default();
    let ctrl = parse_shortcut("Ctrl+V").unwrap().modifiers;
    app.shortcuts.insert("Cut".into(), "Ctrl+K".into());
    app.shortcuts.insert("Copy".into(), String::new());
    app.shortcuts.insert("Paste".into(), "Ctrl+P".into());
    for event in [
        egui::Event::Cut,
        egui::Event::Copy,
        egui::Event::Paste("x".into()),
    ] {
        assert!(clipboard_shortcut_frame(&mut app, &ctx, ctrl, vec![event]).is_empty());
    }
    let commands = clipboard_shortcut_frame(
        &mut app,
        &ctx,
        ctrl,
        vec![clipboard_key_event(Key::K, true, ctrl)],
    );
    assert!(matches!(commands.as_slice(), [Command::Cut]));
    app.shortcuts.insert("Save".into(), "Ctrl+X".into());
    let commands = clipboard_shortcut_frame(&mut app, &ctx, ctrl, vec![egui::Event::Cut]);
    assert!(matches!(commands.as_slice(), [Command::Save]));
    app.shortcuts.insert("Brush tool".into(), "Ctrl+V".into());
    app.tool = Tool::Move;
    assert!(
        clipboard_shortcut_frame(&mut app, &ctx, ctrl, vec![egui::Event::Paste("x".into())])
            .is_empty()
    );
    assert_eq!(app.tool, Tool::Brush);
}

#[test]
fn native_clipboard_events_keep_specific_modifiers_and_do_not_duplicate_real_keys() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let ctx = egui::Context::default();
    let ctrl = parse_shortcut("Ctrl+V").unwrap().modifiers;
    let shifted = parse_shortcut("Ctrl+Shift+X").unwrap().modifiers;
    app.shortcuts
        .insert("Save As".into(), "Ctrl+Shift+X".into());
    let commands = clipboard_shortcut_frame(&mut app, &ctx, shifted, vec![egui::Event::Cut]);
    assert!(matches!(commands.as_slice(), [Command::SaveAs]));
    for (event, key, expected) in [
        (egui::Event::Cut, Key::X, Command::Cut),
        (egui::Event::Copy, Key::C, Command::Copy),
        (egui::Event::Paste("x".into()), Key::V, Command::Paste),
    ] {
        let commands = clipboard_shortcut_frame(
            &mut app,
            &ctx,
            ctrl,
            vec![event, clipboard_key_event(key, true, ctrl)],
        );
        assert_eq!(commands.len(), 1);
        assert_eq!(
            std::mem::discriminant(&commands[0]),
            std::mem::discriminant(&expected)
        );
    }
    assert!(
        clipboard_shortcut_frame(
            &mut app,
            &ctx,
            ctrl,
            vec![clipboard_key_event(Key::V, false, ctrl)]
        )
        .is_empty()
    );
}

#[test]
fn image_only_paste_recovers_unpaired_ctrl_v_release_without_plain_v_side_effects() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let ctx = egui::Context::default();
    let ctrl = parse_shortcut("Ctrl+V").unwrap().modifiers;
    let commands = clipboard_shortcut_frame(
        &mut app,
        &ctx,
        ctrl,
        vec![clipboard_key_event(Key::V, false, ctrl)],
    );
    assert!(matches!(commands.as_slice(), [Command::Paste]));
    assert!(
        clipboard_shortcut_frame(
            &mut app,
            &ctx,
            egui::Modifiers::NONE,
            vec![clipboard_key_event(Key::V, false, egui::Modifiers::NONE)]
        )
        .is_empty()
    );
    app.shortcuts.insert("Paste".into(), "Ctrl+P".into());
    assert!(
        clipboard_shortcut_frame(
            &mut app,
            &ctx,
            ctrl,
            vec![clipboard_key_event(Key::V, false, ctrl)]
        )
        .is_empty()
    );
    let commands = clipboard_shortcut_frame(
        &mut app,
        &ctx,
        ctrl,
        vec![clipboard_key_event(Key::P, true, ctrl)],
    );
    assert!(matches!(commands.as_slice(), [Command::Paste]));
}

#[test]
fn native_or_key_paste_does_not_repeat_on_a_later_release() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let ctx = egui::Context::default();
    let ctrl = parse_shortcut("Ctrl+V").unwrap().modifiers;
    for event in [
        egui::Event::Paste("x".into()),
        clipboard_key_event(Key::V, true, ctrl),
    ] {
        let commands = clipboard_shortcut_frame(&mut app, &ctx, ctrl, vec![event]);
        assert!(matches!(commands.as_slice(), [Command::Paste]));
        assert!(clipboard_shortcut_frame(&mut app, &ctx, ctrl, vec![]).is_empty());
        assert!(
            clipboard_shortcut_frame(
                &mut app,
                &ctx,
                ctrl,
                vec![clipboard_key_event(Key::V, false, ctrl)]
            )
            .is_empty()
        );
        assert!(!app.paste_key_handled);
    }
    app.paste_key_handled = true;
    assert!(
        clipboard_shortcut_frame(
            &mut app,
            &ctx,
            egui::Modifiers::NONE,
            vec![egui::Event::WindowFocused(false)]
        )
        .is_empty()
    );
    assert!(!app.paste_key_handled);
}

#[test]
fn ordinary_v_press_and_ctrl_release_in_one_frame_do_not_dispatch_clipboard_paste() {
    let mut app = editor(vec![Document::new(8, 8)]);
    app.tool = Tool::Brush;
    let ctx = egui::Context::default();
    let ctrl = parse_shortcut("Ctrl+V").unwrap().modifiers;
    let commands = clipboard_shortcut_frame(
        &mut app,
        &ctx,
        ctrl,
        vec![
            clipboard_key_event(Key::V, true, egui::Modifiers::NONE),
            clipboard_key_event(Key::V, false, ctrl),
        ],
    );
    assert!(
        commands.is_empty(),
        "Ctrl pressed after V must not turn its release into Paste"
    );
    assert_eq!(app.tool, Tool::Move);
    assert!(!app.paste_key_handled);
    ctx.input(|input| assert!(!input.key_down(Key::V)));
}

#[test]
fn clipboard_image_release_is_ignored_on_window_focus_loss() {
    let ctrl = parse_shortcut("Ctrl+V").unwrap().modifiers;
    for (focused, events, native_copy) in [
        // winit emits synthetic held-key releases before WindowFocused(false).
        (
            false,
            vec![
                clipboard_key_event(Key::V, false, ctrl),
                egui::Event::WindowFocused(false),
            ],
            false,
        ),
        (false, vec![clipboard_key_event(Key::V, false, ctrl)], false),
        (
            true,
            vec![
                clipboard_key_event(Key::V, false, ctrl),
                egui::Event::WindowFocused(false),
                egui::Event::WindowFocused(true),
            ],
            false,
        ),
        // Only the release fallback is suppressed; native events retain their dispatch rules.
        (
            false,
            vec![
                egui::Event::Copy,
                clipboard_key_event(Key::V, false, ctrl),
                egui::Event::WindowFocused(false),
            ],
            true,
        ),
    ] {
        let mut app = editor(vec![Document::new(8, 8)]);
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        let _ = ctx.run(
            egui::RawInput {
                modifiers: ctrl,
                events,
                focused,
                ..Default::default()
            },
            |ctx| {
                commands = app.shortcuts(ctx);
                assert!(app.shortcuts(ctx).is_empty());
            },
        );
        if native_copy {
            assert!(matches!(commands.as_slice(), [Command::Copy]));
        } else {
            assert!(
                commands.is_empty(),
                "Losing window focus must not recover an image paste"
            );
        }
        assert!(!app.paste_key_handled);
    }
}

#[test]
fn focused_text_keeps_native_clipboard_events_and_unicode_paste() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let before = app.session().doc.clone();
    let ctx = egui::Context::default();
    let field = egui::Id::new("native-clipboard-text-focus");
    let mut text = String::new();
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add(egui::TextEdit::singleline(&mut text).id(field));
            ctx.memory_mut(|memory| memory.request_focus(field));
        });
    });
    for event in [
        egui::Event::Cut,
        egui::Event::Copy,
        egui::Event::Paste("中文🙂".into()),
    ] {
        let _ = ctx.run(
            egui::RawInput {
                events: vec![event.clone()],
                ..Default::default()
            },
            |ctx| {
                assert!(ctx.wants_keyboard_input());
                assert!(app.shortcuts(ctx).is_empty());
                ctx.input(|input| assert!(input.events.contains(&event)));
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut text).id(field));
                });
            },
        );
    }
    assert_eq!(text, "中文🙂");
    assert_eq!(app.session().doc, before);
    assert!(app.pixel_clipboard.is_none());
    ctx.memory_mut(|memory| memory.surrender_focus(field));
    let ctrl = parse_shortcut("Ctrl+V").unwrap().modifiers;
    assert!(
        clipboard_shortcut_frame(
            &mut app,
            &ctx,
            ctrl,
            vec![clipboard_key_event(Key::V, false, ctrl)]
        )
        .is_empty(),
        "Text paste must not become an image paste when focus changes before release"
    );
}

#[test]
fn image_only_paste_release_uses_existing_internal_pixels_and_history() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let before = app.session().doc.clone();
    let raster = Raster::solid(2, 3, [10, 40, 90, 127]);
    app.pixel_clipboard = Some((raster.clone(), [1., 2.]));
    app.clipboard_sequence = clipboard_sequence();
    let ctx = egui::Context::default();
    let ctrl = parse_shortcut("Ctrl+V").unwrap().modifiers;
    let commands = clipboard_shortcut_frame(
        &mut app,
        &ctx,
        ctrl,
        vec![clipboard_key_event(Key::V, false, ctrl)],
    );
    assert!(matches!(commands.as_slice(), [Command::Paste]));
    for command in commands {
        app.command(command);
    }
    assert_eq!(app.session().doc.layers.len(), before.layers.len() + 1);
    let pasted = app.session().doc.active().unwrap();
    assert_eq!(pasted.image.as_deref().unwrap(), &raster);
    assert_eq!(pasted.transform.origin, [1., 2.]);
    let after = app.session().doc.clone();
    app.command(Command::Undo);
    assert_eq!(app.session().doc, before);
    app.command(Command::Redo);
    assert_eq!(app.session().doc, after);
}

fn assert_point_close(actual: [f64; 2], expected: [f64; 2]) {
    for axis in 0..2 {
        assert!(
            (actual[axis] - expected[axis]).abs() < 1e-9,
            "{actual:?} != {expected:?}"
        );
    }
}

#[test]
fn transform_frame_uses_rotated_multi_layer_union_and_explicit_mask_placement() {
    let mut doc = Document::new(400, 300);
    doc.layers[0].transform = crate::model::Transform::new(10., 20., 100., 50.);
    doc.layers[0].transform.rotation = 90.;
    let mut other = Layer::new("Other", 8, 8);
    other.transform = crate::model::Transform::new(200., 100., 20., 30.);
    let ids = HashSet::from([doc.layers[0].id, other.id]);
    doc.layers.push(other);
    let frame = transform_frame(&doc, &ids, false).unwrap();
    assert_point_close(frame.origin, [35., -5.]);
    assert_point_close(frame.size, [185., 135.]);
    assert_eq!(frame.rotation, 0.);
    let placement = crate::model::Transform::new(7., 9., 3., 4.);
    doc.layers[0].mask = Some(crate::model::Mask {
        raster: Arc::new(Raster::solid(3, 4, [255; 4])),
        enabled: true,
        linked: false,
        placement: Some(placement),
    });
    assert_eq!(transform_frame(&doc, &ids, true), Some(placement));
}

#[test]
fn rotated_resize_preserves_opposite_corner_and_centered_resize_preserves_center() {
    let mut old = crate::model::Transform::new(10., 20., 100., 50.);
    old.rotation = 37.;
    let start = old.to_document([1., 1.]);
    let resized = resize_transform(old, 2, start, old.to_document([1.5, 1.5]), false, false);
    assert_point_close(resized.size, [150., 75.]);
    assert_point_close(resized.to_document([0., 0.]), old.to_document([0., 0.]));
    assert_eq!(resized.rotation, old.rotation);
    let centered = resize_transform(old, 2, start, old.to_document([1.25, 1.25]), true, false);
    assert_point_close(centered.size, [150., 75.]);
    assert_point_close(
        centered.to_document([0.5, 0.5]),
        old.to_document([0.5, 0.5]),
    );
}

#[test]
fn resize_crossing_the_opposite_corner_flips_both_axes_and_keeps_anchor() {
    let mut old = crate::model::Transform::new(10., 20., 100., 50.);
    old.rotation = -28.;
    let resized = resize_transform(
        old,
        2,
        old.to_document([1., 1.]),
        old.to_document([-0.5, -0.5]),
        false,
        false,
    );
    assert_point_close(resized.size, [50., 25.]);
    assert!(resized.flip_x && resized.flip_y);
    assert_point_close(resized.to_document([0., 0.]), old.to_document([0., 0.]));
    assert_point_close(resized.to_document([1., 1.]), old.to_document([-0.5, -0.5]));
}

fn distort_drag(document: &Document) -> Drag {
    Drag {
        target: None,
        before: document.clone(),
        work_before: document.clone(),
        start: [8., 8.],
        last: [9., 9.],
        points: Vec::new(),
        mode: Tool::Move,
        selection_mode: SelectionMode::Replace,
        selection_antialias: true,
        constrain_armed: true,
        handle: Some(2),
        pan: Vec2::ZERO,
        distort: true,
        rotate: false,
        centered: false,
        pixel_move: false,
        outline_move: false,
        duplicate_pixels: false,
    }
}

#[test]
fn warp_preview_rejects_old_generation_and_other_session_results() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(8, 8)]);
    app.drag = Some(distort_drag(&app.session().doc));
    app.warp_generation = 4;
    app.warp_preview_busy = true;
    let serial = app.session().serial;
    let preview = app.session().doc.layers[0].clone();
    let worker = Worker::WarpPreview {
        session: app.session().id,
        generation: 3,
        result: Ok(preview.clone()),
    };
    send(&mut app, &egui::Context::default(), worker);
    assert!(app.warp_preview.is_none());
    assert!(app.warp_preview_busy);
    assert_eq!(app.session().serial, serial);
    let worker = Worker::WarpPreview {
        session: app.tabs[1].id,
        generation: 4,
        result: Ok(preview.clone()),
    };
    send(&mut app, &egui::Context::default(), worker);
    assert!(app.warp_preview.is_none());
    assert!(!app.warp_preview_busy);
    let worker = Worker::WarpPreview {
        session: app.session().id,
        generation: 4,
        result: Ok(preview.clone()),
    };
    send(&mut app, &egui::Context::default(), worker);
    assert_eq!(app.warp_preview, Some((app.session().id, preview)));
    assert!(app.session().serial > serial);
}

#[test]
fn escape_restores_distortion_drag_and_invalidates_its_queued_preview() {
    let mut app = editor(vec![Document::new(8, 8)]);
    let before = app.session().doc.clone();
    app.drag = Some(distort_drag(&before));
    app.session_mut().doc.layers[0].opacity = 0.5;
    app.warp_generation = 7;
    app.warp_preview_busy = true;
    app.warp_preview = Some((app.session().id, before.layers[0].clone()));
    let ctx = egui::Context::default();
    shortcut_key(&mut app, &ctx, Key::Escape, egui::Modifiers::NONE);
    app.update_warp_preview();
    assert!(app.drag.is_none());
    assert_eq!(app.session().doc, before);
    assert!(app.warp_preview.is_none());
    assert!(!app.warp_preview_busy);
    assert_eq!(app.warp_generation, 8);
    let worker = Worker::WarpPreview {
        session: app.session().id,
        generation: 7,
        result: Ok(before.layers[0].clone()),
    };
    send(&mut app, &ctx, worker);
    assert!(app.warp_preview.is_none());
}

fn canvas_frame(app: &mut EditorApp, ctx: &egui::Context, events: Vec<egui::Event>) -> Pos2 {
    canvas_frame_modifiers(app, ctx, events, egui::Modifiers::NONE)
}

fn canvas_frame_modifiers(
    app: &mut EditorApp,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
) -> Pos2 {
    let mut origin = Pos2::ZERO;
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280., 860.))),
            events,
            modifiers,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let size = Vec2::new(
                    app.session().doc.width as f32,
                    app.session().doc.height as f32,
                ) * app.session().zoom;
                origin = ui.available_rect_before_wrap().center() + app.session().pan - size / 2.;
                app.canvas(ui);
            });
        },
    );
    origin
}

fn canvas_pointer(app: &mut EditorApp, ctx: &egui::Context, pos: Pos2, pressed: bool) {
    canvas_frame(
        app,
        ctx,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            },
        ],
    );
}

fn selection_pointer(
    app: &mut EditorApp,
    ctx: &egui::Context,
    pos: Pos2,
    pressed: bool,
    modifiers: egui::Modifiers,
) {
    canvas_frame_modifiers(
        app,
        ctx,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            },
        ],
        modifiers,
    );
}

#[test]
fn marquee_and_lasso_capture_add_subtract_and_antialias_at_pointer_press() {
    for tool in [Tool::Rectangle, Tool::Ellipse, Tool::Lasso] {
        for (modifiers, mode) in [
            (egui::Modifiers::SHIFT, SelectionMode::Add),
            (egui::Modifiers::ALT, SelectionMode::Subtract),
            (
                egui::Modifiers {
                    shift: true,
                    alt: true,
                    ..Default::default()
                },
                SelectionMode::Subtract,
            ),
        ] {
            let mut document = Document::new(64, 48);
            document.selection = Some(selection::marquee(
                64,
                48,
                [12., 12., 30., 26.],
                false,
                false,
            ));
            let mut app = editor(vec![document.clone()]);
            app.session_mut().zoom = 8.;
            app.tool = tool;
            app.antialias = false;
            app.selection_mode = SelectionMode::Replace;
            let ctx = egui::Context::default();
            let origin = canvas_frame(&mut app, &ctx, vec![]);
            let screen =
                |point: [f64; 2]| origin + Vec2::new(point[0] as f32, point[1] as f32) * 8.;
            selection_pointer(&mut app, &ctx, screen([28., 20.]), true, modifiers);
            let points = if tool == Tool::Lasso {
                vec![[48., 20.], [48., 30.], [28., 30.]]
            } else {
                vec![[48., 30.]]
            };
            // Releasing the modifier before egui starts the drag must retain the press mode.
            for point in points {
                canvas_frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::PointerMoved(screen(point))],
                );
            }
            let drag = app
                .drag
                .as_ref()
                .expect("selection drag starts with live pointer input");
            assert_eq!(drag.selection_mode, mode);
            assert!(!drag.selection_antialias);
            assert_eq!(drag.start, [28., 20.]);
            app.selection_mode = SelectionMode::Intersect;
            app.antialias = true;
            let end = if tool == Tool::Lasso {
                [28., 30.]
            } else {
                [48., 30.]
            };
            selection_pointer(&mut app, &ctx, screen(end), false, egui::Modifiers::NONE);
            let incoming = if tool == Tool::Lasso {
                selection::polygon(
                    64,
                    48,
                    &[[28., 20.], [48., 20.], [48., 30.], [28., 30.]],
                    false,
                )
            } else {
                selection::marquee(64, 48, [28., 20., 20., 10.], tool == Tool::Ellipse, false)
            };
            let expected = selection::combine(document.selection.as_ref(), &incoming, mode);
            assert_eq!(
                app.session().doc.selection,
                Some(expected),
                "{tool:?} {mode:?}"
            );
            assert_eq!(app.session().history.undo_name(), Some("Selection"));
            app.command(Command::Undo);
            assert_eq!(app.session().doc, document);
        }
    }
}

#[test]
fn marquee_shift_constrains_only_after_press_add_modifier_is_released_and_pressed_again() {
    for tool in [Tool::Rectangle, Tool::Ellipse] {
        let mut document = Document::new(64, 48);
        document.selection = Some(selection::marquee(64, 48, [2., 2., 6., 6.], false, false));
        let mut app = editor(vec![document.clone()]);
        app.session_mut().zoom = 8.;
        app.tool = tool;
        app.antialias = false;
        let ctx = egui::Context::default();
        let origin = canvas_frame(&mut app, &ctx, vec![]);
        let start = origin + Vec2::new(20., 12.) * 8.;
        let end = origin + Vec2::new(40., 22.) * 8.;
        selection_pointer(&mut app, &ctx, start, true, egui::Modifiers::SHIFT);
        canvas_frame_modifiers(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(end)],
            egui::Modifiers::SHIFT,
        );
        assert_eq!(app.drag.as_ref().unwrap().last, [40., 22.]);
        assert!(!app.drag.as_ref().unwrap().constrain_armed);
        canvas_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)]);
        assert!(app.drag.as_ref().unwrap().constrain_armed);
        canvas_frame_modifiers(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(end)],
            egui::Modifiers::SHIFT,
        );
        selection_pointer(&mut app, &ctx, end, false, egui::Modifiers::SHIFT);
        let square = selection::marquee(64, 48, [20., 12., 20., 20.], tool == Tool::Ellipse, false);
        let expected = selection::combine(document.selection.as_ref(), &square, SelectionMode::Add);
        assert_eq!(app.session().doc.selection, Some(expected), "{tool:?}");
        assert_eq!(app.session().doc.selection.as_ref().unwrap().get(3, 3), 255);
        app.command(Command::Undo);
        assert_eq!(app.session().doc, document);
    }
}

#[test]
fn polygon_first_click_captures_mode_and_antialias_for_enter_commit() {
    let points = [[28.25, 20.25], [48.25, 22.75], [42.75, 37.25]];
    for (choice, modifiers, mode, antialias) in [
        (
            SelectionMode::Replace,
            egui::Modifiers::SHIFT,
            SelectionMode::Add,
            false,
        ),
        (
            SelectionMode::Add,
            egui::Modifiers::ALT,
            SelectionMode::Subtract,
            true,
        ),
        (
            SelectionMode::Intersect,
            egui::Modifiers::NONE,
            SelectionMode::Intersect,
            false,
        ),
    ] {
        let mut document = Document::new(64, 48);
        document.selection = Some(selection::marquee(
            64,
            48,
            [12., 12., 30., 26.],
            false,
            false,
        ));
        let mut app = editor(vec![document.clone()]);
        app.session_mut().zoom = 8.;
        app.tool = Tool::Polygon;
        app.antialias = antialias;
        app.selection_mode = choice;
        let ctx = egui::Context::default();
        let origin = canvas_frame(&mut app, &ctx, vec![]);
        for (index, point) in points.into_iter().enumerate() {
            let screen = origin + Vec2::new(point[0] as f32, point[1] as f32) * 8.;
            let held = if index == 0 {
                modifiers
            } else {
                egui::Modifiers::NONE
            };
            selection_pointer(&mut app, &ctx, screen, true, held);
            selection_pointer(&mut app, &ctx, screen, false, held);
            if index == 0 {
                assert_eq!(app.polygon_options, Some((mode, antialias)));
                app.selection_mode = SelectionMode::Replace;
                app.antialias = !antialias;
            }
        }
        assert_eq!(app.polygon, points);
        shortcut_key(&mut app, &ctx, Key::Enter, egui::Modifiers::NONE);
        let incoming = selection::polygon(64, 48, &points, antialias);
        assert!(
            selection::polygon(64, 48, &points, true)
                .canvas_coverage()
                .iter()
                .any(|&v| v > 0 && v < 255)
        );
        if !antialias {
            assert!(
                incoming
                    .canvas_coverage()
                    .iter()
                    .all(|&v| v == 0 || v == 255)
            );
        }
        let expected = selection::combine(document.selection.as_ref(), &incoming, mode);
        assert_eq!(app.session().doc.selection, Some(expected));
        assert!(app.polygon.is_empty());
        assert!(app.polygon_options.is_none());
        assert_eq!(app.session().history.undo_name(), Some("Polygon selection"));
        app.command(Command::Undo);
        assert_eq!(app.session().doc, document);
    }
}

fn toolbar_frame(
    app: &mut EditorApp,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280., 860.))),
            events,
            ..Default::default()
        },
        |ctx| {
            let mut actions = app.menu(ctx);
            actions.extend(app.workspace_toolbar(ctx));
            actions.extend(app.tool_header(ctx));
            for action in actions {
                app.command(action);
            }
        },
    )
}

fn click_toolbar_text(app: &mut EditorApp, ctx: &egui::Context, label: &str) {
    let translated_label = tr(label);
    let label = translated_label.as_ref();
    let output = toolbar_frame(app, ctx, vec![]);
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("toolbar choice {label:?} is visible"));
    for pressed in [true, false] {
        toolbar_frame(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ],
        );
    }
}

#[test]
fn chinese_shape_toolbar_creates_an_ellipse_with_canonical_project_settings() {
    let mut app = editor(vec![Document::new(64, 48)]);
    app.set_tool(Tool::Shape);
    app.snap = false;
    app.session_mut().zoom = 8.;
    let menu_ctx = egui::Context::default();
    toolbar_frame(&mut app, &menu_ctx, vec![]);
    click_toolbar_text(&mut app, &menu_ctx, "矩形");
    click_toolbar_text(&mut app, &menu_ctx, "椭圆");
    assert_eq!(app.shape_kind, "Ellipse");

    let ctx = egui::Context::default();
    let origin = canvas_frame(&mut app, &ctx, vec![]);
    let start = origin + Vec2::new(8., 7.) * 8.;
    let end = origin + Vec2::new(40., 32.) * 8.;
    canvas_pointer(&mut app, &ctx, start, true);
    canvas_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)]);
    assert_eq!(
        app.drag.as_ref().expect("shape drag started").mode,
        Tool::Shape
    );
    canvas_pointer(&mut app, &ctx, end, false);
    assert!(app.session().edit_busy, "shape worker was scheduled");
    while app.session().edit_busy {
        let event = app
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("shape worker completes");
        send(&mut app, &ctx, event);
    }
    let layer = app.session().doc.active().unwrap();
    assert_eq!(layer.shape.as_ref().unwrap()["kind"], "Ellipse");
    assert!(layer.image.as_ref().unwrap().get(24, 19)[3] > 0);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("中文形状.comp");
    project::save(&app.session().doc, &path).unwrap();
    let restored = project::load(&path).unwrap();
    assert_eq!(
        restored.active().unwrap().shape.as_ref().unwrap()["kind"],
        "Ellipse"
    );
}

#[test]
fn polygon_uses_add_mode_chosen_through_real_toolbar_combo_box() {
    let mut document = Document::new(64, 48);
    document.selection = Some(selection::marquee(64, 48, [2., 2., 6., 6.], false, false));
    let mut app = editor(vec![document.clone()]);
    app.session_mut().zoom = 8.;
    app.tool = Tool::Polygon;
    app.antialias = false;
    let menu_ctx = egui::Context::default();
    toolbar_frame(&mut app, &menu_ctx, vec![]);
    click_toolbar_text(&mut app, &menu_ctx, "替换");
    click_toolbar_text(&mut app, &menu_ctx, "添加");
    assert_eq!(app.selection_mode, SelectionMode::Add);
    let points = [[28.25, 20.25], [48.25, 22.75], [42.75, 37.25]];
    let ctx = egui::Context::default();
    let origin = canvas_frame(&mut app, &ctx, vec![]);
    for point in points {
        let screen = origin + Vec2::new(point[0] as f32, point[1] as f32) * 8.;
        selection_pointer(&mut app, &ctx, screen, true, egui::Modifiers::NONE);
        selection_pointer(&mut app, &ctx, screen, false, egui::Modifiers::NONE);
    }
    shortcut_key(&mut app, &ctx, Key::Enter, egui::Modifiers::NONE);
    let incoming = selection::polygon(64, 48, &points, false);
    let expected = selection::combine(document.selection.as_ref(), &incoming, SelectionMode::Add);
    assert_eq!(app.session().doc.selection, Some(expected));
    assert_eq!(app.session().doc.selection.as_ref().unwrap().get(3, 3), 255);
}

#[test]
fn crop_ratio_toolbar_reshapes_existing_frame_then_real_resize_and_commit_keep_the_choice() {
    let mut document = Document::new(400, 300);
    document.layers[0].image = Some(Arc::new(Raster::solid(400, 300, [20, 30, 40, 255])));
    let mut app = editor(vec![document.clone()]);
    app.session_mut().zoom = 1.;
    app.set_tool(Tool::Crop);
    app.crop_rect = Some([50., 60., 200., 120.]);
    app.snap = false;
    let serial = app.session().serial;
    let menu_ctx = egui::Context::default();
    toolbar_frame(&mut app, &menu_ctx, vec![]);
    click_toolbar_text(&mut app, &menu_ctx, "Free");
    click_toolbar_text(&mut app, &menu_ctx, "1:1");
    assert_eq!(app.crop_rect, Some([50., 20., 200., 200.]));
    assert_eq!(app.session().doc, document);
    assert_eq!(app.session().serial, serial);
    assert!(!app.session().history.is_modified());

    let ctx = egui::Context::default();
    let origin = canvas_frame(&mut app, &ctx, vec![]);
    canvas_pointer(&mut app, &ctx, origin + Vec2::new(250., 220.), true);
    canvas_frame(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(origin + Vec2::new(280., 245.))],
    );
    canvas_pointer(&mut app, &ctx, origin + Vec2::new(280., 245.), false);
    let frame = app.crop_rect.unwrap();
    assert_eq!(frame[2], frame[3]);
    assert_eq!(app.session().doc, document);
    app.command(Command::CropCommit);
    assert_eq!(app.session().doc.width, app.session().doc.height);
    assert!(Arc::ptr_eq(
        app.session().doc.layers[0].image.as_ref().unwrap(),
        document.layers[0].image.as_ref().unwrap(),
    ));
    app.command(Command::Undo);
    assert_eq!(app.session().doc, document);
    app.command(Command::Redo);
    assert_eq!(app.session().doc.width, app.session().doc.height);
}

#[test]
fn crop_original_ratio_tracks_each_document_and_free_retains_the_current_frame() {
    let mut app = editor(vec![Document::new(400, 300), Document::new(300, 600)]);
    app.set_tool(Tool::Crop);
    app.crop_rect = Some([50., 60., 200., 120.]);
    let ctx = egui::Context::default();
    toolbar_frame(&mut app, &ctx, vec![]);
    click_toolbar_text(&mut app, &ctx, "Free");
    click_toolbar_text(&mut app, &ctx, "1:1");
    click_toolbar_text(&mut app, &ctx, "1:1");
    click_toolbar_text(&mut app, &ctx, "Original");
    assert_eq!(app.crop_aspect_ratio(), Some(4. / 3.));
    assert_eq!(app.crop_rect, Some([50., 45., 200., 150.]));
    click_toolbar_text(&mut app, &ctx, "Original");
    click_toolbar_text(&mut app, &ctx, "Free");
    assert_eq!(app.crop_rect, Some([50., 45., 200., 150.]));
    click_toolbar_text(&mut app, &ctx, "Free");
    click_toolbar_text(&mut app, &ctx, "Original");
    app.activate_tab(1);
    app.initialize_crop();
    assert_eq!(app.crop_aspect_ratio(), Some(0.5));
    assert_eq!(app.crop_rect, Some([0., 0., 300., 600.]));
    assert!(!app.tabs[0].history.is_modified());
    assert!(!app.tabs[1].history.is_modified());
}

#[test]
fn canvas_crop_pointer_moves_only_overlay_then_commit_translates_and_undo_restores() {
    let mut document = Document::new(400, 300);
    document.layers[0].image = Some(Arc::new(Raster::solid(400, 300, [20, 30, 40, 255])));
    let mut app = editor(vec![document.clone()]);
    app.session_mut().zoom = 1.;
    app.tool = Tool::Crop;
    app.crop_session = Some(app.session().id);
    app.crop_rect = Some([50., 60., 200., 120.]);
    app.snap = false;
    let serial = app.session().serial;
    let ctx = egui::Context::default();
    let origin = canvas_frame(&mut app, &ctx, vec![]);
    let start = origin + Vec2::new(100., 100.);
    canvas_pointer(&mut app, &ctx, start, true);
    for step in 1..=10 {
        canvas_frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(
                start + Vec2::new(step as f32, step as f32 * 2.),
            )],
        );
        assert_eq!(app.session().doc, document);
        assert_eq!(app.session().serial, serial);
        assert!(!app.session().history.is_modified());
    }
    canvas_pointer(&mut app, &ctx, start + Vec2::new(10., 20.), false);
    assert_eq!(app.crop_rect, Some([60., 80., 200., 120.]));
    assert!(app.crop_drag.is_none());
    app.command(Command::CropCommit);
    assert_eq!(
        [app.session().doc.width, app.session().doc.height],
        [200, 120]
    );
    assert_eq!(app.session().doc.layers[0].transform.origin, [-60., -80.]);
    assert!(Arc::ptr_eq(
        app.session().doc.layers[0].image.as_ref().unwrap(),
        document.layers[0].image.as_ref().unwrap(),
    ));
    assert!(app.crop_rect.is_none());
    app.command(Command::Undo);
    assert_eq!(app.session().doc, document);
    app.command(Command::Redo);
    assert_eq!(
        [app.session().doc.width, app.session().doc.height],
        [200, 120]
    );
}

#[test]
fn canvas_crop_pointer_creates_outside_old_frame_and_resizes_its_side_handle() {
    let mut app = editor(vec![Document::new(400, 300)]);
    app.session_mut().zoom = 1.;
    app.tool = Tool::Crop;
    app.crop_session = Some(app.session().id);
    app.crop_rect = Some([50., 60., 200., 120.]);
    app.snap = false;
    let ctx = egui::Context::default();
    let origin = canvas_frame(&mut app, &ctx, vec![]);
    canvas_pointer(&mut app, &ctx, origin + Vec2::new(20., 20.), true);
    canvas_frame(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(origin + Vec2::new(160., 120.))],
    );
    canvas_pointer(&mut app, &ctx, origin + Vec2::new(160., 120.), false);
    assert_eq!(app.crop_rect, Some([20., 20., 140., 100.]));
    canvas_pointer(&mut app, &ctx, origin + Vec2::new(160., 70.), true);
    canvas_frame(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(origin + Vec2::new(180., 70.))],
    );
    canvas_pointer(&mut app, &ctx, origin + Vec2::new(180., 70.), false);
    assert_eq!(app.crop_rect, Some([20., 20., 160., 100.]));
    assert!(!app.session().history.is_modified());
}

#[test]
fn entering_crop_uses_selection_origin_and_extent_without_editing_pixels() {
    let mut document = Document::new(400, 300);
    let mut selection = Selection::empty(400, 300);
    for y in 20..50 {
        for x in 10..80 {
            selection.set(x, y, 255);
        }
    }
    document.selection = Some(selection);
    let mut app = editor(vec![document.clone()]);
    app.crop_ratio = [1, 1];
    app.set_tool(Tool::Crop);
    assert_eq!(app.crop_ratio, [0, 0]);
    assert_eq!(app.crop_rect, Some([10., 20., 70., 30.]));
    assert_eq!(app.crop_session, Some(app.session().id));
    assert_eq!(app.session().doc, document);
    assert!(!app.session().history.is_modified());
    app.set_tool(Tool::Move);
    assert!(app.crop_rect.is_none());
    app.crop_ratio = [3, 4];
    app.set_tool(Tool::Crop);
    assert_eq!(app.crop_ratio, [0, 0]);
    assert_eq!(app.crop_rect, Some([10., 20., 70., 30.]));
    assert_eq!(app.session().doc, document);
}

fn snapping_document() -> Document {
    let mut document = Document::new(300, 200);
    document.layers[0].image = Some(Arc::new(Raster::solid(100, 100, [255; 4])));
    document.layers[0].transform = crate::model::Transform::new(10., 10., 100., 100.);
    document
}

#[test]
fn move_snapping_uses_the_closest_edge_or_center_and_respects_target_switches() {
    let document = snapping_document();
    let mut app = editor(vec![document.clone()]);
    app.session_mut().zoom = 1.;
    app.snap = true;
    app.snap_targets = [false, false, true, false];
    app.session_mut().doc.guides.push(crate::model::Guide {
        id: Uuid::new_v4(),
        axis: "vertical".into(),
        position: 150.,
    });
    let before = app.session().doc.clone();
    assert_eq!(app.snap_delta([37., 0.], &before), [40., 0.]);
    assert_eq!(app.snap_delta([20., 0.], &before), [20., 0.]);
    app.session_mut().doc.guides[0].position = 160.;
    let before = app.session().doc.clone();
    assert_eq!(app.snap_delta([98., 0.], &before), [100., 0.]);
    app.snap_targets[2] = false;
    assert_eq!(app.snap_delta([98., 0.], &before), [98., 0.]);
    app.snap_targets[2] = true;
    app.snap = false;
    assert_eq!(app.snap_delta([98., 0.], &before), [98., 0.]);
}

#[test]
fn shift_canvas_move_keeps_the_locked_axis_after_snapping() {
    let mut document = snapping_document();
    document.guides.extend([
        crate::model::Guide {
            id: Uuid::new_v4(),
            axis: "vertical".into(),
            position: 150.,
        },
        crate::model::Guide {
            id: Uuid::new_v4(),
            axis: "horizontal".into(),
            position: 14.,
        },
    ]);
    let mut app = editor(vec![document]);
    app.session_mut().zoom = 1.;
    app.tool = Tool::Move;
    app.snap = true;
    app.snap_targets = [false, false, true, false];
    let ctx = egui::Context::default();
    let origin = canvas_frame(&mut app, &ctx, vec![]);
    let start = origin + Vec2::new(50., 50.);
    canvas_pointer(&mut app, &ctx, start, true);
    canvas_frame_modifiers(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(start + Vec2::new(37., 2.))],
        egui::Modifiers::SHIFT,
    );
    assert_eq!(app.session().doc.layers[0].transform.origin, [50., 10.]);
    assert_eq!(app.session().doc.layers[0].transform.size, [100., 100.]);
    canvas_pointer(&mut app, &ctx, start + Vec2::new(37., 2.), false);
    assert_eq!(app.session().doc.layers[0].transform.origin, [50., 10.]);
    assert_eq!(app.session().history.undo_name(), Some("Transform"));
}

fn start_canvas_brush(app: &mut EditorApp, ctx: &egui::Context) -> Pos2 {
    app.session_mut().zoom = 4.;
    app.tool = Tool::Brush;
    app.diameter = 4.;
    app.hardness = 1.;
    app.opacity = 1.;
    app.foreground = [220, 30, 40, 255];
    let origin = canvas_frame(app, ctx, vec![]);
    let start = origin + Vec2::new(8.5, 8.5) * 4.;
    let end = origin + Vec2::new(24.5, 8.5) * 4.;
    canvas_pointer(app, ctx, start, true);
    canvas_frame(app, ctx, vec![egui::Event::PointerMoved(end)]);
    assert!(app.drag.is_some());
    assert!(app.brush_stroke.is_some());
    assert_ne!(app.session().doc, app.drag.as_ref().unwrap().before);
    end
}

#[test]
fn shift_brush_line_reuses_only_same_session_layer_and_pixel_or_mask_target() {
    let mut document = Document::new(64, 48);
    document.layers[0].mask = Some(Mask::white());
    let mut other = Layer::new("Other", 64, 48);
    other.mask = Some(Mask::white());
    let first = document.layers[0].id;
    let second = other.id;
    document.layers.push(other);
    let mut app = editor(vec![document.clone(), document]);
    app.session_mut().zoom = 8.;
    app.tool = Tool::Brush;
    app.diameter = 2.;
    app.hardness = 1.;
    app.opacity = 1.;
    app.smoothing = 0.;
    app.foreground = [220, 30, 40, 255];
    let ctx = egui::Context::default();
    let origin = canvas_frame(&mut app, &ctx, vec![]);
    let screen = |point: [f32; 2]| origin + Vec2::new(point[0], point[1]) * 8.;
    for (point, modifiers) in [
        ([8.5, 8.5], egui::Modifiers::NONE),
        ([24.5, 8.5], egui::Modifiers::SHIFT),
    ] {
        selection_pointer(&mut app, &ctx, screen(point), true, modifiers);
        selection_pointer(&mut app, &ctx, screen(point), false, modifiers);
    }
    assert_eq!(
        app.last_brush,
        Some((app.session().id, first, false, [24.5, 8.5]))
    );
    assert!(
        app.session()
            .doc
            .layer(first)
            .unwrap()
            .image
            .as_ref()
            .unwrap()
            .get(16, 8)[3]
            > 0
    );
    app.session_mut().doc.active_layer_id = Some(second);
    app.session_mut().selected = HashSet::from([second]);
    assert_eq!(app.brush_line_start([48.5, 8.5]), [48.5, 8.5]);
    selection_pointer(
        &mut app,
        &ctx,
        screen([48.5, 8.5]),
        true,
        egui::Modifiers::SHIFT,
    );
    selection_pointer(
        &mut app,
        &ctx,
        screen([48.5, 8.5]),
        false,
        egui::Modifiers::SHIFT,
    );
    let image = app
        .session()
        .doc
        .layer(second)
        .unwrap()
        .image
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(image.get(36, 8)[3], 0);
    assert!(image.get(48, 8)[3] > 0);
    app.mask_target = true;
    app.foreground = [0, 0, 0, 255];
    assert_eq!(app.brush_line_start([48.5, 30.5]), [48.5, 30.5]);
    selection_pointer(
        &mut app,
        &ctx,
        screen([48.5, 30.5]),
        true,
        egui::Modifiers::SHIFT,
    );
    selection_pointer(
        &mut app,
        &ctx,
        screen([48.5, 30.5]),
        false,
        egui::Modifiers::SHIFT,
    );
    let layer = app.session().doc.layer(second).unwrap();
    assert!(Arc::ptr_eq(layer.image.as_ref().unwrap(), &image));
    assert_eq!(layer.mask.as_ref().unwrap().raster.get(48, 20), [255; 4]);
    assert_eq!(layer.mask.as_ref().unwrap().raster.get(48, 30)[0], 0);
    app.clone_source = Some([4., 5.]);
    app.clone_offset = Some([6., 7.]);
    app.activate_tab(1);
    assert!(app.last_brush.is_none());
    assert!(app.clone_source.is_none());
    assert!(app.clone_offset.is_none());
    app.session_mut().zoom = 8.;
    app.mask_target = false;
    app.foreground = [220, 30, 40, 255];
    assert_eq!(app.brush_line_start([48.5, 30.5]), [48.5, 30.5]);
    selection_pointer(
        &mut app,
        &ctx,
        screen([48.5, 30.5]),
        true,
        egui::Modifiers::SHIFT,
    );
    selection_pointer(
        &mut app,
        &ctx,
        screen([48.5, 30.5]),
        false,
        egui::Modifiers::SHIFT,
    );
    let image = app.session().doc.active().unwrap().image.as_ref().unwrap();
    assert_eq!(image.get(28, 20)[3], 0);
    assert!(image.get(48, 30)[3] > 0);
}

#[test]
fn move_auto_select_and_control_toggle_choose_target_on_real_click_and_drag() {
    for (automatic, control, picks_other) in [
        (false, false, false),
        (true, false, true),
        (false, true, true),
        (true, true, false),
    ] {
        for dragged in [false, true] {
            let mut document = Document::new(200, 120);
            let mut first = Layer::raster("First", Raster::solid(40, 20, [255, 0, 0, 255]));
            first.transform.origin = [10., 10.];
            let mut second = Layer::raster("Second", Raster::solid(40, 20, [0, 0, 255, 255]));
            second.transform.origin = [100., 50.];
            let ids = [first.id, second.id];
            let images = [first.image.clone().unwrap(), second.image.clone().unwrap()];
            document.layers = vec![first, second];
            document.active_layer_id = Some(ids[0]);
            let mut app = editor(vec![document.clone()]);
            app.session_mut().zoom = 4.;
            app.tool = Tool::Move;
            app.transform_auto_select = automatic;
            app.snap = false;
            let ctx = egui::Context::default();
            let origin = canvas_frame(&mut app, &ctx, vec![]);
            let start = origin + Vec2::new(120., 60.) * 4.;
            let end = start + Vec2::new(15., 5.) * 4.;
            let modifiers = if control {
                egui::Modifiers::CTRL
            } else {
                egui::Modifiers::NONE
            };
            selection_pointer(&mut app, &ctx, start, true, modifiers);
            if dragged {
                canvas_frame_modifiers(
                    &mut app,
                    &ctx,
                    vec![egui::Event::PointerMoved(end)],
                    modifiers,
                );
            }
            selection_pointer(
                &mut app,
                &ctx,
                if dragged { end } else { start },
                false,
                modifiers,
            );
            let selected = usize::from(picks_other);
            assert_eq!(app.session().doc.active_layer_id, Some(ids[selected]));
            assert_eq!(app.session().selected, HashSet::from([ids[selected]]));
            assert_eq!(app.transform_auto_select, automatic);
            for (index, layer) in app.session().doc.layers.iter().enumerate() {
                assert!(Arc::ptr_eq(layer.image.as_ref().unwrap(), &images[index]));
                let mut expected = document.layers[index].transform.origin;
                if dragged && index == selected {
                    expected[0] += 15.;
                    expected[1] += 5.;
                }
                assert_point_close(layer.transform.origin, expected);
            }
            if dragged {
                assert_eq!(app.session().history.undo_name(), Some("Transform"));
                app.command(Command::Undo);
                for (index, layer) in app.session().doc.layers.iter().enumerate() {
                    assert_eq!(layer.transform, document.layers[index].transform);
                    assert!(Arc::ptr_eq(layer.image.as_ref().unwrap(), &images[index]));
                }
            } else {
                assert!(!app.session().history.is_modified());
            }
        }
    }
}

#[test]
fn corner_resize_keeps_ratio_with_default_lock_and_shift_temporarily_inverts_it() {
    for (locked, modifiers, proportional) in [
        (true, egui::Modifiers::NONE, true),
        (true, egui::Modifiers::SHIFT, false),
        (false, egui::Modifiers::NONE, false),
        (false, egui::Modifiers::SHIFT, true),
    ] {
        let mut document = Document::new(200, 120);
        let mut layer = Layer::raster("Image", Raster::solid(40, 20, [20, 70, 180, 255]));
        layer.transform.origin = [50., 40.];
        let id = layer.id;
        let image = layer.image.clone().unwrap();
        document.layers = vec![layer];
        document.active_layer_id = Some(id);
        let mut app = editor(vec![document.clone()]);
        app.session_mut().zoom = 4.;
        app.tool = Tool::Move;
        app.show_transform = true;
        app.transform_auto_select = false;
        app.locks_transform_ratio = locked;
        app.snap = false;
        let ctx = egui::Context::default();
        let origin = canvas_frame(&mut app, &ctx, vec![]);
        let start = origin + Vec2::new(90., 60.) * 4.;
        let end = start + Vec2::new(20., 20.) * 4.;
        selection_pointer(&mut app, &ctx, start, true, modifiers);
        canvas_frame_modifiers(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(end)],
            modifiers,
        );
        assert_eq!(app.drag.as_ref().unwrap().handle, Some(2));
        selection_pointer(&mut app, &ctx, end, false, modifiers);
        let layer = app.session().doc.active().unwrap();
        assert_point_close(layer.transform.origin, [50., 40.]);
        assert_point_close(
            layer.transform.size,
            if proportional { [80., 40.] } else { [60., 40.] },
        );
        assert!(Arc::ptr_eq(layer.image.as_ref().unwrap(), &image));
        assert_eq!(app.locks_transform_ratio, locked);
        assert_eq!(app.session().history.undo_name(), Some("Transform"));
        let resized = app.session().doc.clone();
        app.command(Command::Undo);
        assert_eq!(app.session().doc, document);
        assert!(Arc::ptr_eq(
            app.session().doc.active().unwrap().image.as_ref().unwrap(),
            &image
        ));
        app.command(Command::Redo);
        assert_eq!(app.session().doc, resized);
    }
}

#[test]
fn background_open_cancels_old_canvas_brush_before_activating_new_project() {
    let before = Document::new(64, 64);
    let mut app = editor(vec![before.clone()]);
    let ctx = egui::Context::default();
    let release = start_canvas_brush(&mut app, &ctx);
    let directory = tempfile::tempdir().unwrap();
    let incoming = imported("Background opened", None);
    let opened = incoming.document.clone();
    send(
        &mut app,
        &ctx,
        Worker::Open {
            path: directory.path().join("new.png"),
            result: Ok(incoming),
            target: None,
        },
    );
    assert_eq!(app.current, 1);
    assert_eq!(app.tabs[0].doc, before);
    assert!(!app.tabs[0].history.is_modified());
    assert_eq!(app.session().doc, opened);
    assert!(app.drag.is_none());
    assert!(app.brush_stroke.is_none());
    canvas_pointer(&mut app, &ctx, release, false);
    assert_eq!(app.session().doc, opened);
    assert!(!app.session().history.is_modified());
}

#[test]
fn background_open_restores_original_guide_drag_and_seals_visibility_history() {
    let mut before = Document::new(64, 64);
    let guide = crate::model::Guide {
        id: Uuid::new_v4(),
        axis: "horizontal".into(),
        position: 12.,
    };
    before.guides.push(guide.clone());
    let mut app = editor(vec![before.clone()]);
    app.guide_drag = Some(GuideDrag {
        before: before.clone(),
        id: guide.id,
        horizontal: true,
    });
    app.session_mut().doc.guides[0].position = 40.;
    let directory = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    send(
        &mut app,
        &ctx,
        Worker::Open {
            path: directory.path().join("guide.png"),
            result: Ok(imported("After guide", None)),
            target: None,
        },
    );
    assert_eq!(app.tabs[0].doc, before);
    assert!(app.guide_drag.is_none());
    assert!(app.session().doc.guides.is_empty());
    app.activate_tab(0);
    let id = app.session().doc.layers[0].id;
    app.visibility_sweep = Some((false, before.clone(), HashSet::from([id])));
    app.session_mut().doc.layers[0].visible = false;
    send(
        &mut app,
        &ctx,
        Worker::Open {
            path: directory.path().join("visibility.png"),
            result: Ok(imported("After visibility", None)),
            target: None,
        },
    );
    assert!(app.visibility_sweep.is_none());
    assert!(!app.tabs[0].doc.layers[0].visible);
    assert_eq!(app.tabs[0].history.undo_name(), Some("Layer visibility"));
    assert!(app.tabs[0].history.is_modified());
    assert!(app.session().doc.layers[0].visible);
    app.activate_tab(0);
    app.command(Command::Undo);
    assert_eq!(app.session().doc, before);
}

#[test]
fn import_into_current_project_cancels_brush_then_records_only_layer_import() {
    let before = Document::new(64, 64);
    let mut app = editor(vec![before.clone()]);
    let ctx = egui::Context::default();
    let release = start_canvas_brush(&mut app, &ctx);
    let target = app.session().id;
    let directory = tempfile::tempdir().unwrap();
    send(
        &mut app,
        &ctx,
        Worker::Open {
            path: directory.path().join("layer.png"),
            result: Ok(imported("Imported after brush", None)),
            target: Some(target),
        },
    );
    assert_eq!(app.current, 0);
    assert!(app.drag.is_none());
    assert!(app.brush_stroke.is_none());
    assert_eq!(app.session().doc.layers[0], before.layers[0]);
    assert_eq!(app.session().doc.layers.len(), 2);
    assert_eq!(app.session().history.undo_name(), Some("Import layers"));
    let imported = app.session().doc.clone();
    canvas_pointer(&mut app, &ctx, release, false);
    assert_eq!(app.session().doc, imported);
    app.command(Command::Undo);
    assert_eq!(app.session().doc, before);
}

#[test]
fn import_into_inactive_project_preserves_active_canvas_brush() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(64, 64)]);
    let target = app.tabs[0].id;
    app.activate_tab(1);
    let ctx = egui::Context::default();
    let release = start_canvas_brush(&mut app, &ctx);
    let before = app.drag.as_ref().unwrap().before.clone();
    let live = app.session().doc.clone();
    let directory = tempfile::tempdir().unwrap();
    send(
        &mut app,
        &ctx,
        Worker::Open {
            path: directory.path().join("inactive.png"),
            result: Ok(imported("Inactive imported", None)),
            target: Some(target),
        },
    );
    assert_eq!(app.current, 1);
    assert_eq!(app.session().doc, live);
    assert!(app.drag.is_some());
    assert!(app.brush_stroke.is_some());
    assert_eq!(app.tabs[0].doc.layers.len(), 2);
    assert_eq!(app.tabs[0].history.undo_name(), Some("Import layers"));
    canvas_pointer(&mut app, &ctx, release, false);
    assert_eq!(app.session().history.undo_name(), Some("Brush stroke"));
    app.command(Command::Undo);
    assert_eq!(app.session().doc, before);
    assert_eq!(app.tabs[0].doc.layers.len(), 2);
}

#[test]
fn windows_compatibility_gpu_failure_keeps_all_tabs_cpu_edit_save_and_export_usable() {
    let colors = [[25, 90, 140, 255], [180, 30, 70, 255]];
    let documents = colors
        .into_iter()
        .map(|color| {
            let mut document = Document::new(8, 8);
            document.layers[0].image = Some(Arc::new(Raster::solid(8, 8, color)));
            document
        })
        .collect();
    let mut app = editor(documents);
    let view = crate::preview::ViewRequest {
        region: [0, 0, 8, 8],
        width: 8,
        height: 8,
    };
    for session in &mut app.tabs {
        session.rendered = session.serial;
        session.rendered_view = Some(view);
        session.preview_view = Some(view);
        session.gpu_id = Some(egui::TextureId::User(17));
    }

    let injected = std::cell::Cell::new(false);
    assert!(
        app.gpu_frame(|_| {
            injected.set(true);
            panic!("injected synchronous GPU device/validation failure")
        })
        .is_none()
    );
    assert!(
        injected.get(),
        "The real GPU frame guard must execute the failing operation"
    );
    assert!(app.gpu.is_none());
    assert!(
        app.tabs
            .iter()
            .all(|session| session.gpu_id.is_none() && session.rendered_view.is_none())
    );

    let ctx = egui::Context::default();
    let directory = tempfile::tempdir().unwrap();
    for (index, original) in colors.into_iter().enumerate() {
        app.activate_tab(index);
        app.session_mut().edit("CPU fallback paint", |document| {
            let raster = Arc::make_mut(document.layers[0].image.as_mut().unwrap());
            raster.set(3, 4, [41, 71, 101, 255]);
        });
        app.session_mut().preview_view = Some(view);
        app.render_canvas(&ctx, view);
        assert!(app.session().preview_busy);
        let event = app
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("CPU preview completes after GPU failure");
        let Worker::Preview { raster, .. } = &event else {
            panic!("GPU failure must schedule the CPU compositor");
        };
        assert_eq!(raster.get(0, 0), original);
        assert_eq!(raster.get(3, 4), [41, 71, 101, 255]);
        send(&mut app, &ctx, event);
        assert!(app.session().gpu_id.is_none());
        assert_eq!(app.session().rendered, app.session().serial);
        assert_eq!(app.session().texture.as_ref().unwrap().size(), [8, 8]);

        let path = directory.path().join(format!("CPU 回退 {index}.comp"));
        project::save(&app.session().doc, &path).unwrap();
        let reopened = project::load(&path).unwrap();
        assert_eq!(reopened.layers[0].image, app.session().doc.layers[0].image);
        let png = directory.path().join(format!("CPU 导出 {index}.png"));
        image_io::export_png(&render::render_document(&reopened), &png, 72.).unwrap();
        let exported = image_io::decode_image(&png).unwrap();
        assert_eq!(exported.get(0, 0), original);
        assert_eq!(exported.get(3, 4), [41, 71, 101, 255]);
    }
}

#[test]
fn closing_active_tab_invalidates_survivors_shared_gpu_output() {
    for current in [0, 1] {
        let colors = [[25, 90, 140, 255], [180, 30, 70, 255]];
        let documents = [8, 16]
            .into_iter()
            .zip(colors)
            .map(|(size, color)| {
                let mut document = Document::new(size, size);
                document.layers[0].image = Some(Arc::new(Raster::solid(size, size, color)));
                document
            })
            .collect();
        let mut app = editor(documents);
        app.current = current;
        let survivor = app.tabs[1 - current].id;
        for session in &mut app.tabs {
            session.rendered = session.serial;
            session.gpu_id = Some(egui::TextureId::User(17));
        }
        app.close_tab(current);
        assert_eq!(app.session().id, survivor);
        assert_eq!(app.session().rendered, 0);
        assert!(app.session().gpu_id.is_none());
        let ctx = egui::Context::default();
        let size = if current == 0 { 16 } else { 8 };
        let view = crate::preview::ViewRequest {
            region: [0, 0, size, size],
            width: size,
            height: size,
        };
        app.session_mut().preview_view = Some(view);
        app.render_canvas(&ctx, view);
        assert!(app.session().preview_busy);
        let event = app
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("survivor preview completes");
        let Worker::Preview { id, raster, .. } = &event else {
            panic!("canvas schedules the survivor preview");
        };
        assert_eq!(*id, survivor);
        assert_eq!(raster.get(0, 0), colors[1 - current]);
        send(&mut app, &ctx, event);
        let size = if current == 0 { 16 } else { 8 };
        assert_eq!(app.session().texture.as_ref().unwrap().size(), [size, size]);
        assert_eq!(app.session().rendered, app.session().serial);
    }
}

#[test]
fn closing_inactive_tab_keeps_active_brush_and_its_commit_target() {
    let mut app = editor(vec![Document::new(8, 8), Document::new(64, 64)]);
    app.activate_tab(1);
    let active = app.session().id;
    let ctx = egui::Context::default();
    let release = start_canvas_brush(&mut app, &ctx);
    let before = app.drag.as_ref().unwrap().before.clone();
    app.session_mut().rendered = app.session().serial;
    app.session_mut().gpu_id = Some(egui::TextureId::User(17));
    let rendered = app.session().rendered;
    app.request_close(0);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.session().id, active);
    assert_eq!(app.session().rendered, rendered);
    assert_eq!(app.session().gpu_id, Some(egui::TextureId::User(17)));
    assert!(app.drag.is_some());
    assert!(app.brush_stroke.is_some());
    canvas_pointer(&mut app, &ctx, release, false);
    assert_eq!(app.session().history.undo_name(), Some("Brush stroke"));
    app.command(Command::Undo);
    assert_eq!(app.session().doc, before);
}

#[test]
fn filter_dialog_keeps_its_opening_mask_target_when_sidebar_target_changes() {
    for mask_target in [false, true] {
        let mut document = Document::new(8, 8);
        document.layers[0].image = Some(Arc::new(Raster::solid(8, 8, [10, 20, 30, 255])));
        document.layers[0].mask = Some(Mask {
            raster: Arc::new(Raster::solid(8, 8, [60, 60, 60, 255])),
            ..Mask::white()
        });
        let mut app = editor(vec![document.clone()]);
        app.mask_target = mask_target;
        app.begin_dialog(Dialog::Filter {
            kind: "Invert".into(),
        });
        app.dialog_value = crate::filters::default_settings("Invert");
        app.mask_target = !mask_target;
        let ctx = egui::Context::default();
        click_dialog_button(&mut app, &ctx, "Apply");
        assert!(app.dialog.is_none());
        while app.session().edit_busy {
            let event = app
                .receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("real filter worker completes");
            send(&mut app, &ctx, event);
        }
        assert_eq!(app.session().history.undo_name(), Some("Invert"));
        let layer = &app.session().doc.layers[0];
        if mask_target {
            assert_eq!(layer.image, document.layers[0].image);
            assert_eq!(
                layer.mask.as_ref().unwrap().raster.get(4, 4),
                [195, 195, 195, 255]
            );
        } else {
            assert_eq!(layer.mask, document.layers[0].mask);
            assert_eq!(
                layer.image.as_ref().unwrap().get(4, 4),
                [245, 235, 225, 255]
            );
        }
        app.command(Command::Undo);
        assert_eq!(app.session().doc, document);
    }
}

fn diagnostic_camera_raw_settings() -> (Value, Value) {
    let mut clean = crate::filters::camera_raw_defaults();
    clean["exposure"] = json!(0.8);
    clean["temperature"] = json!(17.);
    clean["clarity"] = json!(25.);
    clean["grainAmount"] = json!(12.);
    clean["seed"] = json!(745);
    clean["detail"]["sharpenAmount"] = json!(60.);
    clean["mixer"]["points"] = json!([
        {"hue":30.,"saturation":0.5,"luminance":0.4,"hueShift":10.,
         "saturationShift":-20.,"luminanceShift":15.,"hueRange":25.,
         "saturationRange":0.3,"luminanceRange":0.2},
        {"hue":210.,"saturation":0.6,"luminance":0.5,"hueShift":-12.,
         "saturationShift":17.,"luminanceShift":5.,"hueRange":20.,
         "saturationRange":0.4,"luminanceRange":0.3}
    ]);
    clean["customFutureField"] = json!({"preserve":true});
    clean.as_object_mut().unwrap().remove("showShadowClipping");
    clean
        .as_object_mut()
        .unwrap()
        .remove("showHighlightClipping");
    clean["detail"]
        .as_object_mut()
        .unwrap()
        .remove("showSharpenMask");
    let mut diagnostic = clean.clone();
    diagnostic["previewScale"] = json!(0.25);
    diagnostic["clipping"] = json!(1);
    diagnostic["showShadowClipping"] = json!(true);
    diagnostic["showHighlightClipping"] = json!(true);
    diagnostic["detail"]["showSharpenMask"] = json!(true);
    diagnostic["mixer"]["points"][0]["visualize"] = json!(true);
    diagnostic["mixer"]["points"][1]["visualize"] = json!(false);
    (diagnostic, clean)
}

#[test]
fn camera_raw_apply_drops_preview_diagnostics() {
    let (diagnostic, clean) = diagnostic_camera_raw_settings();
    assert_eq!(
        committed_filter_settings("Camera Raw Filter", diagnostic.clone()),
        clean
    );
    let mut nested = diagnostic.clone();
    nested["cameraRaw"] = diagnostic.clone();
    nested["kind"] = json!("Camera Raw Filter");
    let mut expected = clean.clone();
    expected["cameraRaw"] = clean;
    expected["kind"] = json!("Camera Raw Filter");
    assert_eq!(
        committed_filter_settings("Camera Raw Filter", nested),
        expected
    );
    for kind in ["Gaussian Blur", "Add Noise", "Dither", "Invert"] {
        assert_eq!(
            committed_filter_settings(kind, diagnostic.clone()),
            diagnostic,
            "{kind} settings must retain unrelated fields"
        );
    }
}

#[test]
fn camera_raw_filter_commits_clean_grade_and_keeps_dialog_preview_flags() {
    let (diagnostic, clean) = diagnostic_camera_raw_settings();
    for nested in [false, true] {
        let value = if nested {
            json!({"cameraRaw":diagnostic})
        } else {
            diagnostic.clone()
        };
        let committed = if nested {
            json!({"cameraRaw":clean})
        } else {
            clean.clone()
        };
        let mut document = Document::new(16, 12);
        document.layers[0].image = Some(Arc::new(Raster::from_rgba(
            16,
            12,
            (0..16 * 12)
                .flat_map(|i| {
                    [
                        (i * 17 % 256) as u8,
                        (i * 31 % 256) as u8,
                        (i * 47 % 256) as u8,
                        255,
                    ]
                })
                .collect(),
        )));
        let id = document.active_layer_id.unwrap();
        let mut expected = document.clone();
        crate::filter_edit::apply(&mut expected, id, false, "Camera Raw Filter", &committed)
            .unwrap();
        let mut painted_diagnostics = document.clone();
        crate::filter_edit::apply(
            &mut painted_diagnostics,
            id,
            false,
            "Camera Raw Filter",
            &value,
        )
        .unwrap();
        assert_ne!(
            expected.layers[0].image, painted_diagnostics.layers[0].image,
            "Fixture must distinguish preview diagnostics from the committed grade"
        );
        let mut app = editor(vec![document.clone()]);
        app.begin_dialog(Dialog::CameraRaw);
        app.dialog_value = value.clone();
        app.filter("Camera Raw Filter", app.dialog_value.clone(), false);
        let ctx = egui::Context::default();
        while app.session().edit_busy {
            let event = app
                .receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("real Camera Raw worker completes");
            send(&mut app, &ctx, event);
        }
        assert_eq!(app.session().doc, expected);
        assert_eq!(app.dialog_value, value);
        assert_eq!(app.session().history.undo_name(), Some("Camera Raw Filter"));
        app.command(Command::Undo);
        assert_eq!(app.session().doc, document);
        app.command(Command::Redo);
        assert_eq!(app.session().doc, expected);
    }
}

#[test]
fn camera_raw_preview_keeps_clean_scope_and_original_sample_separate_from_diagnostics() {
    let (diagnostic, clean) = diagnostic_camera_raw_settings();
    let mut document = Document::new(16, 12);
    let source = Arc::new(Raster::from_rgba(
        16,
        12,
        (0..16 * 12)
            .flat_map(|i| {
                [
                    (i * 17 % 256) as u8,
                    (i * 31 % 256) as u8,
                    (i * 47 % 256) as u8,
                    255,
                ]
            })
            .collect(),
    ));
    document.layers[0].image = Some(source.clone());
    let mut preview_settings = diagnostic.clone();
    preview_settings["previewScale"] = json!(1.);
    let expected_display =
        crate::filters::apply_filter(&source, "Camera Raw Filter", &preview_settings).unwrap();
    let expected_grade =
        crate::filters::apply_filter(&source, "Camera Raw Filter", &clean).unwrap();
    assert_ne!(expected_display.to_rgba(), expected_grade.to_rgba());
    assert_ne!(expected_grade.to_rgba(), source.to_rgba());
    let mut app = editor(vec![document.clone()]);
    app.begin_dialog(Dialog::CameraRaw);
    app.dialog_value = diagnostic.clone();
    let ctx = egui::Context::default();
    app.dialog_preview(&ctx, &Dialog::CameraRaw);
    let event = app
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("real Camera Raw preview worker completes");
    match &event {
        Worker::DialogPreview {
            sample,
            graded_sample,
            result,
            ..
        } => {
            assert_eq!(sample.as_ref().unwrap().to_rgba(), source.to_rgba());
            assert_eq!(
                graded_sample.as_ref().unwrap().to_rgba(),
                expected_grade.to_rgba()
            );
            assert_eq!(
                result.as_ref().unwrap().0.to_rgba(),
                expected_display.to_rgba()
            );
        }
        _ => panic!("Expected Camera Raw dialog preview worker result"),
    }
    send(&mut app, &ctx, event);
    assert_eq!(
        app.dialog_sample.as_ref().unwrap().to_rgba(),
        source.to_rgba()
    );
    assert_eq!(
        app.dialog_graded_sample.as_ref().unwrap().to_rgba(),
        expected_grade.to_rgba()
    );
    assert_eq!(app.dialog_value, diagnostic);
    assert_eq!(app.session().doc, document);
    assert!(!app.session().history.is_modified());
    app.cancel_dialog();
    assert!(app.dialog_graded_sample.is_none());
    assert!(app.dialog_sample.is_none());
    assert_eq!(app.session().doc, document);
}

#[test]
fn camera_raw_preview_retains_original_grid_and_shares_clean_output() {
    let mut document = Document::new(1200, 2);
    let source = Arc::new(Raster::from_rgba(
        1200,
        2,
        (0..2400)
            .flat_map(|i| [(i * 17 % 256) as u8, (i * 31 % 256) as u8, 80, 255])
            .collect(),
    ));
    document.layers[0].image = Some(source.clone());
    let mut app = editor(vec![document]);
    app.begin_dialog(Dialog::CameraRaw);
    app.dialog_value = camera_raw_defaults();
    app.dialog_value["exposure"] = json!(0.5);
    let ctx = egui::Context::default();
    app.dialog_preview(&ctx, &Dialog::CameraRaw);
    let event = app
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("Camera Raw worker completes");
    if let Worker::DialogPreview {
        sample,
        graded_sample,
        display_sample,
        ..
    } = &event
    {
        assert!(Arc::ptr_eq(sample.as_ref().unwrap(), &source));
        let display = display_sample.as_ref().unwrap();
        assert_eq!([display.width, display.height], [1200, 2]);
        assert!(Arc::ptr_eq(graded_sample.as_ref().unwrap(), display));
        assert_eq!(
            display.to_rgba(),
            crate::filters::apply_filter(&source, "Camera Raw Filter", &app.dialog_value)
                .unwrap()
                .to_rgba()
        );
    } else {
        panic!("Expected Camera Raw preview result");
    }
}

fn raw_gesture_frame(
    app: &mut EditorApp,
    ctx: &egui::Context,
    source: &Raster,
    events: Vec<egui::Event>,
) -> Rect {
    let mut rect = Rect::NOTHING;
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let (_, response) =
                    ui.allocate_exact_size(Vec2::new(400., 300.), Sense::click_and_drag());
                rect = response.rect;
                app.raw_preview_gesture(ui, &response, source);
            });
        },
    );
    rect
}

fn raw_gesture_pointer(
    app: &mut EditorApp,
    ctx: &egui::Context,
    source: &Raster,
    position: Pos2,
    pressed: bool,
) {
    raw_gesture_frame(
        app,
        ctx,
        source,
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            },
        ],
    );
}

#[test]
fn raw_preview_pointer_maps_rotated_flipped_layer_and_routes_original_vs_graded_color() {
    let mut document = Document::new(240, 180);
    let mut source = Raster::solid(32, 20, [40, 80, 90, 255]);
    let original_color = [150, 140, 130, 255];
    source.set(10, 7, original_color);
    let mut graded = Raster::solid(32, 20, [100, 50, 150, 255]);
    let displayed_color = [20, 70, 250, 255];
    graded.set(10, 7, displayed_color);
    document.layers[0].image = Some(Arc::new(source.clone()));
    let mut transform = crate::model::Transform::new(70., 60., 96., 60.);
    transform.rotation = 37.;
    transform.flip_x = true;
    transform.flip_y = true;
    document.layers[0].transform = transform;
    let point = transform.to_document([10.5 / 32., 7.5 / 20.]);
    for mode in ["White Balance", "Defringe", "Point Color", "Sample"] {
        let mut app = editor(vec![document.clone()]);
        app.begin_dialog(Dialog::CameraRaw);
        app.dialog_value = crate::filters::camera_raw_defaults();
        app.raw_pointer_mode = mode.into();
        app.dialog_graded_sample = Some(Arc::new(graded.clone()));
        let mut expected = app.dialog_value.clone();
        let original = crate::raw_interaction::ColorSample::from_rgba(original_color).unwrap();
        let displayed = crate::raw_interaction::ColorSample::from_rgba(displayed_color).unwrap();
        match mode {
            "White Balance" => {
                assert!(crate::raw_interaction::pick_white_balance(
                    &mut expected,
                    original
                ));
            }
            "Defringe" => {
                assert!(crate::raw_interaction::pick_defringe(
                    &mut expected,
                    original
                ));
            }
            "Point Color" => {
                assert_eq!(
                    crate::raw_interaction::pick_point_color(&mut expected, displayed, None),
                    Some(0)
                );
            }
            _ => {}
        }
        let ctx = egui::Context::default();
        raw_gesture_frame(&mut app, &ctx, &source, vec![]);
        let rect = raw_gesture_frame(&mut app, &ctx, &source, vec![]);
        let position = rect.min
            + Vec2::new(
                (point[0] / 240.) as f32 * rect.width(),
                (point[1] / 180.) as f32 * rect.height(),
            );
        raw_gesture_pointer(&mut app, &ctx, &source, position, true);
        raw_gesture_pointer(&mut app, &ctx, &source, position, false);
        assert_eq!(app.dialog_value, expected, "{mode}");
        if mode == "Point Color" {
            assert_eq!(app.raw_point_selection, Some(0));
        }
        if mode == "Sample" {
            assert!(app.message.as_ref().unwrap().starts_with("RGB 20, 70, 250"));
        }
        assert_eq!(app.session().doc, document);
        assert!(!app.session().history.is_modified());
    }
}

#[test]
fn raw_preview_curve_drag_uses_graded_tone_and_initial_pointer_press_on_transformed_layer() {
    let mut document = Document::new(240, 180);
    let source = Raster::solid(32, 20, [30, 30, 30, 255]);
    let graded = Raster::solid(32, 20, [170, 170, 170, 255]);
    document.layers[0].image = Some(Arc::new(source.clone()));
    let mut transform = crate::model::Transform::new(70., 60., 96., 60.);
    transform.rotation = 37.;
    transform.flip_x = true;
    document.layers[0].transform = transform;
    let point = transform.to_document([10.5 / 32., 7.5 / 20.]);
    let mut app = editor(vec![document.clone()]);
    app.begin_dialog(Dialog::CameraRaw);
    app.dialog_value = crate::filters::camera_raw_defaults();
    app.raw_pointer_mode = "Curve Parametric".into();
    app.dialog_graded_sample = Some(Arc::new(graded));
    let baseline = app.dialog_value.clone();
    let ctx = egui::Context::default();
    raw_gesture_frame(&mut app, &ctx, &source, vec![]);
    let rect = raw_gesture_frame(&mut app, &ctx, &source, vec![]);
    let start = rect.min
        + Vec2::new(
            (point[0] / 240.) as f32 * rect.width(),
            (point[1] / 180.) as f32 * rect.height(),
        );
    let end = start - Vec2::new(0., 60.);
    raw_gesture_pointer(&mut app, &ctx, &source, start, true);
    raw_gesture_frame(
        &mut app,
        &ctx,
        &source,
        vec![egui::Event::PointerMoved(end)],
    );
    let expected = crate::raw_interaction::CurveDrag {
        baseline,
        tone: crate::raw_interaction::ColorSample::from_rgba([170, 170, 170, 255])
            .unwrap()
            .tone,
        start_y: start.y as f64,
        target: crate::raw_interaction::CurveTarget::Parametric,
    }
    .settings_at(end.y as f64);
    assert_eq!(app.dialog_value, expected);
    assert_eq!(app.dialog_value["curve"]["lights"], json!(21.));
    assert_eq!(app.dialog_value["curve"]["shadows"], json!(0));
    assert!(app.raw_curve_drag.is_some());
    raw_gesture_pointer(&mut app, &ctx, &source, end, false);
    assert!(app.raw_curve_drag.is_none());
    assert_eq!(app.dialog_value, expected);
    assert_eq!(app.session().doc, document);
    assert!(!app.session().history.is_modified());
}

#[test]
fn filter_dialog_preview_and_apply_share_native_growth_and_soft_selection() {
    let mut document = Document::new(64, 40);
    let layer = &mut document.layers[0];
    layer.image = Some(Arc::new(Raster::solid(8, 8, [210, 70, 30, 255])));
    layer.transform = crate::model::Transform::new(20., 14., 8., 8.);
    layer.mask = Some(Mask {
        raster: Arc::new(Raster::from_rgba(
            8,
            8,
            (0..64)
                .flat_map(|i| {
                    let value = if i < 8 { 64 } else { 255 };
                    [value, value, value, 255]
                })
                .collect(),
        )),
        ..Mask::white()
    });
    let original_transform = layer.transform;
    let id = layer.id;
    let mut selection = Selection::empty(64, 40);
    for y in 10..30 {
        for x in 16..36 {
            selection.set(
                x,
                y,
                if (20..28).contains(&x) && (14..22).contains(&y) {
                    255
                } else {
                    128
                },
            );
        }
    }
    document.selection = Some(selection);
    let settings = json!({"kind":"Gaussian Blur","blurRadius":2.});
    let mut expected = document.clone();
    crate::filter_edit::apply(&mut expected, id, false, "Gaussian Blur", &settings).unwrap();
    let grown = expected.active().unwrap();
    assert!(grown.image.as_ref().unwrap().width > 8);
    assert!(grown.image.as_ref().unwrap().height > 8);
    assert_eq!(
        grown.mask.as_ref().unwrap().placement,
        Some(original_transform)
    );
    assert_eq!(
        grown.mask.as_ref().unwrap().raster,
        document.layers[0].mask.as_ref().unwrap().raster
    );
    let expected_preview = crate::preview::PreviewCache::default().render(&expected, 1000);
    assert_ne!(
        expected_preview.to_rgba(),
        render::render_document(&document).to_rgba()
    );
    let mut app = editor(vec![document.clone()]);
    app.begin_dialog(Dialog::Filter {
        kind: "Gaussian Blur".into(),
    });
    app.dialog_value = settings.clone();
    let ctx = egui::Context::default();
    let dialog = app.dialog.clone().unwrap();
    app.dialog_preview(&ctx, &dialog);
    let event = app
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("real Gaussian preview worker completes");
    let preview = match &event {
        Worker::DialogPreview {
            result: Ok((raster, _)),
            ..
        } => raster,
        Worker::DialogPreview {
            result: Err(error), ..
        } => panic!("Gaussian preview failed: {error}"),
        _ => panic!("Expected dialog preview worker result"),
    };
    assert_eq!(preview.to_rgba(), expected_preview.to_rgba());
    assert_eq!(app.session().doc, document);
    assert!(!app.session().history.is_modified());
    send(&mut app, &ctx, event);
    assert!(!app.dialog_preview_busy);
    assert!(app.dialog_preview_error.is_none());
    click_dialog_button(&mut app, &ctx, "Apply");
    while app.session().edit_busy {
        let event = app
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("real Gaussian filter worker completes");
        send(&mut app, &ctx, event);
    }
    assert_eq!(app.session().doc, expected);
    assert_eq!(app.dialog_value, settings);
    assert!(app.dialog.is_none());
    assert_eq!(app.session().history.undo_name(), Some("Gaussian Blur"));
    app.command(Command::Undo);
    assert_eq!(app.session().doc, document);
    app.command(Command::Redo);
    assert_eq!(app.session().doc, expected);
}

fn sample_document() -> Document {
    let mut document = Document::new(4000, 8);
    let mut pixels = Raster::blank(4000, 8);
    pixels.set(3001, 3, [32, 64, 192, 255]);
    document.layers[0].image = Some(Arc::new(pixels));
    document
}

#[test]
fn cached_eyedropper_single_click_reads_original_resolution_and_preserves_palette_alpha() {
    let document = sample_document();
    let mut app = editor(vec![document.clone()]);
    app.session_mut().zoom = 0.25;
    app.tool = Tool::Eyedropper;
    app.foreground = [7, 8, 9, 123];
    app.sample_cache = Some((
        app.session().id,
        Arc::new(render::CompositeSampler::new(&document)),
    ));
    let ctx = egui::Context::default();
    let origin = canvas_frame(&mut app, &ctx, vec![]);
    let point = origin + Vec2::new(3001.5, 3.5) * 0.25;
    canvas_pointer(&mut app, &ctx, point, true);
    canvas_pointer(&mut app, &ctx, point, false);
    assert_eq!(app.foreground, [32, 64, 192, 123]);
    assert!(app.sample_busy.is_none());
    assert_eq!(app.session().doc, document);
    assert!(!app.session().history.is_modified());
}

#[test]
fn sampled_color_worker_rejects_stale_session_revision_and_cancelled_picker() {
    let document = sample_document();
    let sampler = Arc::new(render::CompositeSampler::new(&document));
    for scenario in ["other-session", "stale-serial", "cancel-picker", "current"] {
        let mut app = editor(vec![document.clone(), document.clone()]);
        app.foreground = [7, 8, 9, 123];
        let id = app.session().id;
        let serial = app.session().serial;
        app.sample_busy = Some((id, serial));
        app.sample_point = Some([3001.5, 3.5]);
        match scenario {
            "other-session" => app.activate_tab(1),
            "stale-serial" => app.session_mut().changed(),
            "cancel-picker" => {
                app.color_picker = Some(crate::palette::ColorPicker::new(false, app.foreground));
                app.finish_color_picker(false);
            }
            _ => {}
        }
        send(
            &mut app,
            &egui::Context::default(),
            Worker::ColorSample {
                session: id,
                serial,
                sampler: sampler.clone(),
            },
        );
        assert!(app.sample_busy.is_none());
        if scenario == "current" {
            assert_eq!(app.foreground, [32, 64, 192, 123]);
            assert!(app.sample_cache.is_some());
        } else {
            assert_eq!(app.foreground, [7, 8, 9, 123], "{scenario}");
        }
    }
}

#[test]
fn hand_gesture_cancellation_preserves_background_edit_on_escape_or_new_project() {
    for open_project in [false, true] {
        let mut app = editor(vec![Document::new(64, 64)]);
        app.session_mut().zoom = 4.;
        app.tool = Tool::Hand;
        let ctx = egui::Context::default();
        let origin = canvas_frame(&mut app, &ctx, vec![]);
        let start = origin + Vec2::splat(64.);
        canvas_pointer(&mut app, &ctx, start, true);
        canvas_frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(start + Vec2::new(40., 20.))],
        );
        assert!(app.drag.is_some());
        let before = app.session().doc.clone();
        let mut edited = before.clone();
        edited.layers[0].opacity = 0.4;
        let worker = Worker::Edit {
            id: app.session().id,
            revision: app.session().history.revision(),
            serial: app.session().serial,
            name: "Background opacity".into(),
            before,
            result: Ok(edited.clone()),
        };
        send(&mut app, &ctx, worker);
        if open_project {
            let directory = tempfile::tempdir().unwrap();
            send(
                &mut app,
                &ctx,
                Worker::Open {
                    path: directory.path().join("after-hand.png"),
                    result: Ok(imported("After Hand", None)),
                    target: None,
                },
            );
            assert_eq!(app.current, 1);
        } else {
            shortcut_key(&mut app, &ctx, Key::Escape, egui::Modifiers::NONE);
        }
        assert!(app.drag.is_none());
        assert_eq!(app.tabs[0].doc, edited);
        assert_eq!(app.tabs[0].history.undo_name(), Some("Background opacity"));
    }
}

fn headless_app() -> EditorApp {
    let mut app = EditorApp::create(None, None);
    let mut welcome = Session::new(Document::new(24, 16), None);
    welcome.placeholder = true;
    app.tabs = vec![welcome];
    app.current = 0;
    // The native clipboard is not changed or read by these deterministic state tests.
    app.welcome_clipboard_checked = true;
    app
}

#[test]
fn pdf_pages_open_as_separate_editable_tabs_with_names_sizes_and_resolution() {
    let mut app = headless_app();
    let size = crate::pdf_io::PdfPageSize {
        width_points: 72.,
        height_points: 36.,
    };
    let pages = (0..2)
        .map(|index| crate::pdf_io::PdfPage {
            raster: Raster::solid(300, 150, [index as u8 * 80, 30, 200, 255]),
            size,
            index,
        })
        .collect();
    app.install_pdf_pages(PathBuf::from("样例.pdf"), None, 300, pages);
    assert_eq!(app.tabs.len(), 2);
    for (i, session) in app.tabs.iter().enumerate() {
        assert!(session.title().contains(&format!("第 {} 页", i + 1)));
        assert!(session.path.is_none());
        assert_eq!(session.doc.resolution, 300.);
        assert_eq!((session.doc.width, session.doc.height), (300, 150));
    }
    let before = app.session().doc.clone();
    app.session_mut().edit("PDF page edit", |doc| {
        doc.active_mut().unwrap().opacity = 0.5
    });
    assert!(app.session().history.is_modified());
    let session = app.session_mut();
    assert!(session.history.undo(&mut session.doc));
    assert_eq!(session.doc, before);
}

#[test]
fn pdf_multiple_pages_are_rejected_atomically_for_import_as_layer() {
    let mut app = headless_app();
    app.install_canvas_session(Session::new(Document::new(20, 20), None));
    let target = app.session().id;
    let before = app.session().doc.clone();
    let pages = (0..2)
        .map(|index| crate::pdf_io::PdfPage {
            raster: Raster::solid(20, 20, [255; 4]),
            size: crate::pdf_io::PdfPageSize {
                width_points: 20.,
                height_points: 20.,
            },
            index,
        })
        .collect();
    app.install_pdf_pages(PathBuf::from("two.pdf"), Some(target), 72, pages);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.session().doc, before);
    assert!(app.message.as_deref().unwrap().contains("请选择一页"));
}

#[test]
fn closing_waits_until_background_file_output_is_complete() {
    let mut app = headless_app();
    app.window_motion = crate::window_motion::WindowMotion::with_animations(true);
    app.pending_io = 1;
    app.exit_pending = true;
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        app.handle_window_close(ctx)
    });
    assert!(!app.window_close_ready());
    assert!(!app.window_motion.is_closing());
    app.pending_io = 0;
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        app.handle_window_close(ctx)
    });
    assert!(app.window_close_ready());
    assert!(app.window_motion.is_closing());
}

fn welcome_enter(app: &mut EditorApp) {
    let context = egui::Context::default();
    let input = egui::RawInput {
        events: vec![egui::Event::Key {
            key: Key::Enter,
            physical_key: Some(Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = context.run(input, |context| {
        egui::CentralPanel::default().show(context, |ui| app.original_welcome_ui(ui));
    });
}

fn move_drag(app: &EditorApp) -> Drag {
    let before = app.session().doc.clone();
    Drag {
        target: Some((app.session().id, app.session().history.revision())),
        before: before.clone(),
        work_before: before,
        start: [2., 3.],
        last: [2., 3.],
        points: vec![],
        mode: Tool::Move,
        selection_mode: SelectionMode::Replace,
        selection_antialias: true,
        constrain_armed: false,
        handle: None,
        pan: Vec2::ZERO,
        distort: false,
        rotate: false,
        centered: false,
        pixel_move: false,
        outline_move: false,
        duplicate_pixels: false,
    }
}

#[test]
fn welcome_return_creates_requested_size_resolution_and_replaces_placeholder() {
    let mut app = headless_app();
    let placeholder = app.session().id;
    app.new_size = [640, 400];
    app.new_resolution = 144.;
    welcome_enter(&mut app);
    assert_eq!(app.tabs.len(), 1);
    assert_ne!(app.session().id, placeholder);
    assert!(!app.is_welcome());
    assert_eq!(
        (app.session().doc.width, app.session().doc.height),
        (640, 400)
    );
    assert_eq!(app.session().doc.resolution, 144.);
    assert!(!app.session().history.is_modified());
}

#[test]
fn welcome_return_waits_for_pending_io_and_open_dialog() {
    let mut app = headless_app();
    let placeholder = app.session().id;
    app.pending_io = 1;
    welcome_enter(&mut app);
    assert_eq!(app.session().id, placeholder);
    app.pending_io = 0;
    app.dialog = Some(Dialog::Raw {
        path: PathBuf::from("pending.raw"),
        target: None,
    });
    welcome_enter(&mut app);
    assert_eq!(app.session().id, placeholder);
    app.dialog = None;
    welcome_enter(&mut app);
    assert_ne!(app.session().id, placeholder);
}

#[test]
fn welcome_return_waits_for_pdf_import_dialog_and_resumes_after_cancel() {
    let mut app = headless_app();
    let placeholder = app.session().id;
    app.pdf_ui.enqueue(
        PathBuf::from("pending.pdf"),
        None,
        crate::pdf_io::PdfInfo {
            pages: vec![crate::pdf_io::PdfPageSize {
                width_points: 72.,
                height_points: 72.,
            }],
        },
    );
    let context = egui::Context::default();
    let _ = context.run(egui::RawInput::default(), |context| {
        app.pdf_dialogs(context);
    });
    assert!(app.pdf_ui.is_open());
    welcome_enter(&mut app);
    assert_eq!(app.session().id, placeholder);
    assert!(app.is_welcome());
    app.pdf_ui.cancel();
    welcome_enter(&mut app);
    assert_ne!(app.session().id, placeholder);
    assert!(!app.is_welcome());
}

#[test]
fn welcome_return_does_not_apply_an_open_palette() {
    let mut app = headless_app();
    let placeholder = app.session().id;
    let original = app.foreground;
    app.color_picker = Some(crate::palette::ColorPicker::new(false, original));
    welcome_enter(&mut app);
    assert_eq!(app.session().id, placeholder);
    assert_eq!(app.foreground, original);
    assert!(app.color_picker.is_some());
}

#[test]
fn welcome_keeps_a_previously_read_clipboard_size() {
    let mut app = headless_app();
    app.new_size = [73, 41];
    welcome_enter(&mut app);
    assert_eq!(
        (app.session().doc.width, app.session().doc.height),
        (73, 41)
    );
    assert!(app.welcome_clipboard_checked);
}

#[test]
fn closing_last_clean_tab_restores_welcome_and_command_guards() {
    let mut app = headless_app();
    app.install_canvas_session(Session::new(Document::new(32, 24), None));
    let previous = app.session().id;
    assert!(app.command_available(Command::Save));
    app.request_close(0);
    assert_eq!(app.tabs.len(), 1);
    assert_ne!(app.session().id, previous);
    assert!(app.is_welcome());
    assert!(!app.session().history.is_modified());
    assert!(!app.command_available(Command::Save));
    assert!(!app.command_available(Command::ExportPng));
    assert!(app.command_available(Command::Open));
    assert!(app.command_available(Command::Paste));
}

#[test]
fn closing_last_dirty_tab_requests_confirmation_without_losing_it() {
    let mut app = headless_app();
    app.install_canvas_session(Session::new(Document::new(32, 24), None));
    app.session_mut().edit("Opacity", |document| {
        document.active_mut().unwrap().opacity = 0.25;
    });
    let id = app.session().id;
    let before = app.session().doc.clone();
    app.request_close(0);
    assert_eq!(app.close_pending, Some(id));
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.session().doc, before);
    assert!(!app.is_welcome());
}

#[test]
fn switching_tabs_cancels_the_old_drag_without_overwriting_destination() {
    let mut app = headless_app();
    app.install_canvas_session(Session::new(Document::new(32, 24), None));
    let before = app.session().doc.clone();
    let drag = move_drag(&app);
    app.session_mut().doc.active_mut().unwrap().opacity = 0.37;
    app.drag = Some(drag);
    let destination = Session::new(Document::new(17, 13), None);
    let destination_before = destination.doc.clone();
    app.tabs.push(destination);
    app.activate_tab(1);
    assert_eq!(app.tabs[0].doc, before);
    assert_eq!(app.tabs[1].doc, destination_before);
    assert_eq!(app.current, 1);
    assert!(app.drag.is_none());
}

#[test]
fn stale_drag_revision_does_not_undo_a_later_document_edit() {
    let mut app = headless_app();
    app.install_canvas_session(Session::new(Document::new(32, 24), None));
    let stale = move_drag(&app);
    app.session_mut().edit("Later opacity", |document| {
        document.active_mut().unwrap().opacity = 0.63;
    });
    let later = app.session().doc.clone();
    app.cancel_drag(stale);
    assert_eq!(app.session().doc, later);
    assert_eq!(app.session().history.undo_name(), Some("Later opacity"));
}

#[test]
fn switching_tabs_discards_an_old_color_sample_result() {
    let mut app = headless_app();
    app.install_canvas_session(Session::new(Document::new(16, 12), None));
    let old_session = app.session().id;
    let old_serial = app.session().serial;
    let sampler = Arc::new(render::CompositeSampler::new(&app.session().doc));
    app.sample_busy = Some((old_session, old_serial));
    app.sample_point = Some([3.5, 4.5]);
    app.tabs.push(Session::new(Document::new(8, 6), None));
    app.activate_tab(1);
    let foreground = app.foreground;
    app.sender
        .send(Worker::ColorSample {
            session: old_session,
            serial: old_serial,
            sampler,
        })
        .unwrap();
    app.poll(&egui::Context::default());
    assert_eq!(app.foreground, foreground);
    assert!(app.sample_cache.is_none());
    assert!(app.sample_busy.is_none());
    assert!(app.sample_point.is_none());
}

#[test]
fn switching_tabs_drops_the_previous_brush_line_anchor() {
    let mut app = headless_app();
    app.install_canvas_session(Session::new(Document::new(16, 12), None));
    let layer = app.session().doc.active_layer_id.unwrap();
    app.last_brush = Some((app.session().id, layer, false, [2., 3.]));
    assert_eq!(app.brush_line_start([7., 8.]), [2., 3.]);
    app.tabs.push(Session::new(Document::new(8, 6), None));
    app.activate_tab(1);
    assert_eq!(app.brush_line_start([7., 8.]), [7., 8.]);
    assert!(app.last_brush.is_none());
}
