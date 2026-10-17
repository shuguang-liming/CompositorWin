//! A cached document overview; panning only moves the viewport outline.
use super::*;

const PICTURE_PIXELS: u32 = 300;
const SETTLE: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug)]
struct Geometry {
    document: Vec2,
    image: Rect,
}

impl Geometry {
    fn new(document: Vec2, bounds: Rect) -> Self {
        let image = if document.min_elem() > 0. && bounds.size().min_elem() > 0. {
            let scale = (bounds.width() / document.x).min(bounds.height() / document.y);
            Rect::from_center_size(bounds.center(), document * scale)
        } else {
            Rect::from_min_size(bounds.min, Vec2::ZERO)
        };
        Self { document, image }
    }

    fn scale(self) -> f32 {
        if self.document.x > 0. {
            self.image.width() / self.document.x
        } else {
            0.
        }
    }

    fn document_point(self, point: Pos2) -> Pos2 {
        if self.scale() <= 0. {
            return Pos2::ZERO;
        }
        let point = (point - self.image.min) / self.scale();
        Pos2::new(
            point.x.clamp(0., self.document.x),
            point.y.clamp(0., self.document.y),
        )
    }

    fn thumbnail_rect(self, document: Rect) -> Rect {
        Rect::from_min_size(
            self.image.min + document.min.to_vec2() * self.scale(),
            document.size() * self.scale(),
        )
    }
}

fn minimap_size(document: Vec2) -> Vec2 {
    if document.min_elem() <= 0. {
        Vec2::ZERO
    } else {
        let scale = (126. / document.x).min(90. / document.y);
        (document * scale).round().max(Vec2::splat(1.))
    }
}

fn centered_pan(document: Vec2, point: Pos2, points_per_pixel: f32) -> Vec2 {
    (document * 0.5 - point.to_vec2()) * points_per_pixel
}

fn visible_document(view: Rect, document_on_screen: Rect, points_per_pixel: f32) -> Rect {
    Rect::from_min_size(
        ((view.min - document_on_screen.min) / points_per_pixel).to_pos2(),
        view.size() / points_per_pixel,
    )
}

struct Picture {
    observed: Document,
    serial: u64,
    changed: Instant,
    rendered: Option<Document>,
    texture: Option<egui::TextureHandle>,
    failed: bool,
}

impl Picture {
    fn new(document: &Document, serial: u64, now: Instant) -> Self {
        Self {
            observed: document.clone(),
            serial,
            changed: now,
            rendered: None,
            texture: None,
            failed: false,
        }
    }

    fn observe(&mut self, document: &Document, serial: u64, now: Instant) {
        if self.serial == serial {
            return;
        }
        self.serial = serial;
        if !crate::preview::same_composite(&self.observed, document) {
            self.observed = document.clone();
            self.changed = now;
            self.failed = false;
        }
    }

    fn needs_render(&self) -> bool {
        !self.failed
            && !self
                .rendered
                .as_ref()
                .is_some_and(|document| crate::preview::same_composite(document, &self.observed))
    }
}

struct Pending {
    id: Uuid,
    document: Document,
    receiver: mpsc::Receiver<Result<Arc<Raster>, String>>,
}

#[derive(Default)]
pub(super) struct Navigator {
    pictures: HashMap<Uuid, Picture>,
    pending: Option<Pending>,
    dragging: bool,
}

impl Navigator {
    pub(super) fn retain_sessions(&mut self, ids: impl Iterator<Item = Uuid>) {
        let ids: HashSet<_> = ids.collect();
        self.pictures.retain(|id, _| ids.contains(id));
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !ids.contains(&pending.id))
        {
            self.pending = None;
            self.dragging = false;
        }
    }

    pub(super) fn reset_textures(&mut self) {
        self.pending = None;
        self.dragging = false;
        for picture in self.pictures.values_mut() {
            picture.texture = None;
            picture.rendered = None;
            picture.failed = false;
        }
    }

    fn poll(&mut self, ctx: &egui::Context) -> Option<String> {
        let result = match self.pending.as_ref()?.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("导航预览线程已结束，请重新打开导航器重试".into())
            }
        };
        let pending = self.pending.take().unwrap();
        let picture = self.pictures.get_mut(&pending.id)?;
        // A completed old edit or a closed tab must never replace a newer picture.
        if !crate::preview::same_composite(&pending.document, &picture.observed) {
            return None;
        }
        match result {
            Ok(raster) => {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [raster.width as usize, raster.height as usize],
                    &raster.to_rgba(),
                );
                if let Some(texture) = &mut picture.texture {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    picture.texture = Some(ctx.load_texture(
                        format!("navigator-{}", pending.id),
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                picture.rendered = Some(pending.document);
                None
            }
            Err(error) => {
                picture.failed = true;
                Some(error)
            }
        }
    }

    fn request(
        &mut self,
        id: Uuid,
        cache: Arc<Mutex<crate::preview::PreviewCache>>,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let document = self.pictures[&id].observed.clone();
        let operation_document = document.clone();
        let context = ctx.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        gui_threads::spawn("导航预览", move || {
            let result = gui_threads::run("导航预览", || {
                Ok(cache
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .render(&operation_document, PICTURE_PIXELS))
            });
            let _ = sender.send(result);
            context.request_repaint();
        })?;
        self.pending = Some(Pending {
            id,
            document,
            receiver,
        });
        Ok(())
    }
}

impl EditorApp {
    fn navigator_bounds(&self, space: Rect) -> Option<Rect> {
        if !self.navigator_visible
            || self.canvas_only
            || self.is_welcome()
            || self.session().zoom < 3.
            || self.dialog.is_some()
            || self.pdf_ui.is_open()
            || space.size().min_elem() < 70.
        {
            return None;
        }
        let document = Vec2::new(
            self.session().doc.width as f32,
            self.session().doc.height as f32,
        );
        let size = minimap_size(document).min(space.size() - Vec2::splat(24.));
        Some(Rect::from_min_size(
            Pos2::new(space.right() - size.x - 12., space.top() + 12.),
            size,
        ))
    }

    /// Call before the canvas navigation handler, so the minimap owns its gestures.
    pub(super) fn navigator_hit_test(&mut self, ui: &egui::Ui, space: Rect) -> bool {
        let focused = ui.input(|input| input.focused);
        if !focused || ui.input(|input| !input.pointer.primary_down()) {
            self.navigator.dragging = false;
        }
        let Some(bounds) = self.navigator_bounds(space) else {
            self.navigator.dragging = false;
            return false;
        };
        if self.command_palette.is_open() || self.drag.is_some() || self.guide_drag.is_some() {
            return false;
        }
        // Existing canvas gestures keep their owner when they cross the overview.
        if !self.navigator.dragging
            && ui.input(|input| {
                input.pointer.button_down(egui::PointerButton::Middle)
                    || input.pointer.secondary_down()
                    || (input.pointer.primary_down()
                        && input
                            .pointer
                            .press_origin()
                            .is_some_and(|point| !bounds.contains(point)))
            })
        {
            return false;
        }
        self.navigator.dragging || ui.rect_contains_pointer(bounds)
    }

    /// Draw above canvas decorations and return whether pointer input belongs to the overview.
    pub(super) fn navigator_ui(
        &mut self,
        ui: &mut egui::Ui,
        space: Rect,
        document_rect: Rect,
    ) -> bool {
        self.navigator
            .retain_sessions(self.tabs.iter().map(|session| session.id));
        if let Some(error) = self.navigator.poll(ui.ctx()) {
            self.message = Some(error);
        }
        let Some(bounds) = self.navigator_bounds(space) else {
            self.navigator.dragging = false;
            return false;
        };
        let id = self.session().id;
        let serial = self.session().serial;
        let now = Instant::now();
        self.navigator
            .pictures
            .entry(id)
            .or_insert_with(|| Picture::new(&self.tabs[self.current].doc, serial, now))
            .observe(&self.tabs[self.current].doc, serial, now);
        let picture = &self.navigator.pictures[&id];
        let can_render = self.navigator.pending.is_none()
            && self.drag.is_none()
            && self.guide_drag.is_none()
            && self.brush_stroke.is_none()
            && picture.needs_render();
        if can_render {
            let remaining = SETTLE.saturating_sub(now.duration_since(picture.changed));
            if picture.texture.is_none() || remaining.is_zero() {
                let cache = self.session().preview_cache.clone();
                if let Err(error) = self.navigator.request(id, cache, ui.ctx()) {
                    self.navigator.pictures.get_mut(&id).unwrap().failed = true;
                    self.message = Some(error);
                }
            } else {
                ui.ctx().request_repaint_after(remaining);
            }
        }
        let picture = &self.navigator.pictures[&id];
        let document = Vec2::new(
            self.session().doc.width as f32,
            self.session().doc.height as f32,
        );
        let geometry = Geometry::new(document, bounds);
        let response = ui.interact(
            bounds,
            egui::Id::new(("navigator", id)),
            Sense::click_and_drag(),
        );
        let painter = ui.painter().with_clip_rect(bounds);
        crate::canvas_chrome::document_backdrop(&painter, geometry.image, geometry.image);
        if let Some(texture) = &picture.texture {
            painter.image(
                texture.id(),
                geometry.image,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)),
                Color32::WHITE,
            );
        }
        let points_per_pixel = self.session().zoom / ui.ctx().pixels_per_point();
        let viewport = geometry
            .thumbnail_rect(visible_document(space, document_rect, points_per_pixel))
            .intersect(geometry.image);
        if viewport.is_positive() {
            let viewport = Rect::from_min_size(viewport.min, viewport.size().max(Vec2::splat(3.)))
                .intersect(geometry.image);
            painter.rect_stroke(
                viewport,
                0.,
                Stroke::new(2.5_f32, ui.visuals().selection.stroke.color),
                egui::StrokeKind::Inside,
            );
        }
        painter.rect_stroke(
            bounds,
            4.,
            Stroke::new(1_f32, Color32::from_white_alpha(64)),
            egui::StrokeKind::Inside,
        );
        if !self.command_palette.is_open()
            && self.drag.is_none()
            && self.guide_drag.is_none()
            && ui.input(|input| input.focused)
        {
            if ui.rect_contains_pointer(bounds) && ui.input(|input| input.pointer.primary_pressed())
            {
                self.navigator.dragging = true;
            }
            if self.navigator.dragging
                && let Some(pointer) = ui.input(|input| input.pointer.interact_pos())
            {
                let point = geometry.document_point(pointer);
                self.session_mut().pan = centered_pan(document, point, points_per_pixel);
                ui.ctx().request_repaint();
            }
        }
        let owns_pointer = self.navigator_hit_test(ui, space);
        if owns_pointer {
            ui.ctx().set_cursor_icon(if self.navigator.dragging {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::PointingHand
            });
        }
        response.on_hover_text("导航器 · 点击或拖动移动画布视野");
        owns_pointer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_preserves_aspect_ratio_and_maps_letterboxed_edges() {
        let geometry = Geometry::new(
            Vec2::new(400., 200.),
            Rect::from_min_size(Pos2::ZERO, Vec2::splat(200.)),
        );
        assert_eq!(
            geometry.image,
            Rect::from_min_size(Pos2::new(0., 50.), Vec2::new(200., 100.))
        );
        assert_eq!(
            geometry.thumbnail_rect(Rect::from_min_size(
                Pos2::new(100., 50.),
                Vec2::new(200., 100.)
            )),
            Rect::from_min_size(Pos2::new(50., 75.), Vec2::new(100., 50.))
        );
        assert_eq!(
            geometry.document_point(Pos2::new(100., 100.)),
            Pos2::new(200., 100.)
        );
        assert_eq!(
            geometry.document_point(Pos2::new(-20., 199.)),
            Pos2::new(0., 200.)
        );
        assert_eq!(minimap_size(Vec2::new(4000., 3000.)), Vec2::new(120., 90.));
        assert_eq!(minimap_size(Vec2::new(6000., 2000.)), Vec2::new(126., 42.));
        assert_eq!(minimap_size(Vec2::ZERO), Vec2::ZERO);
    }

    #[test]
    fn centering_changes_only_pan_and_preserves_document_mapping_at_high_dpi() {
        for pixels_per_point in [1., 1.25, 1.5, 2.] {
            let document = Vec2::new(400., 300.);
            let zoom = 4.;
            let scale = zoom / pixels_per_point;
            let view = Rect::from_min_size(Pos2::new(40., 70.), Vec2::new(800., 600.));
            let target = Pos2::new(50., 60.);
            let pan = centered_pan(document, target, scale);
            let shown = Rect::from_center_size(view.center() + pan, document * scale);
            let visible = visible_document(view, shown, scale);
            assert!((visible.center() - target).length() < 0.0001);
            assert!((visible.width() - view.width() / scale).abs() < 0.0001);
        }
    }

    #[test]
    fn visual_edits_are_debounced_but_selection_and_guides_reuse_the_picture() {
        let now = Instant::now();
        let mut document = Document::new(100, 100);
        let mut picture = Picture::new(&document, 1, now);
        picture.rendered = Some(document.clone());
        document.selection = Some(selection::marquee(100, 100, [0., 0., 5., 5.], false, false));
        document.active_layer_id = None;
        document.guides.push(crate::model::Guide {
            id: Uuid::new_v4(),
            axis: "vertical".into(),
            position: 22.,
        });
        picture.observe(&document, 2, now + SETTLE);
        assert!(!picture.needs_render());
        assert_eq!(picture.changed, now);
        document.layers[0].visible = false;
        picture.observe(&document, 3, now + SETTLE);
        assert!(picture.needs_render());
        assert_eq!(picture.changed, now + SETTLE);
    }

    #[test]
    fn overview_uses_bounded_composite_cache_and_shows_adjustments() {
        let mut document = Document::new(20000, 12000);
        document.layers.clear();
        let mut red = Layer::raster("Red", Raster::solid(200, 120, [255, 0, 0, 255]));
        red.transform.size = [20000., 12000.];
        document.layers.push(red);
        let mut invert = Layer::new("Invert", 20000, 12000);
        invert.image = None;
        invert.adjustment = Some(crate::filters::default_settings("Invert"));
        document.layers.push(invert);
        let mut cache = crate::preview::PreviewCache::default();
        let picture = cache.render(&document, PICTURE_PIXELS);
        assert_eq!([picture.width, picture.height], [300, 180]);
        assert_eq!(picture.get(150, 90), [0, 255, 255, 255]);
        assert!(Arc::ptr_eq(
            &picture,
            &cache.render(&document, PICTURE_PIXELS)
        ));
        let small = cache.render(&Document::new(40, 30), PICTURE_PIXELS);
        assert_eq!([small.width, small.height], [40, 30]);
    }

    #[test]
    fn pointer_navigation_preserves_pixels_zoom_history_and_releases_on_focus_loss() {
        let mut app = EditorApp::create(None, None);
        app.tabs = vec![Session::new(Document::new(400, 300), None)];
        app.current = 0;
        app.navigator_visible = true;
        app.session_mut().zoom = 4.;
        let before = app.session().doc.clone();
        let id = app.session().id;
        let mut picture = Picture::new(&before, app.session().serial, Instant::now());
        picture.rendered = Some(before.clone());
        app.navigator.pictures.insert(id, picture);
        let ctx = egui::Context::default();
        let frame = |app: &mut EditorApp, focused, events| {
            let mut handled = false;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
                    focused,
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let space = ui.max_rect();
                        let document_rect = Rect::from_center_size(
                            space.center() + app.session().pan,
                            Vec2::new(1600., 1200.),
                        );
                        let _ = app.navigator_hit_test(ui, space);
                        handled = app.navigator_ui(ui, space, document_rect);
                    });
                },
            );
            handled
        };
        frame(&mut app, true, vec![]);
        // The map is 120 by 90 at the canvas's top right.
        let pointer = Pos2::new(700., 42.5);
        assert!(frame(
            &mut app,
            true,
            vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE
                },
            ]
        ));
        assert!(app.navigator.dragging);
        assert_ne!(app.session().pan, Vec2::ZERO);
        assert!(frame(
            &mut app,
            true,
            vec![egui::Event::PointerMoved(Pos2::new(600., 220.))]
        ));
        assert_eq!(app.session().pan, Vec2::new(800., -600.));
        frame(&mut app, false, vec![]);
        assert!(!app.navigator.dragging);
        assert_eq!(app.session().zoom, 4.);
        assert_eq!(app.session().doc, before);
        assert!(app.session().history.undo_name().is_none());
        assert!(app.navigator.pending.is_none());
    }

    #[test]
    fn worker_failure_is_reported_once_and_leaves_the_document_intact() {
        let mut app = EditorApp::create(None, None);
        app.tabs = vec![Session::new(Document::new(40, 30), None)];
        app.current = 0;
        app.navigator_visible = true;
        app.session_mut().zoom = 3.;
        let before = app.session().doc.clone();
        let ctx = egui::Context::default();
        gui_threads::with_spawn_failure("导航预览", || {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let space = ui.max_rect();
                    app.navigator_ui(
                        ui,
                        space,
                        Rect::from_center_size(space.center(), Vec2::new(120., 90.)),
                    );
                });
            });
        });
        assert!(app.message.as_ref().unwrap().contains("导航预览"));
        assert!(app.navigator.pictures[&app.session().id].failed);
        assert!(app.navigator.pending.is_none());
        assert_eq!(app.session().doc, before);
    }

    #[test]
    fn renderer_recovery_drops_old_context_textures_and_pending_results() {
        let ctx = egui::Context::default();
        let document = Document::new(40, 30);
        let id = Uuid::new_v4();
        let mut picture = Picture::new(&document, 1, Instant::now());
        picture.texture = Some(ctx.load_texture(
            "old-renderer-navigator",
            egui::ColorImage::filled([1, 1], Color32::WHITE),
            egui::TextureOptions::LINEAR,
        ));
        picture.rendered = Some(document.clone());
        picture.failed = true;
        let (_sender, receiver) = mpsc::channel();
        let mut navigator = Navigator {
            pictures: HashMap::from([(id, picture)]),
            pending: Some(Pending {
                id,
                document,
                receiver,
            }),
            dragging: true,
        };
        navigator.reset_textures();
        assert!(navigator.pending.is_none());
        assert!(!navigator.dragging);
        let picture = &navigator.pictures[&id];
        assert!(picture.texture.is_none());
        assert!(picture.needs_render());
    }
}
