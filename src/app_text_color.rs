use super::*;

pub(super) struct TextColorTarget {
    session: Uuid,
    layer: Option<Uuid>,
    generation: u64,
    context: egui::Context,
    request: crate::text_color::TextColorRequest,
}

impl EditorApp {
    fn text_color_target_matches(&self, target: &TextColorTarget) -> bool {
        self.dialog_generation == target.generation
            && self.session().id == target.session
            && self.dialog_target == Some((target.session, target.layer))
            && matches!(&self.dialog,Some(Dialog::Text {layer,..}) if *layer==target.layer)
            && target.request.editor_id
                == egui::Id::new(("canvas-text", self.dialog_target.map(|(id, _)| id)))
    }

    pub(super) fn open_text_color_request(&mut self, ctx: &egui::Context) {
        let Some(request) = crate::text_color::take_request(ctx) else {
            return;
        };
        let Some((session, layer)) = self.dialog_target else {
            return;
        };
        if self.session().id != session
            || !matches!(&self.dialog,Some(Dialog::Text {layer:id,..}) if *id==layer)
            || request.editor_id != egui::Id::new(("canvas-text", Some(session)))
        {
            return;
        }
        let current = match crate::text::TextStyle::from_value(&self.dialog_value) {
            Ok(style) if style.content == request.original.content => style,
            _ => return,
        };
        if request.project(&current, request.opening_color()).is_err() {
            return;
        }
        self.finish_color_picker(false);
        self.color_picker = Some(crate::palette::ColorPicker::new(
            false,
            request.opening_color(),
        ));
        self.text_color_target = Some(TextColorTarget {
            session,
            layer,
            generation: self.dialog_generation,
            context: ctx.clone(),
            request,
        });
        self.prepare_text_color_sampler();
    }

    pub(super) fn finish_text_color_picker(&mut self, accept: bool) {
        let Some(target) = self.text_color_target.take() else {
            return;
        };
        let picker = self.color_picker.take();
        if accept
            && self.text_color_target_matches(&target)
            && let Some(picker) = picker
        {
            let result = crate::text::TextStyle::from_value(&self.dialog_value)
                .and_then(|current| target.request.project(&current, picker.color));
            match result {
                Ok(projected) => self.dialog_value = projected.to_value(),
                Err(error) => self.message = Some(error.to_string()),
            }
        }
        crate::text_color::clear_preview(&target.context, target.request.editor_id);
        if self.text_color_target_matches(&target) {
            target
                .context
                .memory_mut(|memory| memory.request_focus(target.request.editor_id));
        }
        self.sample_drag = None;
        self.sample_point = None;
    }

    pub(super) fn preview_text_color_picker(&mut self) {
        let Some(target) = &self.text_color_target else {
            return;
        };
        if !self.text_color_target_matches(target) {
            self.finish_text_color_picker(false);
            return;
        }
        let Some(picker) = &self.color_picker else {
            self.finish_text_color_picker(false);
            return;
        };
        let result = crate::text::TextStyle::from_value(&self.dialog_value).and_then(|current| {
            crate::text_color::set_preview(&target.context, &target.request, &current, picker.color)
        });
        if let Err(error) = result {
            self.message = Some(error.to_string());
            self.finish_text_color_picker(false);
        }
    }

    fn prepare_text_color_sampler(&mut self) {
        if self.text_color_target.is_none() {
            return;
        }
        if self.sample_cache.as_ref().is_some_and(|(id, sampler)| {
            *id == self.session().id && sampler.matches(&self.session().doc)
        }) {
            return;
        }
        let key = (self.session().id, self.session().serial);
        if self.sample_busy == Some(key) {
            return;
        }
        self.sample_cache = None;
        self.sample_point = None;
        self.sample_busy = Some(key);
        let document = self.session().doc.clone();
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let sampler = Arc::new(render::CompositeSampler::new(&document));
            let _ = sender.send(Worker::ColorSample {
                session: key.0,
                serial: key.1,
                sampler,
            });
        });
    }

    /// The inline editor is disabled only while its picker samples the canvas. The
    /// preview remains drawn over the document and pointer input reaches this response.
    pub(super) fn sample_text_color_canvas(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        rect: Rect,
    ) {
        if self
            .text_color_target
            .as_ref()
            .is_none_or(|target| !self.text_color_target_matches(target))
        {
            return;
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        self.prepare_text_color_sampler();
        if !rect.is_positive() {
            return;
        }
        if !(response.clicked() || response.dragged() || response.drag_stopped()) {
            return;
        }
        let Some(position) = response
            .interact_pointer_pos()
            .or_else(|| response.hover_pos())
            .filter(|position| rect.contains(*position))
        else {
            return;
        };
        let point = [
            (position.x - rect.left()) as f64 / rect.width() as f64
                * self.session().doc.width as f64,
            (position.y - rect.top()) as f64 / rect.height() as f64
                * self.session().doc.height as f64,
        ];
        self.sample_point = Some(point);
        let color = self
            .sample_cache
            .as_ref()
            .filter(|(id, sampler)| {
                *id == self.session().id && sampler.matches(&self.session().doc)
            })
            .and_then(|(_, sampler)| sampler.sample(point));
        if let Some(color) = color {
            self.apply_sampled_color(color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::TextStyle;

    fn text_editor() -> (EditorApp, egui::Context, egui::Id) {
        let mut app = EditorApp::create(None, None);
        let ctx = egui::Context::default();
        app.dialog = Some(Dialog::Text {
            layer: None,
            origin: [0., 0.],
        });
        app.dialog_target = Some((app.session().id, None));
        let id = egui::Id::new(("canvas-text", Some(app.session().id)));
        app.dialog_value = TextStyle {
            content: "A😀中文B".into(),
            red: 0.2,
            green: 0.3,
            blue: 0.4,
            ..Default::default()
        }
        .to_value();
        let style = TextStyle::from_value(&app.dialog_value).unwrap();
        let request = crate::text_color::TextColorRequest::new(id, &style, 3..5).unwrap();
        app.color_picker = Some(crate::palette::ColorPicker::new(
            false,
            request.opening_color(),
        ));
        app.text_color_target = Some(TextColorTarget {
            session: app.session().id,
            layer: None,
            generation: app.dialog_generation,
            context: ctx.clone(),
            request,
        });
        (app, ctx, id)
    }

    #[test]
    fn accept_colors_captured_range_and_canvas_sampling_keeps_palette_colors() {
        let (mut app, _, _) = text_editor();
        let palette = (app.foreground, app.background);
        app.apply_sampled_color([11, 31, 71, 255]);
        let draft = TextStyle::from_value(&app.dialog_value).unwrap();
        assert_eq!(
            draft.color_at(3),
            [0.2, 0.3, 0.4],
            "Working colors are display projections"
        );
        app.finish_color_picker(true);
        let final_style = TextStyle::from_value(&app.dialog_value).unwrap();
        assert_eq!(final_style.color_at(3), [11., 31., 71.].map(|v| v / 255.));
        assert_eq!(final_style.color_at(0), [0.2, 0.3, 0.4]);
        assert_eq!((app.foreground, app.background), palette);
        assert!(app.text_color_target.is_none());
    }

    #[test]
    fn cancel_drops_projection_and_keeps_draft_and_unrelated_font_edits() {
        let (mut app, ctx, id) = text_editor();
        let palette = (app.foreground, app.background);
        app.dialog_value["fontSize"] = json!(103.);
        let opening = app.dialog_value.clone();
        app.apply_sampled_color([140, 24, 91, 255]);
        let style = TextStyle::from_value(&app.dialog_value).unwrap();
        assert!(crate::text_color::preview_style(&ctx, id, &style).is_some());
        app.finish_color_picker(false);
        assert_eq!(app.dialog_value, opening);
        assert_eq!((app.foreground, app.background), palette);
        assert!(crate::text_color::preview_style(&ctx, id, &style).is_none());
    }

    #[test]
    fn reopened_or_switched_draft_and_stale_content_receive_no_color_write() {
        for change in 0..3 {
            let (mut app, _, _) = text_editor();
            app.color_picker.as_mut().unwrap().sample([30, 60, 90, 255]);
            match change {
                0 => app.dialog_generation = app.dialog_generation.wrapping_add(1),
                1 => app.dialog_target = Some((Uuid::new_v4(), None)),
                _ => app.dialog_value["content"] = json!("New draft"),
            }
            let draft = app.dialog_value.clone();
            let palette = (app.foreground, app.background);
            app.finish_color_picker(true);
            assert_eq!(app.dialog_value, draft);
            assert_eq!((app.foreground, app.background), palette);
        }
    }
}
