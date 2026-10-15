//! One executable keeps identical workloads across the frozen and candidate libraries.
#[path = "round2_blur.rs"]
mod blur;
#[path = "round2_brush.rs"]
mod brush;
#[path = "round2_dense.rs"]
#[allow(dead_code)]
mod dense;
#[path = "round2_gpu.rs"]
#[allow(dead_code)]
mod gpu;
#[path = "round2_legacy/benchmark.rs"]
mod legacy;
#[path = "round2_legacy/gpu_performance.rs"]
mod legacy_gpu;
#[path = "round2_legacy/pixel_picking_performance.rs"]
mod picking;
#[path = "round2_legacy/retouch_benchmark.rs"]
mod retouch;
#[path = "round2_legacy/segmentation_smoke.rs"]
mod segmentation;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("blur") => blur::main()?,
        Some("brush") => brush::main()?,
        Some("dense") => dense::run(&args[1..])?,
        Some("gpu") => gpu::run(&args[1..])?,
        Some("legacy") => legacy::main()?,
        Some("retouch") => retouch::main()?,
        Some("picking") => picking::main(),
        Some("legacy-gpu") => legacy_gpu::main()?,
        Some("segmentation") => segmentation::main()?,
        _ => {
            return Err(
                "Expected blur|brush|dense|legacy|retouch|picking|legacy-gpu|segmentation".into(),
            );
        }
    }
    Ok(())
}
