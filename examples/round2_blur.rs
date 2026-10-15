//! Exact blur comparison and timing. Synthetic fixtures are not real photographs.
use compositor::pixels::{clamp_premultiplied, gaussian_blur, hash32, premultiply};
use sha2::{Digest, Sha256};
use std::{hint::black_box, path::PathBuf, time::Instant};

struct Options {
    mode: String,
    width: u32,
    height: u32,
    sigmas: Vec<f64>,
    iterations: usize,
    warmup: usize,
    input: Option<PathBuf>,
}

fn options() -> Result<Options, String> {
    let mut out = Options {
        mode: "optimized".into(),
        width: 2048,
        height: 2048,
        sigmas: vec![1.0, 10.0, 50.0],
        iterations: 1,
        warmup: 0,
        input: None,
    };
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_some_and(|arg| arg == "blur") {
        args.next();
    }
    while let Some(arg) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("Missing value for {arg}"))?;
        match arg.as_str() {
            "--mode" => out.mode = value,
            "--width" => out.width = value.parse().map_err(|_| "Invalid width")?,
            "--height" => out.height = value.parse().map_err(|_| "Invalid height")?,
            "--sigma" => {
                out.sigmas = value
                    .split(',')
                    .map(|v| v.parse::<f64>().map_err(|_| "Invalid sigma".to_owned()))
                    .collect::<Result<_, _>>()?;
            }
            "--iterations" => {
                out.iterations = value.parse().map_err(|_| "Invalid iterations")?;
            }
            "--warmup" => out.warmup = value.parse().map_err(|_| "Invalid warmup")?,
            "--input" => out.input = Some(PathBuf::from(value)),
            _ => return Err(format!("Unknown option {arg}")),
        }
    }
    if !["reference", "optimized", "compare"].contains(&out.mode.as_str())
        || out.width == 0
        || out.height == 0
        || out.iterations == 0
        || out.sigmas.is_empty()
        || out.sigmas.iter().any(|v| !v.is_finite() || *v < 0.0)
    {
        return Err("Invalid benchmark options".into());
    }
    Ok(out)
}

fn reference_box_blur(pixels: &[u8], width: usize, height: usize, r: usize) -> Vec<u8> {
    if r == 0 {
        return pixels.to_vec();
    }
    let mut temp = vec![0.0f64; pixels.len()];
    let mut out = vec![0; pixels.len()];
    let n = (r * 2 + 1) as f64;
    for y in 0..height {
        for c in 0..4 {
            let mut sum = 0.0;
            for i in 0..=r * 2 {
                sum += pixels[(y * width + i.saturating_sub(r).min(width - 1)) * 4 + c] as f64;
            }
            for x in 0..width {
                temp[(y * width + x) * 4 + c] = sum / n;
                sum += pixels[(y * width + (x + r + 1).min(width - 1)) * 4 + c] as f64
                    - pixels[(y * width + x.saturating_sub(r)) * 4 + c] as f64;
            }
        }
    }
    for x in 0..width {
        for c in 0..4 {
            let mut sum = 0.0;
            for i in 0..=r * 2 {
                sum += temp[(i.saturating_sub(r).min(height - 1) * width + x) * 4 + c];
            }
            for y in 0..height {
                out[(y * width + x) * 4 + c] = (sum / n).round().clamp(0.0, 255.0) as u8;
                sum += temp[((y + r + 1).min(height - 1) * width + x) * 4 + c]
                    - temp[(y.saturating_sub(r) * width + x) * 4 + c];
            }
        }
    }
    out
}

fn reference_gaussian_blur(pixels: &[u8], width: usize, height: usize, sigma: f64) -> Vec<u8> {
    if sigma <= 0.01 || width == 0 || height == 0 {
        return pixels.to_vec();
    }
    let ideal = (4.0 * sigma * sigma + 1.0).sqrt();
    let mut lo = ideal.floor() as usize;
    if lo.is_multiple_of(2) {
        lo = lo.saturating_sub(1);
    }
    lo = lo.max(1);
    let hi = lo + 2;
    let m = ((12.0 * sigma * sigma - 3.0 * (lo * lo) as f64 - 12.0 * lo as f64 - 9.0)
        / (-4.0 * lo as f64 - 4.0))
        .round()
        .clamp(0.0, 3.0) as usize;
    let mut current = pixels.to_vec();
    for pass in 0..3 {
        current = reference_box_blur(
            &current,
            width,
            height,
            (if pass < m { lo } else { hi }) / 2,
        );
    }
    clamp_premultiplied(&mut current);
    current
}

fn synthetic(width: u32, height: u32) -> Vec<u8> {
    let mut seed = 0x485b_03b7u32;
    let mut pixels = vec![0; width as usize * height as usize * 4];
    for p in pixels.as_chunks_mut::<4>().0 {
        seed = hash32(seed.wrapping_add(0x9e37_79b9));
        let bytes = seed.to_le_bytes();
        p[3] = bytes[3];
        for c in 0..3 {
            p[c] = bytes[c].min(p[3]);
        }
    }
    pixels
}

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = options()?;
    let pixels = if let Some(path) = &opts.input {
        let image = image::open(path)?.to_rgba8();
        let mut pixels = image::imageops::resize(
            &image,
            opts.width,
            opts.height,
            image::imageops::FilterType::Triangle,
        )
        .into_raw();
        premultiply(&mut pixels);
        pixels
    } else {
        synthetic(opts.width, opts.height)
    };
    let input_hash = format!("{:x}", Sha256::digest(&pixels));
    let width = opts.width as usize;
    let height = opts.height as usize;
    for sigma in opts.sigmas {
        let modes: &[&str] = if opts.mode == "compare" {
            &["reference", "optimized"]
        } else {
            &[&opts.mode]
        };
        let mut reference = None;
        for &mode in modes {
            let run = || {
                if mode == "reference" {
                    reference_gaussian_blur(black_box(&pixels), width, height, black_box(sigma))
                } else {
                    gaussian_blur(black_box(&pixels), width, height, black_box(sigma))
                }
            };
            for _ in 0..opts.warmup {
                black_box(run());
            }
            let mut elapsed_ms = Vec::with_capacity(opts.iterations);
            let mut output_hash = String::new();
            let mut compared = false;
            for iteration in 0..opts.iterations {
                let start = Instant::now();
                let output = run();
                elapsed_ms.push(start.elapsed().as_secs_f64() * 1000.0);
                let hash = format!("{:x}", Sha256::digest(&output));
                if iteration != 0 {
                    assert_eq!(output_hash, hash, "Output changed between iterations");
                }
                output_hash = hash;
                if mode == "optimized"
                    && let Some(reference) = &reference
                {
                    assert_eq!(&output, reference, "Reference mismatch at sigma {sigma}");
                    compared = true;
                }
                if opts.mode == "compare" && mode == "reference" && iteration + 1 == opts.iterations
                {
                    reference = Some(output);
                }
            }
            println!(
                "{}",
                serde_json::json!({
                    "mode": mode,
                    "input_kind": if opts.input.is_some() { "image_resized_triangle" } else { "synthetic_random_premultiplied" },
                    "input_path": opts.input,
                    "width": width,
                    "height": height,
                    "sigma": sigma,
                    "warmup": opts.warmup,
                    "elapsed_ms": elapsed_ms,
                    "input_sha256": input_hash,
                    "output_sha256": output_hash,
                    "exact_reference_comparison": compared,
                    "memory_note": "Compare mode retains the reference output; use separate reference/optimized processes for peak-memory measurements. Input decoding/resizing is outside the timed kernel but included in process memory."
                })
            );
        }
    }
    Ok(())
}
