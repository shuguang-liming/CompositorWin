use compositor::{
    edit,
    history::History,
    image_io,
    model::{Document, Mask, Raster, Transform},
    project, render,
    text::{self, TextStyle},
};
use image::ImageEncoder;
use serde_json::json;
use std::{fs, sync::Arc};
use uuid::Uuid;

#[test]
fn legacy_packages_v1_through_v11_migrate_to_single_files_without_losing_identity_or_alpha() {
    let directory = tempfile::tempdir().unwrap();
    let document_id = Uuid::parse_str("0C5E7A91-3B2D-4F6A-8E1C-9D0B7A6F5E4D").unwrap();
    let layer_id = Uuid::parse_str("6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F").unwrap();
    let layer_name = "透明照片 · 中文图层";
    let filename = format!("{}.png", layer_id.to_string().to_uppercase());
    // Straight-alpha PNG retains RGB even under zero alpha, plus very low and partial alpha.
    let original_pixels = vec![
        44, 77, 99, 0, 255, 40, 10, 1, 200, 120, 60, 127, 10, 20, 30, 255, 0, 255, 80, 128, 30, 60,
        220, 193,
    ];
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&original_pixels, 3, 2, image::ExtendedColorType::Rgba8)
        .unwrap();
    let check_identity = |document: &Document| {
        assert_eq!(document.id, document_id);
        assert_eq!(document.active_layer_id, Some(layer_id));
        assert_eq!(document.layers.len(), 1);
        assert_eq!(document.layers[0].id, layer_id);
        assert_eq!(document.layers[0].name, layer_name);
        assert_eq!((document.width, document.height), (3, 2));
        assert_eq!(document.resolution, 72.0);
    };
    for version in 1..=11 {
        let path = directory
            .path()
            .join(format!("版本{version}-透明中文.comp"));
        fs::create_dir_all(path.join("images")).unwrap();
        fs::write(path.join("images").join(&filename), &png).unwrap();
        // A representative raster fixture common to every version. It contains only v1
        // fields, not newer groups, masks, adjustments, guides or text-run semantics.
        let manifest = json!({
            "format": "com.compositor.project",
            "version": version,
            "colorSpace": "sRGB",
            "documentID": document_id,
            "width": 3,
            "height": 2,
            "activeLayerID": layer_id,
            "layers": [{
                "id": layer_id,
                "name": layer_name,
                "isVisible": true,
                "imageFile": filename,
                "transform": {
                    "origin": [0, 0], "size": [3, 2], "rotation": 0,
                    "flipX": false, "flipY": false, "sampling": "High quality"
                }
            }]
        });
        fs::write(
            path.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let original_fingerprint = project::fingerprint(&path).unwrap();
        let mut document = project::load(&path).unwrap();
        check_identity(&document);
        assert_eq!(
            document.active().unwrap().image.as_ref().unwrap().to_rgba(),
            original_pixels,
            "Version {version} altered original PNG pixels"
        );
        let mut history = History::default();
        let before = document.clone();
        Arc::make_mut(document.active_mut().unwrap().image.as_mut().unwrap()).set(
            2,
            1,
            [207, 39, 101, 73],
        );
        assert!(history.record("编辑透明像素", before.clone(), &document));
        assert!(history.is_modified());
        assert_eq!(
            before.active().unwrap().image.as_ref().unwrap().to_rgba(),
            original_pixels,
            "Version {version} changed the immutable history source"
        );
        let mut edited_pixels = original_pixels.clone();
        edited_pixels[20..24].copy_from_slice(&[207, 39, 101, 73]);

        assert!(history.undo(&mut document));
        check_identity(&document);
        assert!(!history.is_modified());
        assert_eq!(document, before);
        assert_eq!(
            document.active().unwrap().image.as_ref().unwrap().to_rgba(),
            original_pixels
        );
        assert!(history.redo(&mut document));
        check_identity(&document);
        assert!(history.is_modified());
        assert_eq!(
            document.active().unwrap().image.as_ref().unwrap().to_rgba(),
            edited_pixels
        );

        assert!(
            project::save(&document, &path).is_err(),
            "An existing legacy directory must require Save As"
        );
        assert_eq!(project::fingerprint(&path).unwrap(), original_fingerprint);
        assert_eq!(fs::read(path.join("images").join(&filename)).unwrap(), png);
        let saved_path = directory.path().join(format!("版本{version}-单文件.comp"));
        project::save(&document, &saved_path).unwrap();
        assert!(saved_path.is_file());
        assert_eq!(
            fs::read_dir(directory.path()).unwrap().count(),
            version as usize * 2,
            "Migration must leave only the legacy directory and the new file"
        );
        history.mark_saved(history.revision());
        assert!(!history.is_modified());
        let reopened = project::load(&saved_path).unwrap();
        check_identity(&reopened);
        assert_eq!(reopened, document);
        assert_eq!(
            reopened.active().unwrap().image.as_ref().unwrap().to_rgba(),
            edited_pixels,
            "Version {version} lost pixels or transparency after save/reopen"
        );
    }
}

#[test]
fn shapes_created_by_real_editor_apis_survive_save_and_reopen() {
    let mut document = Document::new(64, 64);
    for (kind, ends) in [
        (edit::ShapeKind::Rectangle, None),
        (edit::ShapeKind::Ellipse, None),
        (edit::ShapeKind::Line, Some([[0.0, 0.0], [1.0, 1.0]])),
    ] {
        let id = edit::add_shape(
            &mut document,
            kind,
            [8.0, 8.0, 32.0, 24.0],
            [255, 40, 90, 255],
            4.0,
            3.0,
            ends,
        )
        .unwrap();
        document.layer_mut(id).unwrap().mask = Some(Mask::white());
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shapes.comp");
    project::save(&document, &path).unwrap();
    assert!(path.is_file());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    assert_eq!(project::load(&path).unwrap(), document);
}

#[test]
fn editable_text_created_and_styled_by_editor_api_survives_save() {
    let mut document = Document::new(64, 64);
    let mut style = TextStyle {
        content: "A🦀B\nSecond line".to_owned(),
        font_name: "Arial".to_owned(),
        font_size: 16.0,
        ..Default::default()
    };
    style.set_color([0.8, 0.2, 0.1], 1..3).unwrap();
    style.set_font("Segoe UI", 3..4).unwrap();
    text::add_text(&mut document, style, [0.0, 0.0]).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("text.comp");
    project::save(&document, &path).unwrap();
    assert!(path.is_file());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    assert_eq!(project::load(&path).unwrap(), document);
}

#[test]
fn moving_one_project_file_preserves_layers_masks_text_transforms_and_exported_pixels() {
    let mut document = Document::new(48, 32);
    document.resolution = 300.0;
    document.layers[0].name = "半透明 背景".to_owned();
    document.layers[0].image = Some(Arc::new(Raster::solid(48, 32, [20, 60, 100, 190])));
    let raster_id = edit::add_layer(&mut document, "变换的照片");
    let raster = document.layer_mut(raster_id).unwrap();
    raster.image = Some(Arc::new(Raster::from_rgba(
        3,
        2,
        vec![
            44, 77, 99, 0, 255, 40, 10, 1, 200, 120, 60, 127, 10, 20, 30, 255, 0, 255, 80, 128, 30,
            60, 220, 193,
        ],
    )));
    raster.transform = Transform {
        origin: [5.5, 4.25],
        size: [18.0, 12.0],
        rotation: 17.0,
        flip_x: true,
        ..Transform::new(0.0, 0.0, 3.0, 2.0)
    };
    raster.opacity = 0.75;
    raster.mask = Some(Mask {
        raster: Arc::new(Raster::from_rgba(
            3,
            2,
            vec![
                0, 0, 0, 255, 63, 63, 63, 255, 127, 127, 127, 255, 191, 191, 191, 255, 223, 223,
                223, 255, 255, 255, 255, 255,
            ],
        )),
        enabled: true,
        linked: false,
        placement: Some(Transform::new(4.0, 3.0, 20.0, 14.0)),
    });
    let text_id = text::add_text(
        &mut document,
        TextStyle {
            content: "中文 A🦀B".to_owned(),
            font_name: "Arial".to_owned(),
            font_size: 12.0,
            ..Default::default()
        },
        [1.0, 16.0],
    )
    .unwrap();
    let expected = render::composite(&document);
    let original_directory = tempfile::tempdir().unwrap();
    let original_path = original_directory.path().join("完整 原工程.comp");
    project::save(&document, &original_path).unwrap();
    assert!(original_path.is_file());
    assert_eq!(fs::read_dir(original_directory.path()).unwrap().count(), 1);

    let destination = tempfile::tempdir().unwrap();
    let moved = destination.path().join("移动 后的工程.comp");
    fs::rename(&original_path, &moved).unwrap();
    assert_eq!(fs::read_dir(original_directory.path()).unwrap().count(), 0);
    let reopened = image_io::import(&moved).unwrap().document;
    assert_eq!(reopened, document);
    assert!(reopened.layer(text_id).unwrap().text.is_some());
    assert_eq!(
        reopened.layer(raster_id).unwrap().mask,
        document.layer(raster_id).unwrap().mask
    );
    let rendered = render::composite(&reopened);
    assert_eq!(rendered, expected);
    let exported = destination.path().join("完整 导出.png");
    image_io::export_png(&rendered, &exported, reopened.resolution).unwrap();
    assert_eq!(image_io::decode_image(&exported).unwrap(), expected);
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 2);
}
