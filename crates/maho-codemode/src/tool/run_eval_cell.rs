use super::{
    cell_execution::CellExecution,
    cell_handler::{CellBridgeRuntime, CellHandler},
    cell_runtime::{CellResultBuilder, CellState},
    detached_cell_contract::EvalDetachedCellState,
    detached_eval_result::{
        EvalBackgroundCapacityError, result_after_detach, result_for_detached_state,
    },
    eval_kernel_reset_refused_error::EvalKernelResetRefusedError,
    eval_request::eval_timeout_behavior,
    eval_tool_options::{CreateEvalToolOptions, EvalCellInvocation},
    image::EvalOutputOptions,
    types::{EvalKernelRunInput, TimeoutBehavior},
};
use maho_ai::utils::abort::{AbortController, AbortReason};
use maho_ext_api::AgentToolResult;
use std::sync::{Arc, Mutex};

pub async fn run_eval_cell(
    options: Arc<CreateEvalToolOptions>,
    invocation: EvalCellInvocation,
) -> Result<AgentToolResult, String> {
    invocation
        .signal
        .throw_if_aborted()
        .map_err(|reason| super::cell_execution::abort_error(reason).message)?;
    let detaches =
        eval_timeout_behavior(&invocation.input, &invocation.mode) == TimeoutBehavior::Detach;
    let manager = options.cell_manager.clone();
    let cell = manager
        .lock()
        .expect("cell manager lock")
        .create(invocation.cell_id.clone(), invocation.input.clone())?;
    let started_at = cell.lock().expect("managed cell lock").source.started_at_ms;
    let controller = AbortController::new();
    let bridge_signal = controller.signal();
    let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let active_abort = active.clone();
    // senpi catches `EvalBackgroundCapacityError` by type; the native abort reason carries its name.
    let capacity_cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let capacity_abort = capacity_cancelled.clone();
    // senpi settles `CodemodeSessionDisposedError` through `finalizeCancellation`; every other
    // cancellation (timeouts, caller aborts, deadlines) is rethrown as a tool error.
    let session_disposed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let disposed_abort = session_disposed.clone();
    let execution = CellExecution::new(
        invocation.signal.clone(),
        invocation.cell_id.clone(),
        None,
        Arc::new(move |reason| {
            active_abort.store(false, std::sync::atomic::Ordering::SeqCst);
            if reason.name == "EvalBackgroundCapacityError" {
                capacity_abort.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            if reason.name == "CodemodeSessionDisposedError" {
                disposed_abort.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            controller.abort(Some(reason));
        }),
    );
    let weak = Arc::downgrade(&execution);
    cell.lock().expect("managed cell lock").on_kill = Some(Arc::new(move |error| {
        if let Some(execution) = weak.upgrade() {
            execution.cancel(AbortReason::new("Error", error));
        }
    }));
    if detaches {
        let manager = manager.clone();
        let cell = cell.clone();
        let weak = Arc::downgrade(&execution);
        let cap = manager
            .lock()
            .expect("cell manager lock")
            .max_detached_cells();
        let cell_id = invocation.cell_id.clone();
        let foreground_ms = options.settings.foreground_window_seconds * 1000.0;
        let foreground_start = tokio::time::Instant::now();
        execution.rearm_idle((options.settings.cell_timeout_seconds.min(options.settings.foreground_window_seconds)*1000.0) as u64,foreground_ms as u64,Arc::new(move |error| {
            let Some(execution)=weak.upgrade() else {return;};
            let mut locked=manager.lock().expect("cell manager lock");
            if locked.detach(&cell) {execution.detach();return;}
            let remaining=(foreground_ms-foreground_start.elapsed().as_secs_f64()*1000.0).max(0.0);
            if remaining>0.0 {
                drop(locked);
                let manager=manager.clone();let cell=cell.clone();let weak=Arc::downgrade(&execution);let cell_id=cell_id.clone();
                execution.rearm_idle(remaining.ceil() as u64,remaining.ceil() as u64,Arc::new(move |_| {
                    let Some(execution)=weak.upgrade() else {return;};
                    let mut locked=manager.lock().expect("cell manager lock");
                    if locked.detach(&cell) {execution.detach();return;}
                    let live=locked.live_cells(None,Some(&cell_id));
                    let detached_count=live.iter().filter(|cell|cell.state==super::detached_cell_contract::EvalDetachedCellState::Detached).count();
                    let ids=live.into_iter().map(|cell|cell.cell_id).collect::<Vec<_>>();
                    let capacity=EvalBackgroundCapacityError::new(cap,&cell_id,foreground_start.elapsed().as_secs_f64()*1000.0,&ids);
                    let can_detach=cell.lock().expect("managed cell lock").can_detach;
                    drop(locked);
                    execution.cancel(AbortReason::new(if !can_detach && detached_count<cap {"TimeoutError"} else {"EvalBackgroundCapacityError"},if !can_detach && detached_count<cap {error.clone()} else {capacity.to_string()}));
                }));
                return;
            }
            let live=locked.live_cells(None,Some(&cell_id));
            let detached_count=live.iter().filter(|cell|cell.state==super::detached_cell_contract::EvalDetachedCellState::Detached).count();
            let ids=live.into_iter().map(|cell|cell.cell_id).collect::<Vec<_>>();
            let capacity=EvalBackgroundCapacityError::new(cap,&cell_id,foreground_start.elapsed().as_secs_f64()*1000.0,&ids);
            let can_detach=cell.lock().expect("managed cell lock").can_detach;
            drop(locked);
            execution.cancel(AbortReason::new(if !can_detach && detached_count<cap {"TimeoutError"} else {"EvalBackgroundCapacityError"},if !can_detach && detached_count<cap {error} else {capacity.to_string()}));
        }));
    }
    let mut detached = execution.detached();
    let mut acquisition_deadline = cell.lock().expect("managed cell lock").deadlines.signal();
    let acquisition = execution.wait(options.kernel_manager.get_kernel(invocation.input.language));
    tokio::pin!(acquisition);
    let acquired = tokio::select! {
        result=&mut acquisition=>result,
        changed=acquisition_deadline.changed()=>{
            let expiry=if changed.is_ok() {acquisition_deadline.borrow_and_update().clone()} else {None};
            if let Some(expiry)=expiry {cell.lock().expect("managed cell lock").record_deadline_expiry(expiry.kind);execution.cancel(AbortReason::new("TimeoutError",expiry.error));}
            acquisition.await
        }
    };
    let kernel = match acquired {
        Ok(kernel) => kernel,
        Err(error) if capacity_cancelled.load(std::sync::atomic::Ordering::SeqCst) => {
            execution.finish();
            let details = manager
                .lock()
                .expect("cell manager lock")
                .peek(&invocation.cell_id)?
                .result
                .details;
            let mut result = AgentToolResult::text(error.clone());
            result.details = details;
            let result = capacity_result(&result, 0.0);
            if let Some(callback) = &options.on_cell_settled {
                let completed_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system clock")
                    .as_secs_f64()
                    * 1000.0;
                callback(
                    super::eval_execution_event::build_eval_execution_event_payload(
                        super::eval_execution_event::BuildEvalExecutionEventOptions {
                            cell_id: &invocation.cell_id,
                            language: invocation.input.language,
                            started_at,
                            completed_at,
                            queued_ms: (completed_at - started_at).max(0.0),
                            detached: false,
                            metrics: &[],
                            tool_calls: &[],
                            state_error: None,
                            outcome:
                                super::eval_execution_event::EvalExecutionSettleOutcome::Result(
                                    &result,
                                ),
                        },
                    ),
                );
            }
            manager
                .lock()
                .expect("cell manager lock")
                .complete(&cell, result.clone());
            return Ok(result);
        }
        Err(mut error) => {
            if execution.timed_out() {
                error = super::interrupt_note::describe_timeout_state(
                    error.as_str(),
                    None::<std::future::Ready<(bool, Option<String>)>>,
                )
                .await;
            }
            execution.finish();
            manager
                .lock()
                .expect("cell manager lock")
                .fail(&cell, &error);
            emit_early_failure(&options, &invocation, started_at, &error);
            return Err(error);
        }
    };
    struct InvocationContextGuard(Option<Box<dyn FnOnce() + Send>>);
    impl Drop for InvocationContextGuard {
        fn drop(&mut self) {
            if let Some(clear) = self.0.take() {
                clear();
            }
        }
    }
    let context_guard = InvocationContextGuard(invocation.context.as_ref().and_then(|context| {
        options
            .kernel_manager
            .set_invocation_context(&invocation.cell_id, context.clone())
    }));
    let queue = kernel.queue_snapshot();
    let state = CellState {
        input: invocation.input.clone(),
        runtime: options.runtimes.get(&invocation.input.language).cloned(),
        started_at,
        run_started_at: None,
        queued_behind: Some(queue.0.into_iter().chain(queue.1).collect()),
        on_update: invocation.on_update.clone(),
        tool_calls: vec![],
        tool_call_metrics: vec![],
        status_events: vec![],
        active: true,
        output: String::new(),
        phase: None,
        error: None,
        duration_ms: 0.0,
        status: "queued".into(),
    };
    let builder = CellResultBuilder::new(
        state,
        EvalOutputOptions {
            artifact_path: options
                .artifacts_dir
                .as_ref()
                .map(|root| root.join(format!("eval-{}.log", uuid::Uuid::new_v4()))),
            head_bytes: options.settings.output_sink.head_bytes as usize,
            max_columns: options.settings.output_sink.max_columns as usize,
            provider: invocation
                .model
                .as_ref()
                .map(|model| model.provider.clone()),
            api: invocation.model.as_ref().map(|model| model.api.clone()),
            image_sdk: options.image_sdk.clone(),
        },
    );
    let reply_kernel = kernel.clone();
    let (messages_tx, mut messages) = tokio::sync::mpsc::unbounded_channel();
    let complete = options.complete.as_ref().map(|complete| {
        let complete = complete.clone();
        let context = invocation.context.clone();
        Arc::new(move |request, signal, _| complete(request, signal, context.clone()))
            as super::cell_handler::CellCompletionHandler
    });
    let mut handler = CellHandler::new(
        builder,
        CellBridgeRuntime {
            executor: options.executor.clone(),
            tools: options.list_tools.clone(),
            settings: options.settings.clone(),
            signal: bridge_signal,
            complete,
            eval_handle_host: options.eval_handle_host.clone(),
            owner_session_id: invocation.owner_session_id.clone(),
            completion_handles: options.completion_handles.clone(),
            deliver_reply: Arc::new(move |reply| {
                let _ = reply_kernel.deliver_tool_reply(reply);
            }),
        },
    );
    // Frame updates are published with the live snapshot below; `finish` always emits the final one.
    let environments = options.kernel_manager.js_environments();
    handler.set_environments(environments.clone());
    handler.builder.coalesce_updates();
    let status_tx = messages_tx.clone();
    handler.set_status_emitter(Arc::new(move |event| {
        let _ = status_tx.send(serde_json::json!({"type":"status","event":event}));
    }));
    let live = Arc::new(Mutex::new(handler.builder.live_result()));
    let live_provider = live.clone();
    let queue_kernel = kernel.clone();
    manager.lock().expect("cell manager lock").bind_kernel(
        &cell,
        Arc::new(move || live_provider.lock().expect("live result lock").clone()),
        Arc::new(move || queue_kernel.queue_snapshot()),
    );
    if invocation.input.reset == Some(true) {
        let busy = manager
            .lock()
            .expect("cell manager lock")
            .live_cells(Some(invocation.input.language), Some(&invocation.cell_id));
        if !busy.is_empty() {
            let refused = EvalKernelResetRefusedError::new(
                invocation.input.language,
                &busy
                    .into_iter()
                    .map(|cell| cell.cell_id)
                    .collect::<Vec<_>>(),
            );
            let error = refused.to_string();
            execution.finish();
            manager.lock().expect("cell manager lock").fail_with_code(
                &cell,
                &error,
                Some(refused.code),
            );
            emit_early_failure(&options, &invocation, started_at, &error);
            return Err(error);
        }
        if let Err(error) = execution.wait(kernel.reset()).await {
            execution.finish();
            manager
                .lock()
                .expect("cell manager lock")
                .fail(&cell, &error);
            emit_early_failure(&options, &invocation, started_at, &error);
            return Err(error);
        }
    }
    execution.set_kernel(kernel.clone());
    cell.lock().expect("managed cell lock").kernel = Some(kernel.clone());
    let started_manager = manager.clone();
    let started_cell = cell.clone();
    let started_tx = messages_tx.clone();
    let preludes = match options
        .active_kernel_preludes
        .as_ref()
        .map_or_else(|| Ok(Vec::new()), |f| f())
    {
        Ok(preludes) => preludes,
        Err(error) => {
            execution.finish();
            manager
                .lock()
                .expect("cell manager lock")
                .fail(&cell, &error);
            emit_early_failure(&options, &invocation, started_at, &error);
            return Err(error);
        }
    };
    let run_input = EvalKernelRunInput {
        cell_id: invocation.cell_id.clone(),
        code: invocation.input.code.clone(),
        timeout_ms: None,
        preludes,
        on_started: Some(Arc::new(move || {
            started_manager
                .lock()
                .expect("cell manager lock")
                .mark_running(&started_cell);
            let _ = started_tx.send(serde_json::json!({"type":"host-started"}));
        })),
        on_message: Some(Arc::new(move |message| {
            let _ = messages_tx.send(message.clone());
        })),
        resolve_at_start: None,
        bridge_cell_token: None,
        host_executor: super::magic_cell_host::plan_magic_cell(
            &invocation.input.code,
            environments.clone(),
        ),
        package_root: environments.map(|environment| {
            Arc::new(move || {
                environment
                    .package_root()
                    .map_err(|error| error.to_string())
            })
                as Arc<dyn Fn() -> Result<Option<std::path::PathBuf>, String> + Send + Sync>
        }),
    };
    let mut deadline = cell.lock().expect("managed cell lock").deadlines.signal();
    let (result_tx, mut result_rx) = tokio::sync::oneshot::channel();
    let work_execution = execution.clone();
    let work_manager = manager.clone();
    let work_cell = cell.clone();
    let work_options = options.clone();
    let event_cell_id = invocation.cell_id.clone();
    let event_language = invocation.input.language;
    let kernel_tools = if event_language == super::types::EvalLanguage::Js {
        kernel.clone().kernel_tools()
    } else {
        None
    };
    let steering_active = active.clone();
    let outer_kernel_tools = kernel_tools.clone();
    let failed_execution = execution.clone();
    let failed_manager = manager.clone();
    let failed_cell = cell.clone();
    let failed_options = options.clone();
    let failed_cell_id = invocation.cell_id.clone();
    let failed_language = invocation.input.language;
    let run_bound = async move {
        let _context_guard = context_guard;
        let operation = kernel.run(run_input);
        let guarded = work_execution.wait(operation);
        tokio::pin!(guarded);
        let mut bridge_calls = Vec::<super::cell_handler::PendingCellToolCall>::new();
        // Frame updates publish at most once per LIVE_REBUILD_INTERVAL (a trailing timer publishes
        // the last frames of a quiet burst), and always on host-started, tool-call completion and
        // detach; the final result comes from `finish`, not from this snapshot.
        let mut live_gate = LiveRebuildGate::new();
        let mut detach_watch = work_execution.detached();
        let mut detach_seen = false;
        let mut result = loop {
            tokio::select! {
                result=&mut guarded=>break result,
                completed=std::future::poll_fn(|cx| {
                    for index in 0..bridge_calls.len() {
                        if let std::task::Poll::Ready(result)=bridge_calls[index].as_mut().poll(cx) {
                            drop(bridge_calls.swap_remove(index));
                            return std::task::Poll::Ready(result);
                        }
                    }
                    std::task::Poll::Pending
                }),if !bridge_calls.is_empty()=>{
                    if let Err(error)=handler.finish_tool_call(completed) {work_execution.cancel(AbortReason::new("Error",error));}
                    *live.lock().expect("live result lock")=handler.builder.live_result();live_gate.rebuilt();
                },
                detached=async {detach_watch.wait_for(|detached|*detached).await.is_ok()},if !detach_seen=>{
                    detach_seen=true;
                    if detached {*live.lock().expect("live result lock")=handler.builder.live_result();live_gate.rebuilt();}
                },
                ()=tokio::time::sleep_until(live_gate.trailing()),if handler.builder.update_pending()=>{
                    handler.builder.flush_update();
                    *live.lock().expect("live result lock")=handler.builder.live_result();live_gate.rebuilt();
                },
                message=messages.recv()=>if let Some(message)=message {
                    if !active.load(std::sync::atomic::Ordering::SeqCst) {continue;}
                    if message["type"]=="host-started" {
                        handler.builder.state.run_started_at=work_cell.lock().expect("managed cell lock").source.run_started_at_ms;
                        handler.builder.state.status="running".into();handler.builder.state.queued_behind=None;handler.builder.emit_update(false);
                    } else if message["type"]=="status" && message["event"]["op"]==crate::bridge::reserved::TIMEOUT_PAUSE_OP {
                        work_execution.pause();work_manager.lock().expect("cell manager lock").pause(&work_cell);
                    } else if message["type"]=="status" && message["event"]["op"]==crate::bridge::reserved::TIMEOUT_RESUME_OP {
                        work_execution.resume();work_manager.lock().expect("cell manager lock").resume(&work_cell);
                    } else if message["type"]=="tool-call" {
                        bridge_calls.push(Box::pin(maho_ext_api::with_kernel_tools(kernel_tools.clone(), handler.begin_tool_call(&message))));
                    } else {
                        let pending=work_execution.wait(handler.handle(&message));
                        tokio::pin!(pending);
                        let handled=tokio::select! {
                            result=&mut pending=>result,
                            changed=deadline.changed()=>{
                                if changed.is_ok() && let Some(expiry)=deadline.borrow_and_update().clone() {work_cell.lock().expect("managed cell lock").record_deadline_expiry(expiry.kind);work_execution.cancel(AbortReason::new("TimeoutError",expiry.error));}
                                pending.await
                            }
                        };
                        if let Err(error)=handled {work_execution.cancel(AbortReason::new("Error",error));}
                    }
                    if live_gate.due(message["type"]=="host-started") {
                        handler.builder.flush_update();
                        *live.lock().expect("live result lock")=handler.builder.live_result();live_gate.rebuilt();
                    }
                },
                changed=deadline.changed()=>if changed.is_ok() && let Some(expiry)=deadline.borrow_and_update().clone() {work_cell.lock().expect("managed cell lock").record_deadline_expiry(expiry.kind);work_execution.cancel(AbortReason::new("TimeoutError",expiry.error));}
            }
        };
        // Kernel results can become ready together with their final output frames.
        while let Ok(message) = messages.try_recv() {
            if message["type"] == "host-started" {
                handler.builder.state.run_started_at = work_cell
                    .lock()
                    .expect("managed cell lock")
                    .source
                    .run_started_at_ms;
                handler.builder.state.status = "running".into();
                handler.builder.state.queued_behind = None;
            } else if active.load(std::sync::atomic::Ordering::SeqCst)
                && message["type"] == "tool-call"
            {
                bridge_calls.push(Box::pin(maho_ext_api::with_kernel_tools(
                    kernel_tools.clone(),
                    handler.begin_tool_call(&message),
                )));
            } else if active.load(std::sync::atomic::Ordering::SeqCst)
                && let Err(error) = work_execution.wait(handler.handle(&message)).await
            {
                work_execution.cancel(AbortReason::new("Error", error.clone()));
                result = Err(error);
            }
        }
        if result.as_ref().is_ok_and(|frame| frame["ok"] == true) {
            while !bridge_calls.is_empty() {
                let settlement = work_execution.wait(async {
                    let completed = std::future::poll_fn(|cx| {
                        for index in 0..bridge_calls.len() {
                            if let std::task::Poll::Ready(result) =
                                bridge_calls[index].as_mut().poll(cx)
                            {
                                drop(bridge_calls.swap_remove(index));
                                return std::task::Poll::Ready(result);
                            }
                        }
                        std::task::Poll::Pending
                    })
                    .await;
                    Ok(completed)
                });
                tokio::pin!(settlement);
                let completed = tokio::select! {
                    result=&mut settlement=>result,
                    changed=deadline.changed()=>{
                        if changed.is_ok() && let Some(expiry)=deadline.borrow_and_update().clone() {work_cell.lock().expect("managed cell lock").record_deadline_expiry(expiry.kind);work_execution.cancel(AbortReason::new("TimeoutError",expiry.error));}
                        settlement.await
                    }
                };
                if let Err(error) =
                    completed.and_then(|completed| handler.finish_tool_call(completed))
                {
                    work_execution.cancel(AbortReason::new("Error", error.clone()));
                    result = Err(error);
                    break;
                }
                *live.lock().expect("live result lock") = handler.builder.live_result();
            }
        }
        bridge_calls.clear();
        while let Ok(message) = messages.try_recv() {
            if active.load(std::sync::atomic::Ordering::SeqCst)
                && message["type"] == "status"
                && let Err(error) = handler.handle(&message).await
            {
                result = Err(error);
            }
        }
        if !active.load(std::sync::atomic::Ordering::SeqCst) {
            handler.builder.state.active = false;
        }
        if work_execution.timed_out()
            && let Err(error) = &mut result
        {
            let mut outcome = None;
            let pending = async {
                let Some(handle) = work_execution.wait_interrupt_handle().await else {
                    return std::future::pending().await;
                };
                let Ok(retained) = handle.state_retained.await else {
                    return std::future::pending().await;
                };
                outcome = Some((retained, handle.note.clone()));
                (retained, handle.note)
            };
            *error = super::interrupt_note::describe_timeout_state(error, Some(pending)).await;
            if let Some((retained, note)) = outcome {
                let mut cell = work_cell.lock().expect("managed cell lock");
                cell.source.state_retained = Some(retained);
                cell.source.interrupt_note = note;
            }
        }
        let settles_as_result = capacity_cancelled.load(std::sync::atomic::Ordering::SeqCst)
            || session_disposed.load(std::sync::atomic::Ordering::SeqCst);
        let final_result = match result {
            Ok(result) => handler.builder.finalize(&result).await,
            Err(error) if settles_as_result => handler.builder.finalize_cancellation(&error).await,
            Err(error) => Err(error),
        };
        let final_result = if capacity_cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            final_result.map(|result| capacity_result(&result, handler.builder.state.duration_ms))
        } else {
            final_result
        };
        handler.builder.state.active = false;
        active.store(false, std::sync::atomic::Ordering::SeqCst);
        work_execution.finish();
        let final_result = match handler.builder.flush_output().await {
            Ok(()) => final_result,
            Err(error) => Err(error),
        };
        if let Some(callback) = &work_options.on_cell_settled {
            let state = &handler.builder.state;
            let completed_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_secs_f64()
                * 1000.0;
            callback(
                super::eval_execution_event::build_eval_execution_event_payload(
                    super::eval_execution_event::BuildEvalExecutionEventOptions {
                        cell_id: &event_cell_id,
                        language: event_language,
                        started_at,
                        completed_at,
                        queued_ms: (state.run_started_at.unwrap_or(completed_at) - started_at)
                            .max(0.0),
                        detached: work_cell.lock().expect("managed cell lock").was_detached,
                        metrics: &state.tool_call_metrics,
                        tool_calls: &state.tool_calls,
                        state_error: state.error.as_deref(),
                        outcome: match &final_result {
                            Ok(result) => {
                                super::eval_execution_event::EvalExecutionSettleOutcome::Result(
                                    result,
                                )
                            }
                            Err(error) => {
                                super::eval_execution_event::EvalExecutionSettleOutcome::Error(
                                    error,
                                )
                            }
                        },
                    },
                ),
            );
        }
        match &final_result {
            Ok(result) => {
                work_manager
                    .lock()
                    .expect("cell manager lock")
                    .complete(&work_cell, result.clone());
            }
            Err(error) => {
                work_manager
                    .lock()
                    .expect("cell manager lock")
                    .fail(&work_cell, error);
            }
        }
        final_result
    };
    let worker = tokio::spawn(async move {
        match outer_kernel_tools {
            Some(tools) => {
                maho_ext_host::kernel_tools_context::with_kernel_tools(tools, run_bound).await
            }
            None => run_bound.await,
        }
    });
    // The supervisor outlives the foreground wait, including after a cell detaches.
    // A panicking host tool must not strand the managed cell or its kernel queue.
    tokio::spawn(async move {
        let result = match worker.await {
            Ok(result) => result,
            Err(error) => {
                let error = format!("Eval execution task failed: {error}");
                failed_execution.cancel(AbortReason::new("Error", &error));
                let pending = async {
                    let handle = failed_execution.wait_interrupt_handle().await?;
                    let retained = handle.state_retained.await.ok()?;
                    Some((retained, handle.note))
                };
                let outcome = tokio::time::timeout(
                    std::time::Duration::from_millis(super::interrupt_note::TIMEOUT_STATE_GRACE_MS),
                    pending,
                )
                .await
                .ok()
                .flatten();
                if let Some((retained, note)) = outcome {
                    let mut cell = failed_cell.lock().expect("managed cell lock");
                    cell.source.state_retained = Some(retained);
                    cell.source.interrupt_note = note;
                }
                failed_execution.finish();
                let detached = failed_cell.lock().expect("managed cell lock").was_detached;
                failed_manager.lock().expect("cell manager lock").fail(&failed_cell, &error);
                if let Some(callback) = &failed_options.on_cell_settled {
                    let completed_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("system clock")
                        .as_secs_f64() * 1000.0;
                    callback(super::eval_execution_event::build_eval_execution_event_payload(
                        super::eval_execution_event::BuildEvalExecutionEventOptions {
                            cell_id: &failed_cell_id,
                            language: failed_language,
                            started_at,
                            completed_at,
                            queued_ms: 0.0,
                            detached,
                            metrics: &[],
                            tool_calls: &[],
                            state_error: Some(&error),
                            outcome: super::eval_execution_event::EvalExecutionSettleOutcome::Error(&error),
                        },
                    ));
                }
                Err(error)
            }
        };
        let _ = result_tx.send(result);
    });
    let steering = invocation
        .steering_signal
        .as_ref()
        .filter(|_| detaches && !matches!(invocation.mode.as_str(), "print" | "json"));
    let mut steering_observed = false;
    loop {
        tokio::select! {
            result=&mut result_rx=>break result.map_err(|_|"Eval execution task ended without a result".to_string())?,
            changed=detached.wait_for(|detached|*detached)=>{
                changed.map_err(|_|"Eval detach signal closed".to_string())?;
                let manager=manager.lock().expect("cell manager lock");
                break Ok(result_after_detach(&manager.peek(&invocation.cell_id)?,&invocation.input,manager.live_cells(None,Some(&invocation.cell_id)).len()));
            }
            ()=async {match steering {Some(signal)=>signal.cancelled().await,None=>std::future::pending().await}},if !steering_observed=>{
                steering_observed=true;
                if !invocation.signal.aborted() && steering_active.load(std::sync::atomic::Ordering::SeqCst) && manager.lock().expect("cell manager lock").detach(&cell) {execution.detach();}
            }
        }
    }
}

/// Longest a sustained burst of kernel frames may leave the live (peek) snapshot unrebuilt.
const LIVE_REBUILD_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Time floor between frame-driven live rebuilds; `trailing` is when a pending update is due.
struct LiveRebuildGate {
    built: tokio::time::Instant,
}

impl LiveRebuildGate {
    fn new() -> Self {
        Self {
            built: tokio::time::Instant::now(),
        }
    }
    fn due(&self, host_started: bool) -> bool {
        host_started || self.built.elapsed() >= LIVE_REBUILD_INTERVAL
    }
    fn trailing(&self) -> tokio::time::Instant {
        self.built + LIVE_REBUILD_INTERVAL
    }
    fn rebuilt(&mut self) {
        self.built = tokio::time::Instant::now();
    }
}

/// senpi's `EvalBackgroundCapacityError` catch: the refused cell settles `cancelled` with the
/// machine-readable capacity code on an error result.
fn capacity_result(result: &AgentToolResult, duration_ms: f64) -> AgentToolResult {
    let mut result =
        result_for_detached_state(result, EvalDetachedCellState::Cancelled, duration_ms, None);
    result.details["isError"] = serde_json::json!(true);
    result.details["code"] = serde_json::json!(EvalBackgroundCapacityError::CODE);
    result
}

fn emit_early_failure(
    options: &CreateEvalToolOptions,
    invocation: &EvalCellInvocation,
    started_at: f64,
    error: &str,
) {
    if let Some(callback) = &options.on_cell_settled {
        let completed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_secs_f64()
            * 1000.0;
        callback(
            super::eval_execution_event::build_eval_execution_event_payload(
                super::eval_execution_event::BuildEvalExecutionEventOptions {
                    cell_id: &invocation.cell_id,
                    language: invocation.input.language,
                    started_at,
                    completed_at,
                    queued_ms: (completed_at - started_at).max(0.0),
                    detached: false,
                    metrics: &[],
                    tool_calls: &[],
                    state_error: Some(error),
                    outcome: super::eval_execution_event::EvalExecutionSettleOutcome::Error(error),
                },
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirrors the run loop's frame and trailing-timer arms: a slow stream (1 frame/ms for 200 ms)
    // must flush on the time floor, not once per frame, and still publish its last frame promptly.
    #[tokio::test(start_paused = true)]
    async fn slow_frame_stream_flushes_on_interval_and_publishes_last_frame() {
        let start = tokio::time::Instant::now();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<u32>();
        tokio::spawn(async move {
            for frame in 1..=200 {
                tokio::time::sleep_until(start + std::time::Duration::from_millis(frame.into()))
                    .await;
                if tx.send(frame).is_err() {
                    return;
                }
            }
        });
        let mut gate = LiveRebuildGate::new();
        let (mut pending, mut open, mut latest) = (false, true, 0);
        let mut flushes = Vec::<(std::time::Duration, u32)>::new();
        while open || pending {
            tokio::select! {
                frame=rx.recv(),if open=>match frame {
                    Some(frame)=>{latest=frame;pending=true;if gate.due(false) {pending=false;flushes.push((start.elapsed(),latest));gate.rebuilt();}},
                    None=>open=false,
                },
                ()=tokio::time::sleep_until(gate.trailing()),if pending=>{pending=false;flushes.push((start.elapsed(),latest));gate.rebuilt();},
            }
        }
        let last_frame_at = std::time::Duration::from_millis(200);
        assert!(flushes.len() <= 5, "{flushes:?}");
        let (at, frame) = *flushes.last().expect("a flush");
        assert_eq!(frame, 200, "{flushes:?}");
        assert!(at <= last_frame_at + LIVE_REBUILD_INTERVAL, "{flushes:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn gate_is_due_on_host_started_or_after_interval() {
        let mut gate = LiveRebuildGate::new();
        assert!(!gate.due(false));
        assert!(gate.due(true));
        tokio::time::advance(LIVE_REBUILD_INTERVAL).await;
        assert!(gate.due(false));
        gate.rebuilt();
        assert!(!gate.due(false));
        assert_eq!(
            gate.trailing(),
            tokio::time::Instant::now() + LIVE_REBUILD_INTERVAL
        );
    }
}
