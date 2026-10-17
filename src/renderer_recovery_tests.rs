use super::*;

fn editor() -> EditorApp {
    let mut app = EditorApp::create(None, None);
    let mut document = Document::new(32, 24);
    document.active_mut().unwrap().image = Some(Arc::new(Raster::blank(32, 24)));
    app.tabs = vec![Session::new(document, None)];
    app.current = 0;
    app.welcome_clipboard_checked = true;
    app
}

fn rebind(app: &mut EditorApp, context: &egui::Context) {
    app.recover_renderer(&eframe::CreationContext::_new_kittest(context.clone()));
}

fn texture(context: &egui::Context, name: &str) -> egui::TextureHandle {
    context.load_texture(
        name,
        egui::ColorImage::new([2, 2], vec![Color32::WHITE; 4]),
        egui::TextureOptions::NEAREST,
    )
}

fn drag(app: &EditorApp, mode: Tool) -> Drag {
    let before = app.session().doc.clone();
    Drag {
        target: Some((app.session().id, app.session().history.revision())),
        before: before.clone(),
        work_before: before,
        start: [6., 6.],
        last: [18., 10.],
        points: vec![[6., 6.], [18., 10.]],
        mode,
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
fn replacement_renderer_releases_all_old_textures_and_retains_documents_and_history() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("原工程.comp");
    let mut app = editor();
    project::save(&app.session().doc, &source).unwrap();
    let original = std::fs::read(&source).unwrap();
    app.session_mut().path = Some(source.clone());
    app.session_mut().edit("Opacity", |doc| {
        doc.active_mut().unwrap().opacity = 0.4;
    });
    app.session_mut().edit("Name", |doc| {
        doc.active_mut().unwrap().name = "Redo remains available".into();
    });
    let session = app.session_mut();
    assert!(session.history.undo(&mut session.doc));
    app.tabs.push(Session::new(Document::new(16, 12), None));
    app.tabs[1].edit("Resolution", |doc| doc.resolution = 144.);
    app.pending_io = 3;
    app.tabs[0].save_busy = true;
    app.tabs[1].edit_busy = true;
    app.tabs[1].reload_busy = true;
    let snapshots: Vec<_> = app
        .tabs
        .iter()
        .map(|session| {
            (
                session.id,
                session.doc.clone(),
                session.history.revision(),
                session.serial,
                session.preview_cache.clone(),
            )
        })
        .collect();
    let old = egui::Context::default();
    let mut ids = Vec::new();
    for session in &mut app.tabs {
        session.texture = Some(texture(&old, "old-canvas"));
        ids.push(session.texture.as_ref().unwrap().id());
        session.gpu_id = Some(egui::TextureId::User(99));
        let layer = &session.doc.layers[0];
        let raster = layer
            .image
            .clone()
            .unwrap_or_else(|| Arc::new(Raster::blank(2, 2)));
        ids.push(session.thumbnails.get(&old, layer.id, &raster));
        ids.push(session.mask_thumbnails.get(&old, layer.id, &raster));
    }
    app.raw_texture = Some(texture(&old, "old-raw"));
    app.dialog_texture = Some(texture(&old, "old-dialog"));
    ids.push(app.raw_texture.as_ref().unwrap().id());
    ids.push(app.dialog_texture.as_ref().unwrap().id());
    let raster = Arc::new(Raster::solid(16, 12, [40, 80, 120, 255]));
    let _ = old.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            crate::filter_panel::camera_raw_native_preview(ui, &raster, &mut app.raw_view_texture);
        });
    });
    assert!(app.raw_view_texture.is_some());
    ids.extend(
        old.tex_manager()
            .read()
            .allocated()
            .filter(|(_, meta)| meta.name == "camera-raw-native-region")
            .map(|(id, _)| *id),
    );
    let context = egui::Context::default();
    let marker = egui::Id::new("replacement-renderer");
    context.data_mut(|data| data.insert_temp(marker, 17_u32));
    rebind(&mut app, &context);
    assert!(
        ids.iter()
            .all(|id| old.tex_manager().read().meta(*id).is_none())
    );
    assert!(app.raw_texture.is_none());
    assert!(app.raw_view_texture.is_none());
    assert!(app.dialog_texture.is_none());
    assert_eq!(app.current, 0);
    assert_eq!(app.pending_io, 3);
    assert!(app.tabs[0].save_busy && app.tabs[1].edit_busy && app.tabs[1].reload_busy);
    assert_eq!(app.tabs[0].path, Some(source.clone()));
    assert_eq!(std::fs::read(source).unwrap(), original);
    assert!(Arc::ptr_eq(
        &app.sender.context,
        &app.watcher.sender.context
    ));
    assert_eq!(
        app.sender
            .context
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .data(|data| data.get_temp::<u32>(marker)),
        Some(17)
    );
    for (session, (id, doc, revision, serial, cache)) in app.tabs.iter().zip(&snapshots) {
        assert_eq!(session.id, *id);
        assert_eq!(session.doc, *doc);
        assert_eq!(session.history.revision(), *revision);
        assert_eq!(session.serial, *serial);
        assert!(Arc::ptr_eq(&session.preview_cache, cache));
        assert!(session.texture.is_none() && session.gpu_id.is_none());
        assert!(session.history.is_modified());
    }
    let session = &mut app.tabs[0];
    assert_eq!(session.history.redo_name(), Some("Name"));
    assert!(session.history.redo(&mut session.doc));
    assert_eq!(session.doc.layers[0].name, "Redo remains available");
    assert!(session.history.undo(&mut session.doc));
    assert_eq!(session.doc, snapshots[0].1);
}

#[test]
fn queued_canvas_preview_and_export_survive_context_replacement() {
    let mut app = editor();
    let view = crate::preview::ViewRequest::new([32, 24], [0., 0., 32., 24.], 1.).unwrap();
    let session = app.session_mut();
    session.preview_busy = true;
    session.preview_view = Some(view);
    let event = Worker::Preview {
        id: session.id,
        serial: session.serial,
        view,
        raster: Raster::solid(32, 24, [30, 80, 150, 255]),
    };
    app.pending_io = 1;
    let original_sender = app.sender.clone();
    original_sender.send(event).unwrap();
    original_sender
        .send(Worker::Export(Err("Expected test error".into())))
        .unwrap();
    let context = egui::Context::default();
    rebind(&mut app, &context);
    assert!(app.session().preview_busy);
    assert_eq!(app.session().preview_view, Some(view));
    app.poll(&context);
    assert!(!app.session().preview_busy);
    assert_eq!(app.session().rendered, app.session().serial);
    assert_eq!(app.session().rendered_view, Some(view));
    let id = app.session().texture.as_ref().unwrap().id();
    assert!(context.tex_manager().read().meta(id).is_some());
    assert_eq!(app.pending_io, 0);
    assert_eq!(app.message.as_deref(), Some("Expected test error"));
}

#[test]
fn queued_background_edit_retains_its_target_revision_and_commits_after_recovery() {
    let mut app = editor();
    app.session_mut()
        .edit("User edit", |doc| doc.resolution = 120.);
    let before = app.session().doc.clone();
    let mut after = before.clone();
    after.active_mut().unwrap().opacity = 0.3;
    let session = app.session_mut();
    session.edit_busy = true;
    let event = Worker::Edit {
        id: session.id,
        revision: session.history.revision(),
        serial: session.serial,
        name: "Background edit".into(),
        before: before.clone(),
        result: Ok(after.clone()),
    };
    app.sender.send(event).unwrap();
    let context = egui::Context::default();
    rebind(&mut app, &context);
    app.poll(&context);
    let session = app.session_mut();
    assert!(!session.edit_busy);
    assert_eq!(session.doc, after);
    assert_eq!(session.history.undo_name(), Some("Background edit"));
    assert!(session.history.undo(&mut session.doc));
    assert_eq!(session.doc, before);
    assert_eq!(session.history.undo_name(), Some("User edit"));
}

#[test]
fn queued_raw_and_filter_previews_complete_against_the_new_context() {
    let mut app = editor();
    let path = PathBuf::from("中文原图.dng");
    let key = raw_settings_key(app.raw_settings);
    app.dialog = Some(Dialog::Raw {
        path: path.clone(),
        target: None,
    });
    app.raw_busy = true;
    app.raw_key = Some((path.clone(), key));
    app.dialog_preview_busy = true;
    app.dialog_generation = 9;
    let preview_key = (
        app.session().id,
        app.session().serial,
        "Camera Raw".into(),
        json!({}),
    );
    app.preview_key = Some(preview_key.clone());
    app.sender
        .send(Worker::RawPreview {
            path: path.clone(),
            key,
            result: Ok(Raster::solid(8, 6, [1, 2, 3, 255])),
        })
        .unwrap();
    app.sender
        .send(Worker::DialogPreview {
            generation: 9,
            sample: None,
            graded_sample: None,
            display_sample: None,
            result: Ok((Raster::solid(8, 6, [30, 60, 90, 255]), 192)),
        })
        .unwrap();
    let context = egui::Context::default();
    rebind(&mut app, &context);
    assert_eq!(app.raw_key, Some((path, key)));
    assert_eq!(app.preview_key, Some(preview_key));
    assert!(app.raw_busy && app.dialog_preview_busy);
    app.poll(&context);
    assert!(!app.raw_busy && !app.dialog_preview_busy);
    assert!(app.raw_texture.is_some() && app.dialog_texture.is_some());
    assert_eq!(app.dialog_preview_bytes, 192);
    for handle in [&app.raw_texture, &app.dialog_texture] {
        assert!(
            context
                .tex_manager()
                .read()
                .meta(handle.as_ref().unwrap().id())
                .is_some()
        );
    }
}

#[test]
fn renderer_recovery_finalizes_the_smoothed_brush_tail_and_keeps_undo() {
    let mut app = editor();
    app.tool = Tool::Brush;
    app.smoothing = 0.8;
    app.diameter = 3.;
    app.hardness = 1.;
    let drag = drag(&app, Tool::Brush);
    let before = drag.before.clone();
    app.begin_brush(drag.start);
    app.brush_stroke
        .as_mut()
        .unwrap()
        .append(drag.last)
        .unwrap();
    app.show_brush();
    let mut expected = before.clone();
    let mut stroke = crate::brush::BrushStroke::new(
        &before,
        before.active_layer_id.unwrap(),
        false,
        crate::brush::BrushSettings {
            diameter: app.diameter,
            hardness: app.hardness,
            opacity: app.opacity,
            color: app.palette_color(false),
            smoothing: app.smoothing * 100.,
            mode: crate::brush::BrushMode::Paint,
        },
        None,
    )
    .unwrap();
    stroke.append(drag.start).unwrap();
    stroke.append(drag.last).unwrap();
    stroke.finish_at(drag.last).unwrap();
    stroke.commit(&mut expected).unwrap();
    assert_ne!(expected, before);
    app.drag = Some(drag);
    rebind(&mut app, &egui::Context::default());
    assert!(app.drag.is_none() && app.brush_stroke.is_none());
    let session = app.session_mut();
    assert_eq!(session.doc, expected);
    assert_eq!(session.history.undo_name(), Some("Brush stroke"));
    assert!(session.history.is_modified());
    assert!(session.history.undo(&mut session.doc));
    assert_eq!(session.doc, before);
    assert!(session.history.redo(&mut session.doc));
    assert_eq!(session.doc, expected);
}

#[test]
fn renderer_recovery_seals_property_and_guide_changes_in_undo_history() {
    let mut app = editor();
    let guide = crate::model::Guide {
        id: Uuid::new_v4(),
        axis: "horizontal".into(),
        position: 4.,
    };
    app.session_mut().doc.guides.push(guide.clone());
    let before = app.session().doc.clone();
    app.guide_drag = Some(GuideDrag {
        before: before.clone(),
        id: guide.id,
        horizontal: true,
    });
    app.session_mut().doc.guides[0].position = 13.;
    rebind(&mut app, &egui::Context::default());
    assert_eq!(app.session().doc.guides[0].position, 13.);
    let session = app.session_mut();
    assert_eq!(session.history.undo_name(), Some("Guide"));
    assert!(session.history.undo(&mut session.doc));
    assert_eq!(session.doc, before);
    let id = session.id;
    app.property_edit = Some((id, before.clone()));
    app.session_mut().doc.active_mut().unwrap().opacity = 0.2;
    rebind(&mut app, &egui::Context::default());
    let session = app.session_mut();
    assert_eq!(session.history.undo_name(), Some("Layer properties"));
    assert_eq!(session.doc.active().unwrap().opacity, 0.2);
    assert!(session.history.undo(&mut session.doc));
    assert_eq!(session.doc, before);
}

#[test]
fn renderer_recovery_commits_free_distort_without_a_stranded_background_job() {
    let mut app = editor();
    app.session_mut().doc.active_mut().unwrap().image =
        Some(Arc::new(Raster::solid(32, 24, [40, 80, 120, 255])));
    let mut drag = drag(&app, Tool::Move);
    drag.distort = true;
    drag.handle = Some(2);
    let before = drag.before.clone();
    let (id, corners) = distort_corners(&drag, false).unwrap();
    let mut expected = before.clone();
    crate::edit::free_distort(&mut expected, id, corners, false).unwrap();
    app.drag = Some(drag);
    rebind(&mut app, &egui::Context::default());
    let session = app.session_mut();
    assert_eq!(session.doc, expected);
    assert!(!session.edit_busy);
    assert_eq!(session.history.undo_name(), Some("Free distort"));
    assert!(session.history.undo(&mut session.doc));
    assert_eq!(session.doc, before);
}

#[test]
fn worker_sender_rebinds_old_producers_and_wakes_without_holding_the_context_lock() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (sender, receiver) = mpsc::channel();
    let sender = WorkerSender {
        sender,
        context: Arc::new(Mutex::new(None)),
    };
    let producer = sender.clone();
    let context = egui::Context::default();
    for _ in 0..3 {
        let _ = context.run(egui::RawInput::default(), |_| {});
    }
    let unlocked = Arc::new(AtomicBool::new(false));
    let observed = unlocked.clone();
    let binding = sender.context.clone();
    context.set_request_repaint_callback(move |_| {
        observed.store(binding.try_lock().is_ok(), Ordering::Release);
    });
    sender.bind_context(egui::Context::default());
    sender.bind_context(context);
    producer
        .send(Worker::Export(Ok(PathBuf::from("done.png"))))
        .unwrap();
    assert!(unlocked.load(Ordering::Acquire));
    assert!(matches!(receiver.try_recv(), Ok(Worker::Export(Ok(_)))));
}
