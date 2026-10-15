use super::*;

impl EditorApp {
    pub(super) fn apply_editor_theme(&self, ctx: &egui::Context) {
        crate::chrome::theme::apply_custom(
            ctx,
            self.custom_theme
                .as_ref()
                .filter(|_| self.custom_theme_enabled),
        );
        ctx.set_theme(self.theme_preference);
        ctx.request_repaint();
    }

    pub(super) fn import_custom_theme(
        &mut self,
        ctx: &egui::Context,
        path: &std::path::Path,
    ) -> anyhow::Result<()> {
        let theme = crate::custom_theme::ThemeFile::load(path)?;
        self.custom_theme = Some(theme);
        self.custom_theme_enabled = true;
        self.theme_settings_dirty = true;
        self.apply_editor_theme(ctx);
        Ok(())
    }

    pub(super) fn export_custom_theme(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(theme) = self
            .custom_theme
            .as_ref()
            .filter(|_| self.custom_theme_enabled)
        {
            theme.write(path)
        } else {
            crate::custom_theme::ThemeFile::builtin().write(path)
        }
    }

    pub(super) fn show_theme_settings(&mut self, ctx: &egui::Context) {
        if !self.theme_settings_open {
            return;
        }
        let mut open = true;
        egui::Window::new("主题设置")
            .id(egui::Id::new("custom-theme-settings"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.)
            .show(ctx, |ui| {
                let name = self
                    .custom_theme
                    .as_ref()
                    .filter(|_| self.custom_theme_enabled)
                    .map_or("原版主题", |theme| theme.name.as_str());
                ui.label(format!("当前主题：{name}"));
                ui.label("主题包含日间和夜间两套配色，也可以自动跟随系统。");
                ui.add_space(8.);
                let before = self.custom_theme_enabled;
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.custom_theme_enabled, false, "使用原版主题");
                    ui.add_enabled_ui(self.custom_theme.is_some(), |ui| {
                        ui.radio_value(&mut self.custom_theme_enabled, true, "使用自定义主题");
                    });
                });
                if before != self.custom_theme_enabled {
                    self.theme_settings_dirty = true;
                    self.theme_settings_message = None;
                    self.apply_editor_theme(ctx);
                }
                if let Some(theme) = &self.custom_theme {
                    ui.label(format!("已保存的自定义主题：{}", theme.name));
                }
                ui.horizontal(|ui| {
                    let before = self.theme_preference;
                    for (preference, label) in [
                        (egui::ThemePreference::System, "跟随系统"),
                        (egui::ThemePreference::Light, "日间"),
                        (egui::ThemePreference::Dark, "夜间"),
                    ] {
                        ui.selectable_value(&mut self.theme_preference, preference, label);
                    }
                    if before != self.theme_preference {
                        ctx.set_theme(self.theme_preference);
                        ctx.request_repaint();
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("导入主题…").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .set_title("导入 Compositor 主题")
                            .add_filter("JSON 主题", &["json"])
                            .pick_file()
                    {
                        self.theme_settings_message =
                            Some(match self.import_custom_theme(ctx, &path) {
                                Ok(()) => "主题已导入并应用，重启后仍会保留。".into(),
                                Err(error) => format!("导入失败，当前主题保持：{error:#}"),
                            });
                    }
                    if ui.button("导出主题…").clicked()
                        && let Some(mut path) = rfd::FileDialog::new()
                            .set_title("导出主题或 AI 制作模板")
                            .set_file_name("theme.json")
                            .add_filter("JSON 主题", &["json"])
                            .save_file()
                    {
                        if path.extension().is_none() {
                            path.set_extension("json");
                        }
                        self.theme_settings_message = Some(match self.export_custom_theme(&path) {
                            Ok(()) => {
                                "主题已导出。把 JSON 和 README 中的提示词交给 AI 即可制作新主题。"
                                    .into()
                            }
                            Err(error) => format!("导出失败：{error:#}"),
                        });
                    }
                });
                ui.add_space(8.);
                ui.label("制作方法：导出模板 → 交给 AI 调整配色 → 导入 JSON。");
                ui.label("详细格式、完整示例和 AI 提示词见软件目录的 README.md。");
                if let Some(message) = &self.theme_settings_message {
                    ui.separator();
                    ui.label(message);
                }
            });
        self.theme_settings_open = open;
    }
}
