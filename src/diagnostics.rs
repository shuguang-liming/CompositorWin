//! Small, bounded local diagnostics for graphics and image-loading failures.
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
struct Output {
    file: File,
    bytes: u64,
    path: PathBuf,
}
impl Output {
    fn append(&mut self, message: &str) -> std::io::Result<()> {
        if self.bytes + message.len() as u64 > MAX_LOG_BYTES {
            std::fs::copy(
                &self.path,
                self.path.with_file_name("diagnostics.previous.log"),
            )?;
            // Windows append-only handles do not grant permission to truncate.
            OpenOptions::new()
                .write(true)
                .open(&self.path)?
                .set_len(0)?;
            self.bytes = 0;
        }
        self.file.write_all(message.as_bytes())?;
        self.bytes += message.len() as u64;
        Ok(())
    }
}
static OUTPUT: OnceLock<Mutex<Output>> = OnceLock::new();
static LOGGER: LocalLogger = LocalLogger;
struct LocalLogger;

impl log::Log for LocalLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn || metadata.target().starts_with("compositor")
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let Some(output) = OUTPUT.get() else { return };
        let Ok(mut output) = output.lock() else {
            return;
        };
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let mut message = format!(
            "{}.{:03} [{}] {}: {}\n",
            timestamp.as_secs(),
            timestamp.subsec_millis(),
            record.level(),
            record.target(),
            record.args()
        );
        if message.len() > 8192 {
            let mut end = 8190;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            message.push('\n');
        }
        let _ = output.append(&message);
    }
    fn flush(&self) {
        if let Some(output) = OUTPUT.get()
            && let Ok(mut output) = output.lock()
        {
            let _ = output.file.flush();
        }
    }
}

pub fn initialize(directory: &Path) {
    let prepare = || -> std::io::Result<Output> {
        std::fs::create_dir_all(directory)?;
        let path = directory.join("diagnostics.log");
        if path
            .metadata()
            .is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES)
        {
            let previous = directory.join("diagnostics.previous.log");
            if previous.exists() {
                std::fs::remove_file(&previous)?;
            }
            std::fs::rename(&path, previous)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes = file.metadata()?.len();
        Ok(Output { file, bytes, path })
    };
    if let Ok(output) = prepare() {
        let _ = OUTPUT.set(Mutex::new(output));
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(log::LevelFilter::Info);
        }
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        log::logger().flush();
        previous(info);
    }));
    log::info!(
        "GUI start version={} arch={} pid={}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        std::process::id()
    );
}

pub fn import(path: &Path) -> Result<crate::image_io::ImportResult, String> {
    let started = Instant::now();
    log::info!(
        "import start path={} bytes={:?}",
        path.display(),
        path.metadata().ok().map(|m| m.len())
    );
    let result = std::panic::catch_unwind(|| crate::image_io::import(path))
        .map_err(|_| {
            "读取图片时发生异常，原文件未修改。请查看设置目录中的 diagnostics.log。".to_owned()
        })
        .and_then(|result| result.map_err(|error| format!("{error:#}")));
    match &result {
        Ok(import) => log::info!(
            "import ready {}x{} layers={} elapsed_ms={}",
            import.document.width,
            import.document.height,
            import.document.layers.len(),
            started.elapsed().as_millis()
        ),
        Err(error) => log::error!(
            "import failed elapsed_ms={} error={error}",
            started.elapsed().as_millis()
        ),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_log_rotates_before_dropping_the_failure_record() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.log");
        let old = vec![b'x'; MAX_LOG_BYTES as usize - 10];
        std::fs::write(&path, &old).unwrap();
        let mut output = Output {
            file: OpenOptions::new().append(true).open(&path).unwrap(),
            bytes: old.len() as u64,
            path: path.clone(),
        };
        output.append("import failure: retained\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "import failure: retained\n"
        );
        assert_eq!(
            std::fs::read(directory.path().join("diagnostics.previous.log")).unwrap(),
            old
        );
        assert_eq!(output.bytes, 25);
    }
}
