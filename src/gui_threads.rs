//! Fallible thread creation and recoverable failures for GUI background work.
use std::thread::JoinHandle;

pub(super) fn spawn(
    name: &'static str,
    operation: impl FnOnce() + Send + 'static,
) -> Result<JoinHandle<()>, String> {
    #[cfg(test)]
    if FAIL_SPAWN.with(|value| value.get() == Some(name)) {
        return Err(format!("启动{name}线程失败：测试模拟资源不足"));
    }
    #[cfg(test)]
    let panic_operation = PANIC_OPERATION.with(|value| value.get() == Some(name));
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            #[cfg(test)]
            if panic_operation {
                PANIC_IN_WORKER.with(|value| value.set(Some(name)));
            }
            operation();
        })
        .map_err(|error| format!("启动{name}线程失败：{error}"))
}

pub(super) fn run<T>(
    name: &str,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(test)]
        if PANIC_IN_WORKER.with(|value| value.take().is_some_and(|target| target == name)) {
            panic!("injected GUI background operation failure");
        }
        operation()
    }))
    .unwrap_or_else(|_| {
        Err(format!(
            "{name}时发生异常，请检查结果后重试；当前画布仍保留"
        ))
    })
}

#[cfg(test)]
thread_local! {
    static FAIL_SPAWN: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    static PANIC_OPERATION: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    static PANIC_IN_WORKER: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn with_spawn_failure<T>(name: &'static str, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<&'static str>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAIL_SPAWN.with(|value| value.set(self.0));
        }
    }
    let _restore = Restore(FAIL_SPAWN.with(|value| value.replace(Some(name))));
    operation()
}

#[cfg(test)]
pub(super) fn with_operation_panic<T>(name: &'static str, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<&'static str>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PANIC_OPERATION.with(|value| value.set(self.0));
        }
    }
    let _restore = Restore(PANIC_OPERATION.with(|value| value.replace(Some(name))));
    operation()
}
