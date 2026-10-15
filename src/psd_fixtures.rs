use super::*;
#[derive(Clone)]
struct LayerFixture {
    name: &'static str,
    section: Option<u32>,
    rgba: Option<[u8; 4]>,
    mask: bool,
    mask_bounds: [i32; 4],
    mask_flags: u8,
    clipped: bool,
    opacity: u8,
    hidden: bool,
    extra: Vec<(String, Vec<u8>)>,
}
fn layer(name: &'static str, color: [u8; 4]) -> LayerFixture {
    LayerFixture {
        name,
        section: None,
        rgba: Some(color),
        mask: false,
        mask_bounds: [1, 1, 2, 2],
        mask_flags: 0,
        clipped: false,
        opacity: 255,
        hidden: false,
        extra: Vec::new(),
    }
}
fn extra_block(key: &str, payload: &[u8]) -> Vec<u8> {
    let mut b = b"8BIM".to_vec();
    b.extend(key.as_bytes());
    b.extend((payload.len() as u32).to_be_bytes());
    b.extend(payload);
    if !payload.len().is_multiple_of(2) {
        b.push(0);
    }
    b
}
fn fixture(version: u16, layers: &[LayerFixture], rle: bool) -> Vec<u8> {
    let large = version == 2;
    let mut b = b"8BPS".to_vec();
    b.extend(version.to_be_bytes());
    b.extend([0; 6]);
    b.extend(4_u16.to_be_bytes());
    b.extend(2_u32.to_be_bytes());
    b.extend(2_u32.to_be_bytes());
    b.extend(8_u16.to_be_bytes());
    b.extend(3_u16.to_be_bytes());
    b.extend(0_u32.to_be_bytes());
    let mut resource = b"8BIM".to_vec();
    resource.extend(1005_u16.to_be_bytes());
    resource.extend([0, 0]);
    resource.extend(16_u32.to_be_bytes());
    resource.extend((300_u32 * 65536).to_be_bytes());
    resource.extend([0; 12]);
    b.extend((resource.len() as u32).to_be_bytes());
    b.extend(resource);
    let mut info = Vec::new();
    info.extend((layers.len() as i16).to_be_bytes());
    let mut channels = Vec::new();
    for l in layers {
        let mut layer_channels = Vec::new();
        if let Some(rgba) = l.rgba {
            for (id, value) in [(0, rgba[0]), (1, rgba[1]), (2, rgba[2]), (-1, rgba[3])] {
                let mut bytes = Vec::new();
                bytes.extend((if rle { 1_u16 } else { 0 }).to_be_bytes());
                if rle {
                    for _ in 0..2 {
                        if large {
                            bytes.extend(2_u32.to_be_bytes());
                        } else {
                            bytes.extend(2_u16.to_be_bytes());
                        }
                    }
                    bytes.extend([255, value, 255, value]);
                } else {
                    bytes.extend([value; 4]);
                }
                layer_channels.push((id, bytes));
            }
        }
        if l.mask {
            let mut bytes = Vec::new();
            bytes.extend(0_u16.to_be_bytes());
            bytes.push(128);
            layer_channels.push((-2, bytes));
        }
        for n in [
            0_i32,
            0,
            if l.rgba.is_some() { 2 } else { 0 },
            if l.rgba.is_some() { 2 } else { 0 },
        ] {
            info.extend(n.to_be_bytes());
        }
        info.extend((layer_channels.len() as u16).to_be_bytes());
        for (id, data) in &layer_channels {
            info.extend((*id as i16).to_be_bytes());
            if large {
                info.extend((data.len() as u64).to_be_bytes());
            } else {
                info.extend((data.len() as u32).to_be_bytes());
            }
        }
        info.extend(b"8BIMnorm");
        info.extend([
            l.opacity,
            u8::from(l.clipped),
            if l.hidden { 2 } else { 0 },
            0,
        ]);
        let mut extra = Vec::new();
        if l.mask {
            extra.extend(20_u32.to_be_bytes());
            for n in l.mask_bounds {
                extra.extend(n.to_be_bytes());
            }
            extra.extend([0, l.mask_flags, 0, 0]);
        } else {
            extra.extend(0_u32.to_be_bytes());
        }
        extra.extend(0_u32.to_be_bytes());
        extra.push(1);
        extra.push(b'L');
        extra.extend([0, 0]);
        let units = l.name.encode_utf16().collect::<Vec<_>>();
        let mut unicode = Vec::new();
        unicode.extend((units.len() as u32).to_be_bytes());
        for unit in units {
            unicode.extend(unit.to_be_bytes());
        }
        extra.extend(extra_block("luni", &unicode));
        if let Some(section) = l.section {
            extra.extend(extra_block("lsct", &section.to_be_bytes()));
        }
        for (key, payload) in &l.extra {
            extra.extend(extra_block(key, payload));
        }
        info.extend((extra.len() as u32).to_be_bytes());
        info.extend(extra);
        for (_, data) in layer_channels {
            channels.extend(data);
        }
    }
    info.extend(channels);
    if !info.len().is_multiple_of(2) {
        info.push(0);
    }
    let mut section = Vec::new();
    if large {
        section.extend((info.len() as u64).to_be_bytes());
    } else {
        section.extend((info.len() as u32).to_be_bytes());
    }
    section.extend(info);
    section.extend(0_u32.to_be_bytes());
    if large {
        b.extend((section.len() as u64).to_be_bytes());
    } else {
        b.extend((section.len() as u32).to_be_bytes());
    }
    b.extend(section);
    b
}
#[test]
fn rendered_mask_channels_are_consumed_without_allocating_or_decoding_their_surface() {
    for version in [1, 2] {
        for rle in [false, true] {
            let mut first = layer("Rendered-mask layer", [10, 20, 30, 140]);
            first.mask = true;
            first.mask_flags = 8;
            first.mask_bounds = [0, 0, 20, 20];
            let second = layer("Following layer", [60, 70, 80, 200]);
            let limits = PsdLimits {
                document_pixels: 8,
                ..PsdLimits::default()
            };
            // The ignored mask advertises 400 pixels while the document has
            // only eight. Its deliberately tiny payload also proves that
            // the decoder never attempts to interpret a discarded channel.
            let data = fixture(version, &[first.clone(), second], rle);
            let decoded = decode(&data, limits).unwrap();
            assert_eq!(decoded.layers.len(), 2);
            for (actual, expected, name) in [
                (&decoded.layers[0], [10, 20, 30, 140], "Rendered-mask layer"),
                (&decoded.layers[1], [60, 70, 80, 200], "Following layer"),
            ] {
                assert_eq!(actual.name, name);
                assert!(actual.mask.is_none());
                assert!(
                    actual
                        .pixels
                        .as_ref()
                        .unwrap()
                        .data
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .all(|pixel| *pixel == expected)
                );
            }
            // A normal mask must still respect the same cumulative budget.
            first.mask_flags = 0;
            assert!(decode(&fixture(version, &[first], rle), limits).is_err());
        }
    }
}

#[test]
fn layered_psd_psb_preserve_channels_masks_clipping_unicode_and_resolution() {
    let mut base = layer("背景 🦀", [255, 20, 40, 200]);
    base.mask = true;
    base.opacity = 128;
    base.hidden = true;
    let mut top = layer("Clipped", [1, 2, 3, 255]);
    top.clipped = true;
    for version in [1, 2] {
        for rle in [false, true] {
            let data = fixture(version, &[base.clone(), top.clone()], rle);
            let read = decode(&data, PsdLimits::default()).unwrap();
            assert_eq!(read.resolution, 300.0);
            assert_eq!(read.layers.len(), 2);
            assert_eq!(read.layers[0].name, "背景 🦀");
            assert!(!read.layers[0].visible);
            assert_eq!(
                read.layers[0].pixels.as_ref().unwrap().data[..4],
                [255, 20, 40, 200]
            );
            assert_eq!(read.layers[0].mask.as_ref().unwrap().data, [128]);
            assert_eq!(read.layers[1].clipping_base, Some(read.layers[0].id));
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("fixture.psd");
            std::fs::write(&path, &data).unwrap();
            let document = import_document(&path).unwrap().document;
            let mask = document.layers[0].mask.as_ref().unwrap();
            assert_eq!(mask.raster.get(0, 0), [0, 0, 0, 255]);
            assert_eq!(mask.raster.get(1, 1), [128, 128, 128, 255]);
            let comp = temp.path().join("converted.comp");
            crate::project::save(&document, &comp).unwrap();
            assert_eq!(crate::project::load(&comp).unwrap(), document);
        }
    }
}
#[test]
fn groups_use_bottom_divider_and_top_folder() {
    let mut divider = layer("Divider", [0; 4]);
    divider.rgba = None;
    divider.section = Some(3);
    let child = layer("Child", [10, 20, 30, 255]);
    let mut folder = divider.clone();
    folder.name = "Folder";
    folder.section = Some(1);
    folder.opacity = 100;
    let read = decode(
        &fixture(2, &[divider, child, folder], false),
        PsdLimits::default(),
    )
    .unwrap();
    assert_eq!(read.layers.len(), 2);
    assert_eq!(read.layers[0].parent_id, Some(read.layers[1].id));
    assert!(read.layers[1].is_group);
    assert!((read.layers[1].opacity - 100.0 / 255.0).abs() < 1e-9);
}
#[test]
fn unsupported_depth_and_mode_are_rejected() {
    let valid = fixture(1, &[layer("Layer", [0, 0, 0, 255])], false);
    let mut invalid = valid.clone();
    invalid[22..24].copy_from_slice(&16_u16.to_be_bytes());
    assert!(decode(&invalid, PsdLimits::default()).is_err());
    let mut invalid = valid;
    invalid[24..26].copy_from_slice(&4_u16.to_be_bytes());
    assert!(decode(&invalid, PsdLimits::default()).is_err());
}
#[test]
fn cropped_psd_keeps_only_canvas_pixels() {
    let data = fixture(1, &[layer("Layer", [10, 20, 30, 255])], false);
    let limits = PsdLimits {
        document_pixels: 3,
        ..PsdLimits::default()
    };
    assert!(decode(&data, limits).is_err());
}

fn identifier(key: &str) -> Vec<u8> {
    let mut b = Vec::new();
    if key.len() == 4 {
        b.extend(0_u32.to_be_bytes());
    } else {
        b.extend((key.len() as u32).to_be_bytes());
    }
    b.extend(key.as_bytes());
    b
}
fn descriptor(fields: Vec<(&str, &str, Vec<u8>)>) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(16_u32.to_be_bytes());
    b.extend(0_u32.to_be_bytes());
    b.extend(identifier("TxLr"));
    b.extend((fields.len() as u32).to_be_bytes());
    for (key, kind, data) in fields {
        b.extend(identifier(key));
        b.extend(kind.as_bytes());
        b.extend(data);
    }
    b
}
#[test]
fn photoshop_text_is_editable_after_comp_roundtrip() {
    let mut txt = Vec::new();
    let units = "Hello 🦀".encode_utf16().collect::<Vec<_>>();
    txt.extend((units.len() as u32).to_be_bytes());
    for u in units {
        txt.extend(u.to_be_bytes());
    }
    let engine=br"<< /EngineDict << /StyleRun << /RunArray [ << /StyleSheet << /StyleSheetData << /Font 0 /FontSize 24 /Tracking 50 /FillColor << /Values [ 1 .2 .4 .6 ] >> >> >> >> ] >> >> /ResourceDict << /FontSet [ << /Name (Arial) >> ] >> >>";
    let mut engine_data = Vec::new();
    engine_data.extend((engine.len() as u32).to_be_bytes());
    engine_data.extend(engine);
    let mut tysh = Vec::new();
    tysh.extend(1_u16.to_be_bytes());
    for n in [0_f64, 1.0, 1.0, 0.0, 40.0, 30.0] {
        tysh.extend(n.to_be_bytes());
    }
    tysh.extend(50_u16.to_be_bytes());
    tysh.extend(descriptor(vec![
        ("Txt ", "TEXT", txt),
        ("EngineData", "tdta", engine_data),
    ]));
    let mut text = layer("Text", [0, 0, 0, 255]);
    text.rgba = None;
    text.extra.push(("TySh".to_owned(), tysh));
    let data = fixture(1, &[text], false);
    let read = decode(&data, PsdLimits::default()).unwrap();
    let t = read.layers[0].text.as_ref().unwrap();
    assert_eq!(t["content"], "Hello 🦀");
    assert_eq!(t["fontSize"], 24.0);
    assert_eq!(t["tracking"], 1.2);
    assert_eq!(t["fontName"], "Arial");
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("text.psd");
    std::fs::write(&source, data).unwrap();
    let imported = import_document(&source).unwrap();
    let layer = &imported.document.layers[0];
    assert_eq!(layer.transform.rotation, 90.0);
    assert!(layer.transform.flip_y);
    let style = crate::text::TextStyle::from_value(layer.text.as_ref().unwrap()).unwrap();
    let (rendered, anchor) = crate::text::rasterize_anchored(&style).unwrap();
    let document_anchor = layer.transform.to_document([
        anchor[0] / f64::from(rendered.width),
        anchor[1] / f64::from(rendered.height),
    ]);
    assert!((document_anchor[0] - 40.0).abs() < 1e-6 && (document_anchor[1] - 30.0).abs() < 1e-6);
    assert!(
        layer
            .image
            .as_ref()
            .unwrap()
            .to_rgba()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[3] > 0)
    );
    let package = temp.path().join("text.comp");
    crate::project::save(&imported.document, &package).unwrap();
    assert_eq!(crate::project::load(&package).unwrap(), imported.document);
}

#[test]
fn photoshop_binary_text_engine_keeps_unicode_content_and_style() {
    let mut engine = b"<< /EngineDict << /Editor << /Text (".to_vec();
    engine.extend([0xfe, 0xff]);
    for unit in "海洋 🦀\r".encode_utf16() {
        engine.extend(unit.to_be_bytes());
    }
    engine.extend(b") >> /StyleRun << /RunArray [ << /StyleSheet << /StyleSheetData << /Font 0 /FontSize 24 /Tracking 50 /FillColor << /Values [ 1 .2 .4 .6 ] >> >> >> >> ] >> >> /ResourceDict << /FontSet [ << /Name (Arial) >> ] >> >>");
    let mut expected = None;
    for version in [1, 2] {
        for padding in [0, 65_536] {
            let mut engine_data = Vec::new();
            engine_data.extend(((engine.len() + padding) as u32).to_be_bytes());
            engine_data.extend(&engine);
            engine_data.resize(engine_data.len() + padding, b' ');
            let mut tysh = Vec::new();
            tysh.extend(1_u16.to_be_bytes());
            for n in [1_f64, 0.0, 0.0, 1.0, 40.0, 30.0] {
                tysh.extend(n.to_be_bytes());
            }
            tysh.extend(50_u16.to_be_bytes());
            tysh.extend(descriptor(vec![("EngineData", "tdta", engine_data)]));
            let mut text = layer("Text", [0, 0, 0, 255]);
            text.rgba = None;
            text.extra.push(("TySh".to_owned(), tysh));
            let read = decode(&fixture(version, &[text], false), PsdLimits::default()).unwrap();
            let style = read.layers[0].text.as_ref().unwrap();
            assert_eq!(style["content"], "海洋 🦀");
            assert_eq!(style["fontName"], "Arial");
            assert_eq!(style["fontSize"], 24.0);
            assert_eq!(style["tracking"], 1.2);
            assert_eq!(style["red"], 0.2);
            assert_eq!(style["green"], 0.4);
            assert_eq!(style["blue"], 0.6);
            if let Some(expected) = &expected {
                assert_eq!(style, expected);
            } else {
                expected = Some(style.clone());
            }
        }
    }
}

#[test]
fn photoshop_hue_adjustment_retains_six_ranges_and_wraparound_bands() {
    let mut hue = Vec::new();
    hue.extend(2_u16.to_be_bytes());
    hue.extend([0, 0]);
    for value in [0_i16, 25, 0, 10, 5, -3] {
        hue.extend(value.to_be_bytes());
    }
    for index in 0..6 {
        let center = index * 60;
        for value in [
            center - 45,
            center - 15,
            center + 15,
            center + 45,
            index * 5,
            -index * 3,
            index,
        ] {
            hue.extend((value as i16).to_be_bytes());
        }
    }
    let mut adjustment = layer("Hue", [0, 0, 0, 0]);
    adjustment.rgba = None;
    adjustment.extra.push(("hue2".to_owned(), hue.clone()));
    let read = decode(&fixture(2, &[adjustment], false), PsdLimits::default()).unwrap();
    let settings = &read.layers[0].adjustment.as_ref().unwrap()["hsvSettings"];
    assert_eq!(settings["adjustments"]["Master"]["hue"], 10);
    assert_eq!(settings["adjustments"]["Magentas"]["saturation"], -15);
    assert_eq!(settings["bands"]["Reds"]["falloffStart"], 315.0);
    assert_eq!(settings["bands"]["Reds"]["rangeEnd"], 15.0);
    hue[2] = 1;
    let parsed = parse_adjustment(&BTreeMap::from([("hue2".to_owned(), hue)])).unwrap();
    assert_eq!(
        parsed["hsvSettings"]["adjustments"]["Master"]["saturation"],
        25
    );
    assert_eq!(
        parsed["hsvSettings"]["adjustments"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn photoshop_ellipse_generates_editable_shape_pixels() {
    let mut origin = Vec::new();
    origin.extend(b"keyOriginTypelong");
    origin.extend(5_i32.to_be_bytes());
    for (key, n) in [("Left", 0_f64), ("Top ", 0.0), ("Rght", 2.0), ("Btom", 2.0)] {
        origin.extend(key.as_bytes());
        origin.extend(b"UntF#Pxl");
        origin.extend(n.to_be_bytes());
    }
    let mut color = Vec::new();
    for (key, n) in [("Rd  ", 255_f64), ("Grn ", 0.0), ("Bl  ", 0.0)] {
        color.extend(key.as_bytes());
        color.extend(b"doub");
        color.extend(n.to_be_bytes());
    }
    let mut ellipse = layer("Ellipse", [0, 0, 0, 0]);
    ellipse.extra = vec![("vogk".to_owned(), origin), ("SoCo".to_owned(), color)];
    let read = decode(&fixture(2, &[ellipse], false), PsdLimits::default()).unwrap();
    assert_eq!(read.layers[0].shape.as_ref().unwrap()["kind"], "Ellipse");
    assert!(
        read.layers[0]
            .pixels
            .as_ref()
            .unwrap()
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] > 0 && p[3] > 0)
    );
}

#[test]
fn photoshop_round_rectangle_has_transparent_corners_in_its_initial_raster() {
    let mut origin = b"keyOriginTypelong".to_vec();
    origin.extend(2_i32.to_be_bytes());
    for (key, value) in [
        ("Left", 0_f64),
        ("Top ", 0.0),
        ("Rght", 8.0),
        ("Btom", 8.0),
        ("topLeft", 4.0),
        ("topRight", 4.0),
        ("bottomRight", 4.0),
        ("bottomLeft", 4.0),
    ] {
        origin.extend(key.as_bytes());
        origin.extend(b"UntF#Pxl");
        origin.extend(value.to_be_bytes());
    }
    let mut color = Vec::new();
    for (key, value) in [("Rd  ", 255_f64), ("Grn ", 0.0), ("Bl  ", 0.0)] {
        color.extend(key.as_bytes());
        color.extend(b"doub");
        color.extend(value.to_be_bytes());
    }
    let mut rectangle = layer("Rounded", [0, 0, 0, 0]);
    rectangle.extra = vec![("vogk".to_owned(), origin), ("SoCo".to_owned(), color)];
    let read = decode(&fixture(1, &[rectangle], false), PsdLimits::default()).unwrap();
    let layer = &read.layers[0];
    assert_eq!(layer.shape.as_ref().unwrap()["cornerRadius"], 4.0);
    let image = layer.pixels.as_ref().unwrap();
    assert_eq!([image.width, image.height], [8, 8]);
    assert_eq!(image.data[3], 0);
    assert_eq!(
        &image.data[(4 * 8 + 4) * 4..(4 * 8 + 4) * 4 + 4],
        &[255, 0, 0, 255]
    );
}
