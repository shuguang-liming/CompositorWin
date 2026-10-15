use compositor::{
    gpu::GpuCanvas,
    model::{BlendMode, Document, Layer, Mask, Raster, Sampling},
    pixels, render,
};
use egui_wgpu::{RenderState, Renderer, RendererOptions, wgpu};
use std::sync::Arc;

fn canvas() -> GpuCanvas {
    GpuCanvas::new(render_state())
}
fn render_state() -> RenderState {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("A Windows GPU adapter is required for the GPU validation suite");
    let info = adapter.get_info();
    eprintln!("GPU validation: {} {:?}", info.name, info.backend);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("Compositor validation"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .unwrap();
    let renderer = Renderer::new(
        &device,
        wgpu::TextureFormat::Rgba8Unorm,
        RendererOptions::default(),
    );
    RenderState {
        adapter: adapter.clone(),
        available_adapters: vec![adapter],
        device,
        queue,
        target_format: wgpu::TextureFormat::Rgba8Unorm,
        renderer: Arc::new(eframe::egui::mutex::RwLock::new(renderer)),
    }
}
fn parity(gpu: &mut GpuCanvas, d: &Document, tolerance: u8) {
    gpu.render(d, d.width.max(d.height));
    let actual = gpu.readback().unwrap();
    let expected = render::render_document(d);
    let mut a = actual.to_rgba();
    let mut b = expected.to_rgba();
    pixels::premultiply(&mut a);
    pixels::premultiply(&mut b);
    let differences: Vec<_> = a
        .iter()
        .zip(&b)
        .enumerate()
        .filter(|(_, (a, b))| a.abs_diff(**b) > tolerance)
        .take(15)
        .collect();
    assert!(
        differences.is_empty(),
        "GPU differs from CPU: {differences:?}"
    );
}
#[test]
fn native_viewport_preserves_original_detail_masks_and_resident_sources_while_panning() {
    use compositor::preview::ViewRequest;
    let mut gpu = canvas();
    let mut doc = Document::new(4096, 8);
    let pixels = Raster::from_rgba(
        4096,
        8,
        (0..4096 * 8)
            .flat_map(|index| {
                let value = if index % 2 == 0 { 0 } else { 255 };
                [value, value, value, 255]
            })
            .collect(),
    );
    doc.layers = vec![Layer::raster("Original detail", pixels)];
    let mut view = ViewRequest::new([4096, 8], [2001.0, 1.0, 256.0, 4.0], 1.0).unwrap();
    for stage in 0..3 {
        if stage == 1 {
            view.region[0] += 1;
        } else if stage == 2 {
            let mut mask = Mask::white();
            mask.raster = Arc::new(Raster::from_rgba(
                64,
                8,
                (0..64 * 8)
                    .flat_map(|index| {
                        let value = if index % 64 < 32 { 0 } else { 255 };
                        [value, value, value, 255]
                    })
                    .collect(),
            ));
            mask.placement = Some(crate_transform(2000.0, 0.0, 64.0, 8.0));
            doc.layers[0].mask = Some(mask);
        }
        let uploaded = gpu.uploaded_bytes;
        assert!(gpu.render_view(&doc, view).is_some());
        let actual = gpu.readback().unwrap();
        let expected = render::render_region(&doc, view.document_region(), view.width, view.height);
        assert_eq!(actual, expected, "Viewport mismatch at stage {stage}");
        if stage == 1 {
            assert_eq!(
                gpu.uploaded_bytes, uploaded,
                "Panning reuses resident original source pixels"
            );
        }
    }
    doc.layers[0].image = Some(Arc::new(Raster::solid(16384, 1, [255; 4])));
    assert!(
        gpu.render_view(&doc, view).is_none(),
        "Oversized source must fall back to original-pixel CPU composition"
    );
}
#[test]
fn all_blends_alpha_endpoints_transform_masks_and_cached_tiles() {
    let mut gpu = canvas();
    let colors = [
        [0, 0, 0, 255],
        [255, 255, 255, 255],
        [102, 102, 102, 255],
        [204, 150, 40, 128],
        [10, 220, 180, 64],
        [255, 0, 0, 0],
        [0, 40, 255, 255],
        [255, 0, 255, 128],
    ];
    let bottom: Vec<_> = (0..64).flat_map(|i| colors[i % colors.len()]).collect();
    let top: Vec<_> = (0..64)
        .flat_map(|i| colors[(i / 8 + i * 3) % colors.len()])
        .collect();
    let mut d = Document::new(8, 8);
    d.layers = vec![
        Layer::raster("Base", Raster::from_rgba(8, 8, bottom)),
        Layer::raster("Blend", Raster::from_rgba(8, 8, top)),
    ];
    for mode in BlendMode::ALL {
        eprintln!("Blend parity: {mode:?}");
        d.layers[1].blend_mode = mode;
        parity(&mut gpu, &d, 1);
    }
    d.layers[1].blend_mode = BlendMode::Normal;
    d.layers[1].transform.origin = [1.25, 0.75];
    d.layers[1].transform.size = [5.5, 6.25];
    d.layers[1].transform.rotation = 27.0;
    d.layers[1].transform.flip_x = true;
    d.layers[1].transform.flip_y = true;
    for sampling in [Sampling::Nearest, Sampling::Smooth] {
        d.layers[1].transform.sampling = sampling;
        parity(&mut gpu, &d, 1);
    }
    d.layers[1].transform = crate_transform(0.0, 0.0, 8.0, 8.0);
    let mask = Raster::from_rgba(
        8,
        8,
        (0..64)
            .flat_map(|i| {
                let v = (i * 4) as u8;
                [v, v, v, 255]
            })
            .collect(),
    );
    d.layers[1].mask = Some(Mask {
        raster: Arc::new(mask),
        enabled: true,
        linked: true,
        placement: None,
    });
    parity(&mut gpu, &d, 1);
    let before = gpu.uploaded_bytes;
    gpu.render(&d, 8);
    assert_eq!(
        gpu.uploaded_bytes, before,
        "Unchanged frame uploads no source bytes"
    );
    Arc::make_mut(d.layers[1].image.as_mut().unwrap()).set(2, 2, [12, 23, 34, 255]);
    parity(&mut gpu, &d, 1);
    assert!(
        gpu.uploaded_bytes - before <= 128 * 128 * 4,
        "Brush edit uploads one tile"
    );
}
fn crate_transform(x: f64, y: f64, w: f64, h: f64) -> compositor::model::Transform {
    compositor::model::Transform::new(x, y, w, h)
}

#[test]
fn layer_bindings_survive_edits_and_rebuild_for_texture_mask_and_canvas_changes() {
    let state = render_state();
    let mut gpu = GpuCanvas::new(state.clone());
    let check = |gpu: &mut GpuCanvas, d: &Document, stage: &str| {
        eprintln!("Binding cache stage: {stage}");
        // This two-layer UNORM-clear fixture differs by two channel levels from CPU on Vulkan.
        // Binding reuse must still match independently allocated GPU resources exactly.
        parity(gpu, d, 2);
        let cached = gpu.readback().unwrap();
        let mut fresh = GpuCanvas::new(state.clone());
        fresh.render(d, d.width.max(d.height));
        assert_eq!(
            cached,
            fresh.readback().unwrap(),
            "Cached bindings changed pixels at {stage}"
        );
    };
    let mut d = Document::new(16, 16);
    d.layers = vec![
        Layer::raster("Base", Raster::solid(16, 16, [20, 80, 160, 255])),
        Layer::raster("Top", Raster::solid(8, 8, [220, 80, 30, 200])),
    ];
    check(&mut gpu, &d, "initial allocation");
    let initial = gpu.binding_allocations;
    assert_eq!(initial, 2);
    d.layers[1].transform.origin = [3., 2.];
    d.layers[1].opacity = 0.7;
    d.layers[1].blend_mode = BlendMode::Screen;
    check(&mut gpu, &d, "transform opacity and blend edit");
    Arc::make_mut(d.layers[1].image.as_mut().unwrap()).set(2, 3, [0, 255, 0, 255]);
    check(&mut gpu, &d, "source tile edit");
    d.layers.swap(0, 1);
    check(&mut gpu, &d, "layer order edit");
    assert_eq!(gpu.binding_allocations, initial);

    d.layers[0].mask = Some(Mask::white());
    check(&mut gpu, &d, "add mask");
    assert_eq!(gpu.binding_allocations, initial + 1);
    d.layers[0].mask.as_mut().unwrap().enabled = false;
    check(&mut gpu, &d, "disable mask");
    assert_eq!(gpu.binding_allocations, initial + 2);
    d.layers[0].image = Some(Arc::new(Raster::solid(4, 4, [10, 20, 30, 255])));
    check(&mut gpu, &d, "replace source dimensions");
    assert_eq!(gpu.binding_allocations, initial + 3);
    d.width = 17;
    check(&mut gpu, &d, "resize canvas");
    assert_eq!(gpu.binding_allocations, initial + 5);

    for layer in &mut d.layers {
        layer.visible = false;
    }
    check(&mut gpu, &d, "hide layers");
    assert_eq!(gpu.resident_source_bytes(), 0);
    d.layers[0].visible = true;
    check(&mut gpu, &d, "restore visibility");
    assert_eq!(gpu.binding_allocations, initial + 6);
}
#[test]
fn hierarchy_order_and_folder_opacity() {
    let mut gpu = canvas();
    let mut d = Document::new(2, 1);
    let mut folder = Layer::new("Folder", 2, 1);
    folder.is_group = true;
    folder.opacity = 0.5;
    let id = folder.id;
    let mut child = Layer::raster("Child", Raster::solid(2, 1, [255, 0, 0, 255]));
    child.parent_id = Some(id);
    let outside = Layer::raster("Outside", Raster::solid(2, 1, [0, 0, 255, 255]));
    d.layers = vec![folder, outside, child];
    parity(&mut gpu, &d, 1);
}
#[test]
fn high_sampling_averages_fine_detail_and_regenerates_edited_mips() {
    let mut gpu = canvas();
    let mut d = Document::new(256, 256);
    let data = (0..256 * 256)
        .flat_map(|i| {
            let v = if (i % 256 + i / 256) % 2 == 0 { 0 } else { 255 };
            [v, v, v, 255]
        })
        .collect();
    d.layers = vec![Layer::raster("Checker", Raster::from_rgba(256, 256, data))];
    d.layers[0].transform.sampling = Sampling::High;
    gpu.render(&d, 32);
    let actual = gpu.readback().unwrap();
    let p = actual.get(16, 16);
    assert!(p[..3].iter().all(|&v| v.abs_diff(128) <= 1) && p[3] == 255);
    let before = gpu.uploaded_bytes;
    gpu.render(&d, 32);
    assert_eq!(gpu.uploaded_bytes, before);
    let r = Arc::make_mut(d.layers[0].image.as_mut().unwrap());
    for y in 0..128 {
        for x in 0..128 {
            r.set(x, y, [255, 0, 0, 255]);
        }
    }
    gpu.render(&d, 32);
    assert_eq!(gpu.readback().unwrap().get(8, 8), [255, 0, 0, 255]);
    assert_eq!(gpu.uploaded_bytes - before, 128 * 128 * 4);
}
#[test]
fn oversized_uniform_raster_edits_upload_only_scaled_dirty_tile() {
    let mut gpu = canvas();
    let mut d = Document::new(9000, 1);
    d.layers = vec![Layer::raster(
        "Wide",
        Raster::solid(9000, 1, [0, 0, 255, 255]),
    )];
    gpu.render(&d, 1024);
    assert_eq!(gpu.uploaded_bytes, 4);
    let before = gpu.uploaded_bytes;
    let r = Arc::make_mut(d.layers[0].image.as_mut().unwrap());
    for x in 4096..4224 {
        r.set(x, 0, [255, 0, 0, 255]);
    }
    gpu.render(&d, 1024);
    assert!(
        gpu.uploaded_bytes - before <= 128 * 4,
        "Scaled tile upload: {}",
        gpu.uploaded_bytes - before
    );
    let output = gpu.readback().unwrap();
    assert_eq!(output.get(470, 0), [255, 0, 0, 255]);
    assert_eq!(output.get(100, 0), [0, 0, 255, 255]);
}
#[test]
fn linked_mask_keeps_placement_and_white_exterior() {
    let mut gpu = canvas();
    let mut d = Document::new(5, 1);
    let mut layer = Layer::raster("Source", Raster::solid(5, 1, [255, 0, 0, 255]));
    layer.mask = Some(Mask {
        raster: Arc::new(Raster::from_rgba(
            3,
            1,
            vec![255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255],
        )),
        enabled: true,
        linked: true,
        placement: Some(crate_transform(1., 0., 3., 1.)),
    });
    d.layers = vec![layer];
    parity(&mut gpu, &d, 1);
    assert_eq!(gpu.readback().unwrap().get(0, 0), [255, 0, 0, 255]);
    assert_eq!(gpu.readback().unwrap().get(2, 0), [0; 4]);
    d.layers[0].mask.as_mut().unwrap().raster = Arc::new(Raster::solid(1, 1, [128, 128, 128, 255]));
    parity(&mut gpu, &d, 1);
    assert_eq!(gpu.readback().unwrap().get(0, 0)[3], 128);
}

#[test]
fn high_bicubic_gradient_alpha_transforms_masks_and_all_blends_match_cpu() {
    let mut gpu = canvas();
    let mut d = Document::new(29, 23);
    let source = Raster::from_rgba(
        9,
        7,
        (0..63)
            .flat_map(|i| {
                let x = i % 9;
                let y = i / 9;
                [
                    (x * 31).min(255) as u8,
                    (y * 41).min(255) as u8,
                    if (x + y) % 2 == 0 { 255 } else { 0 },
                    ((x * 27 + y * 13) % 256) as u8,
                ]
            })
            .collect(),
    );
    let background = Layer::raster("Background", Raster::solid(29, 23, [70, 140, 90, 180]));
    let mut layer = Layer::raster("Bicubic", source);
    layer.transform = crate_transform(3.25, 2.125, 18.5, 16.25);
    layer.transform.rotation = 23.;
    layer.transform.flip_x = true;
    d.layers = vec![background, layer];
    for mode in BlendMode::ALL {
        d.layers[1].blend_mode = mode;
        parity(&mut gpu, &d, 2);
    }
    d.layers[1].blend_mode = BlendMode::Normal;
    d.layers[1].mask = Some(Mask {
        raster: Arc::new(Raster::from_rgba(
            5,
            3,
            (0..15)
                .flat_map(|i| {
                    let gray = if i == 7 { 0 } else { 255 };
                    [gray, gray, gray, 255]
                })
                .collect(),
        )),
        enabled: true,
        linked: true,
        placement: Some(crate_transform(2.25, 1.75, 22.5, 18.25)),
    });
    for sampling in [Sampling::Nearest, Sampling::Smooth, Sampling::High] {
        d.layers[1]
            .mask
            .as_mut()
            .unwrap()
            .placement
            .as_mut()
            .unwrap()
            .sampling = sampling;
        parity(&mut gpu, &d, 2);
    }
    d.layers[1].mask = None;
    d.layers[1].transform = crate_transform(3.15, 4.7, 3.75, 2.25);
    d.layers[1].transform.rotation = -17.;
    parity(&mut gpu, &d, 2);
    let before = gpu.uploaded_bytes;
    gpu.render(&d, 29);
    assert_eq!(
        gpu.uploaded_bytes, before,
        "Bicubic sampling retains cached source textures"
    );
}

fn cached_source_matches_fresh(
    gpu: &mut GpuCanvas,
    state: &RenderState,
    doc: &Document,
    stage: &str,
) -> Raster {
    parity(gpu, doc, 1);
    let cached = gpu.readback().unwrap();
    let mut fresh = GpuCanvas::new(state.clone());
    fresh.render(doc, doc.width.max(doc.height));
    let independent = fresh.readback().unwrap();
    assert!(
        cached.to_rgba() == independent.to_rgba(),
        "Cached source differs byte-for-byte from fresh GPU at {stage}"
    );
    cached
}

fn assert_exact_pixels(actual: &Raster, expected: &Raster, stage: &str) {
    assert_eq!(
        (actual.width, actual.height),
        (expected.width, expected.height)
    );
    let actual_bytes = actual.to_rgba();
    let expected_bytes = expected.to_rgba();
    let width = actual.width as usize;
    let differences: Vec<_> = actual_bytes
        .iter()
        .zip(&expected_bytes)
        .enumerate()
        .filter(|(_, (actual, expected))| actual != expected)
        .take(8)
        .map(|(index, (&actual, &expected))| {
            (
                index / 4 % width,
                index / 4 / width,
                index % 4,
                actual,
                expected,
            )
        })
        .collect();
    assert!(
        differences.is_empty(),
        "Pixels differ at {stage}; first (x, y, channel, actual, expected): {differences:?}"
    );
}

#[test]
fn uniform_and_empty_sources_keep_other_tiles_when_one_tile_is_removed() {
    let state = render_state();
    let mut gpu = GpuCanvas::new(state.clone());
    for blank in [false, true] {
        let mut source = if blank {
            Raster::blank(257, 129)
        } else {
            Raster::solid(257, 129, [0, 0, 255, 255])
        };
        source.set(20, 17, [255, 0, 0, 255]);
        source.set(256, 128, [0, 255, 0, 255]);
        let kept_tile = source.tiles[&(0, 0)].clone();
        let mut doc = Document::new(257, 129);
        doc.layers = vec![Layer::raster("Sparse source", source)];
        let undo = doc.layers[0].image.clone().unwrap();
        cached_source_matches_fresh(&mut gpu, &state, &doc, "two sparse tiles");
        let before = gpu.uploaded_bytes;

        let raster = Arc::make_mut(doc.layers[0].image.as_mut().unwrap());
        raster.tiles.remove(&(2, 1));
        assert!(Arc::ptr_eq(&raster.tiles[&(0, 0)], &kept_tile));
        let result = cached_source_matches_fresh(&mut gpu, &state, &doc, "remove edge tile");
        assert_eq!(result.get(20, 17), [255, 0, 0, 255]);
        assert_eq!(
            result.get(256, 128),
            if blank { [0; 4] } else { [0, 0, 255, 255] }
        );
        assert_eq!(gpu.uploaded_bytes - before, 4 + 128 * 128 * 4);

        doc.layers[0].image = Some(undo);
        let result = cached_source_matches_fresh(&mut gpu, &state, &doc, "undo removed edge tile");
        assert_eq!(result.get(20, 17), [255, 0, 0, 255]);
        assert_eq!(result.get(256, 128), [0, 255, 0, 255]);
        let before = gpu.uploaded_bytes;
        gpu.render(&doc, 257);
        assert_eq!(
            gpu.uploaded_bytes, before,
            "Unchanged source uploads nothing"
        );
    }
}

#[test]
fn materialized_source_upload_includes_tiles_once_for_initial_replace_and_removal() {
    let state = render_state();
    let mut gpu = GpuCanvas::new(state.clone());
    let mut source = Raster::from_rgba(257, 129, [20, 80, 160, 255].repeat(257 * 129));
    source.set(20, 17, [255, 0, 0, 255]);
    source.set(256, 128, [0, 255, 0, 255]);
    let mut doc = Document::new(257, 129);
    doc.layers = vec![Layer::raster("Dense source with tiles", source)];
    let undo = doc.layers[0].image.clone().unwrap();
    let full_bytes = 257 * 129 * 4;
    cached_source_matches_fresh(&mut gpu, &state, &doc, "initial materialized upload");
    assert_eq!(gpu.uploaded_bytes, full_bytes);

    let before = gpu.uploaded_bytes;
    Arc::make_mut(doc.layers[0].image.as_mut().unwrap()).pixels =
        Arc::new([40, 80, 160, 255].repeat(257 * 129));
    let result = cached_source_matches_fresh(&mut gpu, &state, &doc, "replace dense base");
    assert_eq!(gpu.uploaded_bytes - before, full_bytes);
    assert_eq!(result.get(20, 17), [255, 0, 0, 255]);
    assert_eq!(result.get(256, 128), [0, 255, 0, 255]);

    let before = gpu.uploaded_bytes;
    Arc::make_mut(doc.layers[0].image.as_mut().unwrap())
        .tiles
        .remove(&(2, 1));
    let result = cached_source_matches_fresh(&mut gpu, &state, &doc, "remove dense edge tile");
    assert_eq!(gpu.uploaded_bytes - before, full_bytes);
    assert_eq!(result.get(20, 17), [255, 0, 0, 255]);
    assert_eq!(result.get(256, 128), [40, 80, 160, 255]);

    let before = gpu.uploaded_bytes;
    doc.layers[0].image = Some(undo);
    cached_source_matches_fresh(&mut gpu, &state, &doc, "undo dense base and edge tile");
    assert_eq!(gpu.uploaded_bytes - before, full_bytes);
}

#[test]
fn uniform_mask_keeps_other_tiles_after_removal_and_undo() {
    let state = render_state();
    let mut gpu = GpuCanvas::new(state.clone());
    let mut mask = Raster::solid(257, 129, [255; 4]);
    mask.set(20, 17, [0, 0, 0, 255]);
    mask.set(256, 128, [0, 0, 0, 255]);
    let kept_tile = mask.tiles[&(0, 0)].clone();
    let mut layer = Layer::raster("Masked source", Raster::solid(257, 129, [255, 0, 0, 255]));
    layer.mask = Some(Mask {
        raster: Arc::new(mask),
        enabled: true,
        linked: true,
        placement: None,
    });
    let mut doc = Document::new(257, 129);
    doc.layers = vec![layer];
    let undo = doc.layers[0].mask.as_ref().unwrap().raster.clone();
    cached_source_matches_fresh(&mut gpu, &state, &doc, "two mask tiles");
    let before = gpu.uploaded_bytes;

    let mask = Arc::make_mut(&mut doc.layers[0].mask.as_mut().unwrap().raster);
    mask.tiles.remove(&(2, 1));
    assert!(Arc::ptr_eq(&mask.tiles[&(0, 0)], &kept_tile));
    let result = cached_source_matches_fresh(&mut gpu, &state, &doc, "remove mask edge tile");
    assert_eq!(gpu.uploaded_bytes - before, 4 + 128 * 128 * 4);
    assert_eq!(result.get(20, 17), [0; 4]);
    assert_eq!(result.get(256, 128), [255, 0, 0, 255]);

    doc.layers[0].mask.as_mut().unwrap().raster = undo;
    let result = cached_source_matches_fresh(&mut gpu, &state, &doc, "undo mask edge tile");
    assert_eq!(result.get(20, 17), [0; 4]);
    assert_eq!(result.get(256, 128), [0; 4]);
}

#[test]
fn mipmap_bindings_reuse_brush_regions_without_changing_downsampled_pixels() {
    let state = render_state();
    let mut gpu = GpuCanvas::new(state.clone());
    let (width, height) = (1281, 1025);
    let pixels = (0..width * height)
        .flat_map(|i| {
            let x = i % width;
            let y = i / width;
            [
                ((x * 17 + y * 31) & 255) as u8,
                ((x * 7 + y * 19) & 255) as u8,
                ((x * 13 + y * 3) & 255) as u8,
                (32 + (x * 11 + y * 5) % 224) as u8,
            ]
        })
        .collect();
    let mut doc = Document::new(width, height);
    doc.layers = vec![Layer::raster(
        "Odd-sized translucent source",
        Raster::from_rgba(width, height, pixels),
    )];
    doc.layers[0].transform.sampling = Sampling::High;
    let initial_source = doc.layers[0].image.clone().unwrap();
    let check = |gpu: &mut GpuCanvas, doc: &Document, stage: &str| {
        gpu.render(doc, 257);
        let actual = gpu.readback().unwrap();
        let mut independent = GpuCanvas::new(state.clone());
        let mut full = doc.clone();
        for layer in &mut full.layers {
            if let Some(image) = &mut layer.image {
                // Preserve uniform clears: their existing UNORM rounding can differ from dense uploads.
                if image.pixels.len() > 4 {
                    *image = Arc::new(image.materialized());
                }
            }
            if let Some(mask) = &mut layer.mask {
                mask.raster = Arc::new(mask.raster.materialized());
            }
        }
        independent.render(&full, 257);
        assert_exact_pixels(&actual, &independent.readback().unwrap(), stage);
        actual
    };
    let original = check(&mut gpu, &doc, "initial translucent source");
    let allocations = gpu.mip_binding_allocations;
    assert_eq!(allocations, 10);
    let edit = |doc: &mut Document, points: &[(u32, u32)], color: [u8; 4]| {
        let raster = Arc::make_mut(doc.layers[0].image.as_mut().unwrap());
        for &(x, y) in points {
            raster.set(x, y, color);
        }
    };
    let crossing = [(127, 127), (128, 127), (127, 128), (128, 128)];
    edit(&mut doc, &crossing, [229, 41, 101, 193]);
    let dispatches = gpu.mip_dispatches;
    check(&mut gpu, &doc, "four adjacent brush tiles");
    assert_eq!(gpu.mip_dispatches - dispatches, 10);
    assert_eq!(gpu.mip_binding_allocations, allocations);
    edit(&mut doc, &crossing, [31, 221, 87, 137]);
    check(&mut gpu, &doc, "repeated adjacent brush tiles");
    assert_eq!(gpu.mip_binding_allocations, allocations);

    let separated = [(3, 4), (512, 384), (width - 1, height - 1)];
    edit(&mut doc, &separated, [244, 189, 17, 212]);
    check(&mut gpu, &doc, "separated regions grow the uniform buffer");
    let grown = gpu.mip_binding_allocations;
    assert!(grown > allocations);
    edit(&mut doc, &separated, [83, 46, 232, 171]);
    check(
        &mut gpu,
        &doc,
        "separated regions reuse distinct dynamic offsets",
    );
    assert_eq!(gpu.mip_binding_allocations, grown);
    let dispatches = gpu.mip_dispatches;
    check(&mut gpu, &doc, "unchanged source");
    assert_eq!(gpu.mip_dispatches, dispatches);

    doc.layers[0].image = Some(initial_source);
    assert_exact_pixels(
        &check(&mut gpu, &doc, "undo"),
        &original,
        "undo original pixels",
    );
    assert_eq!(gpu.mip_binding_allocations, grown);
    doc.layers[0].image = Some(Arc::new(Raster::solid(513, 385, [99, 154, 216, 173])));
    check(&mut gpu, &doc, "replace source dimensions");
    let mut another = Document::new(129, 257);
    another.layers = vec![Layer::raster(
        "Different document",
        Raster::solid(129, 257, [216, 39, 154, 183]),
    )];
    check(&mut gpu, &another, "switch document");
    check(&mut gpu, &doc, "restore previous document");
}

#[test]
fn mask_edits_match_full_refresh_without_unused_mipmap_work() {
    let state = render_state();
    let mut gpu = GpuCanvas::new(state.clone());
    let mut layer = Layer::raster("Masked source", Raster::solid(513, 257, [91, 199, 33, 187]));
    layer.transform.sampling = Sampling::High;
    layer.mask = Some(Mask {
        raster: Arc::new(Raster::solid(513, 257, [255; 4])),
        enabled: true,
        linked: true,
        placement: None,
    });
    let mut doc = Document::new(513, 257);
    doc.layers = vec![layer];
    let undo = doc.layers[0].mask.as_ref().unwrap().raster.clone();
    let check = |gpu: &mut GpuCanvas, doc: &Document| {
        gpu.render(doc, 73);
        let actual = gpu.readback().unwrap();
        let mut independent = GpuCanvas::new(state.clone());
        let mut full = doc.clone();
        let mask = full.layers[0].mask.as_mut().unwrap();
        mask.raster = Arc::new(mask.raster.materialized());
        independent.render(&full, 73);
        assert_exact_pixels(
            &actual,
            &independent.readback().unwrap(),
            "mask full refresh",
        );
    };
    check(&mut gpu, &doc);
    let dispatches = gpu.mip_dispatches;
    let allocations = gpu.mip_binding_allocations;
    assert_eq!(allocations, 9, "Only the image source owns a mip chain");
    let source_mip_bytes: u64 = (0..10)
        .map(|level| ((513_u32 >> level).max(1) * (257_u32 >> level).max(1) * 4) as u64)
        .sum();
    assert_eq!(
        gpu.resident_source_bytes(),
        source_mip_bytes + 513 * 257 * 4
    );
    for value in [64, 128, 192] {
        let mask = Arc::make_mut(&mut doc.layers[0].mask.as_mut().unwrap().raster);
        for (x, y) in [(127, 127), (128, 128), (384, 255), (512, 256)] {
            mask.set(x, y, [value, value, value, 255]);
        }
        check(&mut gpu, &doc);
        assert_eq!(gpu.mip_dispatches, dispatches);
        assert_eq!(gpu.mip_binding_allocations, allocations);
    }
    doc.layers[0].mask.as_mut().unwrap().raster = undo;
    check(&mut gpu, &doc);
    assert_eq!(gpu.mip_dispatches, dispatches);
}
