//! Behavioral checks across codec, document, editing, history, rendering and persistence.
use compositor::{
    deep_pixels::{DeepRaster, Depth, encoded, linear},
    edit, filter_edit,
    history::History,
    image_io,
    model::{Document, Layer, Mask, Raster, Sampling, Transform},
    preview::PreviewCache,
    project, render, selection,
};
use serde_json::json;
use std::{collections::BTreeSet, sync::Arc};

fn document(raster: Raster) -> Document {
    let mut doc = Document::new(raster.width, raster.height);
    doc.layers[0].image = Some(Arc::new(raster));
    doc.layers[0].transform.sampling = Sampling::Nearest;
    doc
}

#[test]
fn sixteen_bit_import_edit_history_project_and_export_retain_individual_low_bits() {
    let folder = tempfile::tempdir().unwrap();
    let source_path = folder.path().join("细腻渐变.png");
    let bits: Vec<u16> = (0..512)
        .flat_map(|x| [30000 + x, 40000 - x, 10000 + x, 65535])
        .collect();
    let source = Raster::from_deep(DeepRaster::from_u16(512, 1, bits).unwrap());
    image_io::export_raster(&source, &source_path, "png", 300., 90, [255; 3]).unwrap();
    let mut doc = document(image_io::decode_image(&source_path).unwrap());
    doc.resolution = 300.;
    let id = doc.active_layer_id.unwrap();
    doc.selection = Some(selection::marquee(
        512,
        1,
        [128., 0., 256., 1.],
        false,
        false,
    ));
    let before = doc.clone();
    filter_edit::apply(&mut doc, id, false, "Invert", &json!({})).unwrap();
    let changed = doc
        .active()
        .unwrap()
        .image
        .as_ref()
        .unwrap()
        .deep
        .as_ref()
        .unwrap()
        .to_bits();
    assert_eq!(changed[0], 30000);
    assert_eq!(changed[128 * 4], 65535 - 30128);
    assert_eq!(changed[384 * 4], 30384);
    let mut history = History::default();
    assert!(history.record("反相", before.clone(), &doc));
    assert!(history.undo(&mut doc));
    assert_eq!(doc, before);
    assert!(history.redo(&mut doc));
    let project_path = folder.path().join("高位深工程.comp");
    project::save(&doc, &project_path).unwrap();
    assert!(project_path.is_file());
    let reopened = project::load(&project_path).unwrap();
    assert_eq!(
        reopened
            .active()
            .unwrap()
            .image
            .as_ref()
            .unwrap()
            .deep
            .as_ref()
            .unwrap()
            .to_bits(),
        changed
    );
    let composite = render::render_document(&reopened);
    assert_eq!(composite.deep.as_ref().unwrap().to_bits(), changed);
    let exported = folder.path().join("编辑后.tiff");
    image_io::export_raster(&composite, &exported, "tiff", 300., 90, [255; 3]).unwrap();
    let decoded = image_io::decode_image(&exported).unwrap();
    assert_eq!(decoded.deep.as_ref().unwrap().to_bits(), changed);
    let colors: BTreeSet<_> = (0..128)
        .map(|x| decoded.deep.as_ref().unwrap().get_bits(x, 0)[0])
        .collect();
    assert_eq!(colors.len(), 128);
}

#[test]
fn hdr_exr_mask_adjustment_project_and_export_recover_unclipped_highlights() {
    let folder = tempfile::tempdir().unwrap();
    let exr = folder.path().join("超白亮部.exr");
    let source =
        Raster::from_deep(DeepRaster::from_f32(1, 1, Depth::Hdr, &[4., 2., 1., 0.5]).unwrap());
    image_io::export_raster(&source, &exr, "exr", 72., 90, [255; 3]).unwrap();
    let mut doc = document(image_io::decode_image(&exr).unwrap());
    assert_eq!(
        doc.active().unwrap().image.as_ref().unwrap().get_f32(0, 0),
        [4., 2., 1., 0.5]
    );
    doc.active_mut().unwrap().mask = Some(Mask {
        raster: Arc::new(Raster::solid(1, 1, [128, 128, 128, 255])),
        enabled: true,
        linked: true,
        placement: None,
    });
    let mut adjustment = Layer::new("找回高光", 1, 1);
    adjustment.adjustment = Some(json!({"kind":"Exposure","exposure":-6.,"offset":0.,"gamma":1.}));
    doc.layers.push(adjustment);
    let project_path = folder.path().join("HDR.comp");
    project::save(&doc, &project_path).unwrap();
    let doc = project::load(&project_path).unwrap();
    let raster = render::render_document(&doc);
    let p = raster.get_f32(0, 0);
    let expected = [4f32, 2., 1.].map(|v| encoded(linear(v) / 64.));
    for c in 0..3 {
        assert!((p[c] - expected[c]).abs() < 0.002, "{p:?}, {expected:?}");
    }
    assert!((p[3] - 0.5 * 128. / 255.).abs() < 0.0003);
    let out = folder.path().join("HDR编辑结果.exr");
    image_io::export_raster(&raster, &out, "exr", 72., 90, [255; 3]).unwrap();
    let decoded = image_io::decode_image(&out).unwrap();
    assert_eq!(
        decoded.deep.unwrap().to_bits(),
        raster.deep.unwrap().to_bits()
    );
}

#[test]
fn native_region_spatial_adjustment_sees_pixels_outside_the_requested_view() {
    let mut source = DeepRaster::solid(32, 16, Depth::U16, [0., 0., 0., 1.]);
    for y in 0..16 {
        for x in 0..16 {
            source.set(x, y, [1., 1., 1., 1.]);
        }
    }
    let mut doc = document(Raster::from_deep(source));
    let mut blur = Layer::new("高斯调整", 32, 16);
    blur.adjustment = Some(json!({"kind":"Gaussian Blur","blurRadius":3.}));
    doc.layers.push(blur);
    let native = render::render_document(&doc);
    let region = render::render_region(&doc, [16., 4., 8., 8.], 8, 8);
    assert!(
        native.get_f32(16, 8)[0] > 0.05,
        "native blur must cross the white/black boundary"
    );
    for y in 0..8 {
        for x in 0..8 {
            let expected = native.get_f32(x + 16, y + 4);
            let actual = region.get_f32(x, y);
            for c in 0..4 {
                assert!(
                    (actual[c] - expected[c]).abs() <= 2. / 65535.,
                    "{x},{y}: {actual:?} vs {expected:?}"
                );
            }
        }
    }
}

#[test]
fn clipping_retains_fractional_deep_alpha_even_with_an_intervening_layer() {
    let mut doc = document(Raster::from_deep(
        DeepRaster::from_f32(1, 1, Depth::Hdr, &[0., 0., 1., 0.5004883]).unwrap(),
    ));
    let base = doc.active_layer_id.unwrap();
    let mut unrelated = Layer::raster("其他图层", Raster::solid(1, 1, [0; 4]));
    unrelated.transform.origin = [10., 10.];
    doc.layers.push(unrelated);
    let mut clipped = Layer::raster("剪贴", Raster::solid(1, 1, [255, 0, 0, 255]));
    clipped.mask_source_id = Some(base);
    clipped.transform = Transform::new(0., 0., 1., 1.);
    doc.layers.push(clipped);
    let result = render::render_document(&doc).get_f32(0, 0);
    let a = 0.5004883f32;
    let expected_alpha = a + a * (1. - a);
    assert!(
        (result[3] - expected_alpha).abs() < 0.0003,
        "{result:?} expected alpha {expected_alpha}"
    );
}

#[test]
fn hdr_only_change_invalidates_view_cache_even_when_display_bytes_match() {
    let mut doc = document(Raster::from_deep(
        DeepRaster::from_f32(1, 1, Depth::Hdr, &[4., 2., 1., 1.]).unwrap(),
    ));
    let previous = doc.clone();
    let mut cache = PreviewCache::default();
    let first = cache.render(&doc, 128);
    Arc::make_mut(doc.active_mut().unwrap().image.as_mut().unwrap()).set_f32(
        0,
        0,
        [8., 2., 1., 1.],
    );
    assert_eq!(
        previous.active().unwrap().image.as_ref().unwrap().to_rgba(),
        doc.active().unwrap().image.as_ref().unwrap().to_rgba()
    );
    assert!(!render::same_composite(&previous, &doc));
    let second = cache.render(&doc, 128);
    assert!(!Arc::ptr_eq(&first, &second));
    let mut history = History::default();
    assert!(history.record("高光", previous.clone(), &doc));
    assert!(history.undo(&mut doc));
    assert_eq!(doc, previous);
    edit::convert_layer_depth(&mut doc, previous.active_layer_id.unwrap(), None).unwrap();
    assert!(doc.active().unwrap().image.as_ref().unwrap().deep.is_none());
}
