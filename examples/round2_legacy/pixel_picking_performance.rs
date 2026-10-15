//! Exact document-pixel picking benchmark; no display-resolution raster participates.
use compositor::{
    model::{Document, Layer, Raster},
    render::CompositeSampler,
};
use std::{hint::black_box, time::Instant};

fn measure(doc: &Document, label: &str) {
    let start = Instant::now();
    let sampler = CompositeSampler::new(doc);
    let prepared = start.elapsed().as_secs_f64() * 1000.;
    let mut elapsed = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        for i in 0..10_000 {
            let p = [(i * 127 % doc.width) as f64, (i * 251 % doc.height) as f64];
            black_box(sampler.sample(black_box(p)));
        }
        elapsed.push(start.elapsed().as_secs_f64() * 1000.);
    }
    elapsed.sort_by(f64::total_cmp);
    println!(
        "{label}: {}x{}, {} layers, prepare {prepared:.3}ms, 10,000 native samples median {:.3}ms ({:.3}us/sample), full surface {}",
        doc.width,
        doc.height,
        doc.layers.len(),
        elapsed[3],
        elapsed[3] / 10.,
        sampler.uses_full_surface()
    );
    let start = Instant::now();
    for _ in 0..10_000 {
        black_box(sampler.matches(black_box(doc)));
    }
    println!(
        "10,000 immutable-state comparisons {:.3}ms",
        start.elapsed().as_secs_f64() * 1000.
    );
}
pub fn main() {
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
        measure(&doc, "Uniform shared rasters");
    }
    let mut doc = Document::new(512, 384);
    let mut data = Vec::with_capacity(512 * 384 * 4);
    for i in 0..512 * 384 {
        data.extend_from_slice(&[
            (i * 11 % 256) as u8,
            (i * 17 % 256) as u8,
            (i * 37 % 256) as u8,
            255,
        ]);
    }
    doc.layers = vec![Layer::raster(
        "Dense texture",
        Raster::from_rgba(512, 384, data),
    )];
    let mut adjustment = Layer::new("Blur", 512, 384);
    adjustment.adjustment = Some(serde_json::json!({"kind":"Gaussian Blur","blurRadius":10}));
    doc.layers.push(adjustment);
    measure(&doc, "Spatial adjustment cache");
}
