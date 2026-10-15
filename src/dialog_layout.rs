//! Layout only: preserve the caller's dialog target, preview, and commit/cancel actions.
use eframe::egui;

/// Width is the entire egui panel, including its frame, rather than its content width.
pub fn window<'open>(
    ctx: &egui::Context,
    caption: &str,
    id: egui::Id,
    outer_width: f32,
    docked: bool,
    canvas_center: egui::Pos2,
) -> egui::Window<'open> {
    let style = ctx.style();
    let title = egui::RichText::new(caption).size(13.).strong();
    let frame = egui::Frame::window(&style).inner_margin(egui::Margin::ZERO);
    let margin = frame.total_margin().sum();
    let viewport = ctx.content_rect();
    let width = outer_width.min(viewport.width());
    let content_width = (width - margin.x).max(0.);
    let mut window = egui::Window::new(title.clone())
        .id(id)
        .frame(frame)
        .default_width(content_width)
        .min_width(content_width)
        .max_width(content_width)
        .default_height((viewport.height() - 40.).max(0.))
        .min_height(0.)
        .collapsible(false)
        .resizable(false)
        .vscroll(false);
    if docked {
        // Match egui 0.33's window chrome calculation, including its title separator.
        let title_height = ctx
            .fonts_mut(|fonts| title.font_height(fonts, &style))
            .max(style.spacing.interact_size.y);
        let content_height =
            (viewport.height() - margin.y - title_height - frame.stroke.width).max(0.);
        window = window
            .fixed_pos(egui::pos2(viewport.right() - width, viewport.top()))
            .fixed_size(egui::vec2(content_width, content_height));
    } else {
        // Only a default: persistent window ID preserves subsequent user positioning.
        window = window
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(canvas_center);
    }
    window
}

pub fn padded<R>(ui: &mut egui::Ui, padding: i8, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE
        .inner_margin(egui::Margin::same(padding))
        .show(ui, |ui| {
            // The outer layout reserves its own gap above the fixed footer.
            ui.spacing_mut().item_spacing.y = 0.;
            contents(ui)
        })
        .inner
}

/// Both the controls and preview belong in this area; keep Cancel/Apply outside it.
pub fn body<R>(ui: &mut egui::Ui, docked: bool, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let maximum = if docked {
        (ui.available_height() - 26. - 16.).max(0.)
    } else {
        (ui.ctx().content_rect().height() - 140.).max(64.)
    };
    egui::ScrollArea::vertical()
        .id_salt("editor-dialog-body")
        .max_height(maximum)
        .min_scrolled_height(0.)
        .auto_shrink([false, !docked])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 16.;
            contents(ui)
        })
        .inner
}

/// Returns (cancel, apply); the original app remains responsible for committing.
pub fn footer(
    ui: &mut egui::Ui,
    docked: bool,
    apply_enabled: bool,
    apply_label: &str,
) -> (bool, bool) {
    let gap = if docked {
        (ui.available_height() - 26.).max(16.)
    } else {
        16.
    };
    ui.add_space(gap);
    ui.spacing_mut().interact_size.y = 26.;
    ui.horizontal(|ui| {
        let cancel = ui
            .add_sized([72., 26.], egui::Button::new("取消"))
            .clicked();
        let apply = ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled(
                    apply_enabled,
                    egui::Button::new(
                        egui::RichText::new(apply_label).color(ui.visuals().selection.stroke.color),
                    )
                    .fill(ui.visuals().selection.bg_fill)
                    .min_size(egui::vec2(72., 26.)),
                )
                .clicked()
            })
            .inner;
        (cancel, apply)
    })
    .inner
}
