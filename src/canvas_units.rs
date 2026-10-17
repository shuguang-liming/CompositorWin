//! New-canvas units and backgrounds, ported from Compositor 1.5.3
//! NewCanvasSheet.swift (Copyright 2026 Wonder Assembly LLC, MIT).

use crate::model::{Document, MAX_SIDE, Raster};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CanvasUnit {
    #[default]
    #[serde(rename = "px")]
    Pixels,
    #[serde(rename = "in")]
    Inches,
    #[serde(rename = "cm")]
    Centimeters,
    #[serde(rename = "mm")]
    Millimeters,
}

impl CanvasUnit {
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Self::Pixels => "px",
            Self::Inches => "in",
            Self::Centimeters => "cm",
            Self::Millimeters => "mm",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pixels => "像素",
            Self::Inches => "英寸",
            Self::Centimeters => "厘米",
            Self::Millimeters => "毫米",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Pixels => Self::Inches,
            Self::Inches => Self::Centimeters,
            Self::Centimeters => Self::Millimeters,
            Self::Millimeters => Self::Pixels,
        }
    }

    fn per_inch(self) -> Option<f64> {
        match self {
            Self::Pixels => None,
            Self::Inches => Some(1.),
            Self::Centimeters => Some(2.54),
            Self::Millimeters => Some(25.4),
        }
    }

    pub(crate) fn pixels(self, text: &str, resolution: f64) -> Option<u32> {
        let value: f64 = text.trim().replace(',', ".").parse().ok()?;
        if !value.is_finite() || value <= 0. {
            return None;
        }
        let pixels = if let Some(per_inch) = self.per_inch() {
            if !valid_resolution(resolution) {
                return None;
            }
            (value / per_inch * resolution).round()
        } else {
            value
        };
        (pixels == pixels.round() && (1. ..=MAX_SIDE as f64).contains(&pixels))
            .then_some(pixels as u32)
    }

    pub(crate) fn text(self, pixels: u32, resolution: f64) -> String {
        let Some(per_inch) = self.per_inch() else {
            return pixels.to_string();
        };
        if !valid_resolution(resolution) {
            return String::new();
        }
        let value = pixels as f64 / resolution * per_inch;
        // Use the original two-decimal presentation when it preserves the exact
        // canvas. More digits keep tiny/high-DPI sizes from becoming zero or
        // changing their pixel dimensions merely because the unit was switched.
        for precision in 2..=10 {
            let text = format!("{value:.precision$}");
            let text = text.trim_end_matches('0').trim_end_matches('.');
            if self.pixels(text, resolution) == Some(pixels) {
                return text.to_owned();
            }
        }
        format!("{value:.10}")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CanvasBackground {
    #[default]
    Transparent,
    White,
    Black,
}

impl CanvasBackground {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Transparent => "透明画布",
            Self::White => "白色画布",
            Self::Black => "黑色画布",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Transparent => Self::White,
            Self::White => Self::Black,
            Self::Black => Self::Transparent,
        }
    }

    fn color(self) -> Option<[u8; 4]> {
        match self {
            Self::Transparent => None,
            Self::White => Some([255; 4]),
            Self::Black => Some([0, 0, 0, 255]),
        }
    }
}

pub(crate) fn valid_resolution(resolution: f64) -> bool {
    resolution.is_finite() && (1. ..=9600.).contains(&resolution)
}

pub(crate) fn new_document(
    width: u32,
    height: u32,
    resolution: f64,
    background: CanvasBackground,
) -> Result<Document> {
    ensure!(Document::valid_size(width, height), "画布尺寸超出上限");
    ensure!(valid_resolution(resolution), "分辨率应为 1–9600 ppi");
    let mut document = Document::new(width, height);
    document.resolution = resolution;
    if let Some(color) = background.color() {
        let layer = document.active_mut().unwrap();
        layer.name = "背景".into();
        layer.image = Some(Arc::new(Raster::solid(width, height, color)));
    }
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_units_match_original_dpi_sizes_and_validation() {
        assert_eq!(CanvasUnit::Inches.pixels("8.5", 300.), Some(2550));
        assert_eq!(CanvasUnit::Inches.pixels("11", 300.), Some(3300));
        assert_eq!(CanvasUnit::Millimeters.pixels("210", 300.), Some(2480));
        assert_eq!(CanvasUnit::Centimeters.pixels(" 2,54 ", 72.), Some(72));
        assert_eq!(CanvasUnit::Pixels.pixels("1920", 300.), Some(1920));
        assert_eq!(CanvasUnit::Pixels.pixels("19.5", 72.), None);
        assert_eq!(CanvasUnit::Inches.pixels("200", 300.), None);
        for text in ["0", "-2", "NaN", "inf", "1e9999", "", "1,2,3"] {
            assert_eq!(CanvasUnit::Inches.pixels(text, 300.), None);
        }
        for resolution in [0., -1., f64::NAN, f64::INFINITY, 9601.] {
            assert_eq!(CanvasUnit::Inches.pixels("1", resolution), None);
        }
        assert_eq!(CanvasUnit::Pixels.pixels("30000", 72.), Some(30_000));
        assert_eq!(CanvasUnit::Pixels.pixels("30001", 72.), None);
    }

    #[test]
    fn unit_cycle_and_print_text_preserve_exact_pixel_dimensions() {
        assert_eq!(CanvasUnit::Inches.text(1920, 72.), "26.67");
        assert_eq!(CanvasUnit::Inches.text(2550, 300.), "8.5");
        assert_eq!(CanvasUnit::Millimeters.text(2480, 300.), "209.97");
        let mut unit = CanvasUnit::Pixels;
        for expected in [
            CanvasUnit::Inches,
            CanvasUnit::Centimeters,
            CanvasUnit::Millimeters,
            CanvasUnit::Pixels,
        ] {
            unit = unit.next();
            assert_eq!(unit, expected);
            for pixels in [1, 2, 17, 2550, 29_999, 30_000] {
                for resolution in [1., 72., 300., 9600.] {
                    assert_eq!(
                        unit.pixels(&unit.text(pixels, resolution), resolution),
                        Some(pixels)
                    );
                }
            }
        }
    }

    #[test]
    fn backgrounds_keep_native_size_and_resolution_without_large_base_allocation() {
        let mut background = CanvasBackground::Transparent;
        for expected in [
            CanvasBackground::White,
            CanvasBackground::Black,
            CanvasBackground::Transparent,
        ] {
            background = background.next();
            assert_eq!(background, expected);
        }
        for (background, color) in [
            (CanvasBackground::White, [255; 4]),
            (CanvasBackground::Black, [0, 0, 0, 255]),
        ] {
            let doc = new_document(40, 30, 300., background).unwrap();
            assert_eq!(doc.resolution, 300.);
            let layer = doc.active().unwrap();
            assert_eq!(layer.name, "背景");
            let image = layer.image.as_ref().unwrap();
            assert_eq!([image.width, image.height], [40, 30]);
            assert_eq!(image.get(0, 0), color);
            assert_eq!(image.get(39, 29), color);
            assert_eq!(image.pixels.len(), 4);
            assert!(image.tiles.is_empty());
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("print-background.comp");
            crate::project::save(&doc, &path).unwrap();
            let reopened = crate::project::load(&path).unwrap();
            assert_eq!(reopened.resolution, 300.);
            assert_eq!(
                reopened
                    .active()
                    .unwrap()
                    .image
                    .as_ref()
                    .unwrap()
                    .get(39, 29),
                color
            );
        }
        let doc = new_document(40, 30, 72., CanvasBackground::Transparent).unwrap();
        assert!(doc.active().unwrap().image.is_none());
    }

    #[test]
    fn new_canvas_rejects_invalid_sizes_and_ppi_before_creation() {
        for resolution in [0., 9600.1, f64::NAN, f64::INFINITY] {
            assert!(new_document(40, 30, resolution, CanvasBackground::White).is_err());
        }
        for (width, height) in [(0, 30), (40, 0), (30_001, 1), (30_000, 30_000)] {
            assert!(new_document(width, height, 72., CanvasBackground::Black).is_err());
        }
        assert!(new_document(1, 1, 1., CanvasBackground::Transparent).is_ok());
        assert!(new_document(1, 1, 9600., CanvasBackground::Transparent).is_ok());
    }
}
