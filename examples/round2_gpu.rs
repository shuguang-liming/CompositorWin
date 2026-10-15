//! GPU submission, completion wait, and readback are measured as separate stages.
use anyhow::{Context, Result, ensure};
use compositor::{
    gpu::GpuCanvas,
    model::{Document, Layer, Raster, TILE_SIZE},
};
use egui_wgpu::{RenderState, Renderer, RendererOptions, wgpu};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

#[allow(dead_code)]
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    run(&args)
}

fn hash(image: &Raster) -> String {
    format!("{:x}", Sha256::digest(image.to_rgba()))
}

fn measure_frame(
    gpu: &mut GpuCanvas,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    doc: &Document,
    preview_max_side: u32,
) -> Result<Value> {
    let uploaded = gpu.uploaded_bytes;
    let bindings = gpu.binding_allocations;
    let start = Instant::now();
    gpu.render(doc, preview_max_side);
    let render_cpu_ms = start.elapsed().as_secs_f64() * 1000.;

    // render() does not expose its submission index. An empty submission fences all preceding work.
    let start = Instant::now();
    let fence = queue.submit(std::iter::empty::<wgpu::CommandBuffer>());
    let completion_fence_submit_ms = start.elapsed().as_secs_f64() * 1000.;
    let start = Instant::now();
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(fence),
            timeout: Some(Duration::from_secs(30)),
        })
        .context("Waiting for submitted GPU work")?;
    let completion_wait_ms = start.elapsed().as_secs_f64() * 1000.;

    // This API includes a new texture copy, map wait, row packing, and CPU unpremultiplication.
    let start = Instant::now();
    let output = gpu.readback().map_err(anyhow::Error::msg)?;
    let readback_and_conversion_ms = start.elapsed().as_secs_f64() * 1000.;
    let start = Instant::now();
    let output_sha = hash(&output);
    let output_rgba_and_sha_ms = start.elapsed().as_secs_f64() * 1000.;
    Ok(json!({
        "renderCpuMilliseconds": render_cpu_ms,
        "completionFenceSubmitMilliseconds": completion_fence_submit_ms,
        "completionWaitMilliseconds": completion_wait_ms,
        "renderCompletionWallMilliseconds": render_cpu_ms + completion_fence_submit_ms + completion_wait_ms,
        "readbackAndConversionMilliseconds": readback_and_conversion_ms,
        "outputRgbaAndSha256Milliseconds": output_rgba_and_sha_ms,
        "uploadedBytes": gpu.uploaded_bytes - uploaded,
        "residentSourceBytes": gpu.resident_source_bytes(),
        "bindingAllocations": gpu.binding_allocations - bindings,
        "outputWidth": output.width,
        "outputHeight": output.height,
        "outputSha256": output_sha,
    }))
}

fn summarize(frames: &[Value]) -> Value {
    let mut summary = serde_json::Map::new();
    for metric in [
        "renderCpuMilliseconds",
        "completionFenceSubmitMilliseconds",
        "completionWaitMilliseconds",
        "renderCompletionWallMilliseconds",
        "readbackAndConversionMilliseconds",
        "outputRgbaAndSha256Milliseconds",
    ] {
        let mut values: Vec<_> = frames
            .iter()
            .map(|frame| frame[metric].as_f64().unwrap())
            .collect();
        values.sort_by(f64::total_cmp);
        summary.insert(
            metric.to_owned(),
            json!({
                "minimum": values[0],
                "median": values[values.len() / 2],
                "maximum": values[values.len() - 1],
            }),
        );
    }
    summary.insert(
        "uploadedBytesTotal".into(),
        json!(
            frames
                .iter()
                .map(|frame| frame["uploadedBytes"].as_u64().unwrap())
                .sum::<u64>()
        ),
    );
    summary.insert(
        "bindingAllocationsTotal".into(),
        json!(
            frames
                .iter()
                .map(|frame| frame["bindingAllocations"].as_u64().unwrap())
                .sum::<u64>()
        ),
    );
    Value::Object(summary)
}

fn measure_case(
    state: &RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: Raster,
    name: &str,
    source_preparation: &str,
) -> Result<Value> {
    let source_sha = hash(&source);
    let width = source.width;
    let height = source.height;
    let mut doc = Document::new(width, height);
    doc.layers = (0..4)
        .map(|index| {
            let mut layer = Layer::raster(format!("Layer {index}"), source.clone());
            layer.opacity = 0.35;
            layer
        })
        .collect();
    doc.active_layer_id = Some(doc.layers[0].id);
    ensure!(
        GpuCanvas::supports(&doc),
        "Benchmark input is unsupported by the GPU canvas"
    );

    let start = Instant::now();
    let mut gpu = GpuCanvas::new(state.clone());
    let gpu_canvas_initialization_ms = start.elapsed().as_secs_f64() * 1000.;
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(30)),
        })
        .context("Waiting for GPU canvas initialization")?;
    let preview_max_side = 1024;
    let first = measure_frame(&mut gpu, device, queue, &doc, preview_max_side)?;
    let mut unchanged = Vec::new();
    for _ in 0..7 {
        let frame = measure_frame(&mut gpu, device, queue, &doc, preview_max_side)?;
        ensure!(
            frame["outputSha256"] == first["outputSha256"],
            "Unchanged rendering altered the output"
        );
        unchanged.push(frame);
    }

    let snapshot = doc.clone();
    let mut edited = Vec::new();
    for index in 0..7_u32 {
        let x_boundaries = ((width - 1) / TILE_SIZE).max(1);
        let y_boundaries = ((height - 1) / TILE_SIZE).max(1);
        let cx = (((index * 3) % x_boundaries + 1) * TILE_SIZE).min(width - 1);
        let cy = (((index * 5) % y_boundaries + 1) * TILE_SIZE).min(height - 1);
        let start = Instant::now();
        let raster = Arc::make_mut(doc.layers[0].image.as_mut().unwrap());
        for y in cy.saturating_sub(8)..(cy + 9).min(height) {
            for x in cx.saturating_sub(8)..(cx + 9).min(width) {
                raster.set(x, y, [229, 41 + index as u8 * 7, 101, 193]);
            }
        }
        let changed_tiles = raster.tiles.len();
        let edit_ms = start.elapsed().as_secs_f64() * 1000.;
        let mut frame = measure_frame(&mut gpu, device, queue, &doc, preview_max_side)?;
        frame["editMillisecondsExcludedFromRender"] = json!(edit_ms);
        frame["editCenter"] = json!([cx, cy]);
        frame["sourceTilesAfterEdit"] = json!(changed_tiles);
        edited.push(frame);
    }
    ensure!(
        hash(snapshot.layers[0].image.as_ref().unwrap()) == source_sha,
        "Editing changed the undo snapshot"
    );
    Ok(json!({
        "name": name,
        "width": width,
        "height": height,
        "layers": 4,
        "previewMaxSide": preview_max_side,
        "sourcePreparation": source_preparation,
        "sourceSha256": source_sha,
        "gpuCanvasInitializationCpuMilliseconds": gpu_canvas_initialization_ms,
        "first": first,
        "unchangedSummary": summarize(&unchanged),
        "unchangedFrames": unchanged,
        "editedSummary": summarize(&edited),
        "editedFrames": edited,
        "snapshotPreserved": true,
    }))
}

pub fn run(args: &[String]) -> Result<()> {
    ensure!(
        args.len() == 2 || args.len() == 3 || args.len() == 5,
        "Usage: round2_gpu vulkan|dx12 [PHOTO [WIDTH HEIGHT]]"
    );
    let backend = match args[1].as_str() {
        "vulkan" => wgpu::Backends::VULKAN,
        "dx12" => wgpu::Backends::DX12,
        _ => anyhow::bail!("Backend must be vulkan or dx12"),
    };
    let start = Instant::now();
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: backend,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))?;
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("Compositor round2 GPU measurement"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))?;
    let renderer = Renderer::new(
        &device,
        wgpu::TextureFormat::Rgba8Unorm,
        RendererOptions::default(),
    );
    let state = RenderState {
        adapter: adapter.clone(),
        available_adapters: vec![adapter],
        device: device.clone(),
        queue: queue.clone(),
        target_format: wgpu::TextureFormat::Rgba8Unorm,
        renderer: Arc::new(eframe::egui::mutex::RwLock::new(renderer)),
    };
    let device_initialization_ms = start.elapsed().as_secs_f64() * 1000.;
    let mut pixels = Vec::with_capacity(257 * 129 * 4);
    for y in 0..129_u32 {
        for x in 0..257_u32 {
            pixels.extend_from_slice(&[
                ((x * 17 + y * 31) & 255) as u8,
                ((x * 7 + y * 19) & 255) as u8,
                ((x * 13 + y * 3) & 255) as u8,
                255,
            ]);
        }
    }
    let mut cases = vec![measure_case(
        &state,
        &device,
        &queue,
        Raster::from_rgba(257, 129, pixels),
        "odd257x129",
        "Deterministic dense RGBA input",
    )?];
    let mut photo_metadata = Value::Null;
    if args.len() >= 3 {
        let photo = compositor::image_io::decode_image(Path::new(&args[2]))?;
        photo_metadata = json!({
            "path": args[2],
            "originalWidth": photo.width,
            "originalHeight": photo.height,
            "originalSha256": hash(&photo),
        });
        let (source, preparation) = if args.len() == 5 {
            let width = args[3].parse::<u32>()?;
            let height = args[4].parse::<u32>()?;
            ensure!(
                Document::valid_size(width, height),
                "Invalid requested photo size"
            );
            let image =
                image::RgbaImage::from_raw(photo.width, photo.height, photo.to_rgba()).unwrap();
            let resized = image::imageops::resize(
                &image,
                width,
                height,
                image::imageops::FilterType::Lanczos3,
            );
            (
                Raster::from_rgba(width, height, resized.into_raw()),
                "Lanczos3 resize of supplied photograph; setup excluded",
            )
        } else {
            (
                photo,
                "Supplied photograph at its decoded native dimensions; decode excluded",
            )
        };
        ensure!(
            Document::valid_size(source.width, source.height),
            "Invalid photo size"
        );
        cases.push(measure_case(
            &state,
            &device,
            &queue,
            source,
            "photograph",
            preparation,
        )?);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "backendRequested": args[1],
            "adapter": info.name,
            "backendActual": format!("{:?}", info.backend),
            "driver": info.driver,
            "driverInfo": info.driver_info,
            "deviceInitializationCpuMilliseconds": device_initialization_ms,
            "measurementScope": {
                "renderCpuMilliseconds": "GPU canvas CPU preparation, upload enqueue, command encoding, and submission; existing API exposes one combined duration",
                "completionWaitMilliseconds": "Host wait until all rendering submissions preceding an empty queue fence complete; includes remaining GPU/driver work and synchronization, not pure GPU kernel time",
                "readbackAndConversionMilliseconds": "Additional GPU texture copy, map wait, row packing and CPU unpremultiplication after rendering completion",
                "interactiveFrameTimeMeasured": false,
                "readbackAfterEachFrameAffectsFollowingFrameScheduling": true,
                "warmup": "First render is reported separately; no hidden warmup or shader cache reset",
            },
            "photo": photo_metadata,
            "cases": cases,
        }))?
    );
    Ok(())
}
