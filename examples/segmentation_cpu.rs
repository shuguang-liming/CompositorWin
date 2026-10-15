//! Compare the frozen 0.1.1 serial implementation with actual production segmentation.
//! Run: cargo run --release --example segmentation_cpu -- --pairs 3 [--photo IMAGE]
use anyhow::{Context, Result, ensure};
use compositor::{
    model,
    segmentation::{Segmentation, SegmentationEngine},
};
use model::Raster;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Instant};

#[allow(dead_code)]
mod reference {
    // Frozen original serial implementation for this benchmark.
    use crate::model::Raster;
    use anyhow::{Context, Result, ensure};
    use flate2::read::GzDecoder;
    use std::{path::Path, sync::Arc, time::Instant};
    use tract_nnef::prelude::*;
    use tract_onnx::tract_hir::prelude::InferenceModelExt;

    pub const MODEL_SIDE: usize = 320;
    pub const MODEL_SHA256: &str =
        "309c8469258dda742793dce0ebea8e6dd393174f89934733ecc8b14c76f4ddd8";
    const MODEL_BYTES: &[u8] = include_bytes!("../assets/u2netp.nnef.tar.gz");

    pub struct SegmentationEngine {
        model: Arc<TypedRunnableModel>,
    }

    #[derive(Clone, Debug)]
    pub struct Segmentation {
        pub width: u32,
        pub height: u32,
        pub alpha: Vec<u8>,
        pub inference_millis: u128,
    }

    impl SegmentationEngine {
        /// The same float32 weights and fixed graph are embedded for offline inference.
        pub fn load_default() -> Result<Self> {
            let mut reader = GzDecoder::new(MODEL_BYTES);
            let model = tract_nnef::nnef()
                .model_for_read(&mut reader)
                .context("读取 U2Netp NNEF 计算图")?
                .into_optimized()?
                .into_runnable()?;
            Ok(Self { model })
        }

        pub fn load(path: impl AsRef<Path>) -> Result<Self> {
            let bytes = std::fs::read(path.as_ref()).context("读取主体分割 ONNX 模型")?;
            Self::from_bytes(&bytes)
        }

        fn from_bytes(bytes: &[u8]) -> Result<Self> {
            let mut model = tract_onnx::onnx()
                .model_for_read(&mut std::io::Cursor::new(bytes))
                .context("读取 U2Netp ONNX 计算图")?
                .with_input_fact(0, f32::fact([1, 3, MODEL_SIDE, MODEL_SIDE]).into())?;
            // U2Netp exports six training supervision heads as well as the final fused mask.
            // Keeping only the first output lets tract remove the unused heads.
            let output = model.output_outlets()?[0];
            model.select_output_outlets(&[output])?;
            let model = model.into_optimized()?.into_runnable()?;
            Ok(Self { model })
        }

        pub fn predict(&self, image: &Raster) -> Result<Segmentation> {
            ensure!(image.width > 0 && image.height > 0, "主体图像为空");
            let start = Instant::now();
            let input = preprocess(image);
            let tensor = Tensor::from_shape(&[1, 3, MODEL_SIDE, MODEL_SIDE], &input)?;
            let result =
                multithread::multithread_tract_scope(multithread::Executor::SingleThread, || {
                    self.model.run(tvec!(tensor.into()))
                })?;
            let output = result[0].to_plain_array_view::<f32>()?;
            ensure!(
                output.len() == MODEL_SIDE * MODEL_SIDE,
                "U2Netp 输出维度异常：{:?}",
                output.shape()
            );
            let values: Vec<f32> = output.iter().copied().collect();
            let min = values.iter().copied().fold(f32::INFINITY, f32::min);
            let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            ensure!(min.is_finite() && max.is_finite(), "分割结果包含无效数值");
            let span = (max - min).max(1e-8);
            let mask: Vec<u8> = values
                .iter()
                .map(|v| (((v - min) / span) * 255.).round().clamp(0., 255.) as u8)
                .collect();
            let low = image::GrayImage::from_raw(MODEL_SIDE as u32, MODEL_SIDE as u32, mask)
                .context("生成主体分割结果")?;
            let alpha = image::imageops::resize(
                &low,
                image.width,
                image.height,
                image::imageops::FilterType::Triangle,
            )
            .into_raw();
            Ok(Segmentation {
                width: image.width,
                height: image.height,
                alpha,
                inference_millis: start.elapsed().as_millis(),
            })
        }
    }

    fn preprocess(image: &Raster) -> Vec<f32> {
        let plane = MODEL_SIDE * MODEL_SIDE;
        let low_width = image.width.min((MODEL_SIDE * 4) as u32) as usize;
        let low_height = image.height.min((MODEL_SIDE * 4) as u32) as usize;
        let mut rgb = vec![0u8; low_width * low_height * 3];
        // A bounded area sampler reads sparse/large rasters without expanding their full RGBA allocation.
        let nx = (image.width as usize / low_width).clamp(1, 8);
        let ny = (image.height as usize / low_height).clamp(1, 8);
        for y in 0..low_height {
            for x in 0..low_width {
                let mut color = [0f32; 3];
                for j in 0..ny {
                    for i in 0..nx {
                        let sx = ((x as f64 + (i as f64 + 0.5) / nx as f64) * image.width as f64
                            / low_width as f64)
                            .floor() as u32;
                        let sy = ((y as f64 + (j as f64 + 0.5) / ny as f64) * image.height as f64
                            / low_height as f64)
                            .floor() as u32;
                        let p = image.get(sx.min(image.width - 1), sy.min(image.height - 1));
                        let a = p[3] as f32 / 255.;
                        for c in 0..3 {
                            color[c] += p[c] as f32 * a + 255. * (1. - a);
                        }
                    }
                }
                for c in 0..3 {
                    rgb[(y * low_width + x) * 3 + c] =
                        (color[c] / (nx * ny) as f32).round().clamp(0., 255.) as u8;
                }
            }
        }
        let low = image::RgbImage::from_raw(low_width as u32, low_height as u32, rgb).unwrap();
        let rgb = image::imageops::resize(
            &low,
            MODEL_SIDE as u32,
            MODEL_SIDE as u32,
            image::imageops::FilterType::Lanczos3,
        )
        .into_raw();
        let max = rgb.iter().copied().max().unwrap_or(1).max(1) as f32;
        let mut data = vec![0f32; plane * 3];
        let mean = [0.485, 0.456, 0.406];
        let std = [0.229, 0.224, 0.225];
        for (i, p) in rgb.as_chunks::<3>().0.iter().enumerate() {
            for c in 0..3 {
                data[c * plane + i] = (p[c] as f32 / max - mean[c]) / std[c];
            }
        }
        data
    }
}

fn elapsed<T>(operation: impl FnOnce() -> Result<T>) -> Result<(T, f64)> {
    let start = Instant::now();
    let result = operation()?;
    Ok((result, start.elapsed().as_secs_f64() * 1000.))
}

fn with_workers<T: Send>(
    pool: Option<&rayon::ThreadPool>,
    operation: impl FnOnce() -> T + Send,
) -> T {
    match pool {
        Some(pool) => pool.install(operation),
        None => operation(),
    }
}

fn original_foreground(source: &Raster, alpha: &[u8]) -> Raster {
    let mut rgba = source.to_rgba();
    for (pixel, &amount) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(alpha) {
        pixel[3] = ((pixel[3] as u16 * amount as u16 + 127) / 255) as u8;
    }
    Raster::from_rgba(source.width, source.height, rgba)
}

fn dense(width: u32, height: u32) -> Raster {
    Raster::from_rgba(
        width,
        height,
        (0..width * height)
            .flat_map(|i| {
                [
                    (i * 11 % 256) as u8,
                    (i * 17 % 256) as u8,
                    (i * 31 % 256) as u8,
                    (i * 7 % 256) as u8,
                ]
            })
            .collect(),
    )
}

fn median(values: impl Iterator<Item = f64>) -> f64 {
    let mut values: Vec<_> = values.collect();
    values.sort_by(f64::total_cmp);
    if values.len().is_multiple_of(2) {
        (values[values.len() / 2 - 1] + values[values.len() / 2]) / 2.
    } else {
        values[values.len() / 2]
    }
}

fn main() -> Result<()> {
    let mut pairs = 3;
    let mut photo: Option<PathBuf> = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--pairs") => {
                pairs = args
                    .next()
                    .context("--pairs requires a value")?
                    .to_str()
                    .context("Invalid pair count")?
                    .parse::<usize>()?
            }
            Some("--photo") => photo = Some(args.next().context("--photo requires a path")?.into()),
            _ => anyhow::bail!("Use --pairs COUNT and optionally --photo IMAGE"),
        }
    }
    ensure!(
        (1..=100).contains(&pairs),
        "Pair count must be 1 through 100"
    );
    let logical_cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let global_threads = rayon::current_num_threads();
    let rayon_num_threads_env =
        std::env::var_os("RAYON_NUM_THREADS").map(|value| value.to_string_lossy().into_owned());
    let mut threads = vec![
        1,
        4.min(logical_cpus),
        8.min(logical_cpus),
        logical_cpus,
        global_threads,
    ];
    threads.sort_unstable();
    threads.dedup();
    let baseline = reference::SegmentationEngine::load_default()?;
    let candidate = SegmentationEngine::load_default()?;
    let mut tiled = Raster::solid(6000, 4000, [126, 48, 199, 173]);
    for y in 0..128 {
        for x in 0..128 {
            tiled.set(
                1024 + x,
                2048 + y,
                [(x * 2) as u8, (y * 2) as u8, 37, (x + y) as u8],
            );
        }
    }
    let mut sources = vec![
        ("small-dense-96x64".to_owned(), dense(96, 64)),
        ("dense-4097x3073".to_owned(), dense(4097, 3073)),
        ("sparse-tiles-6000x4000".to_owned(), tiled),
    ];
    if let Some(path) = photo {
        let image = image::open(&path)?.to_rgba8();
        sources.push((
            format!("photo:{}", path.display()),
            Raster::from_rgba(image.width(), image.height(), image.into_raw()),
        ));
    }
    // Each measured registry warms both implementations on identical inputs.
    let warm = Raster::solid(96, 64, [181, 64, 29, 191]);
    let mut cases = Vec::new();
    for (name, source) in sources {
        let mut samples = Vec::new();
        let mut alpha_hash = None;
        let mut foreground_hash = None;
        for &thread_count in &threads {
            let pool = if thread_count == global_threads {
                None
            } else {
                Some(
                    rayon::ThreadPoolBuilder::new()
                        .num_threads(thread_count)
                        .build()?,
                )
            };
            let worker_context = if pool.is_none() {
                "production-global"
            } else {
                "local-test-pool"
            };
            baseline.predict(&warm)?;
            with_workers(pool.as_ref(), || candidate.predict(&warm))?;
            let warm_alpha = Segmentation {
                width: source.width,
                height: source.height,
                alpha: vec![173; source.width as usize * source.height as usize],
                inference_millis: 0,
            };
            drop(original_foreground(&source, &warm_alpha.alpha));
            drop(with_workers(pool.as_ref(), || {
                warm_alpha.foreground(&source)
            })?);
            drop(warm_alpha);
            for pair in 0..pairs {
                eprintln!("{name}: {thread_count} threads, pair {}/{pairs}", pair + 1);
                let (old, old_ms, new, new_ms) = if pair % 2 == 0 {
                    let (old, old_ms) = elapsed(|| baseline.predict(&source))?;
                    let (new, new_ms) =
                        elapsed(|| with_workers(pool.as_ref(), || candidate.predict(&source)))?;
                    (old, old_ms, new, new_ms)
                } else {
                    let (new, new_ms) =
                        elapsed(|| with_workers(pool.as_ref(), || candidate.predict(&source)))?;
                    let (old, old_ms) = elapsed(|| baseline.predict(&source))?;
                    (old, old_ms, new, new_ms)
                };
                ensure!(
                    old.width == new.width && old.height == new.height && old.alpha == new.alpha,
                    "Full mask differs for {name} with {thread_count} workers"
                );
                let hash = format!("{:x}", Sha256::digest(&new.alpha));
                if let Some(previous) = &alpha_hash {
                    ensure!(
                        previous == &hash,
                        "The candidate mask changed between thread counts"
                    );
                }
                alpha_hash = Some(hash);
                let (old_foreground, old_foreground_ms, new_foreground, new_foreground_ms) =
                    if pair % 2 == 0 {
                        let (old_foreground, old_foreground_ms) =
                            elapsed(|| Ok(original_foreground(&source, &old.alpha)))?;
                        let (new_foreground, new_foreground_ms) =
                            elapsed(|| with_workers(pool.as_ref(), || new.foreground(&source)))?;
                        (
                            old_foreground,
                            old_foreground_ms,
                            new_foreground,
                            new_foreground_ms,
                        )
                    } else {
                        let (new_foreground, new_foreground_ms) =
                            elapsed(|| with_workers(pool.as_ref(), || new.foreground(&source)))?;
                        let (old_foreground, old_foreground_ms) =
                            elapsed(|| Ok(original_foreground(&source, &old.alpha)))?;
                        (
                            old_foreground,
                            old_foreground_ms,
                            new_foreground,
                            new_foreground_ms,
                        )
                    };
                let old_rgba = old_foreground.to_rgba();
                let new_rgba = new_foreground.to_rgba();
                ensure!(
                    old_rgba == new_rgba,
                    "Full foreground pixels differ for {name}"
                );
                let hash = format!("{:x}", Sha256::digest(&new_rgba));
                if let Some(previous) = &foreground_hash {
                    ensure!(
                        previous == &hash,
                        "The candidate foreground changed between thread counts"
                    );
                }
                foreground_hash = Some(hash);
                samples.push(json!({"threads":thread_count,"workerContext":worker_context,"pair":pair,"order":if pair % 2 == 0 {"baseline-first"} else {"candidate-first"},"predictionBaselineMs":old_ms,"predictionCandidateMs":new_ms,"foregroundBaselineMs":old_foreground_ms,"foregroundCandidateMs":new_foreground_ms,"maskBytesCompared":new.alpha.len(),"foregroundBytesCompared":new_rgba.len(),"exactPixels":true}));
            }
        }
        let summaries: Vec<_> = threads.iter().map(|thread_count| {
            let filtered: Vec<_> = samples.iter().filter(|s| s["threads"].as_u64() == Some(*thread_count as u64)).collect();
            json!({"threads":thread_count,"workerContext":filtered[0]["workerContext"],"predictionBaselineMedianMs":median(filtered.iter().map(|s| s["predictionBaselineMs"].as_f64().unwrap())),"predictionCandidateMedianMs":median(filtered.iter().map(|s| s["predictionCandidateMs"].as_f64().unwrap())),"foregroundBaselineMedianMs":median(filtered.iter().map(|s| s["foregroundBaselineMs"].as_f64().unwrap())),"foregroundCandidateMedianMs":median(filtered.iter().map(|s| s["foregroundCandidateMs"].as_f64().unwrap()))})
        }).collect();
        cases.push(json!({"case":name,"width":source.width,"height":source.height,"alphaSha256":alpha_hash,"foregroundSha256":foreground_hash,"summaries":summaries,"samples":samples}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"buildMode":if cfg!(debug_assertions){"debug"}else{"release"},"logicalCpus":logical_cpus,"globalRayonThreads":global_threads,"rayonNumThreadsEnv":rayon_num_threads_env,"pairCountPerThreadCount":pairs,"baselineSourceSha256":BASELINE_SOURCE_SHA256,"candidateSourceSha256":format!("{:x}",Sha256::digest(include_bytes!("../src/segmentation.rs"))),"warmup":{"predictionInputWidth":96,"predictionInputHeight":64,"predictionPerWorkerContextPerEngine":1,"foregroundPerWorkerContextPerImplementation":1,"excludesWarmupFromTimings":true},"cases":cases,"limitations":"Measures the same tract CPU graph and weights plus complete preprocessing and mask resizing. Baseline inference uses SingleThread; the default candidate thread count uses the actual production global Rayon registry, and other thread counts use isolated pools only for scaling observations. Both implementations are warmed on identical inputs in each measured worker context. No GPU, GUI input latency, process startup, CPU utilization or power draw measurement. Baseline preprocessing is frozen 0.1.1, candidate is the actual production implementation; per-pixel output is compared in full."})
        )?
    );
    Ok(())
}

const BASELINE_SOURCE_SHA256: &str =
    "17b84177c623f8b1fadadc90fe1706e5bfed57cb4324e3f21953ad1b6143a631";
