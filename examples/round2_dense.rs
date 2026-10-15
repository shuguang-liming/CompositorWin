//! Usage: round2_dense PHOTO [WIDTH HEIGHT]. Setup/resizing is excluded from preview timings.
//! Sibling original.jpg and cat.jpg provide additional real-photo layers.
use compositor::model::{Document, Layer, Raster, TILE_SIZE};
use compositor::preview::PreviewCache;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{hint::black_box, path::Path, sync::Arc, time::Instant};

fn hash(image: &Raster) -> String {
    // Hash the same top-down straight-alpha RGBA bytes without a second full-size image.
    let mut digest = Sha256::new();
    if image.tiles.is_empty()
        && image.pixels.len() == image.width as usize * image.height as usize * 4
    {
        digest.update(image.pixels.as_slice());
    } else {
        let mut row = vec![0; image.width as usize * 4];
        for y in 0..image.height {
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                *pixel = image.get(x as u32, y);
            }
            digest.update(&row);
        }
    }
    format!("{:x}", digest.finalize())
}

fn make_layer(
    path: &Path,
    width: u32,
    height: u32,
    index: usize,
) -> anyhow::Result<(Layer, Value)> {
    let setup_start = Instant::now();
    let photo = compositor::image_io::decode_image(path)?;
    let input_hash = hash(&photo);
    let original_size = [photo.width, photo.height];
    let encoded_hash = format!("{:x}", Sha256::digest(std::fs::read(path)?));
    let original = image::RgbaImage::from_raw(photo.width, photo.height, photo.to_rgba()).unwrap();
    drop(photo);
    let resized = image::imageops::resize(
        &original,
        width,
        height,
        image::imageops::FilterType::Lanczos3,
    );
    drop(original);
    let source = Raster::from_rgba(width, height, resized.into_raw());
    let source_hash = hash(&source);
    let mut layer = Layer::raster(format!("Real photograph {}", index + 1), source);
    // Three full-canvas sources contribute visibly to the composite.
    layer.opacity = if index == 0 { 1.0 } else { 0.55 };
    let metadata = json!({
        "name": layer.name,
        "path": path,
        "encodedSha256": encoded_hash,
        "decodedRgbaSha256": input_hash,
        "originalPhotoWidth": original_size[0],
        "originalPhotoHeight": original_size[1],
        "sourceSha256": source_hash,
        "opacity": layer.opacity,
        "sourcePreparation": "Decode actual photograph to straight-alpha sRGB RGBA, Lanczos3 resize to full document size; small inputs are enlarged, not native-resolution captures",
        "setupMillisecondsExcludedFromPreviewTimings": setup_start.elapsed().as_secs_f64() * 1000.,
    });
    Ok((layer, metadata))
}

fn case(paths: &[&Path], width: u32, height: u32, kind: &str) -> anyhow::Result<Value> {
    anyhow::ensure!(Document::valid_size(width, height), "Invalid size");
    let dense_bytes = u64::from(width) * u64::from(height) * 4 * paths.len() as u64;
    // Leave room for decode/resize intermediates and preview mipmaps within 1.5 GiB.
    anyhow::ensure!(
        dense_bytes <= 400 * 1024 * 1024,
        "Dense sources exceed benchmark memory budget"
    );
    let setup_start = Instant::now();
    let mut doc = Document::new(width, height);
    doc.layers.clear();
    let mut layers = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let (layer, metadata) = make_layer(path, width, height, index)?;
        doc.layers.push(layer);
        layers.push(metadata);
    }
    doc.active_layer_id = Some(doc.layers.last().unwrap().id);
    let setup_ms = setup_start.elapsed().as_secs_f64() * 1000.;
    let history = doc.clone();
    let mut cache = PreviewCache::default();
    let start = Instant::now();
    let prepared = cache.prepare(&doc, 1600);
    let prepare_ms = start.elapsed().as_secs_f64() * 1000.;
    let prepared_hashes: Vec<_> = prepared
        .layers
        .iter()
        .map(|layer| hash(layer.image.as_ref().unwrap()))
        .collect();
    drop(prepared);
    let start = Instant::now();
    let first = cache.render(&doc, 1600);
    let compose_ms = start.elapsed().as_secs_f64() * 1000.;
    let first_hash = hash(&first);
    let start = Instant::now();
    let cached = cache.render(&doc, 1600);
    let cached_ms = start.elapsed().as_secs_f64() * 1000.;
    anyhow::ensure!(
        Arc::ptr_eq(&first, &cached),
        "Preview cache missed unchanged data"
    );
    let active_index = doc.layers.len() - 1;
    let start = Instant::now();
    let edited = Arc::make_mut(doc.active_mut().unwrap().image.as_mut().unwrap());
    let cx = (width / 2 / TILE_SIZE) * TILE_SIZE;
    let cy = (height / 2 / TILE_SIZE) * TILE_SIZE;
    for y in cy.saturating_sub(8)..(cy + 9).min(height) {
        for x in cx.saturating_sub(8)..(cx + 9).min(width) {
            edited.set(x, y, [229, 41, 101, 193]);
        }
    }
    let edit_ms = start.elapsed().as_secs_f64() * 1000.;
    let changed_tiles = edited.tiles.len();
    anyhow::ensure!(
        Arc::ptr_eq(
            &edited.pixels,
            &history.active().unwrap().image.as_ref().unwrap().pixels
        ),
        "Editing copied the immutable dense base"
    );
    let start = Instant::now();
    let after_edit = cache.render(&doc, 1600);
    let edited_ms = start.elapsed().as_secs_f64() * 1000.;
    black_box(&after_edit);
    for (index, layer) in history.layers.iter().enumerate() {
        anyhow::ensure!(
            hash(layer.image.as_ref().unwrap()) == layers[index]["sourceSha256"].as_str().unwrap(),
            "History layer {index} changed"
        );
        if index != active_index {
            anyhow::ensure!(
                Arc::ptr_eq(
                    layer.image.as_ref().unwrap(),
                    doc.layers[index].image.as_ref().unwrap()
                ),
                "Untouched layer {index} was copied"
            );
        }
    }
    Ok(json!({
        "name": format!("{kind}-{width}x{height}"),
        "width": width,
        "height": height,
        "layerCount": paths.len(),
        "layers": layers,
        "previewMaxSide": 1600,
        "sourcePreparation": "Lanczos3 resize of supplied real photographs; not native-resolution captures",
        "setupMillisecondsExcludedFromPreviewTimings": setup_ms,
        "sourceDenseRgbaBytes": dense_bytes,
        "sourceSha256": layers[active_index]["sourceSha256"],
        "originalPhotoWidth": layers[active_index]["originalPhotoWidth"],
        "originalPhotoHeight": layers[active_index]["originalPhotoHeight"],
        "processMemoryTargetBytes": 1_610_612_736u64,
        "memoryAccounting": "Sources built one layer at a time, decoded original and resize temporaries dropped before next layer; history shares source Arcs; SHA256 uses at most one scanline. Process peak measurement includes setup, decoding, Lanczos3 resize, input hash, preview preparation, composition and validation. Dense source bytes are analytical, not measured process memory.",
        "preparedLayerSha256": prepared_hashes,
        "preparedSha256": prepared_hashes[active_index],
        "prepareMilliseconds": prepare_ms,
        "composeMillisecondsAfterPrepare": compose_ms,
        "unchangedMilliseconds": cached_ms,
        "editMilliseconds": edit_ms,
        "editedPreviewMilliseconds": edited_ms,
        "editedLayerIndex": active_index,
        "editedTiles": changed_tiles,
        "outputSha256": first_hash,
        "editedOutputSha256": hash(&after_edit),
        "snapshotPreserved": true,
        "untouchedLayerArcsPreserved": true,
        "editKeptSharedDenseBase": true,
    }))
}

pub fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    run(&args)
}

pub fn run(args: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.len() == 2 || args.len() == 4,
        "Usage: round2_dense PHOTO [WIDTH HEIGHT]; sibling original.jpg and cat.jpg are required"
    );
    let primary = Path::new(&args[1]);
    let fixtures = primary.parent().unwrap_or_else(|| Path::new("."));
    let original = fixtures.join("original.jpg");
    let cat = fixtures.join("cat.jpg");
    anyhow::ensure!(
        original.is_file() && cat.is_file(),
        "Missing additional real-photo fixtures"
    );
    let sizes = if args.len() == 4 {
        vec![(args[2].parse::<u32>()?, args[3].parse::<u32>()?)]
    } else {
        vec![(6000, 4000), (4097, 3073)]
    };
    let mut cases = Vec::new();
    for (width, height) in sizes {
        cases.push(case(&[primary], width, height, "single-photo")?);
        cases.push(case(
            &[primary, &original, &cat],
            width,
            height,
            "three-real-photos",
        )?);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "photo": args[1],
            "inputSha256": cases[0]["layers"][0]["decodedRgbaSha256"],
            "cases": cases
        }))?
    );
    Ok(())
}
