//! Authenticated, local Agent access. Only the UI thread commits document changes.
use super::EditorApp;
use crate::{automation, cli, history::History, image_io, model::Document, project, render};
use anyhow::{Context, Result, ensure};
use eframe::egui;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use uuid::Uuid;

const REQUEST_LIMIT: u64 = 4 * 1024 * 1024;
const CONNECTION_LIMIT: usize = 8;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    token: String,
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default = "empty_params")]
    params: Value,
}

fn empty_params() -> Value {
    json!({})
}

struct Request {
    id: Value,
    method: String,
    params: Value,
    reply: mpsc::Sender<Value>,
    guard: Arc<RequestGuard>,
}

// A timeout and the beginning of a commit race through one atomic transition.
struct RequestGuard {
    deadline: Instant,
    state: AtomicU8,
}

impl RequestGuard {
    fn new(timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            state: AtomicU8::new(0),
        }
    }

    fn expire(&self) -> bool {
        self.state
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn pending(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            self.expire();
        }
        ensure!(
            self.state.load(Ordering::Acquire) == 0,
            "request_expired: 请求已过期，操作未提交"
        );
        Ok(())
    }

    fn claim(&self) -> Result<()> {
        self.pending()?;
        ensure!(
            self.state
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "request_expired: 请求已过期，操作未提交"
        );
        Ok(())
    }

    fn finish(&self) {
        self.state.store(3, Ordering::Release);
    }
}

enum Event {
    Request(Request),
    Complete(Completion),
}

enum Completion {
    Inspect {
        request: Request,
        result: Value,
    },
    Apply {
        request: Request,
        session: Uuid,
        revision: Uuid,
        serial: u64,
        before: Box<Document>,
        result: Result<Box<(Document, Value)>>,
    },
    Save {
        request: Request,
        session: Uuid,
        revision: Uuid,
        path: PathBuf,
        result: Result<(String, u64)>,
    },
    Export {
        request: Request,
        session: Uuid,
        revision: Uuid,
        path: PathBuf,
        result: Result<()>,
    },
}

pub(super) struct AgentBridge {
    receiver: mpsc::Receiver<Event>,
    sender: mpsc::SyncSender<Event>,
    context: Arc<Mutex<Option<egui::Context>>>,
    engine: Arc<Mutex<automation::Engine>>,
    stopped: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    endpoint_path: PathBuf,
    token: String,
    observed_sessions: std::collections::HashMap<Uuid, (Option<Uuid>, u64)>,
}

impl AgentBridge {
    pub(super) fn start(context: egui::Context) -> Result<Self> {
        let context = Arc::new(Mutex::new(Some(context)));
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("绑定本机 Agent 接口")?;
        listener.set_nonblocking(true)?;
        let token = Uuid::new_v4().to_string();
        let endpoint_path =
            super::preferences_directory().join(format!("agent-{}.json", std::process::id()));
        let endpoint = json!({
            "protocolVersion": 1,
            "pid": std::process::id(),
            "address": listener.local_addr()?.to_string(),
            "token": token,
            "executable": std::env::current_exe()?,
        });
        image_io::write_atomic(&endpoint_path, &serde_json::to_vec_pretty(&endpoint)?)?;
        let (sender, receiver) = mpsc::sync_channel(CONNECTION_LIMIT * 2);
        let stopped = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let accept_sender = sender.clone();
        let accept_context = context.clone();
        let accept_stopped = stopped.clone();
        let accept_token = token.clone();
        let listener = thread::spawn(move || {
            while !accept_stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, peer)) => {
                        if !peer.ip().is_loopback()
                            || active.load(Ordering::Acquire) >= CONNECTION_LIMIT
                        {
                            continue;
                        }
                        active.fetch_add(1, Ordering::AcqRel);
                        let active = active.clone();
                        let sender = accept_sender.clone();
                        let context = accept_context.clone();
                        let stopped = accept_stopped.clone();
                        let token = accept_token.clone();
                        thread::spawn(move || {
                            if let Err(error) =
                                serve_connection(stream, &token, &sender, &context, &stopped)
                            {
                                log::warn!("Local Agent connection {peer} failed: {error:#}");
                            }
                            active.fetch_sub(1, Ordering::AcqRel);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            receiver,
            sender,
            context,
            engine: Arc::new(Mutex::new(automation::Engine::default())),
            stopped,
            listener: Some(listener),
            endpoint_path,
            token,
            observed_sessions: Default::default(),
        })
    }

    pub(super) fn bind_context(&mut self, context: egui::Context) {
        *self.context.lock().unwrap_or_else(|e| e.into_inner()) = Some(context);
    }
}

impl Drop for AgentBridge {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        // A replacement endpoint must never be deleted by a previous instance.
        if fs::read(&self.endpoint_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|value| value["token"].as_str() == Some(self.token.as_str()))
        {
            let _ = fs::remove_file(&self.endpoint_path);
        }
    }
}

fn protocol_error(id: Value, code: &str, message: impl Into<String>) -> Value {
    json!({"protocolVersion":1,"id":id,"ok":false,
        "error":{"code":code,"message":message.into()}})
}

fn serve_connection(
    mut stream: TcpStream,
    token: &str,
    sender: &mpsc::SyncSender<Event>,
    context: &Mutex<Option<egui::Context>>,
    stopped: &AtomicBool,
) -> Result<()> {
    // Windows can inherit the listener's nonblocking mode. The JSONL reader
    // must wait for delayed/fragmented requests instead of closing on WouldBlock.
    stream
        .set_nonblocking(false)
        .context("设置 Agent 连接为阻塞读取")?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut bytes = Vec::new();
    let length = BufReader::new(stream.try_clone()?)
        .take(REQUEST_LIMIT + 1)
        .read_until(b'\n', &mut bytes)
        .context("读取 Agent JSONL 请求")?;
    let response = if length as u64 > REQUEST_LIMIT {
        protocol_error(Value::Null, "request_too_large", "Agent 请求超过 4 MiB")
    } else if bytes.last() != Some(&b'\n') {
        protocol_error(
            Value::Null,
            "invalid_request",
            "请求必须是一行完整 JSON，以换行结束",
        )
    } else {
        match serde_json::from_slice::<WireRequest>(&bytes) {
            Err(error) => protocol_error(Value::Null, "invalid_request", error.to_string()),
            Ok(request) if request.token != token => {
                protocol_error(request.id, "auth_failed", "Agent token 不匹配")
            }
            Ok(request) => {
                let id = request.id.clone();
                let guard = Arc::new(RequestGuard::new(RESPONSE_TIMEOUT));
                let (reply, receiver) = mpsc::channel();
                let request = Request {
                    id: request.id,
                    method: request.method,
                    params: request.params,
                    reply,
                    guard: guard.clone(),
                };
                match sender.try_send(Event::Request(request)) {
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        protocol_error(id, "gui_closed", "编辑器已关闭")
                    }
                    Err(mpsc::TrySendError::Full(_)) => {
                        protocol_error(id, "gui_busy", "Agent 请求队列已满，请稍后重试")
                    }
                    Ok(()) => {
                        super::wake_renderer(context);
                        let started = Instant::now();
                        loop {
                            match receiver.recv_timeout(Duration::from_millis(100)) {
                                Ok(value) => break value,
                                Err(mpsc::RecvTimeoutError::Disconnected) => {
                                    break protocol_error(id, "gui_closed", "编辑器已关闭");
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => {}
                            }
                            if stopped.load(Ordering::Acquire) {
                                guard.expire();
                                break protocol_error(id, "gui_closed", "编辑器正在关闭");
                            }
                            if started.elapsed() >= RESPONSE_TIMEOUT {
                                let expired = guard.expire();
                                let mut response = protocol_error(
                                    id,
                                    "request_timeout",
                                    if expired {
                                        "请求已过期；尚未提交的编辑结果将丢弃"
                                    } else {
                                        "等待提交结果超时；写入或提交已经开始，请先 inspect 再决定是否重试"
                                    },
                                );
                                response["error"]["commitAlreadyStarted"] = json!(!expired);
                                break response;
                            }
                        }
                    }
                }
            }
        }
    };
    serde_json::to_writer(&mut stream, &response).context("写入 Agent JSON 响应")?;
    stream.write_all(b"\n").context("写入 Agent 响应结束符")?;
    stream.flush().context("发送 Agent 响应")?;
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct TargetParams {
    session_id: Option<Uuid>,
    expected_revision: Option<Uuid>,
    expected_serial: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ApplyParams {
    operations: Vec<automation::Operation>,
    session_id: Option<Uuid>,
    expected_revision: Option<Uuid>,
    expected_serial: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SaveParams {
    path: Option<PathBuf>,
    #[serde(default)]
    overwrite: bool,
    expected_fingerprint: Option<String>,
    session_id: Option<Uuid>,
    expected_revision: Option<Uuid>,
    expected_serial: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExportParams {
    path: PathBuf,
    format: Option<String>,
    max_side: Option<u32>,
    #[serde(default = "jpeg_quality")]
    quality: u8,
    #[serde(default = "white_matte")]
    matte: [u8; 3],
    #[serde(default)]
    overwrite: bool,
    session_id: Option<Uuid>,
    expected_revision: Option<Uuid>,
    expected_serial: Option<u64>,
}

fn jpeg_quality() -> u8 {
    90
}
fn white_matte() -> [u8; 3] {
    [255; 3]
}

fn observe_agent_sessions(
    observed: &mut std::collections::HashMap<Uuid, (Option<Uuid>, u64)>,
    sessions: &mut [super::Session],
) {
    observed.retain(|id, _| sessions.iter().any(|session| session.id == *id));
    for session in sessions {
        if let Some((active, serial)) = observed.get(&session.id)
            && *active != session.doc.active_layer_id
            && *serial == session.serial
        {
            session.serial = session.serial.wrapping_add(1);
        }
        observed.insert(session.id, (session.doc.active_layer_id, session.serial));
    }
}

impl EditorApp {
    pub(super) fn poll_agent(&mut self) {
        if let Some(bridge) = &mut self.agent_bridge {
            observe_agent_sessions(&mut bridge.observed_sessions, &mut self.tabs);
        }
        // A bounded channel also contains completions from requests whose clients timed out.
        for _ in 0..CONNECTION_LIMIT * 2 {
            let event = self
                .agent_bridge
                .as_ref()
                .and_then(|bridge| bridge.receiver.try_recv().ok());
            match event {
                Some(Event::Request(request)) => self.handle_agent(request),
                Some(Event::Complete(completion)) => self.finish_agent(completion),
                None => break,
            }
        }
    }

    fn agent_interaction_busy(&self) -> bool {
        self.operation_busy()
            || self.pdf_ui.is_open()
            || self.dialog.is_some()
            || self.color_picker.is_some()
            || self.text_color_target.is_some()
            || self.selection_amount_dialog.is_some()
            || self.drag.is_some()
            || self.brush_stroke.is_some()
            || self.guide_drag.is_some()
            || self.crop_drag.is_some()
            || self.crop_rect.is_some()
            || self.gradient.is_some()
            || self.gradient_handle.is_some()
            || self.visibility_sweep.is_some()
            || self.property_edit.is_some()
            || self.text_box_drag.is_some()
            || !self.polygon.is_empty()
            || self.close_pending.is_some()
            || self.exit_pending
            || self.external_pending.is_some()
    }

    fn agent_session(&self, target: &TargetParams) -> Result<usize> {
        let index = if let Some(id) = target.session_id {
            self.tabs
                .iter()
                .position(|session| session.id == id)
                .context("session_not_found: 指定文档会话不存在")?
        } else {
            self.current
        };
        let session = self
            .tabs
            .get(index)
            .context("no_document: 当前没有打开文档")?;
        ensure!(
            !session.placeholder,
            "no_document: 请先在编辑器打开或新建文档"
        );
        ensure!(
            target
                .expected_revision
                .is_none_or(|revision| revision == session.history.revision())
                && target
                    .expected_serial
                    .is_none_or(|serial| serial == session.serial),
            "revision_conflict: 文档已发生变化，请重新 inspect"
        );
        Ok(index)
    }

    fn agent_info(&self, index: usize) -> Value {
        let session = &self.tabs[index];
        json!({
            "sessionId": session.id,
            "revision": session.history.revision(),
            "serial": session.serial,
            "dirty": session.history.is_modified(),
            "path": session.path,
            "active": index == self.current,
            "busy": self.agent_interaction_busy(),
            "undo": session.history.undo_name(),
            "redo": session.history.redo_name(),
            "document": cli::document_info(&session.doc),
        })
    }

    fn handle_agent(&mut self, request: Request) {
        let result = self.dispatch_agent(&request);
        match result {
            Ok(Some(value)) => send_result(request, Ok(value)),
            Ok(None) => {}
            Err(error) => send_result(request, Err(error)),
        }
    }

    fn dispatch_agent(&mut self, request: &Request) -> Result<Option<Value>> {
        request.guard.pending()?;
        if request.method == "capabilities" {
            ensure!(
                request
                    .params
                    .as_object()
                    .is_some_and(|params| params.is_empty()),
                "invalid_params: capabilities 参数必须为空对象"
            );
            return Ok(Some(json!({
                "protocolVersion": 1,
                "transport": "authenticated_loopback_jsonl",
                "liveControl": true,
                "methods": ["capabilities", "inspect", "apply", "undo", "redo", "save", "export"],
                "exportFormats": ["png", "jpeg", "tiff", "bmp", "gif", "webp", "pdf"],
                "pdf": {"editing":"rasterized_pages", "open":"Use the GUI PDF import dialog to select pages and DPI; live controls existing documents only", "export":"one flattened raster page per document; maxSide preserves physical page size", "limitations":"Original PDF text, vectors, forms and signatures are not preserved"},
                "sessions": self.tabs.iter().enumerate().filter(|(_, session)| !session.placeholder)
                    .map(|(index, _)| self.agent_info(index)).collect::<Vec<_>>(),
                "transaction": "single_undo_step",
                "maxRequestBytes": REQUEST_LIMIT,
                "timeoutSeconds": RESPONSE_TIMEOUT.as_secs(),
            })));
        }
        match request.method.as_str() {
            "inspect" => {
                let target: TargetParams = serde_json::from_value(request.params.clone())
                    .context("invalid_params: inspect 参数无效")?;
                let index = self.agent_session(&target)?;
                let mut result = self.agent_info(index);
                result["fingerprint"] = Value::Null;
                let Some(path) = self.tabs[index].path.clone() else {
                    return Ok(Some(result));
                };
                let stamp = self.tabs[index].stamp;
                let bridge = self
                    .agent_bridge
                    .as_ref()
                    .context("gui_closed: Agent 接口已停止")?;
                let sender = bridge.sender.clone();
                let context = bridge.context.clone();
                let request = clone_request(request);
                thread::spawn(move || {
                    let fingerprint = (|| -> Result<String> {
                        ensure!(
                            super::package_stamp(&path) == stamp,
                            "fingerprint_conflict: 磁盘项目有尚未处理的更新"
                        );
                        let digest = project::fingerprint(&path)?;
                        ensure!(
                            super::package_stamp(&path) == stamp,
                            "fingerprint_conflict: 计算指纹期间磁盘项目发生变化"
                        );
                        Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
                    })();
                    match fingerprint {
                        Ok(value) => result["fingerprint"] = json!(value),
                        Err(error) => result["fingerprintError"] = json!(format!("{error:#}")),
                    }
                    let _ = sender.send(Event::Complete(Completion::Inspect { request, result }));
                    super::wake_renderer(&context);
                });
                Ok(None)
            }
            "undo" | "redo" => {
                let target: TargetParams = serde_json::from_value(request.params.clone())
                    .context("invalid_params: 历史参数无效")?;
                let index = self.agent_session(&target)?;
                ensure!(
                    !self.agent_interaction_busy(),
                    "gui_busy: 请先完成或取消当前交互并等待处理完成"
                );
                request.guard.claim()?;
                let session = &mut self.tabs[index];
                let changed = if request.method == "undo" {
                    session.history.undo(&mut session.doc)
                } else {
                    session.history.redo(&mut session.doc)
                };
                if changed {
                    session.changed();
                    session.selected = session.doc.active_layer_id.into_iter().collect();
                }
                let mut result = self.agent_info(index);
                result["changed"] = json!(changed);
                Ok(Some(result))
            }
            "apply" => {
                let params: ApplyParams = serde_json::from_value(request.params.clone())
                    .context("invalid_params: apply 参数无效")?;
                let index = self.agent_session(&TargetParams {
                    session_id: params.session_id,
                    expected_revision: params.expected_revision,
                    expected_serial: params.expected_serial,
                })?;
                ensure!(
                    !self.agent_interaction_busy(),
                    "gui_busy: 请先完成或取消当前交互并等待处理完成"
                );
                ensure!(
                    !params.operations.is_empty(),
                    "invalid_params: operations 不能为空"
                );
                let bridge = self
                    .agent_bridge
                    .as_ref()
                    .context("gui_closed: Agent 接口已停止")?;
                let sender = bridge.sender.clone();
                let context = bridge.context.clone();
                let engine = bridge.engine.clone();
                let session = &mut self.tabs[index];
                let id = session.id;
                let revision = session.history.revision();
                let serial = session.serial;
                let before = session.doc.clone();
                session.edit_busy = true;
                let request = clone_request(request);
                thread::spawn(move || {
                    let result = background_result(|| {
                        let mut document = before.clone();
                        let mut history = History::default();
                        let mut engine = engine
                            .lock()
                            .map_err(|_| anyhow::anyhow!("operation_failed: AI 引擎状态异常"))?;
                        request.guard.pending()?;
                        let result = cli::apply_transaction(
                            &mut document,
                            &mut history,
                            &mut engine,
                            params.operations,
                        )?;
                        Ok(Box::new((document, result)))
                    });
                    let _ = sender.send(Event::Complete(Completion::Apply {
                        request,
                        session: id,
                        revision,
                        serial,
                        before: Box::new(before),
                        result,
                    }));
                    super::wake_renderer(&context);
                });
                Ok(None)
            }
            "save" => {
                let params: SaveParams = serde_json::from_value(request.params.clone())
                    .context("invalid_params: save 参数无效")?;
                let index = self.agent_session(&TargetParams {
                    session_id: params.session_id,
                    expected_revision: params.expected_revision,
                    expected_serial: params.expected_serial,
                })?;
                ensure!(
                    !self.agent_interaction_busy(),
                    "gui_busy: 请先完成或取消当前交互并等待处理完成"
                );
                let bridge = self
                    .agent_bridge
                    .as_ref()
                    .context("gui_closed: Agent 接口已停止")?;
                let sender = bridge.sender.clone();
                let context = bridge.context.clone();
                let session = &mut self.tabs[index];
                let path = params
                    .path
                    .or_else(|| session.path.clone())
                    .context("invalid_params: 未保存文档需要指定 path")?;
                ensure!(
                    path.extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("comp")),
                    "invalid_params: 项目输出必须使用 .comp 文件"
                );
                let source_path = session.path.clone();
                let source_stamp = session.stamp;
                let document = session.doc.clone();
                let id = session.id;
                let revision = session.history.revision();
                session.save_busy = true;
                let request = clone_request(request);
                thread::spawn(move || {
                    let result = background_result(|| {
                        let same_source = source_path
                            .as_ref()
                            .map(|source| project::same_project_path(source, &path))
                            .transpose()?
                            .unwrap_or(false);
                        let loaded_fingerprint = if same_source {
                            ensure!(
                                super::package_stamp(&path) == source_stamp,
                                "fingerprint_conflict: 磁盘项目有尚未处理的更新"
                            );
                            let digest = project::fingerprint(&path)?;
                            ensure!(
                                super::package_stamp(&path) == source_stamp,
                                "fingerprint_conflict: 计算指纹期间磁盘项目发生变化"
                            );
                            Some(
                                digest
                                    .iter()
                                    .map(|byte| format!("{byte:02x}"))
                                    .collect::<String>(),
                            )
                        } else {
                            None
                        };
                        let expected = params
                            .expected_fingerprint
                            .as_deref()
                            .or(loaded_fingerprint.as_deref());
                        request.guard.claim()?;
                        let (digest, stamp) = project::save_checked_observed(
                            &document,
                            &path,
                            expected,
                            params.overwrite,
                            || super::package_stamp(&path),
                        )?;
                        Ok((
                            digest.iter().map(|byte| format!("{byte:02x}")).collect(),
                            stamp,
                        ))
                    });
                    let _ = sender.send(Event::Complete(Completion::Save {
                        request,
                        session: id,
                        revision,
                        path,
                        result,
                    }));
                    super::wake_renderer(&context);
                });
                Ok(None)
            }
            "export" => {
                let params: ExportParams = serde_json::from_value(request.params.clone())
                    .context("invalid_params: export 参数无效")?;
                let index = self.agent_session(&TargetParams {
                    session_id: params.session_id,
                    expected_revision: params.expected_revision,
                    expected_serial: params.expected_serial,
                })?;
                ensure!(
                    !self.agent_interaction_busy(),
                    "gui_busy: 请先完成或取消当前交互并等待处理完成"
                );
                let format = params.format.clone().unwrap_or_else(|| {
                    params
                        .path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase()
                });
                ensure!(
                    [
                        "png", "jpg", "jpeg", "tif", "tiff", "bmp", "gif", "webp", "pdf"
                    ]
                    .contains(&format.as_str()),
                    "invalid_params: 导出 format 必须是 png、jpeg、tiff、bmp、gif、webp 或 pdf"
                );
                ensure!(
                    (1..=100).contains(&params.quality),
                    "invalid_params: JPEG quality 必须在 1–100 之间"
                );
                ensure!(
                    params
                        .max_side
                        .is_none_or(|side| side > 0 && side <= crate::model::MAX_SIDE),
                    "invalid_params: maxSide 超出有效范围"
                );
                let session = &self.tabs[index];
                let document = session.doc.clone();
                let physical_size = super::pdf_ui::page_size(session);
                let id = session.id;
                let revision = session.history.revision();
                let bridge = self
                    .agent_bridge
                    .as_ref()
                    .context("gui_closed: Agent 接口已停止")?;
                let sender = bridge.sender.clone();
                let context = bridge.context.clone();
                self.pending_io += 1;
                let request = clone_request(request);
                thread::spawn(move || {
                    let result = background_result(|| {
                        ensure!(
                            params.overwrite || !params.path.exists(),
                            "output_exists: 导出目标已存在，请明确设置 overwrite"
                        );
                        let raster = params.max_side.map_or_else(
                            || render::render_document(&document),
                            |side| render::render_preview(&document, side),
                        );
                        request.guard.claim()?;
                        if let Some(parent) = params
                            .path
                            .parent()
                            .filter(|parent| !parent.as_os_str().is_empty())
                        {
                            fs::create_dir_all(parent)?;
                        }
                        cli::export_document_raster(
                            &document,
                            &raster,
                            &params.path,
                            &format,
                            params.quality,
                            params.matte,
                            Some(physical_size),
                        )
                    });
                    let _ = sender.send(Event::Complete(Completion::Export {
                        request,
                        session: id,
                        revision,
                        path: params.path,
                        result,
                    }));
                    super::wake_renderer(&context);
                });
                Ok(None)
            }
            _ => anyhow::bail!("unknown_method: 未知 Agent 方法 {}", request.method),
        }
    }

    fn finish_agent(&mut self, completion: Completion) {
        match completion {
            Completion::Inspect { request, result } => send_result(request, Ok(result)),
            Completion::Apply {
                request,
                session,
                revision,
                serial,
                before,
                result,
            } => {
                let result = (|| {
                    let index = self
                        .tabs
                        .iter()
                        .position(|item| item.id == session)
                        .context("session_not_found: 处理期间文档已关闭")?;
                    self.tabs[index].edit_busy = false;
                    let (document, operations) = *result?;
                    let current = &self.tabs[index];
                    ensure!(
                        current.history.revision() == revision
                            && current.serial == serial
                            && current.doc == *before,
                        "revision_conflict: 处理期间文档已变化，结果未提交"
                    );
                    self.check_workspace(&document)?;
                    request.guard.claim()?;
                    let current = &mut self.tabs[index];
                    let changed = *before != document;
                    let recorded = current
                        .history
                        .record("AI Agent 批量编辑", *before, &document);
                    current.doc = document;
                    current.selected = current.doc.active_layer_id.into_iter().collect();
                    if changed {
                        current.changed();
                    }
                    let mut result = self.agent_info(index);
                    result["changed"] = json!(changed);
                    result["historyRecorded"] = json!(recorded);
                    result["results"] = operations["results"].clone();
                    Ok(result)
                })();
                send_result(request, result);
            }
            Completion::Save {
                request,
                session,
                revision,
                path,
                result,
            } => {
                let result = (|| {
                    let index = self
                        .tabs
                        .iter()
                        .position(|item| item.id == session)
                        .context("session_not_found: 项目已写入磁盘，但文档会话已关闭")?;
                    self.tabs[index].save_busy = false;
                    let (fingerprint, stamp) = result?;
                    let current = &mut self.tabs[index];
                    current.history.mark_saved(revision);
                    current.path = Some(path.clone());
                    current.stamp = stamp;
                    self.save_count += 1;
                    self.remember(&path);
                    let mut result = self.agent_info(index);
                    result["savedRevision"] = json!(revision);
                    result["output"] = json!(path);
                    result["fingerprint"] = json!(fingerprint);
                    Ok(result)
                })();
                send_result(request, result);
            }
            Completion::Export {
                request,
                session,
                revision,
                path,
                result,
            } => {
                self.pending_io = self.pending_io.saturating_sub(1);
                send_result(
                    request,
                    result.map(|()| {
                        json!({"sessionId":session,
                    "exportedRevision":revision,"output":path,"projectMarkedSaved":false})
                    }),
                );
            }
        }
    }
}

fn clone_request(request: &Request) -> Request {
    Request {
        id: request.id.clone(),
        method: request.method.clone(),
        params: Value::Null,
        reply: request.reply.clone(),
        guard: request.guard.clone(),
    }
}

fn send_result(request: Request, result: Result<Value>) {
    request.guard.finish();
    let response = match result {
        Ok(value) => cli::success(request.id, value),
        Err(error) => bridge_failure(request.id, &error),
    };
    let _ = request.reply.send(response);
}

fn bridge_failure(id: Value, error: &anyhow::Error) -> Value {
    let mut response = cli::failure(id, error);
    if let Some((code, _)) = error.to_string().split_once(':')
        && [
            "invalid_params",
            "gui_busy",
            "gui_closed",
            "session_not_found",
            "no_document",
            "revision_conflict",
            "unknown_method",
            "output_exists",
            "fingerprint_conflict",
            "request_expired",
        ]
        .contains(&code)
    {
        response["error"]["code"] = json!(code);
    }
    response
}

fn background_result<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)).unwrap_or_else(|_| {
        Err(anyhow::anyhow!(
            "operation_failed: 后台操作意外终止，文档编辑未提交"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonblocking_accepted_connection_waits_for_delayed_and_fragmented_jsonl() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut accepted = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "Loopback accept timed out");
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("Loopback accept failed: {error}"),
            }
        };
        // Force the Windows inheritance condition on every platform. The client
        // sends no bytes until the server has demonstrably waited for them.
        accepted.set_nonblocking(true).unwrap();
        assert_eq!(
            accepted.read(&mut [0_u8; 1]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let (requests, receiver) = mpsc::sync_channel(1);
        let (started, start) = mpsc::channel();
        let (finished, finish) = mpsc::channel();
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            let result = serve_connection(
                accepted,
                "delayed-request-token",
                &requests,
                &Mutex::new(None),
                &AtomicBool::new(false),
            )
            .map_err(|error| format!("{error:#}"));
            finished.send(result).unwrap();
        });
        start.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            matches!(
                finish.recv_timeout(Duration::from_millis(150)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "The server must wait while a connected client has not sent its request"
        );
        client
            .write_all(br#"{"token":"delayed-request-token","id":73,"method":"capab"#)
            .unwrap();
        assert!(
            matches!(
                finish.recv_timeout(Duration::from_millis(150)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "The server must also wait for the rest of a fragmented request"
        );
        client.write_all(b"ilities\",\"params\":{}}\n").unwrap();
        let Event::Request(request) = receiver.recv_timeout(Duration::from_secs(5)).unwrap() else {
            panic!("The delayed JSONL request must reach the GUI request queue");
        };
        assert_eq!(request.id, json!(73));
        assert_eq!(request.method, "capabilities");
        assert_eq!(request.params, json!({}));
        let expected = json!({"protocolVersion":1,"id":73,"ok":true,"result":{"received":true}});
        request.reply.send(expected.clone()).unwrap();
        let mut response = String::new();
        BufReader::new(client).read_line(&mut response).unwrap();
        assert!(response.ends_with('\n'));
        assert_eq!(serde_json::from_str::<Value>(&response).unwrap(), expected);
        finish
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
    }

    fn editor() -> EditorApp {
        let mut app = EditorApp::create(None, None);
        app.tabs = vec![super::super::Session::new(Document::new(8, 8), None)];
        app.current = 0;
        app
    }

    fn completion(
        app: &mut EditorApp,
        document: Document,
    ) -> (Completion, mpsc::Receiver<Value>, Arc<RequestGuard>) {
        let (reply, receiver) = mpsc::channel();
        let guard = Arc::new(RequestGuard::new(RESPONSE_TIMEOUT));
        let session = &mut app.tabs[0];
        session.edit_busy = true;
        let completion = Completion::Apply {
            request: Request {
                id: json!(7),
                method: "apply".into(),
                params: json!({}),
                reply,
                guard: guard.clone(),
            },
            session: session.id,
            revision: session.history.revision(),
            serial: session.serial,
            before: Box::new(session.doc.clone()),
            result: Ok(Box::new((document, json!({"results":[]})))),
        };
        (completion, receiver, guard)
    }

    #[test]
    fn renderer_rebind_retains_the_live_agent_endpoint_and_in_flight_commit() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = editor();
        app.tabs[0].edit("User edit", |doc| doc.resolution = 144.);
        let before = app.tabs[0].doc.clone();
        let mut after = before.clone();
        after.layers[0].name = "恢复后提交的 Agent 修改".into();
        let (completion, response, guard) = completion(&mut app, after.clone());
        let (sender, receiver) = mpsc::sync_channel(CONNECTION_LIMIT * 2);
        let producer = sender.clone();
        let engine = Arc::new(Mutex::new(automation::Engine::default()));
        let stopped = Arc::new(AtomicBool::new(false));
        let endpoint_path = directory.path().join("agent.json");
        let token = Uuid::new_v4().to_string();
        let endpoint_bytes = serde_json::to_vec(&json!({"token":token})).unwrap();
        fs::write(&endpoint_path, &endpoint_bytes).unwrap();
        app.agent_bridge = Some(AgentBridge {
            receiver,
            sender,
            context: Arc::new(Mutex::new(Some(egui::Context::default()))),
            engine: engine.clone(),
            stopped: stopped.clone(),
            listener: None,
            endpoint_path: endpoint_path.clone(),
            token: token.clone(),
            observed_sessions: Default::default(),
        });
        let context = egui::Context::default();
        let marker = egui::Id::new("agent-replacement-context");
        context.data_mut(|data| data.insert_temp(marker, 23_u32));
        app.recover_renderer(&eframe::CreationContext::_new_kittest(context));
        let bridge = app.agent_bridge.as_ref().unwrap();
        assert!(!stopped.load(Ordering::Acquire));
        assert_eq!(bridge.token, token);
        assert!(Arc::ptr_eq(&bridge.engine, &engine));
        assert_eq!(fs::read(endpoint_path).unwrap(), endpoint_bytes);
        assert_eq!(
            bridge
                .context
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .data(|data| data.get_temp::<u32>(marker)),
            Some(23)
        );
        assert!(guard.pending().is_ok());
        // This sender was captured by a worker before the native renderer failed.
        producer.send(Event::Complete(completion)).unwrap();
        app.poll_agent();
        let result = response.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["result"]["historyRecorded"], true);
        assert_eq!(app.tabs[0].doc, after);
        assert!(!app.tabs[0].edit_busy);
        let session = &mut app.tabs[0];
        assert!(session.history.undo(&mut session.doc));
        assert_eq!(session.doc, before);
        assert_eq!(session.history.undo_name(), Some("User edit"));
    }

    #[test]
    fn live_schema_rejects_unknown_fields_and_foreign_operations() {
        assert!(
            serde_json::from_value::<WireRequest>(
                json!({"token":"a","method":"inspect","unexpected":true})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<TargetParams>(json!({"expectedRevison":Uuid::new_v4()}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<ApplyParams>(
                json!({"operations":[{"op":"execute_shell","command":"echo test"}]})
            )
            .is_err()
        );
    }

    #[test]
    fn missing_welcome_and_stale_sessions_are_rejected_without_indexing() {
        let mut app = editor();
        let target = TargetParams {
            expected_revision: Some(Uuid::new_v4()),
            ..Default::default()
        };
        assert!(
            app.agent_session(&target)
                .unwrap_err()
                .to_string()
                .starts_with("revision_conflict:")
        );
        app.tabs[0].placeholder = true;
        assert!(app.agent_session(&TargetParams::default()).is_err());
        app.tabs.clear();
        assert!(app.agent_session(&TargetParams::default()).is_err());
    }

    #[test]
    fn live_batch_preserves_unsaved_edits_and_undoes_in_one_step() {
        let mut app = editor();
        app.tabs[0].edit("User edit", |doc| {
            doc.layers[0].name = "用户未保存修改".into()
        });
        let before = app.tabs[0].doc.clone();
        let mut after = before.clone();
        after.layers[0].name = "Agent 修改".into();
        after.resolution = 144.;
        let (completion, receiver, _) = completion(&mut app, after.clone());
        app.finish_agent(completion);
        let response = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(response["ok"], true);
        assert_eq!(response["result"]["dirty"], true);
        assert_eq!(response["result"]["historyRecorded"], true);
        assert_eq!(app.tabs[0].doc, after);
        assert!(!app.tabs[0].edit_busy);
        let session = &mut app.tabs[0];
        assert!(session.history.undo(&mut session.doc));
        assert_eq!(session.doc, before);
        assert!(session.history.is_modified());
        assert_eq!(session.history.undo_name(), Some("User edit"));
    }

    #[test]
    fn expired_and_stale_background_results_never_replace_the_live_document() {
        for expired in [true, false] {
            let mut app = editor();
            let mut after = app.tabs[0].doc.clone();
            after.layers[0].name = "不得提交".into();
            let (completion, receiver, guard) = completion(&mut app, after);
            let expected_code = if expired {
                assert!(guard.expire());
                "request_expired"
            } else {
                app.tabs[0].edit("User edit", |doc| doc.layers[0].name = "最新修改".into());
                "revision_conflict"
            };
            let expected = app.tabs[0].doc.clone();
            app.finish_agent(completion);
            let response = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(response["error"]["code"], expected_code);
            assert_eq!(app.tabs[0].doc, expected);
            assert!(!app.tabs[0].edit_busy);
        }
    }

    #[test]
    fn commit_and_timeout_have_only_one_winner() {
        for _ in 0..16 {
            let guard = Arc::new(RequestGuard::new(RESPONSE_TIMEOUT));
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let worker_guard = guard.clone();
            let worker_barrier = barrier.clone();
            let worker = thread::spawn(move || {
                worker_barrier.wait();
                worker_guard.claim().is_ok()
            });
            barrier.wait();
            let expired = guard.expire();
            assert_ne!(expired, worker.join().unwrap());
        }
        assert!(RequestGuard::new(Duration::ZERO).claim().is_err());
    }

    #[test]
    fn active_layer_changes_invalidate_old_inspects_without_double_counting_commits() {
        let mut app = editor();
        let mut other = app.tabs[0].doc.layers[0].clone();
        other.id = Uuid::new_v4();
        let other_id = other.id;
        app.tabs[0].doc.layers.push(other);
        let mut observed = Default::default();
        observe_agent_sessions(&mut observed, &mut app.tabs);
        let before = app.tabs[0].serial;
        let previous_layer = app.tabs[0].doc.active_layer_id;
        app.tabs[0].doc.active_layer_id = Some(other_id);
        observe_agent_sessions(&mut observed, &mut app.tabs);
        assert_eq!(app.tabs[0].serial, before.wrapping_add(1));
        assert!(
            app.agent_session(&TargetParams {
                expected_serial: Some(before),
                ..Default::default()
            })
            .is_err()
        );
        app.tabs[0].doc.active_layer_id = previous_layer;
        app.tabs[0].changed();
        let committed_serial = app.tabs[0].serial;
        observe_agent_sessions(&mut observed, &mut app.tabs);
        assert_eq!(app.tabs[0].serial, committed_serial);
        observe_agent_sessions(&mut observed, &mut app.tabs);
        assert_eq!(app.tabs[0].serial, committed_serial);
        app.tabs.clear();
        observe_agent_sessions(&mut observed, &mut app.tabs);
        assert!(observed.is_empty());
    }
}
