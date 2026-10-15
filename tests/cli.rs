//! Process-level checks for the executable's UTF-8 JSON CLI and editing transactions.
use compositor::{automation::Operation, model::Raster, project, render};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
    sync::Arc,
};

fn run(arguments: &[OsString], input: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_compositor"))
        .arg("cli")
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Launch the compiled Compositor CLI");
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    } else {
        drop(child.stdin.take());
    }
    child.wait_with_output().expect("Read CLI response")
}

fn response(arguments: &[OsString], ok: bool) -> Value {
    let output = run(arguments, None);
    assert_eq!(
        output.status.success(),
        ok,
        "Unexpected exit status; stderr={}; stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("CLI stdout is JSON");
    assert_eq!(value["protocolVersion"], 1);
    assert_eq!(value["ok"], ok);
    value
}

fn new_project(path: &Path, width: u32, height: u32) -> Value {
    let value = response(
        &[
            "new".into(),
            "--width".into(),
            width.to_string().into(),
            "--height".into(),
            height.to_string().into(),
            "--output".into(),
            path.as_os_str().to_owned(),
        ],
        true,
    );
    assert!(path.is_file(), "A new project must be a single file");
    value
}

fn assert_only_files(directory: &Path, expected: &[&Path]) {
    for path in expected {
        assert!(
            path.is_file(),
            "Expected a regular file: {}",
            path.display()
        );
    }
    let actual: BTreeSet<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let expected: BTreeSet<_> = expected.iter().map(|path| path.to_path_buf()).collect();
    assert_eq!(
        actual, expected,
        "Project saves must leave no sidecar files"
    );
}

fn inspect(path: &Path) -> Value {
    response(&["inspect".into(), path.as_os_str().to_owned()], true)
}

fn jsonl(requests: &[Value]) -> Vec<Value> {
    let mut input = Vec::new();
    for request in requests {
        serde_json::to_writer(&mut input, request).unwrap();
        input.push(b'\n');
    }
    let output = run(&["serve".into()], Some(&input));
    assert!(
        output.status.success(),
        "JSONL server failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).expect("JSONL responses are UTF-8");
    let values: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("One JSON object per response line"))
        .collect();
    assert_eq!(values.len(), requests.len());
    for (request, response) in requests.iter().zip(&values) {
        assert_eq!(response["id"], request["id"]);
        assert_eq!(response["protocolVersion"], 1);
    }
    values
}

#[test]
fn utf8_and_space_paths_create_apply_export_and_reopen_real_pixels() {
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("中文 路径");
    fs::create_dir(&folder).unwrap();
    let original = folder.join("原始 项目.comp");
    let edited = folder.join("编辑 结果.comp");
    let operations = folder.join("操作 列表.json");
    new_project(&original, 12, 8);
    assert_only_files(&folder, &[&original]);
    let original_fingerprint = project::fingerprint(&original).unwrap();
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(
        &serde_json::to_vec(&json!([
            {"op":"set_layer","name":"中文 图层"},
            {"op":"fill","color":[25,75,125,255]},
            {"op":"select","selection":{"action":"rectangle","rect":[2,1,3,2],"antialiased":false}},
            {"op":"clear"},
            {"op":"select","selection":{"action":"none"}}
        ]))
        .unwrap(),
    );
    fs::write(&operations, bytes).unwrap();
    let applied = response(
        &[
            "apply".into(),
            original.as_os_str().to_owned(),
            "--operations".into(),
            format!("@{}", operations.display()).into(),
            "--output".into(),
            edited.as_os_str().to_owned(),
        ],
        true,
    );
    assert_eq!(
        applied["result"]["applied"]["results"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        project::fingerprint(&original).unwrap(),
        original_fingerprint
    );
    assert_only_files(&folder, &[&original, &edited, &operations]);
    let reopened = inspect(&edited);
    assert_eq!(reopened["result"]["document"]["width"], 12);
    assert_eq!(reopened["result"]["document"]["height"], 8);
    assert_eq!(
        reopened["result"]["document"]["layers"][0]["name"],
        "中文 图层"
    );
    assert_eq!(reopened["result"]["dirty"], false);

    for extension in ["png", "jpg", "tiff", "bmp", "webp", "gif"] {
        let output = folder.join(format!("导出 结果.{extension}"));
        let exported = response(
            &[
                "export".into(),
                edited.as_os_str().to_owned(),
                "--output".into(),
                output.as_os_str().to_owned(),
            ],
            true,
        );
        assert_eq!(exported["result"]["width"], 12);
        assert_eq!(exported["result"]["height"], 8);
        assert!(exported["result"]["bytes"].as_u64().unwrap() > 0);
        let imported = inspect(&output);
        assert_eq!(imported["result"]["document"]["width"], 12);
        let raster = image::open(&output).unwrap().into_rgba8();
        if extension != "jpg" {
            assert_eq!(raster.get_pixel(0, 0).0, [25, 75, 125, 255]);
            // GIF palette encoding may make a fully transparent pixel RGB vary,
            // but all other lossless formats preserve the transparent alpha.
            if extension != "gif" {
                assert_eq!(raster.get_pixel(2, 1).0[3], 0);
            }
        } else {
            assert_eq!(raster.get_pixel(2, 1).0[3], 255);
        }
    }
    assert_only_files(
        &folder,
        &[
            &original,
            &edited,
            &operations,
            &folder.join("导出 结果.png"),
            &folder.join("导出 结果.jpg"),
            &folder.join("导出 结果.tiff"),
            &folder.join("导出 结果.bmp"),
            &folder.join("导出 结果.webp"),
            &folder.join("导出 结果.gif"),
        ],
    );
}

#[test]
fn jsonl_batches_undo_redo_export_and_save_keep_correct_dirty_state() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("会话 保存.comp");
    let png = temp.path().join("未保存 预览.png");
    let requests = vec![
        json!({"id":1,"method":"new","params":{"width":10,"height":6}}),
        json!({"id":"编辑","method":"apply","params":{"operations":[{"op":"add_layer","name":"新增 图层"},{"op":"fill","color":[200,60,20,255]}]}}),
        json!({"id":3,"method":"inspect"}),
        json!({"id":4,"method":"undo"}),
        json!({"id":5,"method":"redo"}),
        json!({"id":6,"method":"export","params":{"path":png}}),
        json!({"id":7,"method":"inspect"}),
        json!({"id":8,"method":"save","params":{"path":project}}),
        json!({"id":9,"method":"inspect"}),
        json!({"id":10,"method":"undo"}),
        json!({"id":11,"method":"redo"}),
        json!({"id":12,"method":"close"}),
        json!({"id":13,"method":"inspect"}),
        json!({"id":14,"method":"capabilities"}),
    ];
    let values = jsonl(&requests);
    assert_eq!(values[0]["result"]["dirty"], false);
    assert_eq!(values[1]["result"]["changed"], true);
    assert_eq!(values[2]["result"]["dirty"], true);
    assert_eq!(values[2]["result"]["document"]["layerCount"], 2);
    assert_eq!(values[3]["result"]["changed"], true);
    assert_eq!(values[3]["result"]["state"]["document"]["layerCount"], 1);
    assert_eq!(values[3]["result"]["state"]["dirty"], false);
    assert_eq!(values[4]["result"]["state"]["document"]["layerCount"], 2);
    assert_eq!(values[4]["result"]["state"]["dirty"], true);
    assert_eq!(values[6]["result"]["dirty"], true);
    assert_eq!(values[8]["result"]["dirty"], false);
    assert_eq!(values[9]["result"]["state"]["dirty"], true);
    assert_eq!(values[10]["result"]["state"]["dirty"], false);
    assert_eq!(values[11]["result"]["closed"], true);
    assert_eq!(values[12]["ok"], false);
    assert_eq!(values[12]["error"]["code"], "no_document");
    assert_eq!(values[13]["ok"], true);
    assert_eq!(
        values[2]["result"]["revision"],
        values[4]["result"]["state"]["revision"]
    );
    assert_eq!(
        values[0]["result"]["revision"],
        values[3]["result"]["state"]["revision"]
    );
    assert_eq!(project::load(&project).unwrap().layers.len(), 2);
    assert_only_files(temp.path(), &[&project, &png]);
    assert_eq!(
        image::open(png).unwrap().into_rgba8().get_pixel(5, 3).0,
        [200, 60, 20, 255]
    );
}

#[test]
fn failed_batch_unknown_fields_conflicts_and_overwrite_protection_do_not_write() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("原件.comp");
    let target = temp.path().join("失败后 不应存在.comp");
    new_project(&source, 8, 8);
    let fingerprint = project::fingerprint(&source).unwrap();
    let invalid_operations = json!([
        {"op":"add_layer","name":"不得落盘"},
        {"op":"delete_layers","layers":["00000000-0000-0000-0000-000000000000"]}
    ]);
    let failed = response(
        &[
            "apply".into(),
            source.as_os_str().to_owned(),
            "--operations".into(),
            invalid_operations.to_string().into(),
            "--output".into(),
            target.as_os_str().to_owned(),
        ],
        false,
    );
    assert_eq!(failed["error"]["operationIndex"], 1);
    assert!(!target.exists());
    assert_eq!(project::fingerprint(&source).unwrap(), fingerprint);

    let typo = response(
        &[
            "apply".into(),
            source.as_os_str().to_owned(),
            "--operations".into(),
            json!([{"op":"set_layer","opactiy":0.5}]).to_string().into(),
            "--output".into(),
            target.as_os_str().to_owned(),
        ],
        false,
    );
    assert_eq!(typo["error"]["code"], "invalid_request");
    assert!(!target.exists());
    let conflicting = response(
        &[
            "apply".into(),
            source.as_os_str().to_owned(),
            "--operations".into(),
            json!([{"op":"set_layer","name":"不得覆盖"}])
                .to_string()
                .into(),
            "--expected-fingerprint".into(),
            "incorrect".into(),
            "--output".into(),
            source.as_os_str().to_owned(),
        ],
        false,
    );
    assert_eq!(conflicting["error"]["code"], "conflict");
    let existing = response(
        &[
            "new".into(),
            "--width".into(),
            "9".into(),
            "--height".into(),
            "9".into(),
            "--output".into(),
            source.as_os_str().to_owned(),
        ],
        false,
    );
    assert_eq!(existing["error"]["code"], "already_exists");
    assert_eq!(project::fingerprint(&source).unwrap(), fingerprint);
    let protected_png = temp.path().join("保留 原图.png");
    fs::write(&protected_png, b"existing file bytes").unwrap();
    let existing = response(
        &[
            "export".into(),
            source.as_os_str().to_owned(),
            "--output".into(),
            protected_png.as_os_str().to_owned(),
        ],
        false,
    );
    assert_eq!(existing["error"]["code"], "already_exists");
    assert_eq!(fs::read(protected_png).unwrap(), b"existing file bytes");
    assert_only_files(temp.path(), &[&source, &temp.path().join("保留 原图.png")]);
}

#[test]
fn single_file_in_place_apply_uses_fingerprint_and_rejects_stale_writes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("反复 保存.comp");
    new_project(&path, 8, 8);
    let opened = inspect(&path);
    let document_id = opened["result"]["document"]["documentId"].clone();
    let fingerprint = opened["result"]["fingerprint"].as_str().unwrap();
    response(
        &[
            "apply".into(),
            path.as_os_str().to_owned(),
            "--operations".into(),
            json!([{"op":"fill","color":[32,96,160,128]}])
                .to_string()
                .into(),
            "--expected-fingerprint".into(),
            fingerprint.into(),
            "--output".into(),
            path.as_os_str().to_owned(),
        ],
        true,
    );
    assert_only_files(temp.path(), &[&path]);
    let saved_bytes = fs::read(&path).unwrap();
    let saved = inspect(&path);
    assert_eq!(saved["result"]["document"]["documentId"], document_id);
    assert_ne!(saved["result"]["fingerprint"], fingerprint);
    assert_eq!(
        project::load(&path).unwrap().layers[0]
            .image
            .as_ref()
            .unwrap()
            .get(4, 4),
        [32, 96, 160, 128]
    );
    let conflict = response(
        &[
            "apply".into(),
            path.as_os_str().to_owned(),
            "--operations".into(),
            json!([{"op":"set_layer","name":"过期 写入"}])
                .to_string()
                .into(),
            "--expected-fingerprint".into(),
            fingerprint.into(),
            "--output".into(),
            path.as_os_str().to_owned(),
        ],
        false,
    );
    assert_eq!(conflict["error"]["code"], "conflict");
    assert_eq!(fs::read(&path).unwrap(), saved_bytes);
    response(
        &[
            "new".into(),
            "--width".into(),
            "10".into(),
            "--height".into(),
            "4".into(),
            "--output".into(),
            path.as_os_str().to_owned(),
            "--overwrite".into(),
        ],
        true,
    );
    let replaced = project::load(&path).unwrap();
    assert_eq!((replaced.width, replaced.height), (10, 4));
    assert_ne!(json!(replaced.id), document_id);
    assert_only_files(temp.path(), &[&path]);
}

#[test]
fn jsonl_legacy_directory_requires_save_as_and_reopens_as_one_file() {
    let temp = tempfile::tempdir().unwrap();
    let legacy = temp.path().join("旧版 文件夹.comp");
    let migrated = temp.path().join("新版 单文件.comp");
    let png = temp.path().join("迁移 导出.png");
    let document_id = uuid::Uuid::new_v4();
    let layer_id = uuid::Uuid::new_v4();
    fs::create_dir(&legacy).unwrap();
    let manifest = serde_json::to_vec(&json!({
        "format":"com.compositor.project","version":1,"colorSpace":"sRGB",
        "documentID":document_id,"width":8,"height":6,"activeLayerID":layer_id,
        "layers":[{"id":layer_id,"name":"旧图层","transform":{
            "origin":[0,0],"size":[8,6],"rotation":0
        }}]
    }))
    .unwrap();
    fs::write(legacy.join("manifest.json"), &manifest).unwrap();
    let fingerprint = project::fingerprint(&legacy).unwrap();
    let mut expected_document = project::load(&legacy).unwrap();
    let expected_raster = Raster::solid(8, 6, [24, 72, 120, 200]);
    expected_document.layers[0].image = Some(Arc::new(expected_raster.clone()));
    expected_document.layers[0].name = "另存 的图层".into();
    let expected_export = render::composite(&expected_document);
    let values = jsonl(&[
        json!({"id":1,"method":"open","params":{"path":legacy}}),
        json!({"id":2,"method":"apply","params":{"operations":[{"op":"fill","color":[24,72,120,200]},{"op":"set_layer","name":"另存 的图层"}]}}),
        json!({"id":3,"method":"save","params":{"path":legacy,"overwrite":true}}),
        json!({"id":4,"method":"inspect"}),
        json!({"id":5,"method":"save","params":{"path":migrated}}),
        json!({"id":6,"method":"close"}),
        json!({"id":7,"method":"open","params":{"path":migrated}}),
        json!({"id":8,"method":"export","params":{"path":png}}),
    ]);
    assert_eq!(values[0]["ok"], true);
    assert_eq!(values[1]["ok"], true);
    assert_eq!(values[2]["ok"], false);
    assert_eq!(values[3]["result"]["dirty"], true);
    assert_eq!(values[4]["ok"], true);
    assert_eq!(values[6]["ok"], true);
    assert_eq!(values[6]["result"]["dirty"], false);
    assert_eq!(
        values[6]["result"]["document"]["documentId"],
        json!(document_id)
    );
    assert_eq!(
        values[6]["result"]["document"]["layers"][0]["id"],
        json!(layer_id)
    );
    assert_eq!(
        values[6]["result"]["document"]["layers"][0]["name"],
        "另存 的图层"
    );
    assert_eq!(values[7]["ok"], true);
    assert!(legacy.is_dir());
    assert!(migrated.is_file());
    assert_eq!(project::fingerprint(&legacy).unwrap(), fingerprint);
    assert_eq!(fs::read(legacy.join("manifest.json")).unwrap(), manifest);
    assert_eq!(fs::read_dir(&legacy).unwrap().count(), 1);
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 3);
    let reopened = project::load(&migrated).unwrap();
    assert_eq!(
        reopened.layers[0].image.as_deref().unwrap(),
        &expected_raster
    );
    assert_eq!(reopened, expected_document);
    // Export uses the compositor's existing 8-bit premultiplied intermediate surface.
    assert_eq!(
        image::open(png).unwrap().into_rgba8().into_raw(),
        expected_export.to_rgba()
    );
}

#[test]
fn jsonl_failed_batch_preserves_revision_history_and_keeps_processing() {
    let requests = vec![
        json!({"id":1,"method":"new","params":{"width":8,"height":8}}),
        json!({"id":2,"method":"apply","params":{"operations":[{"op":"add_layer","name":"回滚"},{"op":"ungroup"}]}}),
        json!({"id":3,"method":"inspect"}),
        json!({"id":4,"method":"undo"}),
        json!({"id":5,"method":"apply","params":{"operations":[{"op":"set_layer","name":"拼写错误"}],"unknown":true}}),
        json!({"id":6,"method":"inspect","unexpected":1}),
        json!({"id":7,"method":"apply","params":{"operations":[{"op":"set_layer","name":"冲突"}],"expectedRevision":"00000000-0000-0000-0000-000000000000"}}),
        json!({"id":8,"method":"inspect"}),
    ];
    let values = jsonl(&requests);
    assert_eq!(values[1]["error"]["operationIndex"], 1);
    assert_eq!(
        values[2]["result"]["document"],
        values[0]["result"]["document"]
    );
    assert_eq!(
        values[2]["result"]["revision"],
        values[0]["result"]["revision"]
    );
    assert_eq!(values[2]["result"]["dirty"], false);
    assert_eq!(values[3]["result"]["changed"], false);
    assert_eq!(values[4]["error"]["code"], "invalid_request");
    assert_eq!(values[5]["error"]["code"], "invalid_request");
    assert_eq!(values[6]["error"]["code"], "conflict");
    assert_eq!(
        values[7]["result"]["document"],
        values[0]["result"]["document"]
    );
    assert_eq!(
        values[7]["result"]["revision"],
        values[0]["result"]["revision"]
    );
}

#[test]
fn executable_capabilities_exposes_deserializable_operation_examples() {
    let value = response(&["capabilities".into()], true);
    let capabilities = &value["result"];
    assert_eq!(capabilities["stdio"], true);
    assert_eq!(capabilities["headless"], true);
    let operations = capabilities["operations"]["operations"].as_array().unwrap();
    assert!(operations.len() >= 39);
    for operation in operations {
        let decoded: Operation = serde_json::from_value(operation["example"].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            operation["expanded_example"]
        );
        assert!(operation["fields"].is_object());
    }
}

#[test]
fn skill_install_is_self_contained_and_backs_up_existing_local_instructions() {
    let temp = tempfile::tempdir().unwrap();
    let skill = temp.path().join("本地 skills");
    fs::create_dir(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "已有的 用户技能\n").unwrap();
    let installed = response(
        &[
            "skill".into(),
            "install".into(),
            "--path".into(),
            skill.as_os_str().to_owned(),
        ],
        true,
    );
    let backup = installed["result"]["backup"].as_str().unwrap();
    assert_eq!(fs::read_to_string(backup).unwrap(), "已有的 用户技能\n");
    let content = fs::read_to_string(skill.join("SKILL.md")).unwrap();
    assert!(content.contains("name: compositor-editor"));
    assert!(content.contains("COMPOSITOR-INSTALL"));
    assert!(content.contains("compositor"));
    let repeated = response(
        &[
            "skill".into(),
            "install".into(),
            "--path".into(),
            skill.as_os_str().to_owned(),
        ],
        true,
    );
    assert!(repeated["result"]["backup"].is_null());
    assert_eq!(fs::read_dir(skill).unwrap().count(), 2);
}
