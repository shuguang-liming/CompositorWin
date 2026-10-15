//! Run this same source against both releases; timings exclude setup and output hashing.
use compositor::brush::{BrushSettings, BrushStroke};
use compositor::model::{Document, Raster, TILE_SIZE};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{hint::black_box, sync::Arc, time::Instant};

fn percentile(samples: &[f64], fraction: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() - 1) as f64 * fraction).round() as usize]
}

pub fn main() -> anyhow::Result<()> {
    let (width, height) = (2048, 1024);
    let pointers: Vec<_> = (0..1024)
        .map(|i| [100. + i as f64 * 1.75, 512. + (i as f64 / 32.).sin() * 180.])
        .collect();
    let mut cases = vec![];
    for dense in [false, true] {
        for diameter in [8., 48., 192.] {
            let mut doc = Document::new(width, height);
            if dense {
                doc.active_mut().unwrap().image = Some(Arc::new(Raster::from_rgba(
                    width,
                    height,
                    (0..width * height)
                        .flat_map(|i| [i as u8, (i / 7) as u8, (i / 13) as u8, (i / 17) as u8])
                        .collect(),
                )));
            }
            let snapshot = doc.clone();
            let snapshot_hash = snapshot
                .active()
                .unwrap()
                .image
                .as_ref()
                .map(|image| format!("{:x}", Sha256::digest(image.to_rgba())));
            let settings = BrushSettings {
                diameter,
                hardness: 0.35,
                opacity: 0.6,
                color: [197, 73, 139, 219],
                ..Default::default()
            };
            let mut stroke =
                BrushStroke::new(&doc, doc.active_layer_id.unwrap(), false, settings, None)?;
            let mut samples = Vec::with_capacity(pointers.len());
            for &point in &pointers {
                let start = Instant::now();
                black_box(stroke.append(point)?);
                samples.push(start.elapsed().as_secs_f64() * 1000.);
            }
            let start = Instant::now();
            stroke.finish_at(*pointers.last().unwrap())?;
            let result = stroke.commit(&mut doc)?;
            let finish_ms = start.elapsed().as_secs_f64() * 1000.;
            let image = doc.active().unwrap().image.as_ref().unwrap();
            let retained_hash = snapshot
                .active()
                .unwrap()
                .image
                .as_ref()
                .map(|image| format!("{:x}", Sha256::digest(image.to_rgba())));
            anyhow::ensure!(snapshot_hash == retained_hash, "History snapshot changed");
            cases.push(json!({
                "base": if dense {"dense"} else {"blank"},
                "diameter": diameter,
                "pointerCount": pointers.len(),
                "tileSize": TILE_SIZE,
                "allocatedTiles": image.tiles.len(),
                "retainedRasterBytes": image.byte_len(),
                "changedPixels": result.changed_pixels,
                "bounds": result.bounds,
                "appendMilliseconds": samples.iter().sum::<f64>(),
                "pointerP50Milliseconds": percentile(&samples, 0.5),
                "pointerP95Milliseconds": percentile(&samples, 0.95),
                "finishMilliseconds": finish_ms,
                "outputSha256": format!("{:x}", Sha256::digest(image.to_rgba())),
                "snapshotPreserved": true,
                "samplesMilliseconds": samples,
            }));
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"cases": cases}))?
    );
    Ok(())
}
