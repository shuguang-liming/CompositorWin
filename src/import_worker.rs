//! Decode one GUI import at a time so large files do not multiply codec scratch memory.
use super::{Worker, WorkerSender};
use crate::{
    diagnostics, image_io,
    model::{Document, Layer},
};
use std::{path::PathBuf, sync::mpsc};
use uuid::Uuid;

pub(super) enum ImportTask {
    Open {
        path: PathBuf,
        target: Option<Uuid>,
    },
    RawPreview {
        path: PathBuf,
        settings: image_io::RawDevelopSettings,
        key: [u32; 4],
    },
    RawImport {
        path: PathBuf,
        settings: image_io::RawDevelopSettings,
        target: Option<Uuid>,
    },
}

impl ImportTask {
    fn execute(self) -> Worker {
        match self {
            Self::Open { path, target } => {
                let result = diagnostics::import(&path);
                Worker::Open {
                    path,
                    result,
                    target,
                }
            }
            Self::RawPreview {
                path,
                settings,
                key,
            } => {
                let result = image_io::develop_raw(&path, settings, Some(900))
                    .map_err(|error| format!("{error:#}"));
                Worker::RawPreview { path, key, result }
            }
            Self::RawImport {
                path,
                settings,
                target,
            } => {
                let result = image_io::develop_raw(&path, settings, None)
                    .map(|raster| {
                        let mut document = Document::new(raster.width, raster.height);
                        let layer = Layer::raster(
                            path.file_stem().unwrap_or_default().to_string_lossy(),
                            raster,
                        );
                        document.active_layer_id = Some(layer.id);
                        document.layers = vec![layer];
                        image_io::ImportResult {
                            document,
                            conversions: vec![],
                        }
                    })
                    .map_err(|error| format!("{error:#}"));
                Worker::Open {
                    path,
                    result,
                    target,
                }
            }
        }
    }
}

#[derive(Default)]
pub(super) struct ImportQueue {
    sender: Option<mpsc::Sender<ImportTask>>,
}

impl ImportQueue {
    pub(super) fn submit(
        &mut self,
        task: ImportTask,
        replies: &WorkerSender,
    ) -> Result<(), String> {
        if self.sender.is_none() {
            let (sender, receiver) = mpsc::channel::<ImportTask>();
            let replies = replies.clone();
            std::thread::Builder::new()
                .name("image-import".to_owned())
                // PSD metadata and third-party codecs need more stack than
                // the default Windows thread, especially on older drivers.
                .stack_size(32 * 1024 * 1024)
                .spawn(move || {
                    while let Ok(task) = receiver.recv() {
                        if replies.send(task.execute()).is_err() {
                            break;
                        }
                    }
                })
                .map_err(|error| format!("启动图像导入线程失败：{error}"))?;
            self.sender = Some(sender);
        }
        let sender = self.sender.as_ref().expect("The import worker was started");
        if sender.send(task).is_err() {
            // A later request can create a new worker instead of leaving the
            // application permanently stuck with a disconnected sender.
            self.sender = None;
            return Err("图像导入线程已停止，请重新打开文件".to_owned());
        }
        Ok(())
    }
}
