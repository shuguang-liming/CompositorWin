//! Reproducible CPU/editor resource measurements. Run with `cargo run --release --example benchmark`.
use anyhow::Result;
use compositor::{
    brush, edit,
    model::{Document, Raster},
    preview, project, render,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Instant};

#[cfg(windows)]
fn memory() -> Value {
    #[repr(C)]
    struct Counters {
        cb: u32,
        page_faults: u32,
        values: [usize; 9],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut Counters,
            size: u32,
        ) -> i32;
    }
    let mut c = Counters {
        cb: std::mem::size_of::<Counters>() as u32,
        page_faults: 0,
        values: [0; 9],
    };
    // OS process accounting only; the application algorithms remain implemented in Rust.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    if ok != 0 {
        json!({"workingSetBytes":c.values[1],"peakWorkingSetBytes":c.values[0],"privateBytes":c.values[8]})
    } else {
        Value::Null
    }
}
#[cfg(not(windows))]
fn memory() -> Value {
    Value::Null
}

fn main() -> Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("build/verification/performance.json")
        });
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let initial = memory();
    let mut doc = Document::new(4096, 4096);
    let id = doc.active_layer_id.unwrap();
    let points = (0..1024)
        .map(|i| {
            let x = 128. + i as f64 * 3.75;
            [x, 2048. + (x / 250.).sin() * 600.]
        })
        .collect::<Vec<_>>();
    let before = doc.clone();
    let timer = Instant::now();
    let mut stroke = brush::BrushStroke::new(
        &doc,
        id,
        false,
        brush::BrushSettings {
            diameter: 24.,
            hardness: 0.6,
            color: [44, 150, 226, 255],
            ..Default::default()
        },
        None,
    )?;
    for &p in &points {
        stroke.append(p)?;
    }
    stroke.finish_at(*points.last().unwrap())?;
    let stroke_time = timer.elapsed().as_secs_f64() * 1000.;
    let stroke_tiles = stroke.raster.tiles.len();
    let stroke_bytes = stroke.raster.byte_len();
    stroke.commit(&mut doc)?;
    let after_stroke = memory();
    let original_unchanged = before.active().unwrap().image.is_none();
    let timer = Instant::now();
    let mut next = doc.clone();
    let id = next.active_layer_id.unwrap();
    brush::paint(
        &mut next,
        id,
        false,
        &[[3000., 3000.], [3030., 3010.]],
        brush::BrushSettings {
            diameter: 20.,
            color: [255, 120, 0, 255],
            ..Default::default()
        },
        None,
    )?;
    let edit_time = timer.elapsed().as_secs_f64() * 1000.;
    let r1 = doc.active().unwrap().image.as_ref().unwrap().clone();
    let r2 = next.active().unwrap().image.as_ref().unwrap().clone();
    let shared = r1
        .tiles
        .iter()
        .filter(|(key, value)| {
            r2.tiles
                .get(key)
                .is_some_and(|next| Arc::ptr_eq(value, next))
        })
        .count();
    for n in 1..16 {
        let id = edit::add_layer(&mut doc, format!("Benchmark {n}"));
        let point = [256. + n as f64 * 200., 256. + n as f64 * 190.];
        brush::paint(
            &mut doc,
            id,
            false,
            &[point, [point[0] + 64., point[1] + 32.]],
            brush::BrushSettings {
                diameter: 64.,
                opacity: 0.7,
                color: [n * 13, 128, 255 - n * 11, 255],
                ..Default::default()
            },
            None,
        )?;
    }
    let layer_bytes = doc
        .layers
        .iter()
        .filter_map(|l| l.image.as_ref())
        .map(|r| r.byte_len())
        .sum::<usize>();
    let timer = Instant::now();
    let preview = render::render_preview(&doc, 1024);
    let preview_time = timer.elapsed().as_secs_f64() * 1000.;
    std::hint::black_box(preview);
    let after_layers = memory();
    let mut effect_doc = doc.clone();
    effect_doc.layers.truncate(1);
    effect_doc.layers[0].effects = Some(
        json!({"shadow":{"distance":20,"blur":12,"opacity":0.7},"stroke":{"size":5,"red":1,"green":0.7,"blue":0}}),
    );
    let mut effect_cache = preview::PreviewCache::default();
    let timer = Instant::now();
    let first = effect_cache.render(&effect_doc, 1024);
    let first_effect_time = timer.elapsed().as_secs_f64() * 1000.;
    let timer = Instant::now();
    let repeated = effect_cache.render(&effect_doc, 1024);
    let repeated_effect_time = timer.elapsed().as_secs_f64() * 1000.;
    effect_doc.layers[0].opacity = 0.8;
    let timer = Instant::now();
    std::hint::black_box(effect_cache.render(&effect_doc, 1024));
    let opacity_effect_time = timer.elapsed().as_secs_f64() * 1000.;
    let effect_memory = memory();
    let mut small = Document::new(128, 128);
    small.active_mut().unwrap().image =
        Some(Arc::new(Raster::solid(128, 128, [40, 100, 200, 255])));
    let test_path = output
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("benchmark-open-close.comp");
    project::save(&small, &test_path)?;
    for _ in 0..4 {
        std::hint::black_box(project::load(&test_path)?);
    }
    let before_open = memory();
    let timer = Instant::now();
    for _ in 0..100 {
        let d = project::load(&test_path)?;
        std::hint::black_box(render::render_preview(&d, 128));
        drop(d);
    }
    let open_time = timer.elapsed().as_secs_f64() * 1000.;
    let after_open = memory();
    let result = json!({"hardware":{"cpu":std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_default(),"logicalCpus":std::thread::available_parallelism().map_or(1,|n|n.get()),"os":std::env::consts::OS,"arch":std::env::consts::ARCH},"buildMode":if cfg!(debug_assertions){"debug (opt-level=1)"}else{"release"},"initialMemory":initial,
        "continuousBrush":{"canvas":[4096,4096],"layerCount":1,"pointerSamples":points.len(),"diameter":24,"milliseconds":stroke_time,"allocatedTiles":stroke_tiles,"retainedRasterBytes":stroke_bytes,"fullRgbaBytes":4096usize*4096*4,"originalUnchanged":original_unchanged,"memory":after_stroke},
        "historyCowEdit":{"milliseconds":edit_time,"originalTiles":r1.tiles.len(),"sharedUntouchedTiles":shared,"baseShared":Arc::ptr_eq(&r1.pixels,&r2.pixels)},
        "multiLayerPreview":{"canvas":[4096,4096],"layerCount":16,"previewMaxSide":1024,"milliseconds":preview_time,"retainedRasterBytes":layer_bytes,"memory":after_layers},
        "cachedEffectsPreview":{"canvas":[4096,4096],"layerCount":1,"previewMaxSide":1024,"effects":["shadow","stroke"],"firstMilliseconds":first_effect_time,"unchangedMilliseconds":repeated_effect_time,"unchangedOutputShared":Arc::ptr_eq(&first,&repeated),"opacityEditMilliseconds":opacity_effect_time,"memory":effect_memory},
        "repeatedOpenClose":{"canvas":[128,128],"layerCount":1,"iterations":100,"warmup":4,"totalMilliseconds":open_time,"beforeMemory":before_open,"afterMemory":after_open},
        "limitations":"CPU preview and editor operations measured; GPU upload, pointer-event latency, PNG save concurrency and complex retouching need separate GUI profiling."});
    std::fs::write(&output, serde_json::to_vec_pretty(&result)?)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
