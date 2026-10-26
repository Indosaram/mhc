use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

use maho_ai::utils::abort::{AbortController, AbortSignal};
use maho_codemode::kernels::js::context_manager::JavaScriptKernel;
use maho_codemode::kernels::shared::subprocess_contract::KernelRunInput;
use senpi_desktop_protocol::{
    AuditEvent, AuditRecord, AuditStatus, ComputerRunOk, ComputerSessionSnapshot, ErrorCode,
};
use senpi_desktop_service::rpc_client::{CallOptions, DesktopError};
use senpi_desktop_service::service::DesktopService;
use serde_json::{json, Value};

use crate::context::{ComputerRunError, ComputerRunErrorReason, DesktopRunError, EngineCall, RunContext, RunScope};
use crate::facade::DesktopFacade;

pub type ExecuteTool = Arc<
    dyn Fn(
            String,
            Value,
            AbortSignal,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>>
        + Send
        + Sync,
>;

pub struct ComputerRunRequest {
    pub code: String,
    pub snapshot: ComputerSessionSnapshot,
    pub timeout_ms: u64,
    pub signal: Option<AbortSignal>,
}

pub struct ComputerRunHost {
    pub service: Arc<DesktopService>,
    pub execute_tool: Option<ExecuteTool>,
    pub kernel: Option<Arc<JavaScriptKernel>>,
}

fn audit_record(event: AuditEvent, session_id: &str, run_id: &str) -> AuditRecord {
    let code: Option<ErrorCode> = event.code;
    let status = match code {
        None => AuditStatus::Success,
        Some(ErrorCode::Suspended) => AuditStatus::Suspended,
        Some(_) => AuditStatus::Error,
    };
    AuditRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        session_id: session_id.to_string(),
        run_id: run_id.to_string(),
        action: event.action,
        target: event.target,
        delivery: event.delivery,
        frame_id: event.frame_id,
        code,
        status,
        primary_error: None,
        duration_ms: event.duration_ms,
        focus_restored: event.focus_restored,
        screenshot_path: None,
        screenshot_width: None,
        screenshot_height: None,
        text_length: event.text_length,
        text_sha256: event.text_sha256,
        keys: event.keys,
        message: None,
    }
}

pub async fn run_computer_code(
    request: ComputerRunRequest,
    host: ComputerRunHost,
) -> Result<ComputerRunOk, DesktopRunError> {
    let run_id = Uuid::new_v4().to_string();
    let snapshot = request.snapshot;
    let timeout_ms = request.timeout_ms;
    let context = RunContext::new(snapshot.clone());

    if let Some(signal) = &request.signal
        && signal.aborted() {
        return Err(ComputerRunError::aborted().into());
    }

    // Started before the audit listener and helper tasks so a failed start
    // leaves nothing registered behind.
    let (kernel, owned_kernel) = match host.kernel {
        Some(k) => (k, false),
        None => {
            let cwd = std::path::PathBuf::from(&snapshot.cwd);
            let k = JavaScriptKernel::start(
                &cwd,
                &format!("desktop-run-{run_id}"),
                1,
                None,
            )
            .await
            .map_err(|e| ComputerRunError::new(ComputerRunErrorReason::Window, e.to_string()))?;
            (Arc::new(k), true)
        }
    };

    let audit = Arc::new(std::sync::Mutex::new(Vec::<AuditRecord>::new()));
    let audit_sink = audit.clone();
    let session_id_str = snapshot.session_id.clone();
    let run_id_str = run_id.clone();

    let sub = host.service.on_audit(move |event| {
        let rec = audit_record(event, &session_id_str, &run_id_str);
        audit_sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(rec);
    });

    let (cancel_broadcast, _) = broadcast::channel::<()>(16);
    let cancel_tx = cancel_broadcast.clone();
    let host_abort = AbortController::new();
    let host_signal = host_abort.signal();
    let mut host_cancel_rx = cancel_broadcast.subscribe();
    let host_cancel_task = tokio::spawn(async move {
        let _ = host_cancel_rx.recv().await;
        host_abort.abort(None);
    });

    let run_aborted = Arc::new(AtomicBool::new(false));
    let run_timed_out = Arc::new(AtomicBool::new(false));

    let run_aborted_clone = Arc::clone(&run_aborted);
    let cancel_tx_abort = cancel_tx.clone();
    let caller_sub_task = if let Some(caller_signal) = &request.signal {
        let sig_clone = caller_signal.clone();
        Some(tokio::spawn(async move {
            sig_clone.cancelled().await;
            run_aborted_clone.store(true, Ordering::SeqCst);
            let _ = cancel_tx_abort.send(());
        }))
    } else {
        None
    };

    let service = host.service.clone();
    let cancel_rx_factory = cancel_broadcast.clone();
    let engine_call: EngineCall = Arc::new(move |method, params| {
        let svc = service.clone();
        let mut b_rx = cancel_rx_factory.subscribe();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let watcher = tokio::spawn(async move {
            let _ = b_rx.recv().await;
            let _ = tx.send(());
        });
        Box::pin(async move {
            let opts = CallOptions {
                timeout: None,
                cancel_rx: Some(Arc::new(Mutex::new(rx))),
            };
            let result = svc.call(method, params, opts).await;
            watcher.abort();
            result
        })
    });

    let scope = RunScope::new(context.clone(), engine_call);
    let _desktop = DesktopFacade::new(scope.clone());

    let desktop_scope = scope.clone();
    let execute_tool_fn = host.execute_tool.clone();
    let kernel_for_pump = kernel.clone();
    let context_for_pump = context.clone();

    let call_error: Arc<Mutex<HashMap<String, DesktopError>>> = Arc::new(Mutex::new(HashMap::new()));
    let call_error_pump = Arc::clone(&call_error);

    let mut in_flight_tasks = tokio::task::JoinSet::new();
    let (pump_stop_tx, mut pump_stop_rx) = tokio::sync::oneshot::channel::<()>();

    let pump_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut pump_stop_rx => break,
                res = kernel_for_pump.next_tool_call() => {
                    let call = match res {
                        Ok(c) => c,
                        Err(_) => break,
                    };
                    while in_flight_tasks.try_join_next().is_some() {}

                    let call_id = call.get("callId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let tool_name = call.get("toolName").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let args = call.get("args").cloned().unwrap_or(Value::Null);

                    let k_reply = kernel_for_pump.clone();
                    let d_scope = desktop_scope.clone();
                    let ctx = context_for_pump.clone();
                    let exec_tool = execute_tool_fn.clone();
                    let captured_err = Arc::clone(&call_error_pump);
                    let host_signal = host_signal.clone();

                    in_flight_tasks.spawn(async move {
                        if tool_name == "desktop_call" {
                            let method = args.get("method").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let mut params = args.get("params").cloned().unwrap_or(json!({}));
                            if method == "capture" {
                                params["caps"] = json!({
                                    "maxWidth": ctx.snapshot.capture_max_width,
                                    "maxHeight": ctx.snapshot.capture_max_height,
                                    "maxBytes": ctx.snapshot.capture_max_bytes,
                                });
                            }
                            let is_read_only = ctx.read_only;

                            if is_read_only && matches!(method.as_str(), "click" | "moveMouse" | "drag" | "typeText" | "keyChord" | "scroll" | "raiseWindow" | "clipboard.write" | "ax.perform" | "ax.setValue" | "ax.focus" | "ax.click") {
                                let err_msg = format!("read-only run: '{method}' requires read_only: false");
                                let _ = k_reply.deliver_tool_reply(json!({
                                    "type": "tool-reply",
                                    "callId": call_id,
                                    "ok": false,
                                    "error": { "message": err_msg, "name": "ComputerRunError", "code": "readOnly" }
                                }));
                                return;
                            }

                            let static_method: &'static str = match method.as_str() {
                                "capabilities" => "capabilities",
                                "displays" => "displays",
                                "windows" => "windows",
                                "capture" => "capture",
                                "click" => "click",
                                "moveMouse" => "moveMouse",
                                "drag" => "drag",
                                "scroll" => "scroll",
                                "typeText" => "typeText",
                                "keyChord" => "keyChord",
                                "raiseWindow" => "raiseWindow",
                                "clipboard.read" => "clipboard.read",
                                "clipboard.write" => "clipboard.write",
                                "ax.snapshot" => "ax.snapshot",
                                "ax.query" => "ax.query",
                                "ax.node" => "ax.node",
                                "ax.elementAt" => "ax.elementAt",
                                "ax.focused" => "ax.focused",
                                "ax.parent" => "ax.parent",
                                "ax.children" => "ax.children",
                                "ax.attributes" => "ax.attributes",
                                "ax.perform" => "ax.perform",
                                "ax.setValue" => "ax.setValue",
                                "ax.focus" => "ax.focus",
                                "ax.click" => "ax.click",
                                _ => "unknown",
                            };

                            let res = match (d_scope.call)(static_method, params).await {
                                Ok(value) => {
                                    use crate::engine_results::*;
                                    let shape = match static_method {
                                        "capabilities" => check_result::<senpi_desktop_protocol::DesktopCapabilities>(static_method, &value),
                                        "displays" => check_result::<Vec<DesktopDisplay>>(static_method, &value),
                                        "windows" => check_result::<Vec<DesktopWindow>>(static_method, &value),
                                        "capture" => check_capture(static_method, &value),
                                        "clipboard.read" | "ax.snapshot" => check_result::<TextResult>(static_method, &value),
                                        "ax.node" => check_ax_node(static_method, &value),
                                        "ax.elementAt" | "ax.focused" | "ax.parent" => if value.is_null() { Ok(()) } else { check_ax_node(static_method, &value) },
                                        "ax.query" | "ax.children" => check_ax_nodes(static_method, &value),
                                        "ax.attributes" => check_result::<Vec<(String, String)>>(static_method, &value),
                                        _ => Ok(()),
                                    };
                                    shape.map(|()| value)
                                }
                                Err(error) => Err(error),
                            };
                            match res {
                                Ok(val) => {
                                    let mut reply = json!({
                                        "type": "tool-reply",
                                        "callId": call_id,
                                        "ok": true
                                    });
                                    reply["value"] = val;
                                    let _ = k_reply.deliver_tool_reply(reply);
                                }
                                Err(err) => {
                                    captured_err.lock().await.insert(call_id.clone(),err.clone());
                                    let err_msg = match &err {
                                        DesktopError::Rpc(rpc) => rpc.message.clone(),
                                        other => other.to_string(),
                                    };
                                    let _ = k_reply.deliver_tool_reply(json!({
                                        "type": "tool-reply",
                                        "callId": call_id,
                                        "ok": false,
                                        "error": {
                                            "message": err_msg,
                                            "name": "DesktopEngineRpcError",
                                            "code": call_id,
                                            "data": match &err {
                                                DesktopError::Rpc(rpc) => rpc.data.as_ref(),
                                                _ => None,
                                            },
                                        }
                                    }));
                                }
                            }
                        } else if tool_name == "desktop_record_screenshot" {
                            if let Ok(shot) = serde_json::from_value::<senpi_desktop_protocol::ComputerScreenshot>(args.clone()) {
                                let mut sc = ctx.screenshots.lock().await;
                                sc.push(shot);
                            }
                            let _ = k_reply.deliver_tool_reply(json!({
                                "type": "tool-reply",
                                "callId": call_id,
                                "ok": true,
                                "value": Value::Null
                            }));
                        } else if let Some(execute_tool) = &exec_tool {
                            let res = execute_tool(tool_name, args, host_signal).await;
                            match res {
                                Ok(val) => {
                                    let mut reply = json!({
                                        "type": "tool-reply",
                                        "callId": call_id,
                                        "ok": true
                                    });
                                    reply["value"] = val;
                                    let _ = k_reply.deliver_tool_reply(reply);
                                }
                                Err(err) => {
                                    let _ = k_reply.deliver_tool_reply(json!({
                                        "type": "tool-reply",
                                        "callId": call_id,
                                        "ok": false,
                                        "error": { "message": err, "name": "Error" }
                                    }));
                                }
                            }
                        } else {
                            let _ = k_reply.deliver_tool_reply(json!({
                                "type": "tool-reply",
                                "callId": call_id,
                                "ok": false,
                                "error": { "message": format!("unknown tool {tool_name}") }
                            }));
                        }
                    });
                }
            }
        }
        while in_flight_tasks.join_next().await.is_some() {}
    });

    let prelude_code = r#"
function __runError(reason, message) {
    const error = new Error(message);
    error.name = 'ComputerRunError';
    error.code = reason;
    return error;
}
globalThis.assert = function(cond, msg) {
    if (!cond) throw __runError('assertion', msg ?? "Assertion failed");
};
globalThis.wait = async function(msOrPredicate, options) {
    if (typeof msOrPredicate === 'number') {
        await new Promise(r => setTimeout(r, msOrPredicate));
        return;
    }
    if (typeof msOrPredicate === 'function') {
        const explicit = options?.timeout;
        const budget = Math.max(1, __runTimeoutMs - 1000);
        const timeout = explicit === 0 || explicit === Infinity ? budget : Number.isFinite(explicit) && explicit > 0 ? Math.min(explicit, budget) : Math.min(30000, budget);
        const interval = Math.max(options?.interval ?? 100, 10);
        const start = Date.now();
        for (;;) {
            const val = await msOrPredicate();
            if (val) return val;
            if (Date.now() - start >= timeout) throw __runError('wait', "wait(predicate) timed out after " + timeout + " ms; the predicate never returned truthy");
            await new Promise(r => setTimeout(r, interval));
        }
    }
    throw __runError('wait', "wait(...) expects milliseconds or a predicate function to poll");
};

function __caption(frame) {
    const size = frame.width + "x" + frame.height;
    const scaled = (frame.width !== frame.sourceWidth || frame.height !== frame.sourceHeight)
        ? " (scaled from " + frame.sourceWidth + "x" + frame.sourceHeight + ")" : "";
    const artifact = frame.artifactPath ? " -> " + frame.artifactPath : "";
    return "screenshot " + frame.target + " " + size + scaled + artifact;
}

async function __handleCapture(res, silent) {
    const path = res.artifactPath || undefined;
    if (path) {
        await tool.desktop_record_screenshot({
            path: path,
            width: res.width,
            height: res.height,
            sourceWidth: res.sourceWidth,
            sourceHeight: res.sourceHeight,
            target: res.target
        });
    }
    if (!silent) {
        print(__caption(res));
        if (res.mode !== "artifact-only" && res.data && res.mimeType) {
            display({ type: "image", mimeType: res.mimeType, data: res.data });
        } else {
            print(res.note || "screenshot is available only as the artifact above");
        }
    }
    return {
        target: res.target,
        frameId: res.frameId,
        width: res.width,
        height: res.height,
        sourceWidth: res.sourceWidth,
        sourceHeight: res.sourceHeight,
        scale: res.scale,
        ...(path ? { path } : {})
    };
}

function __pointerOpts(opts) {
    if (!opts) return {};
    return { button: opts.button, count: opts.count, modifiers: opts.modifiers, deliveryMode: opts.delivery };
}

function __chordKeys(chord) {
    return Array.isArray(chord) ? chord : chord.split('+').map(key => key.trim()).filter(key => key.length > 0);
}

function __wrapElement(handle) {
    if (!handle) return null;
    return {
        ref: handle.ref,
        role: handle.role,
        nativeRole: handle.nativeRole,
        title: handle.title,
        description: handle.description,
        enabled: handle.enabled,
        focused: handle.focused,
        childCount: handle.childCount,
        value: async () => (await tool.desktop_call({ method: 'ax.node', params: { ref: handle.ref } })).value,
        setValue: async (value) => tool.desktop_call({ method: 'ax.setValue', params: { ref: handle.ref, value } }),
        bounds: async () => {
            const n = await tool.desktop_call({ method: 'ax.node', params: { ref: handle.ref } });
            return (n.x == null || n.y == null || n.width == null || n.height == null) ? null : { x: n.x, y: n.y, width: n.width, height: n.height };
        },
        attributes: async () => {
            const pairs = await tool.desktop_call({ method: 'ax.attributes', params: { ref: handle.ref } });
            return Object.fromEntries(pairs);
        },
        actions: async () => (await tool.desktop_call({ method: 'ax.node', params: { ref: handle.ref } })).actions || [],
        perform: async (action) => tool.desktop_call({ method: 'ax.perform', params: { ref: handle.ref, action } }),
        press: async () => tool.desktop_call({ method: 'ax.perform', params: { ref: handle.ref, action: 'press' } }),
        click: async (opts) => tool.desktop_call({ method: 'ax.click', params: { ref: handle.ref, opts: __pointerOpts(opts) } }),
        focus: async () => tool.desktop_call({ method: 'ax.focus', params: { ref: handle.ref } }),
        parent: async () => __wrapElement(await tool.desktop_call({ method: 'ax.parent', params: { ref: handle.ref } })),
        children: async () => {
            const list = await tool.desktop_call({ method: 'ax.children', params: { ref: handle.ref } });
            return (list || []).map(__wrapElement);
        },

    };
}

function __wrapWindow(w) {
    if (!w) return null;
    return {
        id: w.id,
        title: w.title,
        app: w.app,
        pid: w.pid,
        focused: w.focused,
        bounds: { x: w.x, y: w.y, width: w.width, height: w.height },
        raise: async () => tool.desktop_call({ method: 'raiseWindow', params: { windowId: w.id } }),
        screenshot: async (opts) => {
            const res = await tool.desktop_call({ method: 'capture', params: { target: w.id } });
            return await __handleCapture(res, opts && opts.silent);
        },
        click: async (x, y, opts) => tool.desktop_call({ method: 'click', params: { target: w.id, x, y, opts: __pointerOpts(opts) } }),
        doubleClick: async (x, y, opts) => tool.desktop_call({ method: 'click', params: { target: w.id, x, y, opts: __pointerOpts({ ...opts, count: 2 }) } }),
        move: async (x, y) => tool.desktop_call({ method: 'moveMouse', params: { target: w.id, x, y } }),
        drag: async (points, opts) => tool.desktop_call({ method: 'drag', params: { target: w.id, path: points.map(([x,y]) => ({x,y})), opts: __pointerOpts(opts) } }),
        type: async (text, opts) => tool.desktop_call({ method: 'typeText', params: { target: w.id, text, opts: __pointerOpts(opts) } }),
        press: async (chord, opts) => tool.desktop_call({ method: 'keyChord', params: { target: w.id, keys: __chordKeys(chord), opts: __pointerOpts(opts) } }),
        scroll: async (x, y, opts = {}) => tool.desktop_call({ method: 'scroll', params: { target: w.id, x, y, dx: opts.dx ?? 0, dy: opts.dy ?? 0, opts: __pointerOpts(opts) } }),
        ax: async (opts) => (await tool.desktop_call({ method: 'ax.snapshot', params: { target: w.id, opts } })).text,
        ref: async (ref) => __wrapElement(await tool.desktop_call({ method: 'ax.node', params: { ref } })),
        find: async (selector) => {
            const list = await tool.desktop_call({ method: 'ax.query', params: { target: w.id, query: selector } });
            return list.map(__wrapElement);
        },

    };
}

globalThis.desktop = {
    capabilities: async () => tool.desktop_call({ method: 'capabilities', params: {} }),
    displays: async () => tool.desktop_call({ method: 'displays', params: {} }),
    windows: async (filter = {}) => {
        const list = await tool.desktop_call({ method: 'windows', params: {} });
        return list.filter(w => (!filter.app || w.app.toLocaleLowerCase().includes(filter.app.toLocaleLowerCase())) && (!filter.title || w.title.toLocaleLowerCase().includes(filter.title.toLocaleLowerCase())));
    },
    focusedWindow: async () => __wrapWindow((await desktop.windows()).find(w => w.focused)),
    ref: async (ref) => __wrapElement(await tool.desktop_call({ method: 'ax.node', params: { ref } })),
    window: async (selector) => {
        const list = await tool.desktop_call({ method: 'windows', params: {} });
        let found = null;
        if (typeof selector === 'string') {
            found = (list || []).filter(w => w.id === selector);
        } else if (typeof selector === 'object' && selector !== null) {
            found = (list || []).filter(w => {
                if (selector.title && !w.title.toLocaleLowerCase().includes(selector.title.toLocaleLowerCase())) return false;
                if (selector.app && !w.app.toLowerCase().includes(selector.app.toLowerCase())) return false;
                return true;
            });
        }
        if (!found || found.length === 0) throw new Error("no window matches " + JSON.stringify(selector));
        if (found.length > 1) throw new Error("multiple windows match " + JSON.stringify(selector) + ":\n" + found.map(w => w.id + " " + w.app + " " + JSON.stringify(w.title)).join("\n"));
        return __wrapWindow(found[0]);
    },
    screenshot: async (opts) => {
        const res = await tool.desktop_call({ method: 'capture', params: { target: 'desktop' } });
        return await __handleCapture(res, opts && opts.silent);
    },
    click: async (x, y, opts) => tool.desktop_call({ method: 'click', params: { target: 'desktop', x, y, opts: __pointerOpts(opts) } }),
    doubleClick: async (x, y, opts) => tool.desktop_call({ method: 'click', params: { target: 'desktop', x, y, opts: __pointerOpts({ ...opts, count: 2 }) } }),
    move: async (x, y) => tool.desktop_call({ method: 'moveMouse', params: { target: 'desktop', x, y } }),
    drag: async (points, opts) => tool.desktop_call({ method: 'drag', params: { target: 'desktop', path: points.map(([x,y]) => ({x,y})), opts: __pointerOpts(opts) } }),
    type: async (text, opts) => tool.desktop_call({ method: 'typeText', params: { target: 'desktop', text, opts: __pointerOpts(opts) } }),
    press: async (chord, opts) => tool.desktop_call({ method: 'keyChord', params: { target: 'desktop', keys: __chordKeys(chord), opts: __pointerOpts(opts) } }),
    scroll: async (x, y, opts = {}) => tool.desktop_call({ method: 'scroll', params: { target: 'desktop', x, y, dx: opts.dx ?? 0, dy: opts.dy ?? 0, opts: __pointerOpts(opts) } }),
    clipboard: {
        read: async () => (await tool.desktop_call({ method: 'clipboard.read', params: {} })).text,
        write: async (text) => tool.desktop_call({ method: 'clipboard.write', params: { text } })
    },
    elementAt: async (x, y) => {
        const el = await tool.desktop_call({ method: 'ax.elementAt', params: { target: 'desktop', x, y } });
        return __wrapElement(el);
    },
    focusedElement: async () => {
        const el = await tool.desktop_call({ method: 'ax.focused', params: {} });
        return __wrapElement(el);
    }
};
"#.to_string();

    let wrapped_user_code = format!(
        r#"
{prelude_code}
const __runTimeoutMs = {timeout_ms};

return await (async () => {{
{code}
}})();
"#,
        code = request.code
    );

    let input = KernelRunInput {
        cell_id: run_id.clone(),
        code: wrapped_user_code,
        timeout_ms: None,
    };

    let kernel_run = kernel.clone();
    let cancel_tx_timeout = cancel_tx.clone();
    let run_timed_out_clone = Arc::clone(&run_timed_out);
    let run_id_interrupt = run_id.clone();

    let timeout_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
        run_timed_out_clone.store(true, Ordering::SeqCst);
        let _ = cancel_tx_timeout.send(());
        let _ = kernel_run.interrupt("timeout", Some(&run_id_interrupt)).await;
    });

    let (msg_sender, mut msg_receiver) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let msg_callback: maho_codemode::kernels::shared::subprocess_run::KernelMessageCallback =
        Arc::new(move |msg: &Value| {
            let _ = msg_sender.send(msg.clone());
        });

    let run_operation = kernel.run_with_callbacks(input, Some(msg_callback), None);
    tokio::pin!(run_operation);

    let run_res = loop {
        tokio::select! {
            result = &mut run_operation => break result,
            Some(msg) = msg_receiver.recv() => {
                let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if msg_type == "text" || msg_type == "stdout" {
                    if let Some(data) = msg.get("data").and_then(|v| v.as_str()) {
                        context.output.text(data.trim_end_matches('\n')).await;
                    }
                } else if msg_type == "display" {
                    let mime = msg.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");
                    let data = msg.get("dataBase64")
                        .or_else(|| msg.get("data"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !data.is_empty() {
                        context.output.image(data, mime).await;
                    }
                }
            }
        }
    };

    while let Ok(msg) = msg_receiver.try_recv() {
        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if msg_type == "text" || msg_type == "stdout" {
            if let Some(data) = msg.get("data").and_then(|v| v.as_str()) {
                context.output.text(data.trim_end_matches('\n')).await;
            }
        } else if msg_type == "display" {
            let mime = msg.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");
            let data = msg.get("dataBase64")
                .or_else(|| msg.get("data"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !data.is_empty() {
                context.output.image(data, mime).await;
            }
        }
    }

    timeout_task.abort();
    let timed_out = run_timed_out.load(Ordering::SeqCst);

    let _ = cancel_tx.send(());
    let _ = pump_stop_tx.send(());
    let _ = pump_task.await;
    let _ = host_cancel_task.await;

    if let Some(task) = caller_sub_task {
        task.abort();
        let _ = task.await;
    }
    sub.unsubscribe();

    if owned_kernel {
        let _ = kernel.close().await;
    }

    if timed_out {
        return Err(ComputerRunError::timeout(timeout_ms).into());
    }

    if run_aborted.load(Ordering::SeqCst) {
        return Err(ComputerRunError::aborted().into());
    }

    match run_res {
        Ok(val) => {
            if val["ok"] == true {
                let return_value = val.get("valueRepr").and_then(|v| {
                    if v.is_null() {
                        None
                    } else if let Some(s) = v.as_str() {
                        serde_json::from_str::<Value>(s).ok()
                    } else {
                        Some(v.clone())
                    }
                });

                let displays = context.output.finish().await;
                let screenshots = context.screenshots.lock().await.clone();
                let audit_events = audit.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();

                Ok(ComputerRunOk {
                    displays,
                    return_value,
                    screenshots,
                    capabilities: None,
                    audit: audit_events,
                })
            } else {
                let err_obj = &val["error"];

                if let Some(code) = err_obj.get("code").and_then(Value::as_str)
                    && let Some(typed_err) = call_error.lock().await.remove(code) {
                    return Err(DesktopRunError::Engine(typed_err));
                }

                let err_name = err_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let err_code = err_obj.get("code").and_then(|v| v.as_str()).unwrap_or("");
                let err_msg = err_obj.get("message").and_then(|v| v.as_str()).unwrap_or("unknown error").to_string();

                if err_name == "ComputerRunError" {
                    let reason = match err_code {
                        "readOnly" => ComputerRunErrorReason::ReadOnly,
                        "assertion" => ComputerRunErrorReason::Assertion,
                        "wait" => ComputerRunErrorReason::Wait,
                        _ => ComputerRunErrorReason::Window,
                    };
                    return Err(ComputerRunError::new(reason, err_msg).into());
                }

                Err(ComputerRunError::new(ComputerRunErrorReason::Window, err_msg).into())
            }
        }
        Err(e) => {
            let err_str = e.to_string();
            Err(ComputerRunError::new(ComputerRunErrorReason::Window, err_str).into())
        }
    }
}
