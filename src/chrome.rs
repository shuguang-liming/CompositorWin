//! Compact editor chrome derived from Compositor's Swift UI geometry.
//!
//! Shared controls and day/night palettes for the Windows editor.
//! The stamp, polygonal lasso, object cursor and gradient pattern are adapted from
//! the original project, Copyright (c) 2026 Wonder Assembly LLC, MIT license.

use eframe::egui::{
    self, Color32, FontId, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui,
    Vec2, WidgetInfo, WidgetType,
};
use std::sync::OnceLock;

pub mod metrics {
    pub const RAIL_WIDTH: f32 = 56.0;
    pub const TOOL_HEADER_HEIGHT: f32 = 42.0;
    pub const STATUS_HEIGHT: f32 = 30.0;
    pub const LAYERS_DEFAULT_WIDTH: f32 = 252.0;
    pub const LAYERS_MIN_WIDTH: f32 = 202.0;
    pub const LAYERS_MAX_WIDTH: f32 = 352.0;
    pub const TAB_HEIGHT: f32 = 28.0;
    pub const TAB_STRIP_HEIGHT: f32 = 34.0;
    pub const TAB_SPACING: f32 = 6.0;
    pub const TOOL_BUTTON_SIZE: f32 = 36.0;
    pub const TOOL_SPACING: f32 = 10.0;
    pub const TOOL_TOP_PADDING: f32 = 16.0;
    pub const TOOL_BOTTOM_PADDING: f32 = 12.0;
    pub const ICON_SIZE: f32 = 18.0;
    pub const LAYER_ROW_HEIGHT: f32 = 52.0;
    pub const LAYER_ROW_SPACING: f32 = 2.0;
    pub const LAYER_THUMBNAIL_SIZE: f32 = 36.0;
    pub const MASK_THUMBNAIL_SIZE: f32 = 30.0;
    pub const LAYER_INDENT: f32 = 24.0;
    pub const EFFECT_ROW_HEIGHT: f32 = 24.0;
    pub const RULER_THICKNESS: f32 = 18.0;
    pub const SWATCH_SIZE: f32 = 24.0;
    pub const SWATCH_OFFSET: f32 = 12.0;
    pub const CHROME_PADDING: f32 = 18.0;
}

pub mod theme {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Palette {
        pub shell: Color32,
        pub canvas: Color32,
        pub field: Color32,
        pub text: Color32,
        pub secondary_text: Color32,
        pub disabled_text: Color32,
        pub divider: Color32,
        pub checker_light: Color32,
        pub checker_dark: Color32,
        pub thumb_checker_light: Color32,
        pub thumb_checker_dark: Color32,
        pub ruler_fill: Color32,
        pub ruler_tick: Color32,
        pub ruler_text: Color32,
        pub ruler_divider: Color32,
        pub accent: Color32,
    }

    const DARK: Palette = Palette {
        shell: Color32::from_gray(36),
        canvas: Color32::from_gray(27),
        field: Color32::from_gray(31),
        text: Color32::from_gray(235),
        secondary_text: Color32::from_gray(160),
        disabled_text: Color32::from_gray(100),
        divider: Color32::from_gray(55),
        checker_light: Color32::from_gray(89),
        checker_dark: Color32::from_gray(77),
        thumb_checker_light: Color32::from_gray(82),
        thumb_checker_dark: Color32::from_gray(56),
        ruler_fill: Color32::from_gray(51),
        ruler_tick: Color32::from_gray(158),
        ruler_text: Color32::from_gray(199),
        ruler_divider: Color32::from_gray(20),
        accent: Color32::from_rgb(80, 151, 223),
    };

    const LIGHT: Palette = Palette {
        shell: Color32::from_gray(245),
        canvas: Color32::from_gray(216),
        field: Color32::WHITE,
        text: Color32::from_gray(35),
        secondary_text: Color32::from_gray(100),
        disabled_text: Color32::from_gray(155),
        divider: Color32::from_gray(210),
        checker_light: Color32::from_gray(242),
        checker_dark: Color32::from_gray(220),
        thumb_checker_light: Color32::from_gray(230),
        thumb_checker_dark: Color32::from_gray(205),
        ruler_fill: Color32::from_gray(233),
        ruler_tick: Color32::from_gray(110),
        ruler_text: Color32::from_gray(65),
        ruler_divider: Color32::from_gray(180),
        accent: Color32::from_rgb(35, 112, 190),
    };

    #[derive(Clone, Copy)]
    struct CustomPalettes {
        dark: Palette,
        light: Palette,
    }

    pub fn builtin_palette(dark_mode: bool) -> Palette {
        if dark_mode { DARK } else { LIGHT }
    }

    pub fn palette(ctx: &egui::Context, visuals: &egui::Visuals) -> Palette {
        let custom =
            ctx.data(|data| data.get_temp::<CustomPalettes>(egui::Id::new("custom-theme")));
        match custom {
            Some(colors) if visuals.dark_mode => colors.dark,
            Some(colors) => colors.light,
            None => builtin_palette(visuals.dark_mode),
        }
    }

    pub fn overlay(visuals: &egui::Visuals, alpha: u8) -> Color32 {
        if visuals.text_color().intensity() > visuals.panel_fill.intensity() {
            Color32::from_white_alpha(alpha)
        } else {
            Color32::from_black_alpha(alpha)
        }
    }

    pub fn tool_fill(
        visuals: &egui::Visuals,
        selected: bool,
        hovered: bool,
        pressed: bool,
    ) -> Color32 {
        overlay(
            visuals,
            if pressed {
                41
            } else if selected {
                31
            } else if hovered {
                15
            } else {
                0
            },
        )
    }

    pub fn tool_stroke(visuals: &egui::Visuals, selected: bool) -> Stroke {
        Stroke::new(1.0_f32, overlay(visuals, if selected { 36 } else { 0 }))
    }

    pub fn tab_fill(visuals: &egui::Visuals, selected: bool) -> Color32 {
        overlay(visuals, if selected { 31 } else { 9 })
    }

    pub fn tab_stroke(visuals: &egui::Visuals, selected: bool) -> Stroke {
        Stroke::new(1.0_f32, overlay(visuals, if selected { 56 } else { 20 }))
    }

    pub fn document_shadow(visuals: &egui::Visuals) -> egui::epaint::Shadow {
        egui::epaint::Shadow {
            offset: [0, 3],
            blur: 14,
            spread: 0,
            color: Color32::from_black_alpha(if visuals.dark_mode { 89 } else { 45 }),
        }
    }

    pub fn set_animations(ctx: &egui::Context, enabled: bool) {
        let duration = if enabled { 0.1 } else { 0.0 };
        if ctx.style().animation_time != duration {
            ctx.all_styles_mut(|style| style.animation_time = duration);
        }
    }

    /// Install both styles once after locale::install_fonts, preserving the theme preference.
    pub fn apply(ctx: &egui::Context) {
        apply_custom(ctx, None);
    }

    /// Replace only editor colors, retaining the day/night/system preference and geometry.
    pub fn apply_custom(ctx: &egui::Context, custom: Option<&crate::custom_theme::ThemeFile>) {
        let custom = custom.map(|file| CustomPalettes {
            dark: file.palettes.dark.palette(),
            light: file.palettes.light.palette(),
        });
        ctx.data_mut(|data| {
            let id = egui::Id::new("custom-theme");
            if let Some(colors) = custom {
                data.insert_temp(id, colors);
            } else {
                data.remove::<CustomPalettes>(id);
            }
            // The canvas keeps one checker texture per day/night palette.
            for dark_mode in [true, false] {
                data.remove::<egui::TextureHandle>(egui::Id::new((
                    "original-canvas-checker",
                    dark_mode,
                )));
            }
        });
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let mut style = (*ctx.style_of(theme)).clone();
            let mut visuals = match theme {
                egui::Theme::Dark => egui::Visuals::dark(),
                egui::Theme::Light => egui::Visuals::light(),
            };
            let colors = palette(ctx, &visuals);
            let dark_mode = visuals.dark_mode;
            let gray = |dark, light| Color32::from_gray(if dark_mode { dark } else { light });
            visuals.panel_fill = colors.shell;
            visuals.window_fill = colors.shell;
            visuals.window_stroke = Stroke::new(1.0_f32, gray(74, 190));
            visuals.window_corner_radius = egui::CornerRadius::same(8);
            visuals.faint_bg_color = gray(43, 236);
            visuals.extreme_bg_color = colors.field;
            visuals.code_bg_color = colors.field;
            visuals.override_text_color = Some(colors.text);
            visuals.weak_text_color = Some(colors.secondary_text);
            visuals.selection.bg_fill = if visuals.dark_mode {
                Color32::from_rgb(46, 73, 100)
            } else {
                Color32::from_rgb(199, 223, 247)
            };
            visuals.selection.stroke = Stroke::new(1.0_f32, colors.accent);
            for widget in [
                &mut visuals.widgets.noninteractive,
                &mut visuals.widgets.inactive,
                &mut visuals.widgets.hovered,
                &mut visuals.widgets.active,
                &mut visuals.widgets.open,
            ] {
                widget.corner_radius = egui::CornerRadius::same(5);
                widget.expansion = 0.0;
            }
            visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, colors.divider);
            visuals.widgets.inactive.bg_fill = gray(47, 236);
            visuals.widgets.inactive.weak_bg_fill = gray(47, 236);
            visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, gray(65, 200));
            visuals.widgets.hovered.bg_fill = gray(58, 222);
            visuals.widgets.hovered.weak_bg_fill = gray(58, 222);
            visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, gray(84, 170));
            visuals.widgets.active.bg_fill = gray(64, 210);
            visuals.widgets.active.weak_bg_fill = gray(64, 210);
            visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, gray(100, 145));
            if custom.is_some() {
                let blend =
                    |first: Color32, second: Color32, amount| first.lerp_to_gamma(second, amount);
                let readable = |background| {
                    if crate::custom_theme::contrast_ratio(colors.text, background) >= 4.5 {
                        background
                    } else {
                        colors.field
                    }
                };
                visuals.window_stroke.color = colors.divider;
                visuals.faint_bg_color = readable(blend(colors.shell, colors.field, 0.5));
                visuals.hyperlink_color = colors.accent;
                visuals.text_edit_bg_color = Some(colors.field);
                visuals.text_cursor.stroke.color = colors.text;
                let selection = blend(colors.field, colors.accent, 0.25);
                visuals.selection.bg_fill = readable(selection);
                for (widget, amount) in [
                    (&mut visuals.widgets.noninteractive, 0.0),
                    (&mut visuals.widgets.inactive, 0.0),
                    (&mut visuals.widgets.hovered, 0.4),
                    (&mut visuals.widgets.active, 0.7),
                    (&mut visuals.widgets.open, 0.4),
                ] {
                    let background = readable(blend(colors.field, colors.shell, amount));
                    widget.bg_fill = background;
                    widget.weak_bg_fill = background;
                    widget.bg_stroke.color = colors.divider;
                    widget.fg_stroke.color = colors.text;
                }
            }
            style.visuals = visuals;
            style.spacing.item_spacing = Vec2::new(8.0, 6.0);
            style.spacing.button_padding = Vec2::new(10.0, 4.0);
            style.spacing.interact_size.y = 22.0;
            style.spacing.slider_width = 100.0;
            for (text_style, font) in [
                (egui::TextStyle::Body, FontId::proportional(12.0)),
                (egui::TextStyle::Button, FontId::proportional(12.0)),
                (egui::TextStyle::Small, FontId::proportional(11.0)),
                (egui::TextStyle::Heading, FontId::proportional(16.0)),
                (egui::TextStyle::Monospace, FontId::monospace(11.0)),
            ] {
                style.text_styles.insert(text_style, font);
            }
            ctx.set_style_of(theme, style);
        }
        set_animations(ctx, crate::window_motion::system_animations_enabled());
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn system_frame(ctx: &egui::Context, theme: egui::Theme) {
            let _ = ctx.run(
                egui::RawInput {
                    system_theme: Some(theme),
                    ..Default::default()
                },
                |_| {},
            );
        }

        fn luminance(color: Color32) -> f32 {
            let linear = |channel: u8| {
                let value = f32::from(channel) / 255.;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
        }

        #[test]
        fn system_events_select_both_custom_styles() {
            let ctx = egui::Context::default();
            ctx.set_theme(egui::ThemePreference::System);
            apply(&ctx);
            for theme in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
                system_frame(&ctx, theme);
                assert_eq!(ctx.theme(), theme);
                let style = ctx.style();
                let colors = palette(&ctx, &style.visuals);
                assert_eq!(style.visuals.panel_fill, colors.shell);
                assert_eq!(style.visuals.text_color(), colors.text);
            }
            assert_ne!(LIGHT.shell, DARK.shell);
            assert_ne!(LIGHT.canvas, DARK.canvas);
            assert_ne!(LIGHT.text, DARK.text);
            assert!(luminance(LIGHT.text) < luminance(LIGHT.canvas));
            let contrast = (luminance(LIGHT.shell) + 0.05) / (luminance(LIGHT.text) + 0.05);
            assert!(contrast >= 7., "daytime text contrast is {contrast}");
        }

        #[test]
        fn manual_choice_survives_system_events_and_can_return_to_system() {
            let ctx = egui::Context::default();
            apply(&ctx);
            for theme in [egui::Theme::Light, egui::Theme::Dark] {
                ctx.set_theme(theme);
                for system in [egui::Theme::Dark, egui::Theme::Light] {
                    system_frame(&ctx, system);
                    assert_eq!(ctx.theme(), theme);
                }
                ctx.set_theme(egui::ThemePreference::System);
                assert_eq!(ctx.theme(), egui::Theme::Light);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Move,
    MarqueeRect,
    MarqueeEllipse,
    Lasso,
    PolygonLasso,
    Wand,
    Object,
    Crop,
    Brush,
    Eraser,
    Heal,
    Clone,
    Smear,
    Gradient,
    Shape,
    Text,
    Eyedropper,
    Hand,
    Zoom,
    Eye,
    EyeOff,
    Link,
    Unlink,
    Mask,
    Group,
    Add,
    AddLayer,
    Trash,
    Fit,
    ZoomIn,
    ZoomOut,
    ChevronDown,
    ChevronRight,
    Close,
    Swap,
    Reset,
    Effects,
    Adjustment,
    Lock,
    Unlock,
    FlipHorizontal,
    FlipVertical,
    More,
}

pub fn icon_button(ui: &mut Ui, icon: Icon, selected: bool, tooltip: &str) -> Response {
    icon_button_sized(
        ui,
        icon,
        selected,
        tooltip,
        Vec2::splat(metrics::TOOL_BUTTON_SIZE),
    )
}

pub fn icon_button_sized(
    ui: &mut Ui,
    icon: Icon,
    selected: bool,
    tooltip: &str,
    size: Vec2,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::Button, ui.is_enabled(), selected, tooltip)
    });
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let pressed = response.is_pointer_button_down_on();
        let duration = ui.style().animation_time;
        let fill = theme::tool_fill(ui.visuals(), selected, hovered, pressed);
        let fill_alpha = ui.ctx().animate_value_with_time(
            response.id.with("fill"),
            f32::from(fill.a()),
            if pressed { 0.0 } else { duration },
        );
        let stroke_alpha = ui.ctx().animate_value_with_time(
            response.id.with("stroke"),
            if selected { 36.0 } else { 0.0 },
            duration,
        );
        ui.painter().rect_filled(
            rect,
            7,
            theme::overlay(ui.visuals(), fill_alpha.round() as u8),
        );
        ui.painter().rect_stroke(
            rect,
            7,
            Stroke::new(
                1.0_f32,
                theme::overlay(ui.visuals(), stroke_alpha.round() as u8),
            ),
            StrokeKind::Inside,
        );
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                6,
                Stroke::new(1.0_f32, theme::palette(ui.ctx(), ui.visuals()).accent),
                StrokeKind::Inside,
            );
        }
        let side = metrics::ICON_SIZE.min(size.x - 4.0).min(size.y - 4.0);
        if side > 0.0 {
            let color = if ui.is_enabled() {
                theme::palette(ui.ctx(), ui.visuals()).text
            } else {
                theme::palette(ui.ctx(), ui.visuals()).disabled_text
            };
            paint_icon(
                ui.painter(),
                icon,
                Rect::from_center_size(rect.center(), Vec2::splat(side)),
                color,
            );
        }
    }
    response.on_hover_text(tooltip)
}

pub fn capsule(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let text = egui::RichText::new(label).size(12.0);
    let text = if selected { text.strong() } else { text };
    let selected_amount = ui.ctx().animate_bool_with_time(
        ui.next_auto_id().with("tab-selected"),
        selected,
        ui.style().animation_time,
    );
    let response = ui.add(
        egui::Button::new(text)
            .fill(theme::overlay(
                ui.visuals(),
                (9.0 + 22.0 * selected_amount).round() as u8,
            ))
            .stroke(Stroke::new(
                1.0_f32,
                theme::overlay(ui.visuals(), (20.0 + 36.0 * selected_amount).round() as u8),
            ))
            .corner_radius(14)
            .min_size(Vec2::new(0.0, metrics::TAB_HEIGHT)),
    );
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::Button, ui.is_enabled(), selected, label));
    response
}

struct IconCanvas<'a> {
    painter: &'a Painter,
    origin: Pos2,
    scale: f32,
    color: Color32,
}

impl IconCanvas<'_> {
    fn point(&self, p: [f32; 2]) -> Pos2 {
        self.origin + Vec2::new(p[0], p[1]) * self.scale
    }

    fn stroke(&self) -> Stroke {
        Stroke::new(1.4 * self.scale, self.color)
    }

    fn line(&self, a: [f32; 2], b: [f32; 2]) {
        self.painter
            .line_segment([self.point(a), self.point(b)], self.stroke());
    }

    fn path(&self, points: &[[f32; 2]], closed: bool) {
        let points = points.iter().map(|&p| self.point(p)).collect();
        self.painter.add(if closed {
            Shape::closed_line(points, self.stroke())
        } else {
            Shape::line(points, self.stroke())
        });
    }

    fn polygon(&self, points: &[[f32; 2]]) {
        self.painter.add(Shape::convex_polygon(
            points.iter().map(|&p| self.point(p)).collect(),
            self.color,
            Stroke::NONE,
        ));
    }

    fn cubic(&self, points: [[f32; 2]; 4]) {
        self.painter
            .add(egui::epaint::CubicBezierShape::from_points_stroke(
                points.map(|p| self.point(p)),
                false,
                Color32::TRANSPARENT,
                self.stroke(),
            ));
    }

    fn rect(&self, min: [f32; 2], max: [f32; 2], radius: u8, filled: bool) {
        let rect = Rect::from_min_max(self.point(min), self.point(max));
        let radius = ((radius as f32 * self.scale).round() as u8).max(u8::from(radius != 0));
        if filled {
            self.painter.rect_filled(rect, radius, self.color);
        } else {
            self.painter
                .rect_stroke(rect, radius, self.stroke(), StrokeKind::Middle);
        }
    }

    fn circle(&self, center: [f32; 2], radius: f32, filled: bool) {
        if filled {
            self.painter
                .circle_filled(self.point(center), radius * self.scale, self.color);
        } else {
            self.painter
                .circle_stroke(self.point(center), radius * self.scale, self.stroke());
        }
    }

    fn ellipse(&self, center: [f32; 2], radius: [f32; 2], filled: bool) {
        let center = self.point(center);
        let radius = Vec2::new(radius[0], radius[1]) * self.scale;
        self.painter.add(if filled {
            Shape::ellipse_filled(center, radius, self.color)
        } else {
            Shape::ellipse_stroke(center, radius, self.stroke())
        });
    }

    fn sparkle(&self, center: [f32; 2], radius: f32) {
        let [x, y] = center;
        self.path(
            &[
                [x, y - radius],
                [x + radius * 0.3, y - radius * 0.3],
                [x + radius, y],
                [x + radius * 0.3, y + radius * 0.3],
                [x, y + radius],
                [x - radius * 0.3, y + radius * 0.3],
                [x - radius, y],
                [x - radius * 0.3, y - radius * 0.3],
            ],
            true,
        );
    }

    fn zoom(&self, delta: i8) {
        self.circle([7.2, 7.2], 5.2, false);
        self.line([11.2, 11.2], [16.3, 16.3]);
        if delta != 0 {
            self.line([4.8, 7.2], [9.6, 7.2]);
            if delta > 0 {
                self.line([7.2, 4.8], [7.2, 9.6]);
            }
        }
    }

    fn eye(&self) {
        self.cubic([[1.0, 9.0], [5.3, 2.8], [12.7, 2.8], [17.0, 9.0]]);
        self.cubic([[17.0, 9.0], [12.7, 15.2], [5.3, 15.2], [1.0, 9.0]]);
        self.circle([9.0, 9.0], 2.5, false);
    }

    fn link(&self, linked: bool) {
        self.cubic([[7.7, 5.8], [12.6, 0.0], [17.8, 4.7], [12.2, 10.0]]);
        self.cubic([[10.4, 12.2], [5.5, 18.0], [0.2, 13.3], [5.8, 8.0]]);
        if linked {
            self.line([6.2, 11.8], [11.8, 6.2]);
        } else {
            self.line([1.6, 1.6], [4.0, 4.0]);
            self.line([14.0, 14.0], [16.4, 16.4]);
        }
    }

    fn half_circle(&self, center: [f32; 2], radius: f32) {
        let mut points = Vec::with_capacity(18);
        points.push(self.point(center));
        for index in 0..=16 {
            let angle = std::f32::consts::FRAC_PI_2 + index as f32 * std::f32::consts::PI / 16.0;
            points.push(self.point([
                center[0] + radius * angle.cos(),
                center[1] + radius * angle.sin(),
            ]));
        }
        self.painter
            .add(Shape::convex_polygon(points, self.color, Stroke::NONE));
    }
}

/// Paint a symbol into a square within `rect`; no font or texture is needed.
pub fn paint_icon(painter: &Painter, icon: Icon, rect: Rect, color: Color32) {
    let side = rect.width().min(rect.height());
    if side <= 0.0 || !side.is_finite() {
        return;
    }
    let c = IconCanvas {
        painter,
        origin: rect.center() - Vec2::splat(side * 0.5),
        scale: side / 18.0,
        color,
    };
    match icon {
        Icon::Move => {
            c.line([2.5, 2.5], [15.5, 15.5]);
            c.path(&[[2.5, 7.0], [2.5, 2.5], [7.0, 2.5]], false);
            c.path(&[[11.0, 15.5], [15.5, 15.5], [15.5, 11.0]], false);
        }
        Icon::MarqueeRect => {
            for start in [2.0, 7.0, 12.0] {
                c.line([start, 2.0], [(start + 3.0).min(16.0), 2.0]);
                c.line([start, 16.0], [(start + 3.0).min(16.0), 16.0]);
                c.line([2.0, start], [2.0, (start + 3.0).min(16.0)]);
                c.line([16.0, start], [16.0, (start + 3.0).min(16.0)]);
            }
        }
        Icon::MarqueeEllipse => {
            for index in 0..12 {
                let start = index as f32 * std::f32::consts::TAU / 12.0;
                let points: Vec<_> = (0..=3)
                    .map(|step| {
                        let angle = start + step as f32 * 0.10;
                        [9.0 + 7.0 * angle.cos(), 9.0 + 7.0 * angle.sin()]
                    })
                    .collect();
                c.path(&points, false);
            }
        }
        Icon::Lasso => {
            c.cubic([[15.8, 8.8], [19.0, 1.4], [4.4, -0.3], [2.0, 6.3]]);
            c.cubic([[2.0, 6.3], [-0.2, 12.8], [12.8, 14.2], [15.8, 8.8]]);
            c.cubic([[10.7, 11.4], [16.6, 10.5], [14.0, 16.1], [10.7, 13.5]]);
            c.cubic([[10.7, 13.5], [9.4, 15.7], [12.9, 16.0], [13.0, 17.0]]);
        }
        Icon::PolygonLasso => {
            c.path(
                &[
                    [1.2, 7.0],
                    [4.0, 2.4],
                    [11.8, 1.8],
                    [16.8, 5.2],
                    [15.6, 10.4],
                    [7.0, 11.6],
                ],
                true,
            );
            c.path(&[[8.9, 10.9], [13.3, 10.5], [11.6, 14.5]], true);
            c.line([11.6, 14.5], [12.9, 17.3]);
        }
        Icon::Wand => {
            c.path(&[[2.3, 14.2], [11.8, 4.7], [13.3, 6.2], [3.8, 15.7]], true);
            c.line([9.6, 6.9], [11.1, 8.4]);
            c.sparkle([5.0, 4.0], 2.0);
            c.line([14.8, 1.3], [14.8, 3.3]);
            c.line([13.8, 2.3], [15.8, 2.3]);
            c.line([15.2, 10.0], [15.2, 12.0]);
            c.line([14.2, 11.0], [16.2, 11.0]);
        }
        Icon::Object => {
            for points in [
                [[2.0, 6.0], [2.0, 2.0], [6.0, 2.0]],
                [[12.0, 2.0], [16.0, 2.0], [16.0, 6.0]],
                [[16.0, 12.0], [16.0, 16.0], [12.0, 16.0]],
                [[6.0, 16.0], [2.0, 16.0], [2.0, 12.0]],
            ] {
                c.path(&points, false);
            }
            c.polygon(&[[7.0, 5.0], [7.0, 14.0], [14.5, 10.9]]);
            c.polygon(&[[9.6, 11.7], [11.3, 15.3], [13.2, 14.4], [11.5, 10.9]]);
        }
        Icon::Crop => {
            c.path(&[[5.0, 1.0], [5.0, 13.0], [17.0, 13.0]], false);
            c.path(&[[1.0, 5.0], [13.0, 5.0], [13.0, 17.0]], false);
        }
        Icon::Brush => {
            c.polygon(&[[6.4, 10.6], [12.7, 2.0], [15.9, 5.1], [9.0, 13.2]]);
            c.path(
                &[
                    [6.4, 10.6],
                    [3.7, 11.8],
                    [2.0, 16.0],
                    [6.4, 15.5],
                    [9.0, 13.2],
                ],
                true,
            );
            c.line([4.0, 13.0], [6.0, 14.0]);
        }
        Icon::Eraser => {
            c.path(
                &[
                    [2.4, 10.8],
                    [9.8, 3.2],
                    [15.8, 9.2],
                    [9.2, 15.8],
                    [7.4, 15.8],
                ],
                true,
            );
            c.line([5.8, 7.3], [11.8, 13.3]);
            c.line([8.3, 15.8], [16.0, 15.8]);
        }
        Icon::Heal => {
            c.path(&[[2.0, 9.7], [9.7, 2.0], [16.0, 8.3], [8.3, 16.0]], true);
            c.path(&[[5.4, 6.4], [8.5, 3.4], [14.6, 9.5], [11.6, 12.6]], true);
            for center in [[8.5, 7.0], [11.0, 9.5], [7.0, 8.5], [9.5, 11.0]] {
                c.circle(center, 0.45, true);
            }
        }
        Icon::Clone => {
            c.ellipse([9.0, 3.06], [3.06, 2.70], true);
            c.rect([7.74, 5.04], [10.26, 10.08], 0, true);
            c.rect([2.16, 9.72], [15.84, 13.68], 1, true);
            c.rect([1.08, 14.76], [16.92, 16.92], 0, true);
        }
        Icon::Smear => {
            c.cubic([[9.0, 1.4], [7.1, 4.9], [3.0, 8.5], [3.0, 11.4]]);
            c.cubic([[3.0, 11.4], [3.0, 18.5], [15.0, 18.5], [15.0, 11.4]]);
            c.cubic([[15.0, 11.4], [15.0, 8.5], [10.9, 4.9], [9.0, 1.4]]);
            c.cubic([[5.6, 11.2], [5.6, 13.2], [6.7, 14.4], [8.6, 14.4]]);
        }
        Icon::Gradient => paint_gradient(&c),
        Icon::Shape => {
            c.circle([11.7, 11.7], 4.7, false);
            c.rect([2.0, 2.0], [11.0, 11.0], 1, false);
        }
        Icon::Text => {
            c.line([3.0, 3.0], [15.0, 3.0]);
            c.line([3.0, 3.0], [3.0, 5.0]);
            c.line([15.0, 3.0], [15.0, 5.0]);
            c.line([9.0, 3.0], [9.0, 16.0]);
            c.line([6.3, 16.0], [11.7, 16.0]);
        }
        Icon::Eyedropper => {
            c.polygon(&[[10.7, 2.0], [16.0, 7.3], [13.2, 10.1], [7.9, 4.8]]);
            c.path(
                &[
                    [8.0, 6.8],
                    [2.8, 12.0],
                    [2.0, 16.0],
                    [6.0, 15.2],
                    [11.2, 10.0],
                ],
                true,
            );
            c.line([6.5, 5.3], [12.7, 11.5]);
        }
        Icon::Hand => {
            c.path(
                &[
                    [5.0, 9.4],
                    [5.0, 4.0],
                    [6.2, 3.3],
                    [7.4, 4.0],
                    [7.4, 8.0],
                    [7.4, 2.2],
                    [8.6, 1.6],
                    [9.8, 2.2],
                    [9.8, 7.8],
                    [9.8, 3.3],
                    [11.0, 2.7],
                    [12.2, 3.3],
                    [12.2, 8.2],
                    [12.2, 5.4],
                    [13.4, 4.8],
                    [14.6, 5.4],
                    [14.6, 11.7],
                    [13.0, 15.7],
                    [7.0, 16.1],
                    [3.1, 11.4],
                    [2.0, 8.8],
                    [2.8, 7.7],
                    [4.0, 8.0],
                ],
                true,
            );
        }
        Icon::Zoom => c.zoom(0),
        Icon::ZoomIn => c.zoom(1),
        Icon::ZoomOut => c.zoom(-1),
        Icon::Eye => c.eye(),
        Icon::EyeOff => {
            c.eye();
            c.line([2.0, 2.0], [16.0, 16.0]);
        }
        Icon::Link => c.link(true),
        Icon::Unlink => c.link(false),
        Icon::Mask => {
            c.rect([1.5, 3.0], [16.5, 15.0], 2, false);
            c.circle([9.0, 9.0], 3.5, false);
            c.half_circle([9.0, 9.0], 3.5);
        }
        Icon::Group => {
            c.path(
                &[
                    [1.5, 5.0],
                    [1.5, 3.0],
                    [7.0, 3.0],
                    [9.0, 5.0],
                    [16.5, 5.0],
                    [16.5, 15.0],
                    [1.5, 15.0],
                ],
                true,
            );
        }
        Icon::Add => {
            c.line([9.0, 3.2], [9.0, 14.8]);
            c.line([3.2, 9.0], [14.8, 9.0]);
        }
        Icon::AddLayer => {
            c.rect([2.0, 2.0], [16.0, 16.0], 2, false);
            c.line([9.0, 5.5], [9.0, 12.5]);
            c.line([5.5, 9.0], [12.5, 9.0]);
        }
        Icon::Trash => {
            c.line([2.5, 4.5], [15.5, 4.5]);
            c.path(&[[6.0, 4.5], [6.0, 2.0], [12.0, 2.0], [12.0, 4.5]], false);
            c.path(&[[4.0, 6.0], [4.8, 16.0], [13.2, 16.0], [14.0, 6.0]], false);
            c.line([7.2, 7.0], [7.6, 13.4]);
            c.line([10.8, 7.0], [10.4, 13.4]);
        }
        Icon::Fit => {
            for points in [
                [[2.0, 6.0], [2.0, 2.0], [6.0, 2.0]],
                [[12.0, 2.0], [16.0, 2.0], [16.0, 6.0]],
                [[16.0, 12.0], [16.0, 16.0], [12.0, 16.0]],
                [[6.0, 16.0], [2.0, 16.0], [2.0, 12.0]],
            ] {
                c.path(&points, false);
            }
            c.rect([5.0, 6.0], [13.0, 12.0], 0, false);
        }
        Icon::ChevronDown => c.path(&[[4.0, 6.5], [9.0, 11.5], [14.0, 6.5]], false),
        Icon::ChevronRight => c.path(&[[6.5, 4.0], [11.5, 9.0], [6.5, 14.0]], false),
        Icon::Close => {
            c.line([4.0, 4.0], [14.0, 14.0]);
            c.line([4.0, 14.0], [14.0, 4.0]);
        }
        Icon::Swap => {
            c.path(&[[4.0, 8.0], [4.0, 3.0], [9.0, 3.0]], false);
            c.line([4.0, 3.0], [11.5, 10.5]);
            c.path(&[[14.0, 10.0], [14.0, 15.0], [9.0, 15.0]], false);
            c.line([14.0, 15.0], [6.5, 7.5]);
        }
        Icon::Reset => {
            c.cubic([[4.1, 5.0], [8.1, -0.3], [16.0, 3.0], [16.0, 9.0]]);
            c.cubic([[16.0, 9.0], [16.0, 17.2], [3.0, 17.9], [2.0, 10.0]]);
            c.path(&[[4.0, 1.3], [4.0, 5.3], [8.0, 5.3]], false);
        }
        Icon::Effects => {
            c.sparkle([7.0, 10.2], 5.3);
            c.sparkle([13.5, 3.9], 2.5);
            c.line([14.0, 12.5], [14.0, 15.5]);
            c.line([12.5, 14.0], [15.5, 14.0]);
        }
        Icon::Adjustment => {
            c.circle([9.0, 9.0], 6.6, false);
            c.half_circle([9.0, 9.0], 6.6);
        }
        Icon::Lock | Icon::Unlock => {
            c.rect([3.5, 7.5], [14.5, 16.0], 2, false);
            if icon == Icon::Lock {
                c.cubic([[5.8, 7.5], [4.2, -0.1], [13.8, -0.1], [12.2, 7.5]]);
            } else {
                c.cubic([[7.2, 7.5], [5.8, -0.1], [16.2, -0.1], [15.3, 5.2]]);
            }
            c.circle([9.0, 11.3], 0.9, true);
            c.line([9.0, 11.5], [9.0, 13.7]);
        }
        Icon::FlipHorizontal => {
            c.line([9.0, 1.0], [9.0, 17.0]);
            c.path(&[[6.0, 4.0], [1.5, 14.0], [6.0, 14.0]], true);
            c.path(&[[12.0, 4.0], [16.5, 14.0], [12.0, 14.0]], true);
        }
        Icon::FlipVertical => {
            c.line([1.0, 9.0], [17.0, 9.0]);
            c.path(&[[4.0, 6.0], [14.0, 1.5], [14.0, 6.0]], true);
            c.path(&[[4.0, 12.0], [14.0, 16.5], [14.0, 12.0]], true);
        }
        Icon::More => {
            for y in [4.5, 9.0, 13.5] {
                c.circle([9.0, y], 1.1, true);
            }
        }
    }
}

fn gradient_pattern() -> &'static [[bool; 16]; 16] {
    static PATTERN: OnceLock<[[bool; 16]; 16]> = OnceLock::new();
    PATTERN.get_or_init(|| {
        let mut ramp = [[0.0_f64; 16]; 16];
        for row in &mut ramp {
            for (x, value) in row.iter_mut().enumerate() {
                *value = x as f64 / 15.0;
            }
        }
        let mut result = [[false; 16]; 16];
        for y in 0..16 {
            for x in 0..16 {
                let on = ramp[y][x] >= 0.5;
                result[y][x] = on;
                let error = ramp[y][x] - if on { 1.0 } else { 0.0 };
                if x + 1 < 16 {
                    ramp[y][x + 1] += error * 7.0 / 16.0;
                }
                if y + 1 < 16 {
                    if x > 0 {
                        ramp[y + 1][x - 1] += error * 3.0 / 16.0;
                    }
                    ramp[y + 1][x] += error * 5.0 / 16.0;
                    if x + 1 < 16 {
                        ramp[y + 1][x + 1] += error / 16.0;
                    }
                }
            }
        }
        result
    })
}

fn paint_gradient(c: &IconCanvas<'_>) {
    let mut mesh = egui::epaint::Mesh::default();
    for (y, row) in gradient_pattern().iter().enumerate() {
        for (x, &on) in row.iter().enumerate() {
            if !on {
                continue;
            }
            let local_x = x as f32 + 1.5;
            let local_y = y as f32 + 1.5;
            let dx = (local_x - 9.0).abs() - 4.5;
            let dy = (local_y - 9.0).abs() - 4.5;
            if dx.max(0.0).hypot(dy.max(0.0)) > 3.5 {
                continue;
            }
            mesh.add_colored_rect(
                Rect::from_min_max(
                    c.point([x as f32 + 1.0, y as f32 + 1.0]),
                    c.point([x as f32 + 2.0, y as f32 + 2.0]),
                ),
                c.color,
            );
        }
    }
    c.painter.add(mesh);
    c.rect([1.0, 1.0], [17.0, 17.0], 4, false);
}
