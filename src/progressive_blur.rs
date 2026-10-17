//! Spatially varying Gaussian blur in premultiplied RGBA, in top-left image coordinates.
//!
//! The radius field follows Compositor 1.5.3. Core Image's private variable-blur kernel is
//! replaced by a Gaussian pyramid; small radii use the Gaussian directly to retain detail.
use super::{boolean, number};
use crate::pixels;
use rayon::prelude::*;
use serde_json::Value;

struct Level {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    scale: f64,
    variance: f64,
}

fn buffer(length: usize) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(length)
        .map_err(|_| "渐进模糊所需内存不足，请减小图片后重试".to_owned())?;
    result.resize(length, 0);
    Ok(result)
}

fn point(settings: &Value, internal: &str, public: &str, fallback: [f64; 2]) -> [f64; 2] {
    let value = settings.get(internal).or_else(|| settings.get(public));
    let Some(value) = value else { return fallback };
    std::array::from_fn(|i| {
        value
            .get(i)
            .or_else(|| value.get(["x", "y"][i]))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .unwrap_or(fallback[i])
            .clamp(-1e9, 1e9)
    })
}

/// A radius is local to the output pixel, rather than a blend between sharp and fully blurred.
pub(super) fn apply(
    source: &[u8],
    width: usize,
    height: usize,
    settings: &Value,
) -> Result<Vec<u8>, String> {
    let length = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "渐进模糊图像尺寸超出上限".to_owned())?;
    if length != source.len() {
        return Err("渐进模糊像素数据长度不正确".into());
    }
    if width == 0 || height == 0 {
        return Ok(Vec::new());
    }
    let mut from = number(settings, "progressiveFrom", 0.).clamp(0., 250.);
    let mut to = number(settings, "progressiveTo", 20.).clamp(0., 250.);
    if boolean(settings, "progressiveInverted", false) {
        std::mem::swap(&mut from, &mut to);
    }
    if from == to {
        return Ok(pixels::gaussian_blur(source, width, height, from));
    }
    let start = point(
        settings,
        "_progressiveStartPixels",
        "progressiveStart",
        [width as f64 / 2., 0.],
    );
    let mut end = point(
        settings,
        "_progressiveEndPixels",
        "progressiveEnd",
        [width as f64 / 2., height as f64],
    );
    if (end[0] - start[0]).hypot(end[1] - start[1]) < 1. {
        end = [start[0], start[1] + 1.];
    }
    let delta = [end[0] - start[0], end[1] - start[1]];
    let distance = delta[0].hypot(delta[1]);
    let radial = boolean(settings, "progressiveRadial", false);
    let radius = |x: usize, y: usize| {
        let offset = [x as f64 + 0.5 - start[0], y as f64 + 0.5 - start[1]];
        let share = if radial {
            offset[0].hypot(offset[1]) / distance
        } else {
            (offset[0] * delta[0] + offset[1] * delta[1]) / (distance * distance)
        }
        .clamp(0., 1.);
        from + (to - from) * share
    };

    // The pyramid halves each nontrivial dimension at every step, so storage stays
    // bounded by a few source-sized buffers, independent of the largest radius.
    let strongest = from.max(to);
    let mut levels = Vec::new();
    if strongest >= 1. {
        levels.push(Level {
            pixels: gaussian_one(source, width, height)?,
            width,
            height,
            scale: 1.,
            variance: 1.,
        });
        while levels.last().unwrap().variance < strongest * strongest {
            let previous = levels.last().unwrap();
            if previous.width == 1 && previous.height == 1 {
                break;
            }
            let w = previous.width.div_ceil(2);
            let h = previous.height.div_ceil(2);
            let mut reduced = buffer(w * h * 4)?;
            reduced
                .par_chunks_exact_mut(w * 4)
                .enumerate()
                .for_each(|(y, row)| {
                    for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                        for (c, value) in pixel.iter_mut().enumerate() {
                            let mut sum = 0u32;
                            for yy in [y * 2, (y * 2 + 1).min(previous.height - 1)] {
                                for xx in [x * 2, (x * 2 + 1).min(previous.width - 1)] {
                                    sum += u32::from(
                                        previous.pixels[(yy * previous.width + xx) * 4 + c],
                                    );
                                }
                            }
                            *value = ((sum + 2) / 4) as u8;
                        }
                    }
                });
            let scale = previous.scale * 2.;
            // A 2-pixel area reduction contributes variance 1/4, followed by a
            // sigma-one Gaussian at the new spacing (variance 4 in the old grid).
            let variance = previous.variance + 4.25 * previous.scale * previous.scale;
            levels.push(Level {
                pixels: gaussian_one(&reduced, w, h)?,
                width: w,
                height: h,
                scale,
                variance,
            });
        }
    }
    let mut output = buffer(length)?;
    output
        .par_chunks_exact_mut(width * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let sigma = radius(x, y);
                let result = if sigma < 1. {
                    small_gaussian(source, width, height, x, y, sigma)
                } else {
                    let variance = sigma * sigma;
                    let upper = levels.partition_point(|level| level.variance < variance);
                    if upper == 0 {
                        sample(&levels[0], x, y)
                    } else if upper == levels.len() {
                        sample(levels.last().unwrap(), x, y)
                    } else {
                        let lo = &levels[upper - 1];
                        let hi = &levels[upper];
                        let amount = (variance - lo.variance) / (hi.variance - lo.variance);
                        let a = sample(lo, x, y);
                        let b = sample(hi, x, y);
                        std::array::from_fn(|c| a[c] + (b[c] - a[c]) * amount)
                    }
                };
                for (value, component) in pixel.iter_mut().zip(result) {
                    *value = component.round().clamp(0., 255.) as u8;
                }
                for c in 0..3 {
                    pixel[c] = pixel[c].min(pixel[3]);
                }
            }
        });
    Ok(output)
}

/// Separable seven-tap sigma-one Gaussian, using a row buffer and fixed normalized weights.
fn gaussian_one(source: &[u8], width: usize, height: usize) -> Result<Vec<u8>, String> {
    let weights = [
        0.004433048,
        0.054005583,
        0.242036229,
        0.399050279,
        0.242036229,
        0.054005583,
        0.004433048,
    ];
    let mut horizontal = buffer(source.len())?;
    horizontal
        .par_chunks_exact_mut(width * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                for (c, value) in pixel.iter_mut().enumerate() {
                    let mut sum = 0.;
                    for (i, weight) in weights.iter().enumerate() {
                        let xx = (x + i).saturating_sub(3).min(width - 1);
                        sum += f64::from(source[(y * width + xx) * 4 + c]) * weight;
                    }
                    *value = sum.round() as u8;
                }
            }
        });
    let mut output = buffer(source.len())?;
    output
        .par_chunks_exact_mut(width * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                for (c, value) in pixel.iter_mut().enumerate() {
                    let mut sum = 0.;
                    for (i, weight) in weights.iter().enumerate() {
                        let yy = (y + i).saturating_sub(3).min(height - 1);
                        sum += f64::from(horizontal[(yy * width + x) * 4 + c]) * weight;
                    }
                    *value = sum.round() as u8;
                }
            }
        });
    Ok(output)
}

fn sample(level: &Level, x: usize, y: usize) -> [f64; 4] {
    let xx = ((x as f64 + 0.5) / level.scale - 0.5).clamp(0., (level.width - 1) as f64);
    let yy = ((y as f64 + 0.5) / level.scale - 0.5).clamp(0., (level.height - 1) as f64);
    let (x0, y0) = (xx.floor() as usize, yy.floor() as usize);
    let (fx, fy) = (xx - x0 as f64, yy - y0 as f64);
    std::array::from_fn(|c| {
        let row = |y| {
            f64::from(level.pixels[(y * level.width + x0) * 4 + c]) * (1. - fx)
                + f64::from(level.pixels[(y * level.width + (x0 + 1).min(level.width - 1)) * 4 + c])
                    * fx
        };
        row(y0) * (1. - fy) + row((y0 + 1).min(level.height - 1)) * fy
    })
}

fn small_gaussian(
    source: &[u8],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    sigma: f64,
) -> [f64; 4] {
    if sigma <= 0.1 {
        return std::array::from_fn(|c| f64::from(source[(y * width + x) * 4 + c]));
    }
    let radius = (sigma * 3.).ceil() as usize;
    let mut weights = [0.; 7];
    let mut total = 0.;
    for (i, weight) in weights.iter_mut().enumerate().take(radius * 2 + 1) {
        let distance = i as f64 - radius as f64;
        *weight = (-0.5 * distance * distance / (sigma * sigma)).exp();
        total += *weight;
    }
    let mut result = [0.; 4];
    for (j, wy) in weights.iter().enumerate().take(radius * 2 + 1) {
        let yy = (y + j).saturating_sub(radius).min(height - 1);
        for (i, wx) in weights.iter().enumerate().take(radius * 2 + 1) {
            let xx = (x + i).saturating_sub(radius).min(width - 1);
            let weight = wx * wy / (total * total);
            for c in 0..4 {
                result[c] += f64::from(source[(yy * width + xx) * 4 + c]) * weight;
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stripes() -> Vec<u8> {
        (0..200 * 200)
            .flat_map(|i| {
                let v = if (i % 200) % 4 < 2 { 0 } else { 255 };
                [v, v, v, 255]
            })
            .collect()
    }

    fn contrast(image: &[u8], x: usize, y: usize) -> u8 {
        let values: Vec<_> = (x..x + 8).map(|x| image[(y * 200 + x) * 4]).collect();
        values.iter().max().unwrap() - values.iter().min().unwrap()
    }

    #[test]
    fn upstream_linear_sharp_to_soft_and_two_nonzero_endpoints() {
        let mut settings = json!({"progressiveFrom":0,"progressiveTo":6,
            "progressiveStart":{"x":100,"y":20},"progressiveEnd":{"x":100,"y":180}});
        let image = apply(&stripes(), 200, 200, &settings).unwrap();
        assert!(contrast(&image, 96, 10) > 230);
        assert!(contrast(&image, 96, 100) < contrast(&image, 96, 30));
        assert!(contrast(&image, 96, 190) < 40);
        settings["progressiveFrom"] = json!(1.5);
        let image = apply(&stripes(), 200, 200, &settings).unwrap();
        assert!(contrast(&image, 96, 10) < 230);
        assert!(contrast(&image, 96, 10) > contrast(&image, 96, 190));
    }

    #[test]
    fn upstream_radial_and_inverted_keep_the_expected_center_sharp() {
        let mut settings = json!({"progressiveTo":6,"progressiveRadial":true,
            "progressiveStart":[100,100],"progressiveEnd":[160,100]});
        let image = apply(&stripes(), 200, 200, &settings).unwrap();
        assert!(contrast(&image, 96, 100) > 230);
        assert!(contrast(&image, 4, 4) < 40);
        settings["progressiveInverted"] = json!(true);
        let image = apply(&stripes(), 200, 200, &settings).unwrap();
        assert!(contrast(&image, 96, 100) < 40);
        assert!(contrast(&image, 4, 4) > 230);
    }

    #[test]
    fn local_radius_is_not_a_sharp_to_full_blur_alpha_mix() {
        let settings = json!({"progressiveTo":6,
            "progressiveStart":[100,20],"progressiveEnd":[100,180]});
        let source = stripes();
        let image = apply(&source, 200, 200, &settings).unwrap();
        // At sigma 3 the high-frequency stripes disappear. A 50% source/full-blur
        // mix still leaves a roughly 127-level swing, causing a double exposure.
        assert!(contrast(&image, 96, 100) < 40);
    }

    #[test]
    fn uniform_radius_and_zero_match_the_existing_gaussian() {
        let source = stripes();
        for radius in [0., 0.5, 1., 9.] {
            assert_eq!(
                apply(
                    &source,
                    200,
                    200,
                    &json!({"progressiveFrom":radius,"progressiveTo":radius})
                )
                .unwrap(),
                pixels::gaussian_blur(&source, 200, 200, radius)
            );
        }
    }

    #[test]
    fn transparent_odd_and_degenerate_inputs_stay_valid() {
        for (w, h) in [(1, 1), (1, 37), (39, 1), (17, 21)] {
            let source = [10, 60, 128, 128].repeat(w * h);
            for radial in [false, true] {
                let result = apply(&source, w, h,
                    &json!({"progressiveFrom":0.1,"progressiveTo":250,
                        "progressiveRadial":radial,"progressiveStart":[2,2],"progressiveEnd":[2,2]})).unwrap();
                assert_eq!(result.len(), source.len());
                assert!(
                    result
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .all(|p| *p == [10, 60, 128, 128])
                );
            }
        }
        assert!(apply(&[], 0, 0, &json!({})).unwrap().is_empty());
        assert!(apply(&[0; 3], 1, 1, &json!({})).is_err());
        assert!(apply(&[], usize::MAX, 10, &json!({})).is_err());
    }
}
