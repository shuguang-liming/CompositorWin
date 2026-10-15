//! Run with: cargo run --release --example segmentation_smoke -- INPUT OUTPUT_DIRECTORY
use anyhow::{Context, Result};
use compositor::{model::Raster, segmentation::SegmentationEngine};
use std::{path::PathBuf, time::Instant};

pub fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(2);
    let input = PathBuf::from(
        args.next()
            .context("Usage: segmentation_smoke INPUT OUTPUT_DIRECTORY")?,
    );
    let output = PathBuf::from(args.next().context("Output directory is required")?);
    std::fs::create_dir_all(&output)?;
    let photo = image::open(&input)?.to_rgba8();
    let source = Raster::from_rgba(photo.width(), photo.height(), photo.into_raw());
    let start = Instant::now();
    let engine = SegmentationEngine::load_default()?;
    println!("Model optimized in {:.3}s", start.elapsed().as_secs_f64());
    for run in 0..2 {
        let result = engine.predict(&source)?;
        let foreground = result.foreground(&source)?;
        image::save_buffer(
            output.join("mask.png"),
            &result.mask().to_rgba(),
            result.width,
            result.height,
            image::ColorType::Rgba8,
        )?;
        image::save_buffer(
            output.join("foreground.png"),
            &foreground.to_rgba(),
            foreground.width,
            foreground.height,
            image::ColorType::Rgba8,
        )?;
        let selected = result.alpha.iter().filter(|&&a| a >= 128).count();
        let soft = result.alpha.iter().filter(|&&a| a > 0 && a < 255).count();
        println!(
            "Run {run}: {}x{}, {}ms, foreground {:.2}%, soft alpha {:.2}%",
            result.width,
            result.height,
            result.inference_millis,
            selected as f64 * 100. / result.alpha.len() as f64,
            soft as f64 * 100. / result.alpha.len() as f64
        );
        anyhow::ensure!(
            selected > result.alpha.len() / 100 && selected < result.alpha.len() * 99 / 100,
            "Photo segmentation must contain foreground and background"
        );
    }
    Ok(())
}
