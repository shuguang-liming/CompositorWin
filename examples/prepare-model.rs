//! Prepare a portable float32 graph; CPU-specific packing still happens at runtime.
use anyhow::{Context, Result, ensure};
use flate2::{Compression, GzBuilder, read::GzDecoder};
use sha2::{Digest, Sha256};
use std::{io::Cursor, path::PathBuf, time::Instant};
use tract_onnx::prelude::*;

const SIDE: usize = 320;
const SOURCE_SHA256: &str = "309c8469258dda742793dce0ebea8e6dd393174f89934733ecc8b14c76f4ddd8";

fn prepare(model: &TypedModel) -> Result<Vec<u8>> {
    let encoder = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::best());
    let encoder = tract_nnef::nnef().write_to_tar_with_config(model, encoder, false, true)?;
    Ok(encoder.finish()?)
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let input =
        PathBuf::from(args.next().context(
            "Usage: prepare-segmentation-model INPUT.onnx OUTPUT.nnef.tar.gz REPORT.json",
        )?);
    let output = PathBuf::from(args.next().context("Output model path is required")?);
    let report = PathBuf::from(
        args.next()
            .context("Verification report path is required")?,
    );
    ensure!(args.next().is_none(), "Unexpected argument");
    let bytes = std::fs::read(&input).context("Read original ONNX model")?;
    let source_hash = format!("{:x}", Sha256::digest(&bytes));
    ensure!(
        source_hash == SOURCE_SHA256,
        "Unexpected source model SHA256"
    );

    let start = Instant::now();
    let mut inference = tract_onnx::onnx()
        .model_for_read(&mut Cursor::new(&bytes))?
        .with_input_fact(0, f32::fact([1, 3, SIDE, SIDE]).into())?;
    let primary = inference.output_outlets()?[0];
    inference.select_output_outlets(&[primary])?;
    // Serialize before CPU-specific code generation, retaining every float32 weight.
    let original_typed = inference.into_typed()?;
    let typed = original_typed.clone().into_decluttered()?;
    let node_count = typed.nodes().len();
    let prepared = prepare(&typed)?;
    ensure!(
        prepare(&typed)? == prepared,
        "Repeated model serialization produced different bytes"
    );
    let prepare_millis = start.elapsed().as_millis();

    let start = Instant::now();
    let original = original_typed.into_optimized()?.into_runnable()?;
    let original_optimize_millis = start.elapsed().as_millis();
    let start = Instant::now();
    let round_trip = tract_nnef::nnef()
        .model_for_read(&mut GzDecoder::new(prepared.as_slice()))?
        .into_optimized()?
        .into_runnable()?;
    let nnef_load_millis = start.elapsed().as_millis();

    let plane = SIDE * SIDE;
    let input_values: Vec<f32> = (0..plane * 3)
        .map(|i| {
            let channel = i / plane;
            let x = i % SIDE;
            let y = i / SIDE % SIDE;
            let byte = match channel {
                0 => x * 255 / (SIDE - 1),
                1 => y * 255 / (SIDE - 1),
                _ => (x * 11 + y * 17) % 256,
            };
            (byte as f32 / 255. - [0.485, 0.456, 0.406][channel]) / [0.229, 0.224, 0.225][channel]
        })
        .collect();
    let tensor = Tensor::from_shape(&[1, 3, SIDE, SIDE], &input_values)?;
    let expected = original.run(tvec!(tensor.clone().into()))?;
    let actual = round_trip.run(tvec!(tensor.into()))?;
    let expected = expected[0].to_plain_array_view::<f32>()?;
    let actual = actual[0].to_plain_array_view::<f32>()?;
    ensure!(
        expected.shape() == actual.shape(),
        "Round-trip output shape changed"
    );
    let unequal_values = expected
        .iter()
        .zip(actual.iter())
        .filter(|(a, b)| a.to_bits() != b.to_bits())
        .count();
    let max_error = expected
        .iter()
        .zip(actual.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    ensure!(
        unequal_values == 0,
        "Round-trip changed {unequal_values} float32 output values; maximum error {max_error}"
    );
    let prepared_hash = format!("{:x}", Sha256::digest(&prepared));
    let result = serde_json::json!({
        "status": "passed",
        "source": input,
        "sourceBytes": bytes.len(),
        "sourceSha256": source_hash,
        "prepared": output,
        "preparedBytes": prepared.len(),
        "preparedSha256": prepared_hash,
        "tractVersion": "0.23.8",
        "inputShape": [1, 3, SIDE, SIDE],
        "outputShape": actual.shape(),
        "outputValues": actual.len(),
        "outputDifferentFloat32Values": unequal_values,
        "outputMaximumAbsoluteError": max_error,
        "declutteredNodes": node_count,
        "weights": "unchanged float32",
        "cpuCodeGeneration": "runtime",
        "deterministicSerialization": true,
        "prepareMillis": prepare_millis,
        "originalOptimizeMillis": original_optimize_millis,
        "nnefLoadMillis": nnef_load_millis,
    });
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Some(parent) = report.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output, prepared)?;
    std::fs::write(&report, serde_json::to_vec_pretty(&result)?)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
