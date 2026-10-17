//! Sensor-space RAW processing, before white balance, the camera matrix or quantization.
use anyhow::{Context, Result};
use rawloader::{RawImage, RawImageData};
use rayon::prelude::*;

const CLIPPED: f32 = 0.995;
const CELL: usize = 16;

#[derive(Clone, Copy, Default)]
struct ColorCell {
    color: [f32; 4],
    valid: bool,
}

struct RecoveryGrid {
    width: usize,
    height: usize,
    cells: Vec<ColorCell>,
}

pub(super) struct Sensor<'a> {
    raw: &'a RawImage,
    denoised: Option<Vec<f32>>,
    active: [bool; 4],
    recovery: Option<RecoveryGrid>,
}

fn allocate<T: Clone>(length: usize, value: T) -> Result<Vec<T>> {
    let mut data = Vec::new();
    data.try_reserve_exact(length)
        .context("RAW 线性显影缓冲区内存不足")?;
    data.resize(length, value);
    Ok(data)
}

fn normalized(raw: &RawImage, x: usize, y: usize, color: usize) -> f32 {
    let index = (y * raw.width + x) * raw.cpp + if raw.cpp == 3 { color } else { 0 };
    let value = match &raw.data {
        RawImageData::Integer(data) => data[index] as f32,
        RawImageData::Float(data) => data[index],
    };
    let black = raw.blacklevels[color] as f32;
    let white = raw.whitelevels[color] as f32;
    ((value - black) / (white - black).max(1.)).max(0.)
}

impl<'a> Sensor<'a> {
    pub(super) fn new(raw: &'a RawImage, noise: f32, highlights: bool) -> Result<Self> {
        let mut active = [false; 4];
        if raw.cpp == 3 {
            active[..3].fill(true);
        } else {
            for y in 0..raw.cfa.height {
                for x in 0..raw.cfa.width {
                    active[raw.cfa.color_at(y, x)] = true;
                }
            }
        }
        let mut sensor = Self {
            raw,
            denoised: None,
            active,
            recovery: None,
        };
        if noise > 0. {
            sensor.denoised = Some(sensor.denoise(noise)?);
        }
        if highlights {
            sensor.recovery = Some(sensor.recovery_grid()?);
        }
        Ok(sensor)
    }

    /// Normalized linear sensor intensity, retaining values above the white level.
    pub(super) fn sample(&self, x: usize, y: usize, color: usize) -> f32 {
        self.denoised.as_ref().map_or_else(
            || normalized(self.raw, x, y, color),
            |data| {
                data[(y * self.raw.width + x) * self.raw.cpp
                    + if self.raw.cpp == 3 { color } else { 0 }]
            },
        )
    }

    pub(super) fn is_active(&self, color: usize) -> bool {
        self.active[color]
    }

    fn denoise(&self, amount: f32) -> Result<Vec<f32>> {
        let raw = self.raw;
        let length = raw
            .width
            .checked_mul(raw.height)
            .and_then(|n| n.checked_mul(raw.cpp))
            .context("RAW 降噪缓冲区尺寸溢出")?;
        let mut output = allocate(length, 0_f32)?;
        let step_x = if raw.cpp == 3 { 1 } else { raw.cfa.width } as isize;
        let step_y = if raw.cpp == 3 { 1 } else { raw.cfa.height } as isize;
        output
            .par_chunks_exact_mut(raw.width * raw.cpp)
            .enumerate()
            .for_each(|(y, row)| {
                for (index, result) in row.iter_mut().enumerate() {
                    let x = index / raw.cpp;
                    let color = if raw.cpp == 3 {
                        index % raw.cpp
                    } else {
                        raw.cfa.color_at(y, x)
                    };
                    let center = normalized(raw, x, y, color);
                    if center >= CLIPPED {
                        *result = center;
                        continue;
                    }
                    // A 3x3 bilateral kernel on each CFA phase. No red/blue photosite
                    // is averaged with its green neighbor; RGB sensors use each plane.
                    let sigma = 0.002 + amount * 0.045 * (center + 0.01).sqrt();
                    let mut sum = 0.;
                    let mut weights = 0.;
                    for dy in -1_isize..=1 {
                        for dx in -1_isize..=1 {
                            let nx = x as isize + dx * step_x;
                            let ny = y as isize + dy * step_y;
                            if nx < 0
                                || ny < 0
                                || nx >= raw.width as isize
                                || ny >= raw.height as isize
                            {
                                continue;
                            }
                            let value = normalized(raw, nx as usize, ny as usize, color);
                            if value >= CLIPPED {
                                continue;
                            }
                            let distance = (value - center) / sigma;
                            let range = 1. / (1. + distance * distance);
                            let spatial =
                                (if dx == 0 { 2. } else { 1. }) * (if dy == 0 { 2. } else { 1. });
                            let weight = spatial * range * range;
                            sum += value * weight;
                            weights += weight;
                        }
                    }
                    let filtered = if weights > 0. { sum / weights } else { center };
                    *result = center + amount * (filtered - center);
                }
            });
        Ok(output)
    }

    fn recovery_grid(&self) -> Result<RecoveryGrid> {
        let width = self.raw.width.div_ceil(CELL);
        let height = self.raw.height.div_ceil(CELL);
        let mut cells = allocate(
            width.checked_mul(height).context("RAW 高光网格尺寸溢出")?,
            ColorCell::default(),
        )?;
        cells.par_iter_mut().enumerate().for_each(|(index, cell)| {
            let x0 = index % width * CELL;
            let y0 = index / width * CELL;
            let mut total = [0.; 4];
            let mut counts = [0_usize; 4];
            let mut clipped = false;
            for y in y0..(y0 + CELL).min(self.raw.height) {
                for x in x0..(x0 + CELL).min(self.raw.width) {
                    let colors = if self.raw.cpp == 3 {
                        0..3
                    } else {
                        let color = self.raw.cfa.color_at(y, x);
                        color..color + 1
                    };
                    for color in colors {
                        let value = self.sample(x, y, color);
                        clipped |= value >= 0.98;
                        total[color] += value;
                        counts[color] += 1;
                    }
                }
            }
            cell.valid = !clipped && (0..4).all(|color| !self.active[color] || counts[color] > 0);
            if cell.valid {
                for color in 0..4 {
                    cell.color[color] = total[color] / counts[color].max(1) as f32;
                }
            }
        });
        Ok(RecoveryGrid {
            width,
            height,
            cells,
        })
    }

    /// Reconstruct only clipped camera channels from nearby unclipped chromaticity.
    /// The surviving channel carries the detail; completely clipped light has no
    /// measured detail to reconstruct and is deliberately left alone.
    pub(super) fn recover(&self, x: usize, y: usize, camera: &mut [f32; 4]) {
        let Some(grid) = &self.recovery else {
            return;
        };
        let clipped =
            std::array::from_fn::<_, 4, _>(|color| self.active[color] && camera[color] >= CLIPPED);
        if !clipped.iter().any(|&value| value) {
            return;
        }
        let anchor = (0..4)
            .filter(|&color| self.active[color] && !clipped[color] && camera[color] > 0.01)
            .max_by(|&a, &b| camera[a].total_cmp(&camera[b]));
        let Some(anchor) = anchor else {
            return;
        };
        let original = *camera;
        let cx = (x / CELL) as isize;
        let cy = (y / CELL) as isize;
        let mut sum = [0.; 4];
        let mut weights = [0.; 4];
        for radius in [1_isize, 2, 4, 8, 16, 32] {
            for (dx, dy) in [
                (-radius, -radius),
                (0, -radius),
                (radius, -radius),
                (-radius, 0),
                (radius, 0),
                (-radius, radius),
                (0, radius),
                (radius, radius),
            ] {
                let nx = cx + dx;
                let ny = cy + dy;
                if nx < 0 || ny < 0 || nx >= grid.width as isize || ny >= grid.height as isize {
                    continue;
                }
                let cell = grid.cells[ny as usize * grid.width + nx as usize];
                if !cell.valid || cell.color[anchor] <= 0.01 {
                    continue;
                }
                let mut error = 0.;
                for color in 0..4 {
                    if self.active[color] && !clipped[color] && color != anchor {
                        let observed = original[color] / original[anchor];
                        let candidate = cell.color[color] / cell.color[anchor];
                        error += (observed - candidate).powi(2);
                    }
                }
                let weight = 1. / ((dx * dx + dy * dy) as f32 * (1. + 16. * error));
                for color in 0..4 {
                    if clipped[color] {
                        sum[color] += (cell.color[color] / cell.color[anchor]).min(16.) * weight;
                        weights[color] += weight;
                    }
                }
            }
            if weights.iter().any(|&weight| weight > 0.) {
                break;
            }
        }
        for color in 0..4 {
            if clipped[color] && weights[color] > 0. {
                camera[color] =
                    camera[color].max((original[anchor] * sum[color] / weights[color]).min(16.));
            }
        }
    }
}

/// Stable sub-step rounding, shared by preview and import at sensor coordinates.
pub(super) fn quantize(value: f32, x: usize, y: usize, color: usize) -> u8 {
    let key = (x as u32).wrapping_mul(0x9e3779b9)
        ^ (y as u32).wrapping_mul(0x85ebca6b)
        ^ (color as u32).wrapping_mul(0xc2b2ae35);
    let mut hash = key ^ (key >> 16);
    hash = hash.wrapping_mul(0x7feb352d);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x846ca68b);
    hash ^= hash >> 16;
    let noise = (hash >> 8) as f32 / 16777216. - 0.5;
    (value.clamp(0., 1.) * 255. + noise).round().clamp(0., 255.) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sensor(
        cpp: usize,
        width: usize,
        height: usize,
        sample: impl Fn(usize, usize, usize) -> u16,
    ) -> RawImage {
        let cfa = rawloader::CFA::new(if cpp == 1 { "RGGB" } else { "" });
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                for component in 0..cpp {
                    let color = if cpp == 1 {
                        cfa.color_at(y, x)
                    } else {
                        component
                    };
                    data.push(sample(x, y, color));
                }
            }
        }
        RawImage {
            make: "Synthetic".into(),
            model: "Linear sensor oracle".into(),
            clean_make: "Synthetic".into(),
            clean_model: "Linear sensor oracle".into(),
            width,
            height,
            cpp,
            wb_coeffs: [1.; 4],
            whitelevels: [10000; 4],
            blacklevels: [0; 4],
            xyz_to_cam: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.], [0.; 3]],
            cfa,
            crops: [0; 4],
            blackareas: Vec::new(),
            orientation: rawloader::Orientation::Normal,
            data: RawImageData::Integer(data),
        }
    }

    #[test]
    fn denoise_keeps_cfa_colors_separate_and_reduces_same_color_noise() {
        for cpp in [1, 3] {
            let raw = sensor(cpp, 48, 48, |x, y, color| {
                let base = [7000, 4000, 1000][color];
                let noise = if (x / 2 + y / 2) % 2 == 0 { 80 } else { -80 };
                (base + noise) as u16
            });
            let processed = Sensor::new(&raw, 1., false).unwrap();
            let mut before_error = 0.;
            let mut after_error = 0.;
            for y in 4..44 {
                for x in 4..44 {
                    let colors = if cpp == 3 {
                        0..3
                    } else {
                        let c = raw.cfa.color_at(y, x);
                        c..c + 1
                    };
                    for color in colors {
                        let expected = [0.7, 0.4, 0.1][color];
                        before_error += (normalized(&raw, x, y, color) - expected).powi(2);
                        after_error += (processed.sample(x, y, color) - expected).powi(2);
                        assert!((processed.sample(x, y, color) - expected).abs() < 0.01);
                    }
                }
            }
            assert!(
                after_error < before_error * 0.4,
                "cpp={cpp}: {after_error}/{before_error}"
            );
        }
    }

    #[test]
    fn zero_denoise_shares_source_and_edges_and_clipping_survive() {
        let raw = sensor(1, 32, 32, |x, _, color| {
            if x < 16 {
                [8000, 5000, 2000][color]
            } else {
                10000
            }
        });
        let unchanged = Sensor::new(&raw, 0., false).unwrap();
        assert!(unchanged.denoised.is_none());
        let filtered = Sensor::new(&raw, 1., false).unwrap();
        assert_eq!(filtered.sample(16, 16, raw.cfa.color_at(16, 16)), 1.);
        assert!((filtered.sample(14, 14, 0) - 0.8).abs() < 0.001);
    }

    #[test]
    fn highlight_recovery_reconstructs_only_sensor_clipped_channels() {
        for cpp in [1, 3] {
            let raw = sensor(cpp, 96, 96, |x, y, color| {
                let bright = (32..64).contains(&x) && (32..64).contains(&y);
                let intensity = if bright { 1.5 } else { 0.8 };
                (intensity * [1., 0.5, 0.25][color] * 10000_f32).min(10000.) as u16
            });
            let processed = Sensor::new(&raw, 0., true).unwrap();
            let mut camera = [1., 0.75, 0.375, 0.];
            processed.recover(48, 48, &mut camera);
            assert!((camera[0] - 1.5).abs() < 0.001);
            assert_eq!(&camera[1..3], &[0.75, 0.375]);
            let mut all_clipped = [1.; 4];
            processed.recover(48, 48, &mut all_clipped);
            assert_eq!(all_clipped, [1.; 4]);
            let mut clean = [0.8, 0.4, 0.2, 0.];
            processed.recover(8, 8, &mut clean);
            assert_eq!(clean, [0.8, 0.4, 0.2, 0.]);
        }
    }

    #[test]
    fn quantization_is_stable_opaque_and_preserves_sub_step_average() {
        let mut worst_error = 0_f32;
        for y in 0..400 {
            let value = 0.30 + 0.02 * (y as f32 + 0.5) / 400.;
            let mean = (0..300)
                .map(|x| quantize(value, x, y, 0) as f32)
                .sum::<f32>()
                / 300.;
            worst_error = worst_error.max((mean - value * 255.).abs());
        }
        assert!(worst_error < 0.2, "row error {worst_error}");
        for y in 0..64 {
            for x in 0..64 {
                assert_eq!(quantize(0.5, x, y, 0), quantize(0.5, x, y, 0));
                assert_eq!(quantize(0., x, y, 0), 0);
                assert_eq!(quantize(1., x, y, 0), 255);
                assert!((127..=128).contains(&quantize(0.5, x, y, 0)));
            }
        }
    }
}
