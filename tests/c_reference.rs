use compositor::{filters, model::Raster, pixels};
use serde_json::{Value, json};
fn fixture() -> Value {
    serde_json::from_str(include_str!("reference/pixels.json")).unwrap()
}
fn bytes(v: &Value, key: &str) -> Vec<u8> {
    v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u8)
        .collect()
}
fn compare(key: &str, actual: &[u8], expected: &[u8], tolerance: u8) {
    assert_eq!(actual.len(), expected.len());
    let mismatches: Vec<_> = actual
        .iter()
        .zip(expected)
        .enumerate()
        .filter(|(_, (a, b))| a.abs_diff(**b) > tolerance)
        .take(10)
        .collect();
    assert!(mismatches.is_empty(), "{key}: {mismatches:?}");
}
#[test]
fn original_c_kernels_same_input() {
    let f = fixture();
    let input = bytes(&f, "input");
    let mut p = input.clone();
    pixels::unpremultiply(&mut p);
    for p in p.as_chunks_mut::<4>().0 {
        p[3] = 255;
    }
    compare("opaque", &p, &bytes(&f, "opaque"), 0);
    let mut p = input.clone();
    let table = std::array::from_fn(|i| (255 - i) as f32 / 255.0);
    pixels::levels_apply(&mut p, &[table; 3]);
    compare("levels", &p, &bytes(&f, "levels"), 0);
    for (key, gaussian, mono) in [
        ("noiseUniform", false, false),
        ("noiseGaussian", true, true),
    ] {
        let mut p = input.clone();
        pixels::add_noise(
            &mut p,
            8,
            pixels::Noise {
                amount: 25.0,
                gaussian,
                monochromatic: mono,
                seed: 123456,
                origin: [0, 0],
            },
        );
        compare(key, &p, &bytes(&f, key), 0);
    }
    compare(
        "lens",
        &pixels::lens_distort(&input, 8, 8, -0.35),
        &bytes(&f, "lens"),
        0,
    );
    compare(
        "wand",
        &pixels::wand_mask(&input, 8, 8, [3, 3], 1, 80, false),
        &bytes(&f, "wand"),
        0,
    );
    let mut p = input.clone();
    pixels::gradient_map(
        &mut p,
        &std::array::from_fn(|i| [i as u8, (255 - i) as u8, 100]),
    );
    compare("gradient", &p, &bytes(&f, "gradient"), 0);
    let mut p = input.clone();
    pixels::black_white(&mut p, [0.4, 0.6, 0.4, 0.6, 0.2, 0.8], None);
    compare("blackWhite", &p, &bytes(&f, "blackWhite"), 0);
    let mut p = input.clone();
    pixels::color_balance(
        &mut p,
        [0.2, -0.1, 0.0],
        [0.0, 0.2, -0.2],
        [-0.1, 0.0, 0.3],
        true,
    );
    compare("balance", &p, &bytes(&f, "balance"), 0);
    let mut p = input.clone();
    pixels::grain(
        &mut p,
        8,
        pixels::Grain {
            amount: 45.0,
            size: 2.5,
            roughness: 70.0,
            seed: 123456,
            origin: [0.0, 0.0],
            units_per_pixel: 1.0,
        },
    );
    compare("grain", &p, &bytes(&f, "grain"), 0);
}
#[test]
fn camera_raw_same_input_as_original_c() {
    let f = fixture();
    let input = bytes(&f, "input");
    let mut straight = input.clone();
    pixels::unpremultiply(&mut straight);
    let raster = Raster::from_rgba(8, 8, straight);
    let cases = [
        (
            "cameraEffects",
            json!({"texture":50,"clarity":30,"dehaze":20,"glow":35,"glowStyle":"Bloom","glowRange":15,"glowSpread":20,"glowWarmth":25,"vignetteAmount":-30,"vignetteMidpoint":40,"vignetteRoundness":20,"vignetteFeather":60,"vignetteHighlights":35}),
        ),
        (
            "cameraDetail",
            json!({"detail":{"sharpenAmount":70,"sharpenRadius":20,"sharpenDetail":40,"sharpenMasking":15,"noiseLuminance":30,"noiseLuminanceDetail":60,"noiseLuminanceContrast":20,"noiseColor":35,"noiseColorDetail":55,"noiseColorSmoothness":60}}),
        ),
        (
            "cameraOptics",
            json!({"optics":{"removeChromaticAberration":true,"purpleAmount":35,"purpleHueLow":270,"purpleHueHigh":310,"greenAmount":25,"greenHueLow":60,"greenHueHigh":120,"vignetteAmount":30,"vignetteMidpoint":40}}),
        ),
    ];
    for (key, settings) in cases {
        let detail = Raster::from_rgba(8, 8, bytes(&f, "inputDetail"));
        let source = if key == "cameraDetail" {
            &detail
        } else {
            &raster
        };
        let result = filters::apply_filter(source, "Camera Raw Filter", &settings).unwrap();
        let mut p = result.to_rgba();
        pixels::premultiply(&mut p);
        compare(key, &p, &bytes(&f, key), 1);
    }
}
#[test]
fn healing_and_content_fill_match_original_c() {
    let f = fixture();
    let latest: Value =
        serde_json::from_str(include_str!("reference/patch_match_1_5_3.json")).unwrap();
    for mode in 0..3 {
        // Content-Aware now uses upstream 1.5.3's PatchMatch reconstruction;
        // the unchanged proximity/texture modes retain their original oracle.
        let reference = if mode == 0 { &latest } else { &f };
        let mut p = bytes(reference, "healInput");
        let mask = bytes(reference, "healMask");
        compositor::brush::spot_heal(&mut p, &mask, 24, 24, 0.75, mode, 123456);
        let key = format!("heal{mode}");
        compare(&key, &p, &bytes(reference, &key), 0);
    }
    let mut p = bytes(&latest, "healInput");
    let mask = bytes(&latest, "healMask");
    assert!(compositor::brush::content_fill(&mut p, &mask, 24, 24));
    compare("contentFill", &p, &bytes(&latest, "contentFill"), 0);
}
#[test]
fn ten_nonfont_dither_styles_match_original_c() {
    let f = fixture();
    let source = Raster::from_rgba(24, 24, bytes(&f, "healInput"));
    let styles = [
        "Atkinson (Classic Mac)",
        "Floyd–Steinberg",
        "Bayer 2 × 2",
        "Bayer 4 × 4",
        "Bayer 8 × 8",
        "Halftone Dots",
        "Halftone Lines",
        "Halftone Diamonds",
        "Mac Patterns",
        "ASCII",
        "Scanlines (CRT)",
    ];
    for (style, name) in styles.iter().enumerate().filter(|(i, _)| *i != 9) {
        let output = filters::apply_filter(&source, "Dither", &json!({"dither":{"style":name,"levels":4,"diffusion":80,"density":20,"contrast":15,"cellSize":4,"angle":0.3f64.to_degrees(),"lightOnDark":true,"colors":"Two Colors","dark":{"red":10./255.,"green":20./255.,"blue":30./255.},"light":{"red":220./255.,"green":230./255.,"blue":240./255.},"dots":40,"wobble":3,"pixelSize":1,"lineSpacing":4,"glow":0}})).unwrap();
        let key = format!("dither{style}");
        compare(&key, &output.to_rgba(), &bytes(&f, &key), 0);
    }
}

#[test]
fn camera_raw_1_5_3_matches_upstream_c() {
    let f: Value = serde_json::from_str(include_str!("reference/camera_raw_1_5_3.json")).unwrap();
    for case in f["cases"].as_array().unwrap() {
        let mut straight = bytes(case, "input");
        pixels::unpremultiply(&mut straight);
        let raster = Raster::from_rgba(
            case["width"].as_u64().unwrap() as u32,
            case["height"].as_u64().unwrap() as u32,
            straight,
        );
        let result =
            filters::apply_filter(&raster, "Camera Raw Filter", &case["settings"]).unwrap();
        let mut actual = result.to_rgba();
        pixels::premultiply(&mut actual);
        compare(
            case["name"].as_str().unwrap(),
            &actual,
            &bytes(case, "expected"),
            1,
        );
    }
}
