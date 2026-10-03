use std::sync::{Arc,Mutex};
use maho_ai::utils::abort::{AbortController,AbortReason};
use maho_ext_api::AgentToolResult;
use super::{eval_tool_options::{CreateEvalToolOptions,EvalCellInvocation},types::{EvalKernelRunInput,TimeoutBehavior},cell_execution::CellExecution,cell_runtime::{CellState,CellResultBuilder},cell_handler::{CellHandler,CellBridgeRuntime},image::EvalOutputOptions,eval_request::eval_timeout_behavior,detached_eval_result::{result_after_detach,EvalBackgroundCapacityError},eval_kernel_reset_refused_error::EvalKernelResetRefusedError};

pub async fn run_eval_cell(options:Arc<CreateEvalToolOptions>,invocation:EvalCellInvocation) -> Result<AgentToolResult,String> {
    invocation.signal.throw_if_aborted().map_err(|error|error.to_string())?;
    let detaches=eval_timeout_behavior(&invocation.input,&invocation.mode)==TimeoutBehavior::Detach;
    let manager=options.cell_manager.clone();
    let cell=manager.lock().expect("cell manager lock").create(invocation.cell_id.clone(),invocation.input.clone())?;
    let started_at=cell.lock().expect("managed cell lock").source.started_at_ms;
    let controller=AbortController::new();
    let bridge_signal=controller.signal();
    let active=Arc::new(std::sync::atomic::AtomicBool::new(true));
    let active_abort=active.clone();
    let execution=CellExecution::new(invocation.signal.clone(),invocation.cell_id.clone(),None,Arc::new(move |reason| {
        active_abort.store(false,std::sync::atomic::Ordering::SeqCst);
        controller.abort(Some(reason));
    }));
    let weak=Arc::downgrade(&execution);
    cell.lock().expect("managed cell lock").on_kill=Some(Arc::new(move |error|{if let Some(execution)=weak.upgrade() {execution.cancel(AbortReason::new("Error",error));}}));
    if detaches {
        let manager=manager.clone();let cell=cell.clone();let weak=Arc::downgrade(&execution);
        let cap=manager.lock().expect("cell manager lock").max_detached_cells();
        let cell_id=invocation.cell_id.clone();
        let foreground_ms=options.settings.foreground_window_seconds*1000.0;
        let foreground_start=tokio::time::Instant::now();
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
    let mut detached=execution.detached();
    let mut acquisition_deadline=cell.lock().expect("managed cell lock").deadlines.signal();
    let acquisition=execution.wait(options.kernel_manager.get_kernel(invocation.input.language));
    tokio::pin!(acquisition);
    let acquired=tokio::select! {
        result=&mut acquisition=>result,
        changed=acquisition_deadline.changed()=>{
            let expiry=if changed.is_ok() {acquisition_deadline.borrow_and_update().clone()} else {None};
            if let Some(expiry)=expiry {cell.lock().expect("managed cell lock").record_deadline_expiry(expiry.kind);execution.cancel(AbortReason::new("TimeoutError",expiry.error));}
            acquisition.await
        }
    };
    let kernel=match acquired {
        Ok(kernel)=>kernel,
        Err(error)=>{execution.finish();manager.lock().expect("cell manager lock").fail(&cell,&error);return Err(error);}
    };
    let queue=kernel.queue_snapshot();
    let state=CellState {input:invocation.input.clone(),runtime:options.runtimes.get(&invocation.input.language).cloned(),started_at,run_started_at:None,queued_behind:Some(queue.0.into_iter().chain(queue.1).collect()),on_update:invocation.on_update,tool_calls:vec![],tool_call_metrics:vec![],status_events:vec![],active:true,output:String::new(),phase:None,error:None,duration_ms:0.0,status:"queued".into()};
    let builder=CellResultBuilder::new(state,EvalOutputOptions {artifact_path:options.artifacts_dir.as_ref().map(|root|root.join(format!("eval-{}.log",uuid::Uuid::new_v4()))),head_bytes:options.settings.output_sink.head_bytes as usize,max_columns:options.settings.output_sink.max_columns as usize,provider:None,api:None,image_sdk:options.image_sdk.clone()});
    let reply_kernel=kernel.clone();
    let (messages_tx,mut messages)=tokio::sync::mpsc::unbounded_channel();
    let mut handler=CellHandler::new(builder,CellBridgeRuntime {executor:options.executor.clone(),tools:options.list_tools.clone(),settings:options.settings.clone(),signal:bridge_signal,complete:options.complete.clone(),deliver_reply:Arc::new(move |reply|{let _=reply_kernel.deliver_tool_reply(reply);})});
    let status_tx=messages_tx.clone();
    handler.set_status_emitter(Arc::new(move |event|{let _=status_tx.send(serde_json::json!({"type":"status","event":event}));}));
    let live=Arc::new(Mutex::new(handler.builder.live_result()));
    let live_provider=live.clone();let queue_kernel=kernel.clone();
    manager.lock().expect("cell manager lock").bind_kernel(&cell,Arc::new(move ||live_provider.lock().expect("live result lock").clone()),Arc::new(move ||queue_kernel.queue_snapshot()));
    if invocation.input.reset==Some(true) {
        let busy=manager.lock().expect("cell manager lock").live_cells(Some(invocation.input.language),Some(&invocation.cell_id));
        if !busy.is_empty() {
            let error=EvalKernelResetRefusedError::new(invocation.input.language,&busy.into_iter().map(|cell|cell.cell_id).collect::<Vec<_>>()).to_string();
            execution.finish();manager.lock().expect("cell manager lock").fail(&cell,&error);return Err(error);
        }
        if let Err(error)=execution.wait(kernel.reset()).await {execution.finish();manager.lock().expect("cell manager lock").fail(&cell,&error);return Err(error);}
    }
    execution.set_kernel(kernel.clone());
    cell.lock().expect("managed cell lock").kernel=Some(kernel.clone());
    let started_manager=manager.clone();let started_cell=cell.clone();let started_tx=messages_tx.clone();
    let run_input=EvalKernelRunInput {cell_id:invocation.cell_id.clone(),code:invocation.input.code.clone(),timeout_ms:None,on_started:Some(Arc::new(move ||{
        started_manager.lock().expect("cell manager lock").mark_running(&started_cell);
        let _=started_tx.send(serde_json::json!({"type":"host-started"}));
    })),on_message:Some(Arc::new(move |message|{let _=messages_tx.send(message.clone());}))};
    let mut deadline=cell.lock().expect("managed cell lock").deadlines.signal();
    let (result_tx,mut result_rx)=tokio::sync::oneshot::channel();
    let work_execution=execution.clone();let work_manager=manager.clone();let work_cell=cell.clone();
    let work_options=options.clone();
    let event_cell_id=invocation.cell_id.clone();
    let event_language=invocation.input.language;
    let kernel_tools=if event_language==super::types::EvalLanguage::Js {kernel.clone().kernel_tools()} else {None};
    let run_bound=async move {
        let operation=kernel.run(run_input);
        let guarded=work_execution.wait(operation);
        tokio::pin!(guarded);
        let mut bridge_calls=Vec::<super::cell_handler::PendingCellToolCall>::new();
        let mut result=loop {
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
                    *live.lock().expect("live result lock")=handler.builder.live_result();
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
                        bridge_calls.push(handler.begin_tool_call(&message));
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
                    *live.lock().expect("live result lock")=handler.builder.live_result();
                },
                changed=deadline.changed()=>if changed.is_ok() && let Some(expiry)=deadline.borrow_and_update().clone() {work_cell.lock().expect("managed cell lock").record_deadline_expiry(expiry.kind);work_execution.cancel(AbortReason::new("TimeoutError",expiry.error));}
            }
        };
        // Kernel results can become ready together with their final output frames.
        while let Ok(message)=messages.try_recv() {
            if message["type"]=="host-started" {
                handler.builder.state.run_started_at=work_cell.lock().expect("managed cell lock").source.run_started_at_ms;
                handler.builder.state.status="running".into();handler.builder.state.queued_behind=None;
            } else if active.load(std::sync::atomic::Ordering::SeqCst) && message["type"]=="tool-call" {
                bridge_calls.push(handler.begin_tool_call(&message));
            } else if active.load(std::sync::atomic::Ordering::SeqCst) && let Err(error)=work_execution.wait(handler.handle(&message)).await {
                work_execution.cancel(AbortReason::new("Error",error.clone()));
                result=Err(error);
            }
        }
        if result.as_ref().is_ok_and(|frame|frame["ok"]==true) {
            while !bridge_calls.is_empty() {
                let settlement=work_execution.wait(async {
                    let completed=std::future::poll_fn(|cx| {
                        for index in 0..bridge_calls.len() {
                            if let std::task::Poll::Ready(result)=bridge_calls[index].as_mut().poll(cx) {
                                drop(bridge_calls.swap_remove(index));
                                return std::task::Poll::Ready(result);
                            }
                        }
                        std::task::Poll::Pending
                    }).await;
                    Ok(completed)
                });
                tokio::pin!(settlement);
                let completed=tokio::select! {
                    result=&mut settlement=>result,
                    changed=deadline.changed()=>{
                        if changed.is_ok() && let Some(expiry)=deadline.borrow_and_update().clone() {work_cell.lock().expect("managed cell lock").record_deadline_expiry(expiry.kind);work_execution.cancel(AbortReason::new("TimeoutError",expiry.error));}
                        settlement.await
                    }
                };
                if let Err(error)=completed.and_then(|completed|handler.finish_tool_call(completed)) {
                    work_execution.cancel(AbortReason::new("Error",error.clone()));
                    result=Err(error);break;
                }
                *live.lock().expect("live result lock")=handler.builder.live_result();
            }
        }
        bridge_calls.clear();
        while let Ok(message)=messages.try_recv() {
            if active.load(std::sync::atomic::Ordering::SeqCst) && message["type"]=="status" && let Err(error)=handler.handle(&message).await {result=Err(error);}
        }
        if !active.load(std::sync::atomic::Ordering::SeqCst) {handler.builder.state.active=false;}
        let final_result=match result {Ok(result)=>handler.builder.finalize(&result).await,Err(error)=>handler.builder.finalize_cancellation(&error).await};
        handler.builder.state.active=false;
        active.store(false,std::sync::atomic::Ordering::SeqCst);
        work_execution.finish();
        let final_result=match handler.builder.flush_output().await {Ok(())=>final_result,Err(error)=>Err(error)};
        if let Some(callback)=&work_options.on_cell_settled {
            let state=&handler.builder.state;
            let completed_at=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock").as_secs_f64()*1000.0;
            callback(super::eval_execution_event::build_eval_execution_event_payload(super::eval_execution_event::BuildEvalExecutionEventOptions {
                cell_id:&event_cell_id,language:event_language,started_at,completed_at,queued_ms:(state.run_started_at.unwrap_or(completed_at)-started_at).max(0.0),detached:work_cell.lock().expect("managed cell lock").was_detached,metrics:&state.tool_call_metrics,tool_calls:&state.tool_calls,state_error:state.error.as_deref(),outcome:match &final_result {Ok(result)=>super::eval_execution_event::EvalExecutionSettleOutcome::Result(result),Err(error)=>super::eval_execution_event::EvalExecutionSettleOutcome::Error(error)},
            }));
        }
        match &final_result {Ok(result)=>{work_manager.lock().expect("cell manager lock").complete(&work_cell,result.clone());},Err(error)=>{work_manager.lock().expect("cell manager lock").fail(&work_cell,error);}}
        let _=result_tx.send(final_result);
    };
    tokio::spawn(async move {
        match kernel_tools {
            Some(tools)=>maho_ext_host::kernel_tools_context::with_kernel_tools(tools,run_bound).await,
            None=>run_bound.await,
        }
    });
    tokio::select! {
        result=&mut result_rx=>result.map_err(|_|"Eval execution task ended without a result".to_string())?,
        changed=detached.wait_for(|detached|*detached)=>{
            changed.map_err(|_|"Eval detach signal closed".to_string())?;
            let manager=manager.lock().expect("cell manager lock");
            Ok(result_after_detach(&manager.peek(&invocation.cell_id)?,&invocation.input,manager.live_cells(None,Some(&invocation.cell_id)).len()))
        }
    }
}
