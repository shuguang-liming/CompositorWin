//! Simplified Chinese presentation; persisted enum names and project keys remain unchanged.
use eframe::egui;
use std::{
    borrow::Cow,
    sync::{Arc, OnceLock},
};

/// Translate canonical option/action names only at their presentation boundary.
/// User text, font face names and file paths do not pass through this function.
pub fn text(value: &str) -> Cow<'_, str> {
    Cow::Borrowed(match value {
        "New" => "新建",
        "Open" => "打开",
        "Import" => "导入",
        "Scanlines" => "扫描线",
        "Progressive Blur" => "渐进模糊",
        "Repeat Last Filter" => "重复上次滤镜",
        "Search Commands" => "搜索命令",
        "Discard" => "不保存",
        "Apply" => "应用",
        "Cancel" => "取消",
        "Degenerate perspective quadrilateral" => "透视四边形退化，请调整控制点",
        "Invalid matte dimensions" => "蒙版尺寸无效",
        "Subject mask dimensions differ from source" => "主体蒙版尺寸与原图不同",
        "Layer does not exist" => "图层不存在",
        "Layer has no source image" => "图层没有原始图像",
        "Invalid source layer" => "原始图层无效",
        "Invalid layer transform" => "图层变换无效",
        "Rendering full-resolution export…" => "正在渲染完整分辨率的导出图像…",
        "Project saved" => "项目已保存",
        "The destination project was closed before import completed" => {
            "导入完成前，目标项目已关闭"
        }
        "Developing RAW image…" => "正在显影 RAW 图像…",
        "This document is already processing an operation" => "此文档正在处理另一项操作",
        "Alt-click to set a clone source" => "按住 Alt 单击以设置仿制源",
        "The project for this dialog was closed" => "此对话框对应的项目已关闭",
        "The layer for this dialog was deleted" => "此对话框对应的图层已被删除",
        "Document exceeds dimension or memory limits" => "文档超出尺寸或内存上限",
        "Apply or cancel the open dialog to finish closing" => {
            "请先应用或取消当前对话框，再完成关闭"
        }
        "The document changed during gradient processing. Draw a new gradient." => {
            "渐变处理期间文档发生变化，请重新绘制渐变。"
        }
        "The document changed during processing. Reapply the operation to your current edits." => {
            "处理期间文档发生变化，请在当前内容上重新应用操作。"
        }
        "Select a pixel layer" => "请选择像素图层",
        "Shape layer was deleted" => "形状图层已被删除",
        "The subject source was released" => "主体源图像已释放",
        "The preview layer was removed" => "预览图层已被删除",
        "New document" => "新建文档",
        "Canvas Size" => "画布大小",
        "Image Size" => "图像大小",
        "Grid Settings" => "网格设置",
        "Color Range" => "色彩范围",
        "Export JPEG" => "导出 JPEG",
        "Editable Text" => "编辑文字",
        "Camera Raw" => "Camera Raw 图像调整",
        "Develop Camera RAW" => "显影相机 RAW 图像",
        "Save" => "保存",
        "Save As" => "另存为",
        "Close" => "关闭",
        "Undo" => "撤销",
        "Redo" => "重做",
        "Copy" => "复制",
        "Cut" => "剪切",
        "Paste" => "粘贴",
        "Select All" => "全选",
        "Deselect" => "取消选择",
        "Swap colors" => "交换前景色和背景色",
        "Default colors" => "恢复默认颜色",
        "Cycle tool mode" => "切换工具模式",
        "Move tool" => "移动工具",
        "Marquee tool" => "选框工具",
        "Lasso tool" => "套索工具",
        "Magic tool" => "魔棒和对象选择",
        "Crop tool" => "裁剪工具",
        "Brush tool" => "画笔工具",
        "Eraser tool" => "橡皮擦工具",
        "Healing tool" => "修复工具",
        "Clone tool" => "仿制图章工具",
        "Blur tool" => "模糊工具",
        "Gradient tool" => "渐变工具",
        "Shape tool" => "形状工具",
        "Text tool" => "文字工具",
        "Eyedropper tool" => "吸管工具",
        "Hand tool" => "抓手工具",
        "Zoom tool" => "缩放工具",
        "Normal" => "正常",
        "Darken" => "变暗",
        "Multiply" => "正片叠底",
        "Color Burn" | "ColorBurn" => "颜色加深",
        "Linear Burn" | "LinearBurn" => "线性加深",
        "Lighten" => "变亮",
        "Screen" => "滤色",
        "Color Dodge" | "ColorDodge" => "颜色减淡",
        "Linear Dodge" | "LinearDodge" | "Linear Dodge (Add)" => "线性减淡（添加）",
        "Overlay" => "叠加",
        "Soft Light" | "SoftLight" => "柔光",
        "Hard Light" | "HardLight" => "强光",
        "Vivid Light" | "VividLight" => "亮光",
        "Linear Light" | "LinearLight" => "线性光",
        "Pin Light" | "PinLight" => "点光",
        "Hard Mix" | "HardMix" => "实色混合",
        "Difference" => "差值",
        "Exclusion" => "排除",
        "Subtract" => "减去",
        "Divide" => "划分",
        "Hue" | "hue" => "色相",
        "Saturation" | "saturation" => "饱和度",
        "Color" => "颜色",
        "Luminosity" | "Luminance" | "luminance" => "明度",
        "Replace" => "新建选区",
        "Add" => "添加到选区",
        "Intersect" | "Intersection" => "与选区交叉",
        "Nearest" => "邻近（硬边缘）",
        "Smooth" => "平滑",
        "High" | "High quality" => "高质量",
        "Free" => "自由",
        "Original" => "原始",
        "Basic" => "基础",
        "Advanced" => "高级",
        "Top Left" => "左上角",
        "Bottom Right" => "右下角",
        "horizontal" => "水平",
        "vertical" => "垂直",
        "Corner Radius" | "Corner radius" => "圆角半径",
        "Line Width" | "Line width" => "线宽",
        "Basis" => "依据",
        "Tolerance" => "容差",
        "Top" => "顶部",
        "Bottom" => "底部",
        "Rectangle" => "矩形",
        "Ellipse" => "椭圆",
        "Line" => "直线",
        "Linear" => "线性",
        "Radial" => "径向",
        "Angle" => "角度",
        "Reflected" => "对称",
        "Diamond" => "菱形",
        "Content-Aware" => "内容识别",
        "Create Texture" => "创建纹理",
        "Proximity Match" => "近似匹配",
        "Gaussian Blur" => "高斯模糊",
        "Motion Blur" => "动感模糊",
        "Add Noise" => "添加杂色",
        "Vignette" => "暗角",
        "Bloom / Glow" => "泛光／辉光",
        "Dither" => "抖色",
        "Tonal Contrast" => "色调对比度",
        "Lens Correction" => "镜头校正",
        "Camera Raw Filter" => "Camera Raw 滤镜",
        "Curves" => "曲线",
        "Exposure" => "曝光度",
        "Gradient Map" => "渐变映射",
        "Grain" => "颗粒",
        "Black & White" => "黑白",
        "Color Balance" => "色彩平衡",
        "Hue/Saturation" => "色相／饱和度",
        "Levels" => "色阶",
        "Invert" => "反相",
        "stroke" | "Stroke" => "描边",
        "shadow" | "Shadow" | "Drop Shadow" => "投影",
        "colorOverlay" | "Color Overlay" | "Color overlay" => "颜色叠加",
        "innerShadow" | "Inner Shadow" | "Inner shadow" => "内阴影",
        "outerGlow" | "Outer Glow" | "Outer glow" => "外发光",
        "innerGlow" | "Inner Glow" | "Inner glow" => "内发光",
        "Red" | "red" => "红色",
        "Green" | "green" => "绿色",
        "Blue" | "blue" => "蓝色",
        "Master" => "全色域",
        "Reds" => "红色",
        "Yellows" => "黄色",
        "Greens" => "绿色",
        "Cyans" => "青色",
        "Blues" => "蓝色",
        "Magentas" => "洋红色",
        "Shadows" | "shadows" => "阴影",
        "Midtones" | "midtones" => "中间调",
        "Highlights" | "highlights" => "高光",
        "Global" | "global" => "全局",
        "Sample" => "取样",
        "White Balance" => "白平衡",
        "Defringe" => "去边",
        "Curve Parametric" => "参数曲线",
        "Curve RGB" => "RGB 曲线",
        "Curve Red" => "红色曲线",
        "Curve Green" => "绿色曲线",
        "Curve Blue" => "蓝色曲线",
        "Mixer Hue" => "混色器：色相",
        "Mixer Saturation" => "混色器：饱和度",
        "Mixer Luminance" => "混色器：明度",
        "Point Color" => "点颜色",
        "Upright Guide" => "Upright 参考线",
        "Custom" => "自定",
        "Auto" => "自动",
        "As Shot" | "As shot" => "原照设置",
        "Daylight" => "日光",
        "Cloudy" => "阴天",
        "Shade" => "阴影",
        "Tungsten" => "钨丝灯",
        "Fluorescent" => "荧光灯",
        "Flash" => "闪光灯",
        "Diffusion" => "柔光扩散",
        "Bloom" => "泛光",
        "Halation" => "光晕",
        "Highlight Priority" => "高光优先",
        "Color Priority" => "颜色优先",
        "Paint Overlay" => "油彩叠加",
        "Off" => "关闭",
        "Guided" => "引导式",
        "Perspective" => "透视",
        "Rectilinear" => "直线投影",
        "Version 1" => "版本 1",
        "Version 2" => "版本 2",
        "Version 3" => "版本 3",
        "Version 4" => "版本 4",
        "Version 5" => "版本 5",
        "Version 6" => "版本 6",
        "Left" => "左对齐",
        "Center" => "居中",
        "Right" => "右对齐",
        "Atkinson (Classic Mac)" => "Atkinson（经典 Mac）",
        "Floyd–Steinberg" => "Floyd–Steinberg 误差扩散",
        "Bayer 2 × 2" => "Bayer 2 × 2 有序抖色",
        "Bayer 4 × 4" => "Bayer 4 × 4 有序抖色",
        "Bayer 8 × 8" => "Bayer 8 × 8 有序抖色",
        "Halftone Dots" => "半调网点",
        "Halftone Lines" => "半调线条",
        "Halftone Diamonds" => "半调菱形",
        "Mac Patterns" => "Mac 图案",
        "ASCII" => "ASCII 字符画",
        "Scanlines (CRT)" => "扫描线（CRT）",
        "Two Colors" => "双色",
        "Square" => "方形",
        "Dot" => "圆点",
        "pixels" | "Pixels" | "px" => "像素",
        "inches" | "Inches" => "英寸",
        "centimeters" | "Centimeters" => "厘米",
        "millimeters" | "Millimeters" => "毫米",
        "percent" | "Percent" => "百分比",
        "Transparent" => "透明",
        "Top-left pixel" => "左上角像素",
        "Bottom-right pixel" => "右下角像素",
        "Brush stroke" => "画笔笔画",
        "Layer visibility" => "图层可见性",
        "Transform" => "变换",
        "Mask transform" => "蒙版变换",
        "Selection" => "选区",
        "Polygon selection" => "多边形选区",
        "Crop" => "裁剪",
        "Canvas size" => "画布大小",
        "Image size" => "图像大小",
        "Gradient" => "渐变",
        "Import layers" => "导入图层",
        "Copy layers from project" => "从项目复制图层",
        "Rename" => "重命名",
        "Opacity" => "不透明度",
        "Layer properties" => "图层属性",
        "Layer appearance" => "图层外观",
        "Duplicate" => "复制图层",
        "Delete" => "删除",
        "Group" => "组",
        "Ungroup" => "取消编组",
        "Merge Down" => "向下合并",
        "Merge Layers" => "合并图层",
        "Merge Selected" => "合并选中图层",
        "Merge Group" => "合并组",
        "Add Mask" => "添加蒙版",
        "Clipping Mask" => "剪贴蒙版",
        "Fill" => "填充",
        "Clear" => "清除",
        "Move selection" => "移动选区",
        "Flip selection" => "翻转选区",
        "Resize selection" => "调整选区大小",
        "Feather" => "羽化",
        "Expand" => "扩展",
        "Contract" => "收缩",
        "Trim" => "修边",
        "Flip canvas" => "翻转画布",
        "Guides" => "参考线",
        "Add guide" => "添加参考线",
        "Move guide" => "移动参考线",
        "Remove guide" => "删除参考线",
        "Clear guides" => "清除参考线",
        "Lock guides" => "锁定参考线",
        "Remove Background" => "移除背景",
        "Select Subject" => "选择主体",
        "Object Selection" => "对象选择",
        "Content-Aware Fill" => "内容识别填充",
        "Rasterize" => "栅格化",
        "Text" => "文字",
        "Shape" => "形状",
        "New layer" => "新建图层",
        "New group" => "新建组",
        "Move selection outline" => "移动选区轮廓",
        "Move mask" => "移动蒙版",
        "Move selected pixels" => "移动选中的像素",
        "Nudge" => "微移",
        "Shape appearance" => "形状外观",
        "Edit selected pixels" => "编辑选中的像素",
        "Content-aware fill" => "内容识别填充",
        "Add mask" => "添加蒙版",
        "Delete mask" => "删除蒙版",
        "Paste mask" => "粘贴蒙版",
        "Select masked area" => "选择蒙版遮挡区域",
        "Invert pixels" => "像素反相",
        "Invert mask" => "蒙版反相",
        "Apply mask" => "应用蒙版",
        "Clipping mask" => "剪贴蒙版",
        "Select all" => "全选",
        "Inverse selection" => "反选",
        "Load alpha" => "载入图层透明度",
        "Modify selection" => "修改选区",
        "Reorder layers" => "调整图层顺序",
        "Cut pixels" => "剪切像素",
        "Cut layers" => "剪切图层",
        "Paste layers" => "粘贴图层",
        "Paste pixels" => "粘贴像素",
        "Merge layers" => "合并图层",
        "Magic selection" => "魔棒选择",
        "Guide" => "参考线",
        "Free distort" => "自由变形",
        "New shape" => "新建形状",
        "Delete guide" => "删除参考线",
        "Color range" => "色彩范围",
        "Edit text" => "编辑文字",
        "New adjustment" => "新建调整图层",
        "Layer effect" => "图层效果",
        other => other,
    })
}

type UiFont = (&'static str, Arc<egui::FontData>);
static UI_FONT: OnceLock<Option<UiFont>> = OnceLock::new();

fn ui_font(bytes: Vec<u8>, index: u32) -> Option<Arc<egui::FontData>> {
    let face = cosmic_text::ttf_parser::Face::parse(&bytes, index).ok()?;
    if !"文件编辑图层蒙版中文"
        .chars()
        .all(|c| face.glyph_index(c).is_some())
    {
        return None;
    }
    let mut font = egui::FontData::from_owned(bytes);
    font.index = index;
    Some(Arc::new(font))
}

#[cfg(windows)]
fn windows_ui_font() -> Option<UiFont> {
    let fonts = std::path::PathBuf::from(
        std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into()),
    )
    .join("Fonts");
    // Load only the UI face; canvas text discovers the full installed font database on demand.
    [
        ("Microsoft YaHei", "msyh.ttc"),
        ("Microsoft JhengHei", "msjh.ttc"),
        ("SimSun", "simsun.ttc"),
    ]
    .into_iter()
    .find_map(|(name, file)| {
        let bytes = std::fs::read(fonts.join(file)).ok()?;
        ui_font(bytes, 0).map(|font| (name, font))
    })
}

fn chinese_font() -> Option<&'static UiFont> {
    UI_FONT
        .get_or_init(|| {
            #[cfg(windows)]
            if let Some(font) = windows_ui_font() {
                return Some(font);
            }
            [
                "Microsoft YaHei",
                "Microsoft JhengHei",
                "SimSun",
                "Noto Sans CJK SC",
                "Noto Sans SC",
                "Source Han Sans SC",
            ]
            .into_iter()
            .find_map(|name| {
                let (bytes, index) = crate::text::font_data(name)?;
                ui_font(bytes, index).map(|font| (name, font))
            })
        })
        .as_ref()
}

pub(crate) fn shared_ui_font(name: &str) -> Option<Arc<egui::FontData>> {
    UI_FONT
        .get()?
        .as_ref()
        .filter(|(installed, _)| *installed == name)
        .map(|(_, font)| font.clone())
}

/// Windows fonts are read at runtime and are never copied into the distribution.
pub fn install_fonts(ctx: &egui::Context) -> Option<&'static str> {
    let mut definitions = egui::FontDefinitions::default();
    let installed = chinese_font().map(|(name, font)| {
        let key = "compositor-ui-cjk".to_owned();
        definitions.font_data.insert(key.clone(), font.clone());
        definitions
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, key.clone());
        definitions
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push(key);
        *name
    });
    ctx.set_fonts(definitions);
    installed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localized_blend_display_preserves_project_enum_values() {
        use crate::model::BlendMode;
        for blend in BlendMode::ALL {
            let canonical = serde_json::to_value(blend).unwrap();
            let label = canonical.as_str().unwrap();
            assert!(
                text(label)
                    .chars()
                    .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
            );
            assert_eq!(
                serde_json::from_value::<BlendMode>(canonical).unwrap(),
                blend
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn direct_windows_ui_font_preserves_the_installed_regular_face_and_collection_index() {
        let (name, font) = windows_ui_font().expect("A standard Chinese Windows font is installed");
        let (bytes, index) = crate::text::font_data(name).unwrap();
        assert_eq!(font.index, index);
        assert_eq!(font.font.as_ref(), bytes.as_slice());
    }

    #[cfg(windows)]
    #[test]
    fn installed_ui_font_has_real_chinese_glyphs_in_both_default_families() {
        let ctx = egui::Context::default();
        assert!(
            install_fonts(&ctx).is_some(),
            "Install a supported Chinese Windows font to test the Chinese UI."
        );
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            ctx.fonts_mut(|fonts| {
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    let font = egui::FontId::new(16., family.clone());
                    assert!(fonts.has_glyphs(&font, "文件编辑图层选择滤镜视图蒙版中文取消保存"));
                    assert!(fonts.has_glyphs(&font, "😀 ❤ 🚀"));
                    assert!(!fonts.has_glyph(&font, '\u{10ffff}'));
                    if family == egui::FontFamily::Monospace {
                        assert_eq!(fonts.glyph_width(&font, 'i'), fonts.glyph_width(&font, 'W'));
                    }
                    let galley =
                        fonts.layout_no_wrap("中文图层".into(), font, egui::Color32::WHITE);
                    assert!(galley.size().x > 40. && galley.size().y > 8.);
                }
            });
        });
    }

    #[cfg(windows)]
    #[test]
    fn inline_text_font_updates_preserve_chinese_ui_glyphs_and_project_values() {
        let ctx = egui::Context::default();
        assert!(install_fonts(&ctx).is_some());
        let mut style = serde_json::to_value(crate::text::TextStyle {
            content: "中文示例".into(),
            alignment: "Center".into(),
            ..Default::default()
        })
        .unwrap();
        let before = style.clone();
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    crate::text_panel::show(ui, &mut style).unwrap();
                });
                ctx.fonts_mut(|fonts| {
                    assert!(fonts.has_glyphs(&egui::FontId::proportional(16.), "文件编辑中文图层"));
                });
            });
        }
        assert_eq!(style, before);
        assert_eq!(style["alignment"], "Center");
        let (name, shared) = chinese_font().unwrap();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            ctx.fonts(|fonts| {
                let definitions = fonts.definitions();
                assert!(Arc::ptr_eq(
                    &definitions.font_data["compositor-ui-cjk"],
                    shared
                ));
                assert!(Arc::ptr_eq(
                    &definitions.font_data[&format!("compositor:{name}")],
                    shared
                ));
            });
        });
    }
}
