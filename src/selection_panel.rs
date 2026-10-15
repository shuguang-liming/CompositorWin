//! Selection amount panel; pixel operations remain the existing Windows algorithms.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SelectionAmountOperation {
    Expand,
    Contract,
    Feather,
}

impl SelectionAmountOperation {
    fn from_command(command: Command) -> Option<Self> {
        match command {
            Command::Expand => Some(Self::Expand),
            Command::Contract => Some(Self::Contract),
            Command::Feather => Some(Self::Feather),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Expand => 0,
            Self::Contract => 1,
            Self::Feather => 2,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Expand => "扩展选区",
            Self::Contract => "收缩选区",
            Self::Feather => "羽化选区",
        }
    }
}

fn parsed_amount(input: &str) -> Option<f64> {
    let amount: f64 = input.trim().parse().ok()?;
    (amount.is_finite() && (0.1..=500.).contains(&amount)).then_some(amount)
}

pub(super) struct SelectionAmountDialog {
    session: Uuid,
    revision: Uuid,
    serial: u64,
    operation: SelectionAmountOperation,
    original: Selection,
    input: String,
    focus: bool,
}

impl SelectionAmountDialog {
    fn new(session: &Session, operation: SelectionAmountOperation) -> Option<Self> {
        let original = session.doc.selection.as_ref()?;
        if original.is_empty() || session.save_busy || session.edit_busy || session.reload_busy {
            return None;
        }
        Some(Self {
            session: session.id,
            revision: session.history.revision(),
            serial: session.serial,
            operation,
            original: original.clone(),
            input: session.selection_amounts[operation.index()].to_string(),
            focus: true,
        })
    }

    fn matches(&self, session: &Session) -> bool {
        session.id == self.session
            && session.serial == self.serial
            && session.history.revision() == self.revision
            && !session.save_busy
            && !session.edit_busy
            && !session.reload_busy
            && session.doc.selection.is_some()
    }

    fn apply(self, session: &mut Session) -> bool {
        let Some(amount) = parsed_amount(&self.input) else {
            return false;
        };
        // Also reject an uncommitted canvas change. Compare coverage only on Apply,
        // never while drawing an unchanged panel.
        if !self.matches(session) || session.doc.selection.as_ref() != Some(&self.original) {
            return false;
        }
        session.selection_amounts[self.operation.index()] = amount;
        session.edit("Modify selection", |document| {
            if let Some(selection) = &document.selection {
                document.selection = Some(match self.operation {
                    SelectionAmountOperation::Expand => selection::expand(selection, amount as u32),
                    SelectionAmountOperation::Contract => {
                        selection::contract(selection, amount as u32)
                    }
                    SelectionAmountOperation::Feather => selection::feather(selection, amount),
                });
            }
        });
        true
    }
}

impl EditorApp {
    pub(super) fn prompt_selection_amount(&mut self, command: Command) {
        let Some(operation) = SelectionAmountOperation::from_command(command) else {
            return;
        };
        if self.is_welcome()
            || self.dialog.is_some()
            || self.color_picker.is_some()
            || self.drag.is_some()
            || self.guide_drag.is_some()
            || self.gradient_handle.is_some()
            || self.property_edit.is_some()
            || self.visibility_sweep.is_some()
            || self.selection_press.is_some()
            || !self.polygon.is_empty()
            || self.close_pending.is_some()
            || self.exit_pending
        {
            return;
        }
        self.selection_amount_dialog = SelectionAmountDialog::new(self.session(), operation);
    }

    pub(super) fn show_selection_amount_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.selection_amount_dialog.take() else {
            return;
        };
        if !dialog.matches(self.session())
            || self.dialog.is_some()
            || self.color_picker.is_some()
            || self.drag.is_some()
            || self.guide_drag.is_some()
            || self.gradient_handle.is_some()
            || self.property_edit.is_some()
            || self.visibility_sweep.is_some()
            || self.selection_press.is_some()
            || !self.polygon.is_empty()
            || self.close_pending.is_some()
            || self.exit_pending
        {
            return;
        }
        let (escape, enter) = ctx.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::NONE, Key::Escape),
                input.consume_key(egui::Modifiers::NONE, Key::Enter),
            )
        });
        if escape {
            return;
        }
        let mut open = true;
        let mut cancel = false;
        let mut apply = false;
        crate::dialog_layout::window(
            ctx,
            dialog.operation.title(),
            egui::Id::new(("selection-amount", dialog.session, dialog.operation.index())),
            380.,
            false,
            ctx.available_rect().center(),
        )
        .open(&mut open)
        .show(ctx, |ui| {
            crate::dialog_layout::padded(ui, 24, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(10., 16.);
                ui.spacing_mut().interact_size.y = 26.;
                ui.horizontal(|ui| {
                    let label = ui
                        .add_sized([60., 26.], egui::Label::new("数量").sense(Sense::drag()))
                        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                        .on_hover_text("左右拖动以调整像素数量");
                    let mut amount = parsed_amount(&dialog.input).unwrap_or(0.1);
                    if label.dragged() {
                        amount = (amount + ui.input(|input| input.pointer.delta().x) as f64)
                            .clamp(0.1, 500.);
                        dialog.input = amount.to_string();
                    }
                    let unit_width = ui
                        .painter()
                        .layout_no_wrap(
                            "px".into(),
                            egui::FontId::proportional(12.),
                            ui.visuals().text_color(),
                        )
                        .size()
                        .x;
                    let slider_width = (ui.available_width() - 56. - unit_width - 20.).max(0.);
                    ui.spacing_mut().slider_width = slider_width;
                    if ui
                        .add_sized(
                            [slider_width, 26.],
                            egui::Slider::new(&mut amount, 0.1..=500.).show_value(false),
                        )
                        .changed()
                    {
                        dialog.input = amount.to_string();
                    }
                    let input = ui.add_sized(
                        [56., 26.],
                        egui::TextEdit::singleline(&mut dialog.input)
                            .id_source("selection-amount-input")
                            .horizontal_align(egui::Align::RIGHT)
                            .font(egui::TextStyle::Body),
                    );
                    if dialog.focus {
                        input.request_focus();
                        dialog.focus = false;
                    }
                    ui.label(egui::RichText::new("px").size(12.));
                });
                let valid = parsed_amount(&dialog.input).is_some();
                ui.label(
                    egui::RichText::new("请输入 0.1 至 500 px 的数值。")
                        .size(12.)
                        .color(if valid {
                            Color32::TRANSPARENT
                        } else {
                            ui.visuals().weak_text_color()
                        }),
                );
                ui.separator();
                ui.horizontal(|ui| {
                    cancel = ui
                        .add_sized([72., 26.], egui::Button::new("取消"))
                        .clicked();
                    apply = ui
                        .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_enabled(
                                valid,
                                egui::Button::new(
                                    egui::RichText::new("确定")
                                        .color(ui.visuals().selection.stroke.color),
                                )
                                .fill(ui.visuals().selection.bg_fill)
                                .min_size(Vec2::new(72., 26.)),
                            )
                            .clicked()
                        })
                        .inner;
                });
            });
        });
        if !open || cancel {
            return;
        }
        if (enter || apply) && parsed_amount(&dialog.input).is_some() {
            // The destination is the captured session, and matches rejects a tab
            // switch or an intervening edit before any amount or history changes.
            dialog.apply(self.session_mut());
        } else {
            self.selection_amount_dialog = Some(dialog);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected_session() -> Session {
        let mut document = Document::new(16, 16);
        document.selection = Some(selection::marquee(16, 16, [4., 4., 8., 8.], false, false));
        Session::new(document, None)
    }

    #[test]
    fn amount_accepts_existing_windows_range_and_rejects_nonfinite_input() {
        assert_eq!(parsed_amount(" 0.1 "), Some(0.1));
        assert_eq!(parsed_amount("2.75"), Some(2.75));
        assert_eq!(parsed_amount("500"), Some(500.));
        for input in ["", "0", "0.09", "500.1", "NaN", "inf", "-inf", "invalid"] {
            assert_eq!(parsed_amount(input), None, "{input}");
        }
    }

    #[test]
    fn amount_apply_preserves_fractional_algorithm_and_one_undo_step() {
        for operation in [
            SelectionAmountOperation::Expand,
            SelectionAmountOperation::Contract,
            SelectionAmountOperation::Feather,
        ] {
            let mut session = selected_session();
            let before = session.doc.clone();
            let mut dialog = SelectionAmountDialog::new(&session, operation).unwrap();
            dialog.input = "2.75".into();
            let mut expected = before.clone();
            let selected = before.selection.as_ref().unwrap();
            expected.selection = Some(match operation {
                SelectionAmountOperation::Expand => selection::expand(selected, 2),
                SelectionAmountOperation::Contract => selection::contract(selected, 2),
                SelectionAmountOperation::Feather => selection::feather(selected, 2.75),
            });
            assert!(dialog.apply(&mut session));
            assert_eq!(session.doc, expected);
            assert_eq!(session.selection_amounts[operation.index()], 2.75);
            assert_eq!(session.history.undo_name(), Some("Modify selection"));
            assert!(session.history.undo(&mut session.doc));
            assert_eq!(session.doc, before);
            assert!(session.history.undo_name().is_none());
            assert!(session.history.redo(&mut session.doc));
            assert_eq!(session.doc, expected);
        }
    }

    #[test]
    fn amount_rejects_other_session_or_stale_revision_serial_and_draft() {
        for change in 0..6 {
            let mut session = selected_session();
            let dialog =
                SelectionAmountDialog::new(&session, SelectionAmountOperation::Expand).unwrap();
            match change {
                0 => session.id = Uuid::new_v4(),
                1 => session.changed(),
                2 => session.edit("Other edit", |doc| doc.resolution = 144.),
                3 => session.doc.selection = Some(Selection::empty(16, 16)),
                4 => session.edit_busy = true,
                _ => session.reload_busy = true,
            }
            let before = session.doc.clone();
            let revision = session.history.revision();
            let amounts = session.selection_amounts;
            assert!(!dialog.apply(&mut session), "change {change}");
            assert_eq!(session.doc, before);
            assert_eq!(session.history.revision(), revision);
            assert_eq!(session.selection_amounts, amounts);
        }
    }
}
