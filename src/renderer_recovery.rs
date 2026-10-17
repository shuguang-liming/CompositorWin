//! Last-resort document backups when neither native renderer can be restored.
use super::{EditorApp, preferences_directory};
use crate::{image_io, project};
use serde_json::json;
use std::path::{Path, PathBuf};
use uuid::Uuid;

impl EditorApp {
    /// Back up every visible document without writing to its original project path.
    /// Normal renderer recovery keeps these documents in memory and never calls this.
    pub fn save_recovery_projects(&mut self) -> Result<PathBuf, String> {
        self.save_recovery_projects_in(&preferences_directory())
    }

    fn save_recovery_projects_in(&mut self, parent: &Path) -> Result<PathBuf, String> {
        // Renderer creation can fail before recover_renderer has run. Seal any
        // visible brush/property edits before deciding which documents are dirty.
        self.finish_renderer_gestures();
        let directory = parent.join(format!("recovery-{}", Uuid::new_v4()));
        std::fs::create_dir_all(parent)
            .and_then(|()| std::fs::create_dir(&directory))
            .map_err(|error| format!("创建恢复备份目录 {}：{error}", directory.display()))?;
        let mut entries = Vec::new();
        let mut errors = Vec::new();
        for (index, session) in self.tabs.iter().enumerate().filter(|(_, s)| !s.placeholder) {
            let file = format!("{:03}-{}.comp", index + 1, session.id);
            let path = directory.join(&file);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                project::save(&session.doc, &path)
            }))
            .map_err(|_| "保存恢复工程时发生异常".to_owned())
            .and_then(|result| result.map_err(|error| format!("{error:#}")));
            if let Err(error) = &result {
                errors.push(format!("{}：{error}", session.title()));
            }
            let mut entry = json!({
                "sessionId": session.id,
                "file": result.as_ref().ok().map(|_| file),
                "sourcePath": session.path.as_ref().map(|path| path.to_string_lossy().into_owned()),
                "name": session.title(),
                "displayName": session.display_name,
                "modified": session.history.is_modified(),
                "revision": session.history.revision(),
                "serial": session.serial,
                "active": index == self.current,
                "saveInProgress": session.save_busy,
                "editInProgress": session.edit_busy,
                "reloadInProgress": session.reload_busy,
                "error": result.err(),
            });
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                entry["sourcePathUtf16"] = json!(
                    session
                        .path
                        .as_ref()
                        .map(|path| { path.as_os_str().encode_wide().collect::<Vec<_>>() })
                );
            }
            entries.push(entry);
        }
        let index = json!({
            "formatVersion": 1,
            "reason": "native_renderer_recovery_failed",
            "pendingIo": self.pending_io,
            "documents": entries,
            "errors": errors,
        });
        let bytes = serde_json::to_vec_pretty(&index).map_err(|error| {
            format!(
                "工程备份已写入 {}；生成索引失败：{error}",
                directory.display()
            )
        })?;
        if let Err(error) = image_io::write_atomic(&directory.join("index.json"), &bytes) {
            errors.push(format!("恢复索引：{error:#}"));
        }
        if errors.is_empty() {
            Ok(directory)
        } else {
            Err(format!(
                "部分恢复备份失败；已保存的工程位于 {}。{}",
                directory.display(),
                errors.join("；")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Document;
    use serde_json::Value;

    fn editor() -> EditorApp {
        let mut app = EditorApp::create(None, None);
        app.tabs = vec![super::super::Session::new(Document::new(16, 12), None)];
        app.current = 0;
        app
    }

    fn read_index(directory: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(directory.join("index.json")).unwrap()).unwrap()
    }

    #[test]
    fn last_resort_backups_preserve_all_documents_and_source_files_in_unique_directories() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("preferences");
        let source = directory.path().join("原工程.comp");
        let mut app = editor();
        project::save(&app.tabs[0].doc, &source).unwrap();
        let source_bytes = std::fs::read(&source).unwrap();
        app.tabs[0].path = Some(source.clone());
        let before = app.tabs[0].doc.clone();
        app.property_edit = Some((app.tabs[0].id, before));
        app.tabs[0].doc.layers[0].opacity = 0.3;
        let mut second = super::super::Session::new(Document::new(12, 10), None);
        second.display_name = Some("PDF 第 2 页".into());
        app.tabs.push(second);
        let mut welcome = super::super::Session::new(Document::new(8, 8), None);
        welcome.placeholder = true;
        app.tabs.push(welcome);
        app.current = 1;
        app.tabs[0].save_busy = true;
        app.pending_io = 1;
        let backup = app.save_recovery_projects_in(&parent).unwrap();
        assert_eq!(backup.parent(), Some(parent.as_path()));
        assert!(
            backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("recovery-")
        );
        assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
        assert_eq!(app.tabs[0].path.as_ref(), Some(&source));
        assert_eq!(app.tabs[0].history.undo_name(), Some("Layer properties"));
        assert!(app.tabs[0].history.is_modified());
        let index = read_index(&backup);
        let documents = index["documents"].as_array().unwrap();
        assert_eq!(documents.len(), 2);
        assert_eq!(index["pendingIo"], 1);
        assert_eq!(index["errors"], json!([]));
        for (entry, session) in documents.iter().zip(&app.tabs) {
            let path = backup.join(entry["file"].as_str().unwrap());
            assert!(path.is_file());
            assert_eq!(project::load(&path).unwrap(), session.doc);
            assert_eq!(entry["sessionId"], session.id.to_string());
            assert_eq!(entry["modified"], session.history.is_modified());
            assert_eq!(entry["name"], session.title());
        }
        assert_eq!(
            documents[0]["sourcePath"],
            source.to_string_lossy().as_ref()
        );
        assert_eq!(documents[0]["saveInProgress"], true);
        assert_eq!(documents[1]["displayName"], "PDF 第 2 页");
        assert_eq!(documents[1]["modified"], false);
        assert_eq!(documents[1]["active"], true);
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            assert_eq!(
                documents[0]["sourcePathUtf16"],
                json!(source.as_os_str().encode_wide().collect::<Vec<_>>())
            );
        }
        let another = app.save_recovery_projects_in(&parent).unwrap();
        assert_ne!(backup, another);
        assert_eq!(read_index(&backup), index);
        assert_eq!(std::fs::read(source).unwrap(), source_bytes);
    }

    #[test]
    fn backup_failure_is_reported_without_skipping_the_remaining_documents() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = editor();
        let mut invalid = super::super::Session::new(Document::new(12, 10), None);
        invalid.doc.width = 0;
        invalid.display_name = Some("损坏的中间页".into());
        app.tabs.push(invalid);
        app.tabs
            .push(super::super::Session::new(Document::new(8, 6), None));
        let error = app.save_recovery_projects_in(directory.path()).unwrap_err();
        let backups: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect();
        assert_eq!(backups.len(), 1);
        assert!(error.contains("损坏的中间页"));
        assert!(error.contains(backups[0].to_string_lossy().as_ref()));
        let index = read_index(&backups[0]);
        let documents = index["documents"].as_array().unwrap();
        assert_eq!(documents.len(), 3);
        assert_eq!(index["errors"].as_array().unwrap().len(), 1);
        assert!(documents[1]["file"].is_null());
        assert!(documents[1]["error"].as_str().is_some());
        for index in [0, 2] {
            let path = backups[0].join(documents[index]["file"].as_str().unwrap());
            assert_eq!(project::load(&path).unwrap(), app.tabs[index].doc);
            assert!(documents[index]["error"].is_null());
        }
    }

    #[test]
    fn inaccessible_recovery_parent_returns_the_error_without_altering_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("file-instead-of-directory");
        std::fs::write(&parent, b"Keep unrelated data").unwrap();
        let mut app = editor();
        assert!(app.save_recovery_projects_in(&parent).is_err());
        assert_eq!(std::fs::read(parent).unwrap(), b"Keep unrelated data");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
