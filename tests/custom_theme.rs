use compositor::{
    chrome::theme,
    custom_theme::{MAX_FILE_BYTES, ThemeColor, ThemeFile},
};
use eframe::egui::{self, Color32};
use serde_json::{Value, json};

fn template_value() -> Value {
    serde_json::to_value(ThemeFile::builtin()).unwrap()
}

fn parse(value: &Value) -> anyhow::Result<ThemeFile> {
    ThemeFile::from_json(&serde_json::to_vec(value).unwrap())
}

fn custom() -> ThemeFile {
    let mut value = ThemeFile::builtin();
    value.name = "海洋主题".into();
    value.palettes.dark.shell = ThemeColor::from_rgb(8, 30, 45);
    value.palettes.dark.field = ThemeColor::from_rgb(5, 20, 32);
    value.palettes.dark.canvas = ThemeColor::from_rgb(4, 16, 23);
    value.palettes.dark.accent = ThemeColor::from_rgb(60, 180, 210);
    value.palettes.dark.checker_light = ThemeColor::from_rgb(46, 71, 81);
    value.palettes.dark.checker_dark = ThemeColor::from_rgb(31, 54, 63);
    value.palettes.light.shell = ThemeColor::from_rgb(220, 244, 246);
    value.palettes.light.field = ThemeColor::from_rgb(245, 253, 255);
    value.palettes.light.canvas = ThemeColor::from_rgb(185, 217, 225);
    value.validate().unwrap();
    value
}

fn system_frame(ctx: &egui::Context, theme: egui::Theme) {
    let _ = ctx.run(
        egui::RawInput {
            system_theme: Some(theme),
            ..Default::default()
        },
        |_| {},
    );
}

#[test]
fn builtin_template_is_complete_and_round_trips() {
    let original = ThemeFile::builtin();
    original.validate().unwrap();
    let value = serde_json::to_value(&original).unwrap();
    for mode in ["dark", "light"] {
        let fields = value["palettes"][mode].as_object().unwrap();
        assert_eq!(fields.len(), 16);
        for color in fields.values() {
            let color = color.as_str().unwrap();
            assert_eq!(color.len(), 7);
            assert!(color.starts_with('#'));
        }
    }
    assert_eq!(parse(&value).unwrap(), original);
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend(serde_json::to_vec(&original).unwrap());
    assert_eq!(ThemeFile::from_json(&bom).unwrap(), original);
}

#[test]
fn unknown_missing_and_duplicate_fields_are_rejected() {
    for location in ["root", "palettes", "dark", "light"] {
        let mut value = template_value();
        let target = match location {
            "root" => &mut value,
            "palettes" => &mut value["palettes"],
            mode => &mut value["palettes"][mode],
        };
        target["unexpected"] = json!("anything");
        assert!(
            parse(&value).is_err(),
            "unknown field accepted in {location}"
        );
    }
    for field in template_value()["palettes"]["dark"]
        .as_object()
        .unwrap()
        .keys()
    {
        for mode in ["dark", "light"] {
            let mut value = template_value();
            value["palettes"][mode]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(parse(&value).is_err(), "missing {mode}.{field} accepted");
        }
    }
    for required in ["version", "name", "palettes"] {
        let mut value = template_value();
        value.as_object_mut().unwrap().remove(required);
        assert!(parse(&value).is_err());
    }
    let serialized = serde_json::to_string(&ThemeFile::builtin()).unwrap();
    let duplicate = serialized.replacen("{", "{\"version\":1,", 1);
    assert!(ThemeFile::from_json(duplicate.as_bytes()).is_err());
    let duplicate = serialized.replacen("\"shell\":", "\"shell\":\"#123456\",\"shell\":", 1);
    assert!(ThemeFile::from_json(duplicate.as_bytes()).is_err());
}

#[test]
fn colors_accept_only_six_digit_rgb_and_normalize_case() {
    for color in [
        "#abc",
        "#AABBCCDD",
        "AABBCC",
        "#GG0000",
        "#ＦＦ0000",
        "rgb(1,2,3)",
        "#12345 ",
        "",
    ] {
        let mut value = template_value();
        value["palettes"]["dark"]["accent"] = json!(color);
        assert!(parse(&value).is_err(), "accepted color {color:?}");
    }
    for color in [json!(null), json!(1), json!([1, 2, 3])] {
        let mut value = template_value();
        value["palettes"]["dark"]["accent"] = color;
        assert!(parse(&value).is_err());
    }
    let mut value = template_value();
    value["palettes"]["dark"]["accent"] = json!("#abcdef");
    let parsed = parse(&value).unwrap();
    assert_eq!(
        parsed.palettes.dark.accent.color32(),
        Color32::from_rgb(171, 205, 239)
    );
    assert_eq!(
        serde_json::to_value(parsed).unwrap()["palettes"]["dark"]["accent"],
        "#ABCDEF"
    );
}

#[test]
fn name_and_version_are_validated_during_deserialization() {
    for name in [
        "".to_owned(),
        "   ".to_owned(),
        "名".repeat(65),
        "主题\n名称".to_owned(),
        "\0".to_owned(),
    ] {
        let mut value = template_value();
        value["name"] = json!(name);
        assert!(parse(&value).is_err());
        assert!(serde_json::from_value::<ThemeFile>(value).is_err());
    }
    let mut value = template_value();
    value["name"] = json!("名".repeat(64));
    assert!(parse(&value).is_ok());
    for version in [json!(0), json!(2), json!(1.5), json!(-1), json!("1")] {
        let mut value = template_value();
        value["version"] = version;
        assert!(parse(&value).is_err());
    }
}

#[test]
fn both_input_and_menu_text_contrast_are_required_in_both_modes() {
    for mode in ["dark", "light"] {
        for surface in ["shell", "field"] {
            let mut value = template_value();
            value["palettes"][mode][surface] = value["palettes"][mode]["text"].clone();
            let error = format!("{:#}", parse(&value).unwrap_err());
            assert!(error.contains(surface));
            assert!(error.contains(mode));
            assert!(error.contains("4.5:1"));
        }
    }
}

#[test]
fn bounded_file_loading_supports_bom_and_rejects_invalid_utf8() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("theme.json");
    let original = custom();
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend(serde_json::to_vec(&original).unwrap());
    bytes.resize(MAX_FILE_BYTES, b' ');
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(ThemeFile::load(&path).unwrap(), original);
    bytes.push(b' ');
    std::fs::write(&path, &bytes).unwrap();
    assert!(format!("{:#}", ThemeFile::load(&path).unwrap_err()).contains("64 KiB"));
    std::fs::write(&path, [0xFF, 0xFE, b'{', b'}']).unwrap();
    assert!(ThemeFile::load(&path).is_err());
    assert!(ThemeFile::load(&directory.path().join("absent.json")).is_err());
}

#[test]
fn theme_write_is_validated_before_replacing_an_existing_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("theme.json");
    let original = custom();
    original.write(&path).unwrap();
    assert_eq!(ThemeFile::load(&path).unwrap(), original);
    let bytes = std::fs::read(&path).unwrap();
    let mut invalid = original.clone();
    invalid.palettes.dark.field = invalid.palettes.dark.text;
    assert!(invalid.write(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn custom_palettes_follow_system_without_changing_geometry_or_preference() {
    let ctx = egui::Context::default();
    ctx.set_theme(egui::ThemePreference::System);
    theme::apply(&ctx);
    let spacing = ctx.style().spacing.clone();
    let custom = custom();
    theme::apply_custom(&ctx, Some(&custom));
    assert_eq!(ctx.style().spacing, spacing);
    for mode in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
        system_frame(&ctx, mode);
        assert_eq!(ctx.theme(), mode);
        let style = ctx.style();
        let expected = if mode == egui::Theme::Dark {
            custom.palettes.dark
        } else {
            custom.palettes.light
        };
        assert_eq!(theme::palette(&ctx, &style.visuals), expected.palette());
        assert_eq!(style.visuals.panel_fill, expected.shell.color32());
        assert_eq!(style.visuals.text_color(), expected.text.color32());
        assert_eq!(style.visuals.extreme_bg_color, expected.field.color32());
        assert_eq!(
            style.visuals.selection.stroke.color,
            expected.accent.color32()
        );
        assert_eq!(style.visuals.hyperlink_color, expected.accent.color32());
    }
    ctx.set_theme(egui::Theme::Dark);
    theme::apply_custom(&ctx, Some(&custom));
    system_frame(&ctx, egui::Theme::Light);
    assert_eq!(ctx.theme(), egui::Theme::Dark);
}

#[test]
fn themes_are_isolated_per_context_and_restoring_builtin_clears_custom_colors() {
    let ctx = egui::Context::default();
    let other = egui::Context::default();
    theme::apply(&ctx);
    theme::apply(&other);
    let custom = custom();
    theme::apply_custom(&ctx, Some(&custom));
    for mode in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.set_theme(mode);
        other.set_theme(mode);
        assert_ne!(
            theme::palette(&ctx, &ctx.style().visuals),
            theme::palette(&other, &other.style().visuals)
        );
    }
    theme::apply(&ctx);
    for mode in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.set_theme(mode);
        other.set_theme(mode);
        assert_eq!(ctx.style().visuals, other.style().visuals);
        assert_eq!(
            theme::palette(&ctx, &ctx.style().visuals),
            theme::builtin_palette(mode == egui::Theme::Dark)
        );
    }
}

#[test]
fn changing_theme_invalidates_both_cached_checker_textures() {
    let ctx = egui::Context::default();
    theme::apply(&ctx);
    for dark_mode in [true, false] {
        let texture = ctx.load_texture(
            "test checker",
            egui::ColorImage::filled([2, 2], Color32::WHITE),
            egui::TextureOptions::NEAREST,
        );
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new(("original-canvas-checker", dark_mode)),
                texture,
            )
        });
    }
    theme::apply_custom(&ctx, Some(&custom()));
    for dark_mode in [true, false] {
        let texture = ctx.data(|data| {
            data.get_temp::<egui::TextureHandle>(egui::Id::new((
                "original-canvas-checker",
                dark_mode,
            )))
        });
        assert!(texture.is_none());
    }
}

#[test]
fn valid_opposite_backgrounds_keep_derived_button_and_striped_text_readable() {
    let mut file = ThemeFile::builtin();
    for colors in [&mut file.palettes.dark, &mut file.palettes.light] {
        colors.shell = ThemeColor::from_rgb(0, 0, 0);
        colors.field = ThemeColor::from_rgb(255, 255, 255);
        colors.text = ThemeColor::from_rgb(118, 118, 118);
    }
    file.validate().unwrap();
    let ctx = egui::Context::default();
    theme::apply_custom(&ctx, Some(&file));
    for mode in [egui::Theme::Dark, egui::Theme::Light] {
        let style = ctx.style_of(mode);
        let visuals = &style.visuals;
        assert_eq!(visuals.text_color(), Color32::from_gray(118));
        for widget in [
            &visuals.widgets.noninteractive,
            &visuals.widgets.inactive,
            &visuals.widgets.hovered,
            &visuals.widgets.active,
            &visuals.widgets.open,
        ] {
            assert_eq!(widget.bg_fill, Color32::WHITE);
            assert_eq!(widget.weak_bg_fill, Color32::WHITE);
            assert_eq!(widget.fg_stroke.color, Color32::from_gray(118));
        }
        assert_eq!(visuals.faint_bg_color, Color32::WHITE);
        assert_eq!(visuals.selection.bg_fill, Color32::WHITE);
    }
}
