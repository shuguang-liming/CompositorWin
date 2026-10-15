//! Offline U²-Net salient subject segmentation. Inference is tract CPU, not a color-distance heuristic.
use crate::model::{Raster, Selection};
use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use rayon::prelude::*;
use std::{collections::VecDeque, path::Path, sync::Arc, time::Instant};
use tract_nnef::prelude::*;
use tract_onnx::tract_hir::prelude::InferenceModelExt;

pub const MODEL_SIDE: usize = 320;
pub const MODEL_SHA256: &str = "309c8469258dda742793dce0ebea8e6dd393174f89934733ecc8b14c76f4ddd8";
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
            multithread::multithread_tract_scope(multithread::Executor::RayonGlobal, || {
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
    let sample_row = |(y, row): (usize, &mut [u8])| {
        for (x, out) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
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
                out[c] = (color[c] / (nx * ny) as f32).round().clamp(0., 255.) as u8;
            }
        }
    };
    // Each row keeps the original sample and summation order. Small images stay
    // inline; larger inputs share the application's existing Rayon workers.
    if low_width as u64 * low_height as u64 * nx as u64 * ny as u64 >= 64 * 1024
        && low_height >= 16
        && rayon::current_num_threads() > 1
    {
        rgb.par_chunks_exact_mut(low_width * 3)
            .enumerate()
            .for_each(sample_row);
    } else {
        rgb.chunks_exact_mut(low_width * 3)
            .enumerate()
            .for_each(sample_row);
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

impl Segmentation {
    pub fn selection(&self) -> Selection {
        Selection::from_coverage(self.width, self.height, self.alpha.clone())
    }

    pub fn mask(&self) -> Raster {
        Raster::from_rgba(
            self.width,
            self.height,
            self.alpha.iter().flat_map(|&a| [a, a, a, 255]).collect(),
        )
    }

    pub fn foreground(&self, source: &Raster) -> Result<Raster> {
        ensure!(
            source.width == self.width && source.height == self.height,
            "分割结果与源图像尺寸不一致"
        );
        let mut data = source.to_rgba();
        let apply_alpha = |(pixels, alpha): (&mut [u8], &[u8])| {
            for (p, &a) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(alpha) {
                p[3] = ((p[3] as u16 * a as u16 + 127) / 255) as u8;
            }
        };
        if self.alpha.len() >= 64 * 1024 && rayon::current_num_threads() > 1 {
            data.par_chunks_mut(4 * 1024)
                .zip(self.alpha.par_chunks(1024))
                .for_each(apply_alpha);
        } else {
            data.chunks_mut(4 * 1024)
                .zip(self.alpha.chunks(1024))
                .for_each(apply_alpha);
        }
        Ok(Raster::from_rgba(self.width, self.height, data))
    }

    /// Object selection keeps the inferred connected subject component under the clicked point.
    pub fn object_at(&self, point: [u32; 2]) -> Result<Selection> {
        ensure!(
            point[0] < self.width && point[1] < self.height,
            "对象选择位置位于图像外"
        );
        let seed = (point[1] * self.width + point[0]) as usize;
        ensure!(self.alpha[seed] >= 96, "此位置没有识别到前景对象");
        let mut selected = vec![0; self.alpha.len()];
        let mut visited = vec![false; self.alpha.len()];
        visited[seed] = true;
        let mut queue = VecDeque::from([seed]);
        while let Some(p) = queue.pop_front() {
            selected[p] = self.alpha[p];
            let x = p % self.width as usize;
            let y = p / self.width as usize;
            for (nx, ny) in [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ] {
                if nx < self.width as usize && ny < self.height as usize {
                    let n = ny * self.width as usize + nx;
                    if !visited[n] && self.alpha[n] >= 32 {
                        visited[n] = true;
                        queue.push_back(n);
                    }
                }
            }
        }
        Ok(Selection::from_coverage(self.width, self.height, selected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_inference_preserves_all_fused_float_bits_and_masks() {
        let engine = SegmentationEngine::load_default().unwrap();
        let global_threads = rayon::current_num_threads();
        let mut thread_counts = vec![1, 4, 8, 12, global_threads];
        thread_counts.sort_unstable();
        thread_counts.dedup();
        let workers: Vec<_> = thread_counts
            .into_iter()
            .map(|threads| {
                let pool = (threads != global_threads).then(|| {
                    rayon::ThreadPoolBuilder::new()
                        .num_threads(threads)
                        .build()
                        .unwrap()
                });
                (threads, pool)
            })
            .collect();
        for (width, height, transparent) in [(96, 64, false), (320, 320, true)] {
            let image = Raster::from_rgba(
                width,
                height,
                (0..width * height)
                    .flat_map(|i| {
                        [
                            (i * 11 % 256) as u8,
                            (i * 17 % 256) as u8,
                            (i * 31 % 256) as u8,
                            if transparent {
                                (i * 7 % 256) as u8
                            } else {
                                255
                            },
                        ]
                    })
                    .collect(),
            );
            let input =
                Tensor::from_shape(&[1, 3, MODEL_SIDE, MODEL_SIDE], &preprocess(&image)).unwrap();
            let serial =
                multithread::multithread_tract_scope(multithread::Executor::SingleThread, || {
                    engine.model.run(tvec!(input.clone().into()))
                })
                .unwrap();
            let serial = serial[0].to_plain_array_view::<f32>().unwrap();
            assert_eq!(serial.len(), MODEL_SIDE * MODEL_SIDE);
            for (threads, pool) in &workers {
                let run = || {
                    multithread::multithread_tract_scope(multithread::Executor::RayonGlobal, || {
                        engine.model.run(tvec!(input.clone().into()))
                    })
                };
                // The default count runs on the actual production global registry.
                let actual = match pool {
                    Some(pool) => pool.install(run),
                    None => run(),
                }
                .unwrap();
                let actual = actual[0].to_plain_array_view::<f32>().unwrap();
                assert_eq!(serial.shape(), actual.shape());
                let different = serial
                    .iter()
                    .zip(actual.iter())
                    .filter(|(a, b)| a.to_bits() != b.to_bits())
                    .count();
                assert_eq!(
                    different,
                    0,
                    "Parallel inference changed fused output for {width}x{height}, {threads} workers (production global: {})",
                    pool.is_none()
                );
            }
        }
    }

    #[test]
    fn parallel_input_rows_preserve_float_bits_for_dense_sparse_and_transparent_images() {
        let serial = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let parallel = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let dense = Raster::from_rgba(
            513,
            1291,
            (0..513 * 1291)
                .flat_map(|i| {
                    [
                        (i * 11 % 256) as u8,
                        (i * 17 % 256) as u8,
                        (i * 31 % 256) as u8,
                        (i * 7 % 256) as u8,
                    ]
                })
                .collect(),
        );
        let mut sparse = Raster::solid(4097, 3073, [137, 48, 211, 128]);
        sparse.set(127, 128, [3, 88, 223, 255]);
        sparse.set(4096, 3072, [191, 102, 17, 64]);
        for image in [
            Raster::solid(1, 41, [0, 0, 0, 0]),
            Raster::solid(96, 64, [231, 123, 57, 255]),
            dense,
            sparse,
            Raster::blank(1301, 19),
        ] {
            let expected = serial.install(|| preprocess(&image));
            let actual = parallel.install(|| preprocess(&image));
            assert_eq!(expected.len(), 3 * MODEL_SIDE * MODEL_SIDE);
            assert!(
                expected
                    .iter()
                    .zip(&actual)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "Input changed for {}x{}",
                image.width,
                image.height
            );
        }
    }

    #[test]
    fn parallel_foreground_keeps_rgb_and_exact_alpha_across_chunk_edges() {
        let parallel = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let width = 257;
        let height = 259;
        let pixels: Vec<u8> = (0..width * height)
            .flat_map(|i| {
                [
                    (i * 11 % 256) as u8,
                    (i * 17 % 256) as u8,
                    (i * 31 % 256) as u8,
                    (i % 256) as u8,
                ]
            })
            .collect();
        let source = Raster::from_rgba(width, height, pixels.clone());
        let alpha: Vec<u8> = (0..width * height)
            .map(|i| (255 - (i * 13 % 256)) as u8)
            .collect();
        let mut expected = pixels;
        for (pixel, &amount) in expected.as_chunks_mut::<4>().0.iter_mut().zip(&alpha) {
            pixel[3] = ((pixel[3] as u16 * amount as u16 + 127) / 255) as u8;
        }
        let result = Segmentation {
            width,
            height,
            alpha,
            inference_millis: 0,
        };
        assert_eq!(
            parallel
                .install(|| result.foreground(&source))
                .unwrap()
                .to_rgba(),
            expected
        );
    }

    #[test]
    fn embedded_nnef_model_preserves_onnx_mask_and_external_model_loading() {
        let original = SegmentationEngine::load(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/u2netp.onnx"),
        )
        .unwrap();
        let embedded = SegmentationEngine::load_default().unwrap();
        let width = 96;
        let height = 64;
        let pixels = (0..height)
            .flat_map(|y| {
                (0..width).flat_map(move |x| {
                    [
                        (x * 255 / (width - 1)) as u8,
                        (y * 255 / (height - 1)) as u8,
                        ((x * 11 + y * 17) % 256) as u8,
                        ((x * 3 + y * 5) % 256) as u8,
                    ]
                })
            })
            .collect();
        let source = Raster::from_rgba(width, height, pixels);
        let input =
            Tensor::from_shape(&[1, 3, MODEL_SIDE, MODEL_SIDE], &preprocess(&source)).unwrap();
        let old_output = original.model.run(tvec!(input.clone().into())).unwrap();
        let new_output = embedded.model.run(tvec!(input.into())).unwrap();
        let old_values = old_output[0].to_plain_array_view::<f32>().unwrap();
        let new_values = new_output[0].to_plain_array_view::<f32>().unwrap();
        assert_eq!(old_values.len(), MODEL_SIDE * MODEL_SIDE);
        assert_eq!(old_values.shape(), new_values.shape());
        assert!(
            old_values
                .iter()
                .zip(new_values.iter())
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        let reference = original.predict(&source).unwrap();
        let result = embedded.predict(&source).unwrap();
        assert_eq!(result.width, reference.width);
        assert_eq!(result.height, reference.height);
        assert_eq!(result.alpha, reference.alpha);
        assert_eq!(
            result.foreground(&source).unwrap().to_rgba(),
            reference.foreground(&source).unwrap().to_rgba()
        );
    }

    #[test]
    fn original_alpha_is_preserved_by_background_removal() {
        let result = Segmentation {
            width: 2,
            height: 1,
            alpha: vec![128, 255],
            inference_millis: 0,
        };
        let source = Raster::from_rgba(2, 1, vec![100, 150, 200, 128, 10, 20, 30, 0]);
        assert_eq!(
            result.foreground(&source).unwrap().get(0, 0),
            [100, 150, 200, 64]
        );
        assert_eq!(result.foreground(&source).unwrap().get(1, 0)[3], 0);
    }
}
