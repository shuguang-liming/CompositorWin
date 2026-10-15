//! A bounded, data-only theme format. Themes never execute code or affect image pixels.

use crate::chrome::theme::{self, Palette};
use anyhow::{Context, Result, bail};
use eframe::egui::Color32;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{fs::File, io::Read, path::Path};

pub const CURRENT_VERSION: u32 = 1;
pub const MAX_FILE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeColor([u8; 3]);

impl ThemeColor {
    pub const fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Self([red, green, blue])
    }

    pub fn color32(self) -> Color32 {
        Color32::from_rgb(self.0[0], self.0[1], self.0[2])
    }
}

impl From<Color32> for ThemeColor {
    fn from(color: Color32) -> Self {
        Self([color.r(), color.g(), color.b()])
    }
}

impl Serialize for ThemeColor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!(
            "#{:02X}{:02X}{:02X}",
            self.0[0], self.0[1], self.0[2]
        ))
    }
}

impl<'de> Deserialize<'de> for ThemeColor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let bytes = value.as_bytes();
        if bytes.len() != 7 || bytes[0] != b'#' || !bytes[1..].iter().all(u8::is_ascii_hexdigit) {
            return Err(de::Error::custom(
                "颜色必须为 #RRGGBB，使用六位十六进制 RGB，不含透明度",
            ));
        }
        let channel = |start| u8::from_str_radix(&value[start..start + 2], 16).unwrap();
        Ok(Self([channel(1), channel(3), channel(5)]))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeColors {
    pub shell: ThemeColor,
    pub canvas: ThemeColor,
    pub field: ThemeColor,
    pub text: ThemeColor,
    pub secondary_text: ThemeColor,
    pub disabled_text: ThemeColor,
    pub divider: ThemeColor,
    pub checker_light: ThemeColor,
    pub checker_dark: ThemeColor,
    pub thumb_checker_light: ThemeColor,
    pub thumb_checker_dark: ThemeColor,
    pub ruler_fill: ThemeColor,
    pub ruler_tick: ThemeColor,
    pub ruler_text: ThemeColor,
    pub ruler_divider: ThemeColor,
    pub accent: ThemeColor,
}

impl ThemeColors {
    pub fn palette(&self) -> Palette {
        Palette {
            shell: self.shell.color32(),
            canvas: self.canvas.color32(),
            field: self.field.color32(),
            text: self.text.color32(),
            secondary_text: self.secondary_text.color32(),
            disabled_text: self.disabled_text.color32(),
            divider: self.divider.color32(),
            checker_light: self.checker_light.color32(),
            checker_dark: self.checker_dark.color32(),
            thumb_checker_light: self.thumb_checker_light.color32(),
            thumb_checker_dark: self.thumb_checker_dark.color32(),
            ruler_fill: self.ruler_fill.color32(),
            ruler_tick: self.ruler_tick.color32(),
            ruler_text: self.ruler_text.color32(),
            ruler_divider: self.ruler_divider.color32(),
            accent: self.accent.color32(),
        }
    }

    fn validate(&self, name: &str) -> Result<()> {
        let text = self.text.color32();
        for (field, color) in [("shell", self.shell), ("field", self.field)] {
            let contrast = contrast_ratio(text, color.color32());
            if contrast < 4.5 {
                bail!("{name} 配色的 text 与 {field} 对比度为 {contrast:.2}:1，至少需要 4.5:1");
            }
        }
        Ok(())
    }
}

impl From<Palette> for ThemeColors {
    fn from(colors: Palette) -> Self {
        Self {
            shell: colors.shell.into(),
            canvas: colors.canvas.into(),
            field: colors.field.into(),
            text: colors.text.into(),
            secondary_text: colors.secondary_text.into(),
            disabled_text: colors.disabled_text.into(),
            divider: colors.divider.into(),
            checker_light: colors.checker_light.into(),
            checker_dark: colors.checker_dark.into(),
            thumb_checker_light: colors.thumb_checker_light.into(),
            thumb_checker_dark: colors.thumb_checker_dark.into(),
            ruler_fill: colors.ruler_fill.into(),
            ruler_tick: colors.ruler_tick.into(),
            ruler_text: colors.ruler_text.into(),
            ruler_divider: colors.ruler_divider.into(),
            accent: colors.accent.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemePalettes {
    pub dark: ThemeColors,
    pub light: ThemeColors,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ThemeFile {
    pub version: u32,
    pub name: String,
    pub palettes: ThemePalettes,
}

impl<'de> Deserialize<'de> for ThemeFile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            version: u32,
            name: String,
            palettes: ThemePalettes,
        }
        let fields = Fields::deserialize(deserializer)?;
        let theme = Self {
            version: fields.version,
            name: fields.name,
            palettes: fields.palettes,
        };
        theme.validate().map_err(de::Error::custom)?;
        Ok(theme)
    }
}

impl ThemeFile {
    pub fn builtin() -> Self {
        Self {
            version: CURRENT_VERSION,
            name: "Compositor 原版".into(),
            palettes: ThemePalettes {
                dark: theme::builtin_palette(true).into(),
                light: theme::builtin_palette(false).into(),
            },
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != CURRENT_VERSION {
            bail!(
                "不支持主题格式版本 {}，当前支持版本 {CURRENT_VERSION}",
                self.version
            );
        }
        if self.name.trim().is_empty()
            || self.name.chars().count() > 64
            || self.name.chars().any(char::is_control)
        {
            bail!("主题名称须为 1 至 64 个字符，且不含控制字符");
        }
        self.palettes.dark.validate("dark（夜间）")?;
        self.palettes.light.validate("light（日间）")?;
        Ok(())
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_FILE_BYTES {
            bail!("主题文件超过 64 KiB");
        }
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        serde_json::from_slice(bytes).context("主题 JSON 格式错误（字段须完整，且不含未知字段）")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        File::open(path)
            .context("读取主题文件失败")?
            .take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .context("读取主题文件失败")?;
        Self::from_json(&bytes)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).context("生成主题 JSON 失败")?;
        if bytes.len() > MAX_FILE_BYTES {
            bail!("主题文件超过 64 KiB");
        }
        crate::image_io::write_atomic(path, &bytes).context("保存主题文件失败")
    }
}

pub(crate) fn luminance(color: Color32) -> f64 {
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
}

pub(crate) fn contrast_ratio(first: Color32, second: Color32) -> f64 {
    let first = luminance(first);
    let second = luminance(second);
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}
