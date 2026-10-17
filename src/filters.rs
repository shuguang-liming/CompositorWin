//! Non-destructive adjustment and layer-effect implementation.
use crate::model::Raster;
use crate::pixels;
use serde_json::Value;
#[path = "camera_raw.rs"]
mod camera_raw;
#[path = "dither.rs"]
mod dither;
#[path = "progressive_blur.rs"]
mod progressive_blur;
#[path = "scanlines.rs"]
mod scanlines;

pub fn bind_camera_raw_gpu(device: wgpu::Device, queue: wgpu::Queue) {
    camera_raw::bind_gpu(device, queue);
}

pub fn discard_camera_raw_gpu_buffers() {
    camera_raw::discard_gpu_buffers();
}

pub fn unbind_camera_raw_gpu() {
    camera_raw::unbind_gpu();
}

pub(crate) fn neutralize_camera_raw_color(color: [f64; 3]) -> Option<[f64; 2]> {
    camera_raw::neutralize(color)
}

pub const FILTERS: &[&str] = &[
    "Gaussian Blur",
    "Progressive Blur",
    "Motion Blur",
    "Add Noise",
    "Vignette",
    "Bloom / Glow",
    "Dither",
    "Scanlines",
    "Tonal Contrast",
    "Lens Correction",
    "Camera Raw Filter",
    "Curves",
    "Exposure",
    "Gradient Map",
    "Grain",
    "Black & White",
    "Color Balance",
    "Hue/Saturation",
    "Levels",
    "Invert",
];
/// Original settings schema and defaults, shared by the editor and adjustment records.
pub fn default_settings(kind: &str) -> Value {
    use serde_json::json;
    let value = match kind {
        "Hue/Saturation" => {
            json!({"hue":0,"saturation":0,"lightness":0,"colorize":false,"hsvSettings":{"range":"Master","colorize":false,"invertRange":false,"adjustments":{"Master":{"hue":0,"saturation":0,"lightness":0},"Reds":{"hue":0,"saturation":0,"lightness":0},"Yellows":{"hue":0,"saturation":0,"lightness":0},"Greens":{"hue":0,"saturation":0,"lightness":0},"Cyans":{"hue":0,"saturation":0,"lightness":0},"Blues":{"hue":0,"saturation":0,"lightness":0},"Magentas":{"hue":0,"saturation":0,"lightness":0}},"bands":{"Master":{"falloffStart":0,"rangeStart":0,"rangeEnd":360,"falloffEnd":360},"Reds":{"falloffStart":315,"rangeStart":345,"rangeEnd":15,"falloffEnd":45},"Yellows":{"falloffStart":15,"rangeStart":45,"rangeEnd":75,"falloffEnd":105},"Greens":{"falloffStart":75,"rangeStart":105,"rangeEnd":135,"falloffEnd":165},"Cyans":{"falloffStart":135,"rangeStart":165,"rangeEnd":195,"falloffEnd":225},"Blues":{"falloffStart":195,"rangeStart":225,"rangeEnd":255,"falloffEnd":285},"Magentas":{"falloffStart":255,"rangeStart":285,"rangeEnd":315,"falloffEnd":345}}}})
        }
        "Levels" => {
            json!({"levels":{"channel":"RGB","ranges":vec![json!({"black":0,"gamma":1,"white":255,"outputBlack":0,"outputWhite":255});4]}})
        }
        "Curves" => {
            json!({"curves":{"channel":"RGB","channels":vec![json!([{"x":0,"y":0},{"x":255,"y":255}]);4]}})
        }
        "Exposure" => json!({"exposureSettings":{"exposure":0,"offset":0,"gamma":1}}),
        "Gradient Map" => {
            json!({"gradientMapSettings":{"shadows":{"red":0,"green":0,"blue":0},"highlights":{"red":1,"green":1,"blue":1},"reversed":false}})
        }
        "Grain" => json!({"grainSettings":{"amount":25,"size":1.5,"roughness":50,"seed":0}}),
        "Black & White" => {
            json!({"blackWhiteSettings":{"reds":40,"yellows":60,"greens":40,"cyans":60,"blues":20,"magentas":80,"tint":false,"tintHue":40,"tintSaturation":20}})
        }
        "Color Balance" => {
            json!({"colorBalanceSettings":{"shadowCyanRed":0,"shadowMagentaGreen":0,"shadowYellowBlue":0,"midCyanRed":0,"midMagentaGreen":0,"midYellowBlue":0,"highlightCyanRed":0,"highlightMagentaGreen":0,"highlightYellowBlue":0,"preserveLuminosity":true}})
        }
        "Gaussian Blur" => json!({"blurRadius":10}),
        "Progressive Blur" => {
            json!({"progressiveFrom":0,"progressiveTo":20,"progressiveRadial":false,"progressiveInverted":false})
        }
        "Motion Blur" => json!({"motionAngle":0,"motionDistance":10}),
        "Add Noise" => {
            json!({"noiseAmount":10,"noiseGaussian":false,"noiseMonochromatic":false,"noiseSeed":0})
        }
        "Vignette" => {
            json!({"vignetteAmount":35,"vignetteColor":{"red":0,"green":0,"blue":0},"vignetteMidpoint":50,"vignetteRoundness":100,"vignetteFeather":60,"vignetteHighlights":25})
        }
        "Bloom / Glow" => json!({"bloomAmount":40,"bloomRadius":24}),
        "Tonal Contrast" => {
            json!({"tonalAmount":50,"tonalRadius":16,"tonalShadows":40,"tonalMidtones":60,"tonalHighlights":30})
        }
        "Lens Correction" => json!({"distortion":0}),
        "Dither" => {
            json!({"dither":{"style":"Atkinson (Classic Mac)","pixelSize":2,"pixelShape":"Square","cellSize":8,"textSize":14,"lineSpacing":4,"glow":35,"dots":0,"wobble":0,"angle":45,"levels":2,"diffusion":100,"density":0,"contrast":0,"colors":"Black & White","dark":{"red":0,"green":0,"blue":0},"light":{"red":1,"green":1,"blue":1},"lightOnDark":true,"characters":" .:-=+*#%@"}})
        }
        "Scanlines" => {
            serde_json::json!({"scanlines":{"lineSpacing":4,"thickness":70,"glow":0,"dots":0,"wobble":0,"displace":0,"smoothness":50,"threshold":0,"split":0,"density":0,"contrast":0,"blackLevel":0,"colors":"Black & White","dark":{"red":0,"green":0,"blue":0},"light":{"red":1,"green":1,"blue":1}}})
        }
        "Camera Raw Filter" => camera_raw_defaults(),
        _ => json!({}),
    };
    let mut value = value;
    value["kind"] = json!(kind);
    value
}
pub fn camera_raw_defaults() -> Value {
    use serde_json::json;
    let linear = json!([{"x":0,"y":0},{"x":1,"y":1}]);
    let wheel = json!({"hue":0,"saturation":0,"luminance":0});
    let mut v = json!({"whiteBalance":"Custom","temperature":0,"tint":0,"exposure":0,"contrast":0,"highlights":0,"shadows":0,"whites":0,"blacks":0,"vibrance":0,"saturation":0,"seed":0,"showShadowClipping":false,"showHighlightClipping":false});
    let effects = json!({"texture":0,"clarity":0,"dehaze":0,"glow":0,"glowStyle":"Diffusion","glowRange":0,"glowSpread":0,"glowWarmth":0,"vignetteAmount":0,"vignetteStyle":"Highlight Priority","vignetteMidpoint":50,"vignetteRoundness":0,"vignetteFeather":50,"vignetteHighlights":0,"grainAmount":0,"grainSize":25,"grainRoughness":50});
    v.as_object_mut()
        .unwrap()
        .extend(effects.as_object().unwrap().clone());
    v["curve"] = json!({"shadows":0,"darks":0,"lights":0,"highlights":0,"shadowSplit":25,"darkSplit":50,"lightSplit":75,"rgb":linear,"red":linear,"green":linear,"blue":linear,"refineSaturation":0});
    v["mixer"] = json!({"hue":vec![0;8],"saturation":vec![0;8],"luminance":vec![0;8],"points":[]});
    v["grading"] = json!({"shadows":wheel,"midtones":wheel,"highlights":wheel,"global":wheel,"blending":50,"balance":0});
    v["detail"] = json!({"sharpenAmount":0,"sharpenRadius":10,"sharpenDetail":25,"sharpenMasking":0,"noiseLuminance":0,"noiseLuminanceDetail":50,"noiseLuminanceContrast":0,"noiseColor":0,"noiseColorDetail":50,"noiseColorSmoothness":50,"showSharpenMask":false});
    v["optics"] = json!({"removeChromaticAberration":false,"enableLensProfile":false,"profileDistortion":100,"profileVignetting":100,"distortion":0,"purpleAmount":0,"purpleHueLow":270,"purpleHueHigh":310,"greenAmount":0,"greenHueLow":60,"greenHueHigh":120,"vignetteAmount":0,"vignetteMidpoint":50});
    v["geometry"] = json!({"upright":"Off","projection":"Perspective","vertical":0,"horizontal":0,"rotate":0,"aspect":0,"scale":0,"offsetX":0,"offsetY":0,"constrainCrop":false,"guides":[]});
    v["calibration"] = json!({"process":"Version 6","shadowTint":0,"redHue":0,"redSaturation":0,"greenHue":0,"greenSaturation":0,"blueHue":0,"blueSaturation":0});
    v
}
pub fn number(v: &Value, key: &str, default: f64) -> f64 {
    v.get(key)
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .unwrap_or(default)
}
pub fn boolean(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(default)
}
pub fn string<'a>(v: &'a Value, key: &str, default: &'a str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or(default)
}
pub fn color(v: &Value, default: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| number(v, ["red", "green", "blue"][i], default[i]).clamp(0.0, 1.0))
}
fn section<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(v)
}
pub fn apply_adjustment(image: &Raster, settings: &Value) -> Raster {
    let kind = string(settings, "kind", "Invert");
    // Project reader validates the kind before the renderer reaches this path.
    apply_filter(image, kind, settings).expect("validated adjustment kind")
}
pub fn apply_filter(image: &Raster, kind: &str, settings: &Value) -> Result<Raster, String> {
    if !FILTERS.contains(&kind) {
        return Err(format!("未知滤镜：{kind}"));
    }
    if image.deep.is_some() && kind != "Dither" {
        return apply_deep_filter(image, kind, settings);
    }
    let (w, h) = (image.width as usize, image.height as usize);
    let mut p = image.to_rgba();
    pixels::premultiply(&mut p);
    match kind {
        "Invert" => {
            for q in p.as_chunks_mut::<4>().0 {
                for c in 0..3 {
                    q[c] = q[3] - q[c];
                }
            }
        }
        "Gaussian Blur" => {
            p = pixels::gaussian_blur(
                &p,
                w,
                h,
                number(settings, "blurRadius", number(settings, "radius", 10.0)),
            )
        }
        "Progressive Blur" => p = progressive_blur::apply(&p, w, h, settings)?,
        "Motion Blur" => {
            p = motion_blur(
                &p,
                w,
                h,
                number(settings, "motionAngle", number(settings, "angle", 0.0)),
                number(
                    settings,
                    "motionDistance",
                    number(settings, "distance", 10.0),
                ),
            )
        }
        "Levels" => {
            let s = section(settings, "levels");
            let ranges = s.get("ranges").and_then(Value::as_array);
            let identity = Value::Null;
            let r = |i: usize| ranges.and_then(|r| r.get(i)).unwrap_or(&identity);
            let tables = std::array::from_fn(|c| {
                std::array::from_fn(|i| level(level(i as f64 / 255.0, r(c + 1)), r(0)) as f32)
            });
            pixels::levels_apply(&mut p, &tables);
        }
        "Curves" => {
            let s = section(settings, "curves");
            let channels = s.get("channels").and_then(Value::as_array);
            let identity = Value::Null;
            let points = |i: usize| channels.and_then(|r| r.get(i)).unwrap_or(&identity);
            let tables = std::array::from_fn(|c| {
                std::array::from_fn(|i| {
                    (curve(curve(i as f64, points(c + 1), 255.0), points(0), 255.0) / 255.0) as f32
                })
            });
            pixels::levels_apply(&mut p, &tables);
        }
        "Exposure" => {
            let s = section(settings, "exposureSettings");
            let scale = 2f64.powf(number(s, "exposure", 0.0));
            let offset = number(s, "offset", 0.0);
            let gamma = number(s, "gamma", 1.0).max(0.01);
            let table = std::array::from_fn(|i| {
                linear_to_srgb(
                    (srgb_to_linear(i as f64 / 255.0) * scale + offset)
                        .max(0.0)
                        .powf(1.0 / gamma),
                ) as f32
            });
            pixels::levels_apply(&mut p, &[table; 3]);
        }
        "Gradient Map" => {
            let s = section(settings, "gradientMapSettings");
            let mut a = color(&s["shadows"], [0.0; 3]);
            let mut b = color(&s["highlights"], [1.0; 3]);
            if boolean(s, "reversed", false) {
                std::mem::swap(&mut a, &mut b);
            }
            let table = std::array::from_fn(|i| {
                std::array::from_fn(|c| {
                    ((a[c] + (b[c] - a[c]) * i as f64 / 255.0) * 255.0).round() as u8
                })
            });
            pixels::gradient_map(&mut p, &table);
        }
        "Black & White" => {
            let s = section(settings, "blackWhiteSettings");
            let names = ["reds", "yellows", "greens", "cyans", "blues", "magentas"];
            let defaults = [40.0, 60.0, 40.0, 60.0, 20.0, 80.0];
            let weights =
                std::array::from_fn(|i| (number(s, names[i], defaults[i]) / 100.0) as f32);
            let tint = boolean(s, "tint", false).then(|| {
                [
                    number(s, "tintHue", 42.0) as f32,
                    number(s, "tintSaturation", 20.0) as f32 / 100.0,
                ]
            });
            pixels::black_white(&mut p, weights, tint);
        }
        "Color Balance" => {
            let s = section(settings, "colorBalanceSettings");
            let read = |prefix: &str| {
                std::array::from_fn(|i| {
                    (number(
                        s,
                        &format!("{prefix}{}", ["CyanRed", "MagentaGreen", "YellowBlue"][i]),
                        0.0,
                    ) / 100.0) as f32
                })
            };
            pixels::color_balance(
                &mut p,
                read("shadow"),
                read("mid"),
                read("highlight"),
                boolean(s, "preserveLuminosity", true),
            );
        }
        "Add Noise" => pixels::add_noise(
            &mut p,
            w,
            pixels::Noise {
                amount: number(settings, "noiseAmount", number(settings, "amount", 10.0)) as f32,
                gaussian: boolean(
                    settings,
                    "noiseGaussian",
                    boolean(settings, "gaussian", false),
                ),
                monochromatic: boolean(
                    settings,
                    "noiseMonochromatic",
                    boolean(settings, "monochromatic", false),
                ),
                seed: number(settings, "noiseSeed", number(settings, "seed", 0.0)) as u32,
                origin: [0, 0],
            },
        ),
        "Grain" => {
            let s = section(settings, "grainSettings");
            pixels::grain(
                &mut p,
                w,
                pixels::Grain {
                    amount: number(s, "amount", 25.0),
                    size: number(s, "size", 1.5),
                    roughness: number(s, "roughness", 50.0),
                    seed: number(s, "seed", 0.0) as u32,
                    origin: [0.0, 0.0],
                    units_per_pixel: 1.0,
                },
            );
        }
        "Hue/Saturation" => hue_saturation(&mut p, section(settings, "hsvSettings")),
        "Vignette" => vignette(&mut p, w, h, settings),
        "Bloom / Glow" => bloom(&mut p, w, h, settings),
        "Tonal Contrast" => {
            let blurred = pixels::gaussian_blur(&p, w, h, number(settings, "tonalRadius", 20.0));
            tonal_contrast(
                &mut p,
                &blurred,
                number(settings, "tonalAmount", 50.0),
                number(settings, "tonalShadows", 50.0),
                number(settings, "tonalMidtones", 100.0),
                number(settings, "tonalHighlights", 50.0),
            );
        }
        "Lens Correction" => {
            p = pixels::lens_distort(
                &p,
                w,
                h,
                number(
                    settings,
                    "distortion",
                    number(settings, "lensDistortion", 0.0),
                ) / 100.0
                    * 0.35,
            )
        }
        "Dither" => dither::apply(&mut p, w, h, section(settings, "dither"))?,
        "Scanlines" => scanlines::apply(&mut p, w, h, section(settings, "scanlines"))?,
        "Camera Raw Filter" => camera_raw::apply(&mut p, w, h, section(settings, "cameraRaw"))?,
        _ => return Err(format!("滤镜处理程序缺失：{kind}")),
    }
    pixels::unpremultiply(&mut p);
    Ok(Raster::from_rgba(image.width, image.height, p))
}

/// Keep the authoritative 16-bit or half-float surface throughout an edit.
/// Point curves use full precision; byte kernels use the upstream 4/10-pass
/// derivative reconstruction instead of applying once to the display preview.
fn apply_deep_filter(image: &Raster, kind: &str, settings: &Value) -> Result<Raster, String> {
    let deep = image.deep.as_ref().expect("checked high-precision image");
    let output = match kind {
        "Invert" => deep.map_colors(|p| {
            let scale = if deep.depth == crate::deep_pixels::Depth::Hdr {
                p[0].max(p[1]).max(p[2]).max(1.)
            } else {
                1.
            };
            p.map(|v| scale - v)
        }),
        "Exposure" => {
            let s = section(settings, "exposureSettings");
            deep.exposure(
                number(s, "exposure", 0.) as f32,
                number(s, "offset", 0.) as f32,
                number(s, "gamma", 1.) as f32,
            )
        }
        "Levels" => {
            let s = section(settings, "levels");
            let ranges = s.get("ranges").and_then(Value::as_array);
            let identity = Value::Null;
            let r = |i: usize| ranges.and_then(|r| r.get(i)).unwrap_or(&identity);
            normalized_deep_colors(deep, |p| {
                std::array::from_fn(|c| level(level(p[c] as f64, r(c + 1)), r(0)) as f32)
            })
        }
        "Curves" => {
            let s = section(settings, "curves");
            let channels = s.get("channels").and_then(Value::as_array);
            let identity = Value::Null;
            let points = |i: usize| channels.and_then(|r| r.get(i)).unwrap_or(&identity);
            normalized_deep_colors(deep, |p| {
                std::array::from_fn(|c| {
                    (curve(
                        curve(p[c] as f64 * 255., points(c + 1), 255.),
                        points(0),
                        255.,
                    ) / 255.) as f32
                })
            })
        }
        "Gaussian Blur" => deep.gaussian_blur(number(
            settings,
            "blurRadius",
            number(settings, "radius", 10.),
        )),
        "Gradient Map" | "Hue/Saturation" | "Black & White" | "Color Balance" => {
            // A red-fast color cube smooths channel-mixing point adjustments.
            // This mirrors DeepLayers.lookup rather than a quantized per-pixel edit.
            let dimension = 33usize;
            let mut cube_pixels = Vec::with_capacity(dimension.pow(3) * 4);
            for b in 0..dimension {
                for g in 0..dimension {
                    for r in 0..dimension {
                        cube_pixels.extend_from_slice(
                            &[r, g, b]
                                .map(|v| (v as f64 * 255. / (dimension - 1) as f64).round() as u8),
                        );
                        cube_pixels.push(255);
                    }
                }
            }
            let cube = apply_filter(
                &Raster::from_rgba(
                    dimension as u32,
                    (dimension * dimension) as u32,
                    cube_pixels.clone(),
                ),
                kind,
                settings,
            )?;
            let cube: Vec<[f32; 3]> = cube
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    std::array::from_fn(|c| {
                        let position = match c {
                            0 => i % dimension,
                            1 => (i / dimension) % dimension,
                            _ => i / (dimension * dimension),
                        };
                        (position as f32 / (dimension - 1) as f32
                            + (p[c] as f32 - cube_pixels[i * 4 + c] as f32) / 255.)
                            .clamp(0., 1.)
                    })
                })
                .collect();
            deep.apply_cube(&cube, dimension)?
        }
        _ => deep.run_8bit(kind == "Camera Raw Filter", |mut p| {
            pixels::unpremultiply(&mut p);
            let out = apply_filter(
                &Raster::from_rgba(image.width, image.height, p),
                kind,
                settings,
            )?;
            let mut out = out.to_rgba();
            pixels::premultiply(&mut out);
            Ok(out)
        })?,
    };
    Ok(Raster::from_deep(output))
}

fn normalized_deep_colors(
    deep: &crate::deep_pixels::DeepRaster,
    operation: impl Fn([f32; 3]) -> [f32; 3],
) -> crate::deep_pixels::DeepRaster {
    deep.map_colors(|p| {
        let scale = if deep.depth == crate::deep_pixels::Depth::Hdr {
            p[0].max(p[1]).max(p[2]).max(1.)
        } else {
            1.
        };
        operation(p.map(|v| v / scale)).map(|v| v * scale)
    })
}
fn level(v: f64, r: &Value) -> f64 {
    let black = number(r, "black", 0.0).clamp(0.0, 254.0);
    let white = number(r, "white", 255.0).clamp(black + 1.0, 255.0);
    let x = ((v * 255.0 - black) / (white - black))
        .clamp(0.0, 1.0)
        .powf(1.0 / number(r, "gamma", 1.0).clamp(0.1, 9.99));
    (number(r, "outputBlack", 0.0)
        + x * (number(r, "outputWhite", 255.0) - number(r, "outputBlack", 0.0)))
        / 255.0
}
/// Monotone cubic Hermite interpolation, matching CurvesSettings.value.
pub fn curve(x: f64, points: &Value, extent: f64) -> f64 {
    let Some(p) = points.as_array().filter(|p| p.len() >= 2) else {
        return x;
    };
    let point = |i: usize| [number(&p[i], "x", 0.0), number(&p[i], "y", 0.0)];
    let i = p
        .iter()
        .rposition(|q| number(q, "x", 0.0) <= x)
        .unwrap_or(0)
        .min(p.len() - 2);
    let a = point(i);
    let b = point(i + 1);
    let h = b[0] - a[0];
    if h <= 0.0 {
        return x;
    }
    let delta = |j: usize| {
        let a = point(j);
        let b = point(j + 1);
        (b[1] - a[1]) / (b[0] - a[0]).max(1e-10)
    };
    let slope = |j: usize| {
        if j == 0 {
            delta(0)
        } else if j + 1 == p.len() {
            delta(j - 1)
        } else {
            let a = delta(j - 1);
            let b = delta(j);
            if a * b <= 0.0 {
                0.0
            } else {
                2.0 / (1.0 / a + 1.0 / b)
            }
        }
    };
    let t = ((x - a[0]) / h).clamp(0.0, 1.0);
    ((2.0 * t * t * t - 3.0 * t * t + 1.0) * a[1]
        + (t * t * t - 2.0 * t * t + t) * h * slope(i)
        + (-2.0 * t * t * t + 3.0 * t * t) * b[1]
        + (t * t * t - t * t) * h * slope(i + 1))
    .clamp(0.0, extent)
}
pub fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
pub fn linear_to_srgb(v: f64) -> f64 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
    .clamp(0.0, 1.0)
}
pub fn map_colors(p: &mut [u8], mut f: impl FnMut([f64; 3]) -> [f64; 3]) {
    for p in p.as_chunks_mut::<4>().0 {
        let a = p[3] as f64;
        if a == 0.0 {
            continue;
        }
        let c = f(std::array::from_fn(|i| (p[i] as f64 / a).min(1.0)));
        for i in 0..3 {
            p[i] = (c[i] * a).round().clamp(0.0, a) as u8;
        }
    }
}
fn hue_saturation(p: &mut [u8], s: &Value) {
    let colorize = boolean(s, "colorize", false);
    let selected = s["range"].as_str().unwrap_or("Master");
    map_colors(p, |c| {
        let [h, sat, l] = pixels::rgb_to_hsl(c.map(|v| v as f32));
        let mut d = [
            number(s, "hue", 0.0),
            number(s, "saturation", 0.0),
            number(s, "lightness", 0.0),
        ];
        let names = [
            "Master", "Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas",
        ];
        if let Some(adjust) = s["adjustments"].as_object() {
            d = [0.0; 3];
            for (index, name) in names.iter().enumerate() {
                if colorize && *name != selected {
                    continue;
                }
                if let Some(a) = adjust.get(*name) {
                    let mut weight = if index == 0 || colorize {
                        1.0
                    } else {
                        let center = ((index - 1) * 60) as f64;
                        let band = &s["bands"][*name];
                        hue_band_weight(
                            (h as f64).round(),
                            number(band, "falloffStart", center - 45.0),
                            number(band, "rangeStart", center - 15.0),
                            number(band, "rangeEnd", center + 15.0),
                            number(band, "falloffEnd", center + 45.0),
                        )
                    };
                    if !colorize
                        && index != 0
                        && *name == selected
                        && boolean(s, "invertRange", false)
                    {
                        weight = 1.0 - weight;
                    }
                    for i in 0..3 {
                        d[i] += number(a, ["hue", "saturation", "lightness"][i], 0.0) * weight;
                    }
                }
            }
        }
        d[1] = d[1].clamp(-100.0, 100.0);
        d[2] = d[2].clamp(-100.0, 100.0);
        let hue = if colorize { d[0] } else { h as f64 + d[0] };
        let saturation = if colorize {
            d[1] / 100.0
        } else if d[1] >= 100.0 {
            if sat > 0.0 { 1.0 } else { 0.0 }
        } else if d[1] > 0.0 {
            sat as f64 / (1.0 - d[1] / 100.0)
        } else {
            sat as f64 * (1.0 + d[1] / 100.0)
        };
        let lightness = if d[2] >= 0.0 {
            l as f64 + (1.0 - l as f64) * d[2] / 100.0
        } else {
            l as f64 * (1.0 + d[2] / 100.0)
        };
        pixels::hsl_to_rgb(
            hue as f32,
            saturation.clamp(0.0, 1.0) as f32,
            lightness as f32,
        )
        .map(|v| v as f64)
    });
}
fn hue_band_weight(h: f64, a: f64, b: f64, c: f64, d: f64) -> f64 {
    let span = (d - a).rem_euclid(360.0);
    if span == 0.0 {
        return 1.0;
    }
    let pos = (h - a).rem_euclid(360.0);
    if pos > span {
        return 0.0;
    }
    let lead = (b - a).rem_euclid(360.0);
    let plateau = (c - a).rem_euclid(360.0);
    if pos < lead {
        pos / lead.max(1e-10)
    } else if pos <= plateau {
        1.0
    } else {
        (span - pos) / (span - plateau).max(1e-10)
    }
}
pub fn motion_blur(p: &[u8], w: usize, h: usize, angle: f64, distance: f64) -> Vec<u8> {
    let steps = distance.ceil().clamp(1.0, 2000.0) as usize;
    let a = (-angle).to_radians();
    let mut out = vec![0; p.len()];
    for y in 0..h {
        for x in 0..w {
            let mut sums = [0.0; 4];
            for i in 0..=steps {
                let t = (i as f64 / steps as f64 - 0.5) * distance;
                let q = pixels::bilinear(
                    p,
                    w,
                    h,
                    x as f64 + a.cos() * t,
                    y as f64 + a.sin() * t,
                    true,
                );
                for c in 0..4 {
                    sums[c] += q[c] as f64;
                }
            }
            for c in 0..4 {
                out[(y * w + x) * 4 + c] = (sums[c] / (steps + 1) as f64).round() as u8;
            }
        }
    }
    out
}
pub fn tonal_contrast(
    p: &mut [u8],
    blurred: &[u8],
    amount: f64,
    shadows: f64,
    midtones: f64,
    highlights: f64,
) {
    for (q, b) in p
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(blurred.as_chunks::<4>().0.iter())
    {
        if q[3] == 0 || b[3] == 0 {
            continue;
        }
        let lum = 0.2126 * q[0] as f64 / q[3] as f64
            + 0.7152 * q[1] as f64 / q[3] as f64
            + 0.0722 * q[2] as f64 / q[3] as f64;
        let bl = 0.2126 * b[0] as f64 / b[3] as f64
            + 0.7152 * b[1] as f64 / b[3] as f64
            + 0.0722 * b[2] as f64 / b[3] as f64;
        let smooth = |lo: f64, hi: f64, v: f64| {
            let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let sw = 1.0 - smooth(0.15, 0.5, bl);
        let hw = smooth(0.5, 0.85, bl);
        let mw = 1.0 - sw - hw;
        let weight = (sw * shadows + mw * midtones + hw * highlights) / 100.0;
        let delta =
            0.18 * ((lum - bl) * 6.0).tanh() * weight * amount / 50.0 * (4.0 * lum * (1.0 - lum));
        for c in 0..3 {
            q[c] = ((q[c] as f64 / q[3] as f64 + delta) * q[3] as f64)
                .round()
                .clamp(0.0, q[3] as f64) as u8;
        }
    }
}
fn vignette(p: &mut [u8], w: usize, h: usize, s: &Value) {
    camera_raw::colored_vignette(
        p,
        w,
        h,
        number(s, "vignetteAmount", number(s, "amount", 50.0)),
        number(s, "vignetteMidpoint", 50.0),
        number(s, "vignetteRoundness", 0.0),
        number(s, "vignetteFeather", 50.0),
        number(s, "vignetteHighlights", 0.0),
        color(&s["vignetteColor"], [0.0; 3]),
    );
}
fn bloom(p: &mut [u8], w: usize, h: usize, s: &Value) {
    let mut glow = p.to_vec();
    let threshold = number(s, "bloomThreshold", 0.0) / 100.0;
    map_colors(&mut glow, |c| {
        let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let weight = ((l - threshold) / (1.0 - threshold).max(0.01)).clamp(0.0, 1.0);
        c.map(|v| v * weight)
    });
    glow = pixels::gaussian_blur(&glow, w, h, number(s, "bloomRadius", 24.0));
    let amount = number(s, "bloomAmount", number(s, "bloomIntensity", 40.0)) / 100.0;
    for (q, b) in p
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(glow.as_chunks::<4>().0.iter())
    {
        for c in 0..3 {
            q[c] = (q[c] as f64 + b[c] as f64 * amount)
                .round()
                .min(q[3] as f64) as u8;
        }
    }
}

/// Apply all six effects around the visible alpha. Returned inset is in source-layer pixels.
pub fn apply_effects(image: &Raster, effects: &Value) -> (Raster, f64) {
    if image.deep.is_some() {
        return apply_deep_effects(image, effects);
    }
    let visible = |key: &str| {
        effects.get(key).filter(|v| {
            v.is_object() && boolean(v, "enabled", true) && number(v, "opacity", 1.0) > 0.0
        })
    };
    let mut margin = 0.0f64;
    if let Some(s) = visible("stroke")
        && !boolean(s, "inside", false)
    {
        margin = margin.max(number(s, "size", 4.0));
    }
    if let Some(s) = visible("shadow") {
        margin = margin.max(number(s, "distance", 20.0) + number(s, "blur", 20.0) * 3.0);
    }
    if let Some(s) = visible("outerGlow") {
        margin = margin.max(number(s, "size", 20.0) * 3.0);
    }
    let inset = margin.ceil() as usize + 2;
    let w = image.width as usize + inset * 2;
    let h = image.height as usize + inset * 2;
    let mut source = vec![0u8; w * h * 4];
    for y in 0..image.height {
        for x in 0..image.width {
            let i = ((y as usize + inset) * w + x as usize + inset) * 4;
            let mut p = image.get(x, y);
            pixels::premultiply(&mut p);
            source[i..i + 4].copy_from_slice(&p);
        }
    }
    if let Some(s) = visible("colorOverlay") {
        let color = color(s, [0.; 3]);
        let opacity = number(s, "opacity", 1.).clamp(0., 1.);
        for p in source.as_chunks_mut::<4>().0 {
            for c in 0..3 {
                p[c] =
                    (p[c] as f64 * (1. - opacity) + color[c] * p[3] as f64 * opacity).round() as u8;
            }
        }
    }
    let alpha = pixels::extract_alpha(&source);
    let mut out = vec![0u8; source.len()];
    let shifted = |dx: f64, dy: f64, blur: f64| {
        let mut rgba = vec![0; source.len()];
        for y in 0..h {
            for x in 0..w {
                let a = pixels::bilinear(&source, w, h, x as f64 - dx, y as f64 - dy, false)[3];
                rgba[(y * w + x) * 4..(y * w + x + 1) * 4].fill(a);
            }
        }
        pixels::extract_alpha(&pixels::gaussian_blur(&rgba, w, h, blur / 2.0))
    };
    let stroke = visible("stroke");
    let stroke_mask = |s: &Value| {
        let erode = boolean(s, "inside", false);
        let shape = pixels::morphology(
            &alpha,
            w,
            h,
            number(s, "size", 4.0).round().max(1.0) as usize,
            erode,
        );
        alpha
            .iter()
            .zip(shape)
            .map(|(&a, b)| {
                if erode {
                    a.saturating_sub(b)
                } else {
                    b.saturating_sub(a)
                }
            })
            .collect::<Vec<_>>()
    };
    if let Some(s) = visible("shadow") {
        let a = number(s, "angle", 90.0).to_radians();
        let d = number(s, "distance", 20.0);
        paint_coverage(
            &mut out,
            &shifted(-a.cos() * d, a.sin() * d, number(s, "blur", 20.0)),
            s,
        );
    }
    if let Some(s) = visible("outerGlow") {
        let blurred = shifted(0.0, 0.0, number(s, "size", 20.0));
        let coverage: Vec<_> = blurred
            .iter()
            .zip(&alpha)
            .map(|(&b, &a)| ((b as u32 * (255 - a as u32) + 127) / 255) as u8)
            .collect();
        paint_coverage(&mut out, &coverage, s);
    }
    if let Some(s) = stroke
        && !boolean(s, "inside", false)
    {
        paint_coverage(&mut out, &stroke_mask(s), s);
    }
    for (o, p) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(source.as_chunks::<4>().0.iter())
    {
        let keep = 1.0 - p[3] as f64 / 255.0;
        for c in 0..4 {
            o[c] = (p[c] as f64 + o[c] as f64 * keep).round().min(255.0) as u8;
        }
    }
    for key in ["innerGlow", "innerShadow"] {
        if let Some(s) = visible(key) {
            let (dx, dy, blur) = if key == "innerShadow" {
                let angle = number(s, "angle", 90.0).to_radians();
                let d = number(s, "distance", 10.0);
                (-angle.cos() * d, angle.sin() * d, number(s, "blur", 10.0))
            } else {
                (0.0, 0.0, number(s, "size", 10.0))
            };
            let blurred = shifted(dx, dy, blur);
            let coverage: Vec<_> = alpha
                .iter()
                .zip(blurred)
                .map(|(&a, b)| ((a as u32 * (255 - b as u32) + 127) / 255) as u8)
                .collect();
            paint_coverage(&mut out, &coverage, s);
        }
    }
    if let Some(s) = stroke
        && boolean(s, "inside", false)
    {
        paint_coverage(&mut out, &stroke_mask(s), s);
    }
    pixels::unpremultiply(&mut out);
    (Raster::from_rgba(w as u32, h as u32, out), inset as f64)
}
fn paint_coverage(p: &mut [u8], coverage: &[u8], s: &Value) {
    let c = color(s, [0.0; 3]);
    let opacity = number(s, "opacity", 1.0).clamp(0.0, 1.0);
    for (p, &a) in p.as_chunks_mut::<4>().0.iter_mut().zip(coverage) {
        let a = a as f64 / 255.0 * opacity;
        for i in 0..3 {
            p[i] = (c[i] * a * 255.0 + p[i] as f64 * (1.0 - a))
                .round()
                .min(255.0) as u8;
        }
        p[3] = (a * 255.0 + p[3] as f64 * (1.0 - a)).round().min(255.0) as u8;
    }
}

/// Coverage effects are masks; the source and compositing remain at layer depth.
/// This preserves sub-byte colors and HDR highlights under shadows and overlays.
fn apply_deep_effects(image: &Raster, effects: &Value) -> (Raster, f64) {
    let visible = |key: &str| {
        effects.get(key).filter(|s| {
            s.is_object() && boolean(s, "enabled", true) && number(s, "opacity", 1.) > 0.
        })
    };
    let active = [
        "stroke",
        "shadow",
        "outerGlow",
        "colorOverlay",
        "innerGlow",
        "innerShadow",
    ];
    if active.iter().all(|key| visible(key).is_none()) {
        return (image.clone(), 0.);
    }
    let mut margin = 0f64;
    if let Some(s) = visible("stroke")
        && !boolean(s, "inside", false)
    {
        margin = margin.max(number(s, "size", 4.));
    }
    if let Some(s) = visible("shadow") {
        margin = margin.max(number(s, "distance", 20.) + number(s, "blur", 20.) * 3.);
    }
    if let Some(s) = visible("outerGlow") {
        margin = margin.max(number(s, "size", 20.) * 3.);
    }
    let inset = margin.ceil() as usize + 2;
    let w = image.width as usize + inset * 2;
    let h = image.height as usize + inset * 2;
    let mut coverage = vec![0u8; w * h];
    for y in 0..image.height {
        for x in 0..image.width {
            coverage[(y as usize + inset) * w + x as usize + inset] =
                (image.get_f32(x, y)[3] * 255.).round() as u8;
        }
    }
    let shifted = |dx: f64, dy: f64, blur: f64| {
        let rgba: Vec<u8> = coverage.iter().flat_map(|&a| [a; 4]).collect();
        let mut moved = vec![0u8; rgba.len()];
        for y in 0..h {
            for x in 0..w {
                let alpha = pixels::bilinear(&rgba, w, h, x as f64 - dx, y as f64 - dy, false)[3];
                moved[(y * w + x) * 4..(y * w + x + 1) * 4].fill(alpha);
            }
        }
        pixels::extract_alpha(&pixels::gaussian_blur(&moved, w, h, blur / 2.))
    };
    let stroke = |s: &Value| {
        let inside = boolean(s, "inside", false);
        let morphed = pixels::morphology(
            &coverage,
            w,
            h,
            number(s, "size", 4.).round().max(1.) as usize,
            inside,
        );
        coverage
            .iter()
            .zip(morphed)
            .map(|(&a, b)| {
                if inside {
                    a.saturating_sub(b)
                } else {
                    b.saturating_sub(a)
                }
            })
            .collect::<Vec<_>>()
    };
    let mut out = vec![0f32; w * h * 4];
    let paint = |out: &mut [f32], mask: &[u8], s: &Value| {
        let color = color(s, [0.; 3]).map(|v| v as f32);
        let opacity = number(s, "opacity", 1.).clamp(0., 1.) as f32;
        for (p, &coverage) in out.as_chunks_mut::<4>().0.iter_mut().zip(mask) {
            let alpha = coverage as f32 / 255. * opacity;
            for c in 0..3 {
                p[c] = color[c] * alpha + p[c] * (1. - alpha);
            }
            p[3] = alpha + p[3] * (1. - alpha);
        }
    };
    if let Some(s) = visible("shadow") {
        let angle = number(s, "angle", 90.).to_radians();
        let distance = number(s, "distance", 20.);
        paint(
            &mut out,
            &shifted(
                -angle.cos() * distance,
                angle.sin() * distance,
                number(s, "blur", 20.),
            ),
            s,
        );
    }
    if let Some(s) = visible("outerGlow") {
        let blurred = shifted(0., 0., number(s, "size", 20.));
        let mask: Vec<u8> = blurred
            .iter()
            .zip(&coverage)
            .map(|(&b, &a)| ((b as u32 * (255 - a as u32) + 127) / 255) as u8)
            .collect();
        paint(&mut out, &mask, s);
    }
    if let Some(s) = visible("stroke")
        && !boolean(s, "inside", false)
    {
        paint(&mut out, &stroke(s), s);
    }
    for y in 0..image.height {
        for x in 0..image.width {
            let mut source = image.get_f32(x, y);
            if let Some(s) = visible("colorOverlay") {
                let color = color(s, [0.; 3]);
                let opacity = number(s, "opacity", 1.).clamp(0., 1.) as f32;
                for c in 0..3 {
                    source[c] = source[c] * (1. - opacity) + color[c] as f32 * opacity;
                }
            }
            let i = ((y as usize + inset) * w + x as usize + inset) * 4;
            for c in 0..3 {
                out[i + c] = source[c] * source[3] + out[i + c] * (1. - source[3]);
            }
            out[i + 3] = source[3] + out[i + 3] * (1. - source[3]);
        }
    }
    for key in ["innerGlow", "innerShadow"] {
        if let Some(s) = visible(key) {
            let (dx, dy, blur) = if key == "innerShadow" {
                let angle = number(s, "angle", 90.).to_radians();
                let d = number(s, "distance", 10.);
                (-angle.cos() * d, angle.sin() * d, number(s, "blur", 10.))
            } else {
                (0., 0., number(s, "size", 10.))
            };
            let shifted = shifted(dx, dy, blur);
            let mask: Vec<u8> = coverage
                .iter()
                .zip(shifted)
                .map(|(&a, b)| ((a as u32 * (255 - b as u32) + 127) / 255) as u8)
                .collect();
            paint(&mut out, &mask, s);
        }
    }
    if let Some(s) = visible("stroke")
        && boolean(s, "inside", false)
    {
        paint(&mut out, &stroke(s), s);
    }
    for p in out.as_chunks_mut::<4>().0 {
        if p[3] > 0. {
            for c in 0..3 {
                p[c] /= p[3];
            }
        }
    }
    let deep = crate::deep_pixels::DeepRaster::from_f32(
        w as u32,
        h as u32,
        image.deep.as_ref().unwrap().depth,
        &out,
    )
    .expect("finite layer-effect composition");
    (Raster::from_deep(deep), inset as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn deep_point_filters_keep_low_bits_and_hdr_exposure_recovers_highlights() {
        use crate::deep_pixels::{DeepRaster, Depth};
        let source = Raster::from_deep(
            DeepRaster::from_u16(
                2,
                1,
                vec![30001, 30101, 30201, 32769, 30002, 30102, 30202, 32769],
            )
            .unwrap(),
        );
        for kind in [
            "Levels",
            "Curves",
            "Exposure",
            "Hue/Saturation",
            "Color Balance",
            "Camera Raw Filter",
        ] {
            let out = apply_filter(&source, kind, &default_settings(kind)).unwrap();
            let bits = out.deep.as_ref().unwrap().to_bits();
            assert_eq!(bits[3], 32769, "{kind} alpha");
            assert!(
                bits[4] > bits[0],
                "{kind} lost adjacent sub-byte colors: {bits:?}"
            );
        }
        let hdr =
            Raster::from_deep(DeepRaster::from_f32(1, 1, Depth::Hdr, &[4., 2., 1., 0.5]).unwrap());
        let out = apply_filter(&hdr, "Exposure", &json!({"exposure":-6.})).unwrap();
        let p = out.get_f32(0, 0);
        assert!(p[0] > p[1] && p[1] > p[2] && p[0] < 1.);
        assert_eq!(p[3], 0.5);
        let inverted = apply_filter(&source, "Invert", &Value::Null).unwrap();
        assert_eq!(
            inverted.deep.as_ref().unwrap().get_bits(0, 0),
            [35534, 35434, 35334, 32769]
        );
        let dithered = apply_filter(&source, "Dither", &default_settings("Dither")).unwrap();
        assert!(dithered.deep.is_none());
    }
    #[test]
    fn color_overlay_changes_only_source_color_and_preserves_coverage_and_shadow() {
        let white = json!({"red":1.,"green":1.,"blue":1.,"opacity":1.});
        for (source, effects, expected) in [
            (
                [0, 0, 0, 128],
                json!({"colorOverlay":white}),
                [128, 128, 128, 128],
            ),
            (
                [255, 0, 0, 255],
                json!({"colorOverlay":{"red":0.,"green":0.,"blue":1.,"opacity":0.5}}),
                [128, 0, 128, 255],
            ),
            (
                [0, 0, 0, 128],
                json!({"colorOverlay":white,"shadow":{"red":1.,"green":0.,"blue":0.,"opacity":1.,"distance":0.,"blur":0.}}),
                [192, 128, 128, 192],
            ),
        ] {
            let (out, inset) = apply_effects(&Raster::solid(1, 1, source), &effects);
            let mut p = out.get(inset as u32, inset as u32);
            pixels::premultiply(&mut p);
            for c in 0..4 {
                assert!(
                    (p[c] as i32 - expected[c]).abs() <= 1,
                    "{p:?} vs {expected:?}"
                );
            }
        }
    }
    #[test]
    fn deep_effects_keep_hdr_source_and_exact_overlay_alpha() {
        use crate::deep_pixels::{DeepRaster, Depth};
        let source = Raster::from_deep(
            DeepRaster::from_f32(1, 1, Depth::Hdr, &[4., 2., 0.25, 0.5]).unwrap(),
        );
        let (out, inset) = apply_effects(
            &source,
            &json!({"shadow":{"distance":4.,"blur":0.,"opacity":0.5}}),
        );
        assert_eq!(
            out.get_f32(inset as u32, inset as u32),
            source.get_f32(0, 0)
        );
        let (out, inset) = apply_effects(
            &source,
            &json!({"colorOverlay":{"red":1.,"green":1.,"blue":1.,"opacity":1.}}),
        );
        assert_eq!(out.get_f32(inset as u32, inset as u32), [1., 1., 1., 0.5]);
    }
    #[test]
    fn exposure_alpha_and_linear_stops() {
        let r = Raster::solid(1, 1, [128, 128, 128, 128]);
        let p = apply_filter(&r, "Exposure", &json!({"exposure":1.0}))
            .unwrap()
            .get(0, 0);
        assert!((p[0] as i32 - 176).abs() <= 1);
        assert_eq!(p[3], 128);
    }
    #[test]
    fn levels_identity_soft_alpha() {
        let r = Raster::solid(2, 2, [128, 64, 0, 128]);
        assert_eq!(apply_filter(&r, "Levels", &Value::Null).unwrap(), r);
    }
    #[test]
    fn effects_follow_alpha_and_extend_bounds() {
        let r = Raster::solid(1, 1, [255, 0, 0, 255]);
        let (out, inset) = apply_effects(
            &r,
            &json!({"stroke":{"size":1,"red":0,"green":1,"blue":0,"opacity":1}}),
        );
        let i = inset as u32;
        assert_eq!(out.get(i, i), [255, 0, 0, 255]);
        assert_eq!(out.get(i - 1, i), [0, 255, 0, 255]);
        assert_eq!(out.get(0, 0), [0; 4]);
    }
    #[test]
    fn hermite_preserves_nonmonotone_peak() {
        let p = json!([{"x":0,"y":0},{"x":128,"y":255},{"x":255,"y":0}]);
        assert_eq!(curve(128.0, &p, 255.0), 255.0);
        assert!(curve(100.0, &p, 255.0) < 255.0);
    }
    #[test]
    fn inverted_range_and_selected_colorize() {
        let r = Raster::from_rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]);
        let inverted = apply_filter(&r, "Hue/Saturation", &json!({"hsvSettings":{"range":"Reds","invertRange":true,"adjustments":{"Reds":{"saturation":-100}}}})).unwrap();
        assert_eq!(inverted.get(0, 0), [255, 0, 0, 255]);
        assert_eq!(inverted.get(1, 0), [128, 128, 128, 255]);
        let colorized = apply_filter(&r, "Hue/Saturation", &json!({"hsvSettings":{"range":"Greens","colorize":true,"adjustments":{"Master":{"hue":30,"saturation":20},"Greens":{"hue":120,"saturation":100}}}})).unwrap();
        assert_eq!(colorized.get(0, 0), [0, 255, 0, 255]);
        assert_eq!(colorized.get(1, 0), [0, 255, 0, 255]);
    }
}
