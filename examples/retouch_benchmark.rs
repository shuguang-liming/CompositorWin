//! Real document-grid retouching and tiled matte resource measurements.
use anyhow::Result;
use compositor::{
    brush::{BrushMode, BrushSettings, BrushStroke},
    matte,
    model::{Document, Raster},
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
    // Read-only process accounting; no pointer is retained by Windows.
    if unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } != 0 {
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
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("build/verification/retouch-performance.json")
        });
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let source = Arc::new(Raster::from_rgba(
        4096,
        4096,
        (0..4096u32 * 4096)
            .flat_map(|i| {
                let x = i % 4096;
                let y = i / 4096;
                [
                    (x / 16 % 256) as u8,
                    (y / 16 % 256) as u8,
                    ((x + y) / 32 % 256) as u8,
                    255,
                ]
            })
            .collect(),
    ));
    let points = (0..128)
        .map(|i| [512. + i as f64 * 16., 2048. + (i as f64 / 16.).sin() * 250.])
        .collect::<Vec<_>>();
    let mut trials = vec![];
    for mode in [BrushMode::Smudge, BrushMode::Liquify] {
        let mut doc = Document::new(4096, 4096);
        let id = doc.active_layer_id.unwrap();
        doc.active_mut().unwrap().image = Some(source.clone());
        let before = memory();
        let timer = Instant::now();
        let mut stroke = BrushStroke::new(
            &doc,
            id,
            false,
            BrushSettings {
                diameter: 32.,
                hardness: 0.6,
                opacity: 0.65,
                mode,
                ..Default::default()
            },
            None,
        )?;
        for &point in &points {
            stroke.append(point)?;
        }
        stroke.finish_at(*points.last().unwrap())?;
        let work = stroke.warp_working_raster_usage().unwrap();
        let tiles = stroke.raster.tiles.len();
        let retained = stroke.raster.byte_len();
        stroke.commit(&mut doc)?;
        trials.push(json!({"mode":format!("{mode:?}"),"canvas":[4096,4096],"sourceRgbaBytes":source.pixels.len(),"pointerSamples":points.len(),"diameter":32,"milliseconds":timer.elapsed().as_secs_f64()*1000.,"documentWorkingTiles":work.0,"documentWorkingRasterBytes":work.1,"nativeEditedTiles":tiles,"nativeRetainedRasterBytes":retained,"sourceBaseShared":Arc::ptr_eq(&doc.active().unwrap().image.as_ref().unwrap().pixels,&source.pixels),"beforeMemory":before,"afterMemory":memory()}));
    }
    drop(source);
    let (width, height) = (4096, 2048);
    let source = Raster::from_rgba(
        width,
        height,
        (0..width * height)
            .flat_map(|i| {
                let x = i % width;
                let y = i / width;
                let v = if x >= 1600 + ((y as f64 / 80.).sin() * 40.) as u32 {
                    220
                } else {
                    30
                };
                [v, v, v, 255]
            })
            .collect(),
    );
    let alpha = (0..width * height)
        .map(|i| (((i % width) as f64 - 1580.) / 80.).clamp(0., 1.) * 255.)
        .map(|v| v.round() as u8)
        .collect::<Vec<_>>();
    let before = memory();
    let timer = Instant::now();
    let refined = matte::refine(
        &alpha,
        &source,
        &json!({"backgroundQuality":"Advanced","refineEdges":12,"shiftEdge":-2,"matteContrast":25}),
    )?;
    let elapsed = timer.elapsed().as_secs_f64() * 1000.;
    let changed = refined.iter().zip(&alpha).filter(|(a, b)| a != b).count();
    let result = json!({"buildMode":if cfg!(debug_assertions){"debug (opt-level=1)"}else{"release"},"logicalCpus":std::thread::available_parallelism().map_or(1,|v|v.get()),"retouch":trials,"matte":{"canvas":[width,height],"sourceRgbaBytes":source.pixels.len(),"refineEdges":12,"shiftEdge":-2,"matteContrast":25,"milliseconds":elapsed,"changedMaskPixels":changed,"outputBytes":refined.len(),"beforeMemory":before,"afterMemory":memory()},"limitations":"Working-raster counters exclude carry/scratch, mip prefilters, native coverage map and shared source. Process peak includes prior cases. No GPU or GUI pointer-event timing measured."});
    std::fs::write(output, serde_json::to_vec_pretty(&result)?)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
