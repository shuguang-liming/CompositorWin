use crate::model::{Document, MAX_PIXELS, Raster, Transform};
use anyhow::{Result, ensure};
use cosmic_text::{
    Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{cell::RefCell, ops::Range, sync::Arc};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ColorRun {
    pub location: usize,
    pub length: usize,
    pub red: f64,
    pub green: f64,
    pub blue: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FontRun {
    pub location: usize,
    pub length: usize,
    pub font_name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TextStyle {
    pub content: String,
    pub font_name: String,
    pub font_size: f64,
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alignment: String,
    pub tracking: f64,
    pub leading: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub box_size: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_runs: Option<Vec<ColorRun>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_runs: Option<Vec<FontRun>>,
}
impl Default for TextStyle {
    fn default() -> Self {
        Self {
            content: "文字".into(),
            font_name: "Arial".into(),
            font_size: 72.,
            red: 0.,
            green: 0.,
            blue: 0.,
            alignment: "Left".into(),
            tracking: 0.,
            leading: 0.,
            box_size: None,
            color_runs: None,
            font_runs: None,
        }
    }
}
impl TextStyle {
    pub fn from_value(value: &Value) -> Result<Self> {
        let mut normalized = value.clone();
        if normalized["boxSize"].is_object() {
            let b = &normalized["boxSize"];
            let size = [
                b["width"].as_f64().unwrap_or(0.),
                b["height"].as_f64().unwrap_or(0.),
            ];
            normalized["boxSize"] = serde_json::to_value(size)?;
        }
        let style: Self = serde_json::from_value(normalized)?;
        style.validate()?;
        Ok(style)
    }
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("Text style is serializable")
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.content.encode_utf16().count() <= 100_000,
            "文字超出 100,000 个 UTF-16 单元"
        );
        ensure!(
            self.font_size.is_finite() && (1. ..=2000.).contains(&self.font_size),
            "字号必须在 1–2000 像素之间"
        );
        ensure!(
            [self.red, self.green, self.blue]
                .iter()
                .all(|v| v.is_finite() && (0. ..=1.).contains(v)),
            "文字颜色无效"
        );
        ensure!(
            self.tracking.is_finite()
                && (-100. ..=1000.).contains(&self.tracking)
                && self.leading.is_finite()
                && (0. ..=5000.).contains(&self.leading),
            "文字间距无效"
        );
        if let Some(b) = self.box_size {
            ensure!(
                b.iter()
                    .all(|v| v.is_finite() && (16. ..=300_000.).contains(v))
                    && b[0] * b[1] <= MAX_PIXELS as f64,
                "段落框无效"
            );
        }
        let count = self.content.encode_utf16().count();
        let mut end = 0;
        if let Some(runs) = &self.color_runs {
            ensure!(!runs.is_empty(), "字符颜色样式列表为空");
            for run in runs {
                ensure!(
                    run.location >= end
                        && run.length > 0
                        && run
                            .location
                            .checked_add(run.length)
                            .is_some_and(|v| v <= count),
                    "UTF-16 字符颜色范围无效"
                );
                ensure!(
                    [run.red, run.green, run.blue]
                        .iter()
                        .all(|v| v.is_finite() && (0. ..=1.).contains(v)),
                    "字符颜色无效"
                );
                end = run.location + run.length;
            }
        }
        end = 0;
        if let Some(runs) = &self.font_runs {
            ensure!(!runs.is_empty(), "字符字体样式列表为空");
            for run in runs {
                ensure!(
                    run.location >= end
                        && run.length > 0
                        && run
                            .location
                            .checked_add(run.length)
                            .is_some_and(|v| v <= count),
                    "UTF-16 字符字体范围无效"
                );
                ensure!(
                    !run.font_name.is_empty()
                        && run.font_name.chars().count() <= 200
                        && !run.font_name.contains(['\n', '\r']),
                    "字符字体无效"
                );
                end = run.location + run.length;
            }
        }
        Ok(())
    }
    pub fn color_at(&self, index: usize) -> [f64; 3] {
        self.color_runs
            .as_ref()
            .and_then(|r| {
                r.iter()
                    .find(|r| r.location <= index && index < r.location + r.length)
            })
            .map_or([self.red, self.green, self.blue], |r| {
                [r.red, r.green, r.blue]
            })
    }
    pub fn font_at(&self, index: usize) -> &str {
        self.font_runs
            .as_ref()
            .and_then(|r| {
                r.iter()
                    .find(|r| r.location <= index && index < r.location + r.length)
            })
            .map_or(&self.font_name, |r| &r.font_name)
    }
    pub fn set_color(&mut self, color: [f64; 3], range: Range<usize>) -> Result<()> {
        ensure!(
            color
                .iter()
                .all(|v| v.is_finite() && (0. ..=1.).contains(v)),
            "文字颜色无效"
        );
        let n = self.content.encode_utf16().count();
        let start = range.start.min(n);
        let end = range.end.max(start).min(n);
        if start == end || (start == 0 && end == n) {
            [self.red, self.green, self.blue] = color;
            self.color_runs = None;
            return Ok(());
        }
        let mut colors = (0..n).map(|i| self.color_at(i)).collect::<Vec<_>>();
        colors[start..end].fill(color);
        self.store_colors(&colors);
        Ok(())
    }
    pub fn set_font(&mut self, font: &str, range: Range<usize>) -> Result<()> {
        ensure!(
            !font.is_empty() && font.chars().count() <= 200 && !font.contains(['\n', '\r']),
            "字体名称无效"
        );
        let n = self.content.encode_utf16().count();
        let start = range.start.min(n);
        let end = range.end.max(start).min(n);
        if start == end || (start == 0 && end == n) {
            self.font_name = font.into();
            self.font_runs = None;
            return Ok(());
        }
        let mut fonts = (0..n)
            .map(|i| self.font_at(i).to_owned())
            .collect::<Vec<_>>();
        fonts[start..end].fill(font.into());
        self.store_fonts(&fonts);
        Ok(())
    }
    pub fn replace_utf16(&mut self, range: Range<usize>, replacement: &str) -> Result<()> {
        let count = self.content.encode_utf16().count();
        ensure!(
            range.start <= range.end && range.end <= count,
            "文字替换范围无效"
        );
        let start = utf16_to_byte(&self.content, range.start)
            .ok_or_else(|| anyhow::anyhow!("替换范围分割了 UTF-16 代理项对"))?;
        let end = utf16_to_byte(&self.content, range.end)
            .ok_or_else(|| anyhow::anyhow!("替换范围分割了 UTF-16 代理项对"))?;
        let mut colors = (0..count).map(|i| self.color_at(i)).collect::<Vec<_>>();
        let mut fonts = (0..count)
            .map(|i| self.font_at(i).to_owned())
            .collect::<Vec<_>>();
        let inherited = range.start.saturating_sub(1).min(count.saturating_sub(1));
        let color = colors
            .get(inherited)
            .copied()
            .unwrap_or([self.red, self.green, self.blue]);
        let font = fonts
            .get(inherited)
            .cloned()
            .unwrap_or(self.font_name.clone());
        let n = replacement.encode_utf16().count();
        colors.splice(range.clone(), std::iter::repeat_n(color, n));
        fonts.splice(range, std::iter::repeat_n(font, n));
        self.content.replace_range(start..end, replacement);
        self.store_colors(&colors);
        self.store_fonts(&fonts);
        self.validate()
    }
    fn store_colors(&mut self, colors: &[[f64; 3]]) {
        let base = [self.red, self.green, self.blue];
        let mut runs: Vec<ColorRun> = vec![];
        for (i, &color) in colors.iter().enumerate() {
            if color == base {
                continue;
            }
            if let Some(last) = runs.last_mut()
                && last.location + last.length == i
                && [last.red, last.green, last.blue] == color
            {
                last.length += 1;
                continue;
            }
            runs.push(ColorRun {
                location: i,
                length: 1,
                red: color[0],
                green: color[1],
                blue: color[2],
            });
        }
        self.color_runs = (!runs.is_empty()).then_some(runs);
    }
    fn store_fonts(&mut self, fonts: &[String]) {
        if let Some(first) = fonts.first()
            && fonts.iter().all(|f| f == first)
        {
            self.font_name = first.clone();
            self.font_runs = None;
            return;
        }
        let mut runs: Vec<FontRun> = vec![];
        for (i, font) in fonts.iter().enumerate() {
            if font == &self.font_name {
                continue;
            }
            if let Some(last) = runs.last_mut()
                && last.location + last.length == i
                && last.font_name == *font
            {
                last.length += 1;
                continue;
            }
            runs.push(FontRun {
                location: i,
                length: 1,
                font_name: font.clone(),
            });
        }
        self.font_runs = (!runs.is_empty()).then_some(runs);
    }
}
pub fn utf16_to_byte(text: &str, index: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units == index {
            return Some(byte);
        }
        units += ch.len_utf16();
    }
    if units == index {
        Some(text.len())
    } else {
        None
    }
}

struct TextEngine {
    font_system: FontSystem,
    cache: SwashCache,
}
#[cfg(windows)]
#[path = "text_windows_fonts.rs"]
mod windows_fonts;

impl TextEngine {
    fn new() -> Self {
        let font_system = FontSystem::new();
        #[cfg(windows)]
        let font_system = {
            let (locale, mut database) = font_system.into_locale_and_db();
            windows_fonts::load(&mut database);
            FontSystem::new_with_locale_and_db(locale, database)
        };
        Self {
            font_system,
            cache: SwashCache::new(),
        }
    }
}

thread_local! {static ENGINE:RefCell<TextEngine>=RefCell::new(TextEngine::new());}

fn selected_face<'a>(
    database: &'a cosmic_text::fontdb::Database,
    name: &str,
) -> Option<&'a cosmic_text::fontdb::FaceInfo> {
    database
        .faces()
        .filter(|face| {
            face.post_script_name.eq_ignore_ascii_case(name)
                || face
                    .families
                    .iter()
                    .any(|(family, _)| family.eq_ignore_ascii_case(name))
        })
        .min_by_key(|face| {
            (
                !face.post_script_name.eq_ignore_ascii_case(name),
                face.style != Default::default(),
                face.weight.0.abs_diff(400),
            )
        })
}

pub fn rasterize(style: &TextStyle) -> Result<Raster> {
    rasterize_anchored(style).map(|(raster, _)| raster)
}

pub fn rasterize_anchored(style: &TextStyle) -> Result<(Raster, [f64; 2])> {
    style.validate()?;
    ENGINE.with(|engine| {
        let mut engine = engine.borrow_mut();
        let TextEngine { font_system, cache } = &mut *engine;
        let leading = if style.leading > 0. {
            style.leading
        } else {
            style.font_size * 1.2
        };
        let mut buffer = Buffer::new(
            font_system,
            Metrics::new(style.font_size as f32, leading as f32),
        );
        let face_for = |name: &str| {
            selected_face(font_system.db(), name)
                .and_then(|face| {
                    face.families
                        .first()
                        .map(|family| (family.0.clone(), face.weight, face.stretch, face.style))
                })
                .unwrap_or_else(|| {
                    (
                        name.to_owned(),
                        Default::default(),
                        Default::default(),
                        Default::default(),
                    )
                })
        };
        let base_face = face_for(&style.font_name);
        let default = Attrs::new()
            .family(Family::Name(&base_face.0))
            .weight(base_face.1)
            .stretch(base_face.2)
            .style(base_face.3)
            .letter_spacing((style.tracking / style.font_size) as f32);
        let mut units = 0;
        let mut pieces = Vec::new();
        for (byte, ch) in style.content.char_indices() {
            let color = style.color_at(units);
            let face = face_for(style.font_at(units));
            pieces.push((byte..byte + ch.len_utf8(), face, color));
            units += ch.len_utf16();
        }
        let spans = pieces.iter().map(|(range, font, color)| {
            (
                &style.content[range.clone()],
                Attrs::new()
                    .family(Family::Name(&font.0))
                    .weight(font.1)
                    .stretch(font.2)
                    .style(font.3)
                    .letter_spacing((style.tracking / style.font_size) as f32)
                    .color(Color::rgb(
                        (color[0] * 255.).round() as u8,
                        (color[1] * 255.).round() as u8,
                        (color[2] * 255.).round() as u8,
                    )),
            )
        });
        let align = Some(match style.alignment.as_str() {
            "Center" => Align::Center,
            "Right" => Align::Right,
            _ => Align::Left,
        });
        buffer.set_size(
            font_system,
            style.box_size.map(|b| (b[0] - 24.).max(1.) as f32),
            style.box_size.map(|b| (b[1] - 24.).max(1.) as f32),
        );
        buffer.set_wrap(
            font_system,
            if style.box_size.is_some() {
                Wrap::WordOrGlyph
            } else {
                Wrap::None
            },
        );
        buffer.set_rich_text(font_system, spans, &default, Shaping::Advanced, align);
        buffer.shape_until_scroll(font_system, false);
        let width = style
            .box_size
            .map_or_else(
                || buffer.layout_runs().map(|r| r.line_w).fold(1., f32::max) as f64 + 24.,
                |b| b[0],
            )
            .ceil() as u32;
        let height = style
            .box_size
            .map_or_else(
                || {
                    buffer
                        .layout_runs()
                        .map(|r| r.line_top + r.line_height)
                        .fold(leading as f32, f32::max) as f64
                        + 24.
                },
                |b| b[1],
            )
            .ceil() as u32;
        ensure!(
            width > 0
                && height > 0
                && width <= 300_000
                && height <= 300_000
                && (width as u64 * height as u64) <= MAX_PIXELS,
            "文字超出图像大小上限"
        );
        let mut pixels = vec![0; width as usize * height as usize * 4];
        let base_color = Color::rgb(
            (style.red * 255.).round() as u8,
            (style.green * 255.).round() as u8,
            (style.blue * 255.).round() as u8,
        );
        buffer.draw(font_system, cache, base_color, |x, y, w, h, color| {
            let [r, g, b, a] = color.as_rgba();
            for yy in 0..h {
                for xx in 0..w {
                    let px = x + 12 + xx as i32;
                    let py = y + 12 + yy as i32;
                    if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                        continue;
                    }
                    let i = ((py as u32 * width + px as u32) * 4) as usize;
                    let base = pixels[i..i + 4].try_into().unwrap();
                    pixels[i..i + 4].copy_from_slice(&crate::brush::over(base, [r, g, b, a], 1.));
                }
            }
        });
        let anchor = if style.box_size.is_some() {
            [12., 12.]
        } else {
            [
                match style.alignment.as_str() {
                    "Center" => width as f64 / 2.,
                    "Right" => width as f64 - 12.,
                    _ => 12.,
                },
                12. + buffer
                    .layout_runs()
                    .next()
                    .map_or(leading, |r| r.line_y as f64),
            ]
        };
        Ok((Raster::from_rgba(width, height, pixels), anchor))
    })
}

pub fn render_plain(content: &str, font: &str, size: f64, color: [u8; 4]) -> Result<Raster> {
    rasterize(&TextStyle {
        content: content.into(),
        font_name: font.into(),
        font_size: size,
        red: color[0] as f64 / 255.,
        green: color[1] as f64 / 255.,
        blue: color[2] as f64 / 255.,
        ..Default::default()
    })
}
pub fn add_text(doc: &mut Document, style: TextStyle, origin: [f64; 2]) -> Result<Uuid> {
    ensure!(!style.content.trim().is_empty(), "空文字不会创建图层");
    let raster = rasterize(&style)?;
    let id = crate::edit::add_layer(
        doc,
        style
            .content
            .lines()
            .next()
            .unwrap_or("Text")
            .chars()
            .take(50)
            .collect::<String>(),
    );
    let l = doc.layer_mut(id).unwrap();
    l.transform = Transform::new(
        origin[0],
        origin[1],
        raster.width as f64,
        raster.height as f64,
    );
    l.image = Some(Arc::new(raster));
    l.text = Some(style.to_value());
    Ok(id)
}
pub fn update_text(doc: &mut Document, id: Uuid, style: TextStyle) -> Result<()> {
    let raster = rasterize(&style)?;
    let l = doc
        .layer_mut(id)
        .ok_or_else(|| anyhow::anyhow!("文字图层已被删除"))?;
    let old = l
        .image
        .as_ref()
        .map_or(l.transform.size, |r| [r.width as f64, r.height as f64]);
    l.transform.size = [
        l.transform.size[0] * raster.width as f64 / old[0],
        l.transform.size[1] * raster.height as f64 / old[1],
    ];
    l.image = Some(Arc::new(raster));
    l.text = Some(style.to_value());
    l.shape = None;
    Ok(())
}
pub fn font_names() -> Vec<String> {
    ENGINE.with(|e| {
        let engine = e.borrow();
        let mut names = engine
            .font_system
            .db()
            .faces()
            .flat_map(|face| {
                face.families.iter().map(|(name, _)| name.clone()).chain(
                    (!face.post_script_name.is_empty()).then(|| face.post_script_name.clone()),
                )
            })
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    })
}

/// The installed face bytes and collection index used by the canvas text editor.
pub fn font_data(name: &str) -> Option<(Vec<u8>, u32)> {
    ENGINE.with(|engine| {
        let engine = engine.borrow();
        let database = engine.font_system.db();
        let face = selected_face(database, name)?;
        database.with_face_data(face.id, |bytes, index| {
            cosmic_text::ttf_parser::Face::parse(bytes, index)
                .ok()
                .map(|_| (bytes.to_vec(), index))
        })?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn with_font_database<T>(
        database: cosmic_text::fontdb::Database,
        test: impl FnOnce() -> T,
    ) -> T {
        struct Restore(Option<TextEngine>);
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Some(original) = self.0.take() {
                    ENGINE.with(|engine| {
                        engine.replace(original);
                    });
                }
            }
        }
        let _restore = Restore(Some(ENGINE.with(|engine| {
            engine.replace(TextEngine {
                font_system: FontSystem::new_with_locale_and_db("zh-CN".into(), database),
                cache: SwashCache::new(),
            })
        })));
        test()
    }

    #[cfg(windows)]
    #[test]
    fn chinese_font_file_and_directory_names_load_identical_ttf_and_ttc_faces() {
        let directory = tempfile::tempdir().unwrap();
        let chinese_directory = directory.path().join("中文 字体目录");
        std::fs::create_dir(&chinese_directory).unwrap();
        for (family, extension) in [("Arial", "ttf"), ("Microsoft YaHei", "ttc")] {
            let original = font_data(family).unwrap();
            let english = directory.path().join(format!("test-font.{extension}"));
            let chinese = chinese_directory.join(format!("中文 字体 💡.{extension}"));
            std::fs::write(&english, &original.0).unwrap();
            std::fs::write(&chinese, &original.0).unwrap();
            let mut results = Vec::new();
            for path in [&english, &chinese] {
                let mut database = cosmic_text::fontdb::Database::new();
                database.load_font_file(path).unwrap();
                assert!(database.faces().next().is_some());
                let rendered = with_font_database(database, || {
                    assert!(font_data(family).as_ref() == Some(&original));
                    render_plain("中文测试 ABC123", family, 32., [20, 50, 90, 255]).unwrap()
                });
                results.push(rendered);
            }
            assert!(results[0] == results[1], "{extension}");
            assert!(
                results[1]
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|p| p[3] > 0)
            );
        }
    }

    #[test]
    fn utf16_offsets_handle_emoji_and_style_inheritance() {
        let mut s = TextStyle {
            content: "A😀BC".into(),
            ..Default::default()
        };
        s.set_color([1., 0., 0.], 1..3).unwrap();
        assert_eq!(s.color_at(2), [1., 0., 0.]);
        s.replace_utf16(3..4, "XY").unwrap();
        assert_eq!(s.content, "A😀XYC");
        assert_eq!(s.color_at(3), [1., 0., 0.]);
        assert_eq!(s.color_at(4), [1., 0., 0.]);
        assert_eq!(utf16_to_byte("A😀", 2), None);
        assert_eq!(utf16_to_byte("A😀", 3), Some(5));
    }
    #[test]
    fn invalid_runs_are_rejected() {
        let s = TextStyle {
            content: "abc".into(),
            color_runs: Some(vec![ColorRun {
                location: 2,
                length: 4,
                red: 1.,
                green: 0.,
                blue: 0.,
            }]),
            ..Default::default()
        };
        assert!(s.validate().is_err());
    }
    #[test]
    fn real_fonts_render_glyphs_on_transparency() {
        let r = render_plain("Hello 中文", "Arial", 24., [255, 0, 0, 255]).unwrap();
        assert_eq!(r.get(0, 0)[3], 0);
        let data = r.to_rgba();
        assert!(
            data.as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[3] > 0 && p[0] > 0 && p[1] == 0)
        );
    }
    #[test]
    fn paragraph_wrap_and_character_color_are_real() {
        let mut s = TextStyle {
            content: "ABC ABC ABC".into(),
            font_size: 24.,
            box_size: Some([100., 120.]),
            ..Default::default()
        };
        s.set_color([1., 0., 0.], 0..3).unwrap();
        let r = rasterize(&s).unwrap();
        assert_eq!([r.width, r.height], [100, 120]);
        let p = r.to_rgba();
        assert!(p.as_chunks::<4>().0.iter().any(|p| p[3] > 0 && p[0] > 200));
        assert!(p.as_chunks::<4>().0.iter().any(|p| p[3] > 0 && p[0] == 0));
        assert!(
            p[(r.width * 50 * 4) as usize..]
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[3] > 0)
        );
    }

    #[cfg(windows)]
    #[test]
    fn postscript_bold_and_italic_faces_produce_different_glyph_pixels() {
        let names = font_names();
        for name in ["ArialMT", "Arial-BoldMT", "Arial-ItalicMT"] {
            assert!(
                names.iter().any(|font| font == name),
                "Installed face {name} is selectable"
            );
        }
        let mut style = TextStyle {
            content: "Hamburgefonts".into(),
            font_name: "ArialMT".into(),
            font_size: 48.,
            box_size: Some([500., 120.]),
            ..Default::default()
        };
        let regular = rasterize(&style).unwrap();
        let (bytes, index) = font_data("Arial-BoldMT").expect("Windows Arial Bold is installed");
        let face = cosmic_text::ttf_parser::Face::parse(&bytes, index).unwrap();
        assert!(face.is_bold());
        style.font_name = "Arial-BoldMT".into();
        let bold = rasterize(&style).unwrap();
        assert_ne!(regular.pixels, bold.pixels);
        let alpha_sum = |raster: &Raster| {
            raster
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| u64::from(pixel[3]))
                .sum::<u64>()
        };
        assert!(alpha_sum(&bold) > alpha_sum(&regular));
        style.font_name = "Arial-ItalicMT".into();
        let italic = rasterize(&style).unwrap();
        assert_ne!(regular.pixels, italic.pixels);
        assert_ne!(bold.pixels, italic.pixels);
    }

    #[cfg(windows)]
    #[test]
    fn localized_chinese_font_names_keep_real_faces_and_chinese_glyphs() {
        let names = font_names();
        let pairs = [
            ("微软雅黑", "Microsoft YaHei"),
            ("宋体", "SimSun"),
            ("黑体", "SimHei"),
            ("楷体", "KaiTi"),
            ("仿宋", "FangSong"),
            ("等线", "DengXian"),
            ("华文行楷", "STXingkai"),
        ];
        let mut tested = 0;
        for (localized, canonical) in pairs {
            if !names.iter().any(|name| name == localized)
                || !names.iter().any(|name| name == canonical)
            {
                continue;
            }
            tested += 1;
            let localized_data = font_data(localized).unwrap();
            let canonical_data = font_data(canonical).unwrap();
            assert!(localized_data == canonical_data, "{localized}");
            let face =
                cosmic_text::ttf_parser::Face::parse(&localized_data.0, localized_data.1).unwrap();
            for ch in "中文字体图层测试".chars() {
                assert!(face.glyph_index(ch).is_some(), "{localized} lacks {ch}");
            }
            let style = TextStyle {
                content: "中文字体，图层测试。ABC123".into(),
                font_name: canonical.into(),
                font_size: 40.,
                box_size: Some([580., 120.]),
                ..Default::default()
            };
            let expected = rasterize(&style).unwrap();
            let actual = rasterize(&TextStyle {
                font_name: localized.into(),
                ..style
            })
            .unwrap();
            assert!(actual == expected, "{localized} and {canonical}");
            assert!(actual.pixels.as_chunks::<4>().0.iter().any(|p| p[3] > 0));
        }
        assert!(
            tested > 0,
            "At least one installed Chinese font is required"
        );
    }

    #[cfg(windows)]
    #[test]
    fn rasterizer_uses_the_same_case_insensitive_face_as_inline_editor() {
        let canonical = "Microsoft YaHei";
        let requested = "microsoft yahei";
        assert!(font_data(requested) == font_data(canonical));
        let style = TextStyle {
            content: "中文字体，图层测试。ABC123".into(),
            font_name: canonical.into(),
            font_size: 40.,
            box_size: Some([580., 120.]),
            ..Default::default()
        };
        let expected = rasterize(&style).unwrap();
        let actual = rasterize(&TextStyle {
            font_name: requested.into(),
            ..style
        })
        .unwrap();
        assert!(
            actual == expected,
            "Canvas and inline face resolution must agree"
        );
    }
}
