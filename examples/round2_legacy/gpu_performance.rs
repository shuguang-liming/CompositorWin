//! Headless GPU benchmark, including an explicit readback to wait for completed work.
use anyhow::Result;
use compositor::{
    gpu::GpuCanvas,
    model::{Document, Layer, Raster},
};
use egui_wgpu::{RenderState, Renderer, RendererOptions, wgpu};
use std::{sync::Arc, time::Instant};

pub fn main() -> Result<()> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))?;
    let info = adapter.get_info();
    println!("GPU: {} {:?}", info.name, info.backend);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("Compositor performance"),
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
        device,
        queue,
        target_format: wgpu::TextureFormat::Rgba8Unorm,
        renderer: Arc::new(eframe::egui::mutex::RwLock::new(renderer)),
    };
    let mut gpu = GpuCanvas::new(state);
    for (side, layers) in [(4096, 4), (8192, 12)] {
        let mut doc = Document::new(side, side);
        doc.layers = (0..layers)
            .map(|i| {
                let mut layer = Layer::raster(
                    format!("Layer {i}"),
                    Raster::solid(side, side, [i * 17, 80, 180, 255]),
                );
                layer.opacity = 0.35;
                layer
            })
            .collect();
        let bytes = gpu.uploaded_bytes;
        let start = Instant::now();
        gpu.render(&doc, 1024);
        let _completed = gpu.readback().map_err(anyhow::Error::msg)?;
        println!(
            "{side}x{side}, {layers} layers, preview 1024x1024: first {:.3}ms; upload {} bytes; layer textures {} bytes",
            start.elapsed().as_secs_f64() * 1000.,
            gpu.uploaded_bytes - bytes,
            gpu.resident_source_bytes()
        );
        let before = gpu.uploaded_bytes;
        let mut timings = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            gpu.render(&doc, 1024);
            let _completed = gpu.readback().map_err(anyhow::Error::msg)?;
            timings.push(start.elapsed().as_secs_f64() * 1000.);
        }
        timings.sort_by(f64::total_cmp);
        println!(
            "Unchanged median {:.3}ms (7 frames, readback included), upload {} bytes",
            timings[3],
            gpu.uploaded_bytes - before
        );
        let r = Arc::make_mut(doc.layers[0].image.as_mut().unwrap());
        for y in 0..128 {
            for x in 0..128 {
                r.set(x, y, [255, 0, 0, 255]);
            }
        }
        let start = Instant::now();
        gpu.render(&doc, 1024);
        let _completed = gpu.readback().map_err(anyhow::Error::msg)?;
        println!(
            "Edited 128x128 tile: {:.3}ms, upload {} bytes",
            start.elapsed().as_secs_f64() * 1000.,
            gpu.uploaded_bytes - before
        );
    }
    Ok(())
}
