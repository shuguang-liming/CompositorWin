//! Validate real source files with the editor's IO and full-resolution CPU renderer.
use anyhow::{Context, Result, ensure};
use compositor::{image_io, project, render};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Instant};

fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let output = PathBuf::from(
        arguments
            .next()
            .context("Usage: io_samples OUTPUT_DIRECTORY IMAGE...")?,
    );
    std::fs::create_dir_all(&output)?;
    let mut records = Vec::new();
    for argument in arguments {
        let path = PathBuf::from(argument);
        let name = path
            .file_name()
            .context("Source has no filename")?
            .to_string_lossy();
        let started = Instant::now();
        let imported = image_io::import(&path)?;
        let import_seconds = started.elapsed().as_secs_f64();
        let package = output.join(format!("{name}.comp"));
        ensure!(
            package != path,
            "Validation destination must differ from the source"
        );
        let started = Instant::now();
        project::save(&imported.document, &package)?;
        let reopened = project::load(&package)?;
        ensure!(
            reopened == imported.document,
            "Project roundtrip changed document data for {name}"
        );
        let roundtrip_seconds = started.elapsed().as_secs_f64();
        let composite = render::render_document(&reopened);
        let png = output.join(format!("{name}.png"));
        image_io::export_png(&composite, &png, reopened.resolution)?;
        let decoded = image_io::decode_image(&png)?;
        ensure!(decoded == composite, "PNG export changed pixels for {name}");
        records.push(json!({
            "source":path,"inputSha256":format!("{:x}",Sha256::digest(std::fs::read(&path)?)),
            "width":reopened.width,"height":reopened.height,"layers":reopened.layers.len(),
            "importSeconds":import_seconds,"saveAndReopenSeconds":roundtrip_seconds,
            "conversions":imported.conversions,"package":package,"png":png,
            "pngPixelsExact":true,"projectRoundtripExact":true
        }));
    }
    ensure!(!records.is_empty(), "Supply at least one source image");
    let report = serde_json::to_string_pretty(&records)?;
    std::fs::write(output.join("io-validation.json"), &report)?;
    println!("{report}");
    Ok(())
}
