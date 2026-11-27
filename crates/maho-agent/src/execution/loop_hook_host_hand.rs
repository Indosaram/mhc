//! Port of senpi `packages/agent/src/execution/loop-hook-host-hand.ts`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use maho_ai::types::BoxFuture;
use serde_json::Value;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::transport::transport::{FrameHandler, Transport, TransportFrame};

use super::loop_hook_host_protocol::{
    LoopHookMessage, decode_loop_hook_message, encode_loop_hook_message,
};

pub type LoopHookRunner =
    Arc<dyn Fn(String, Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopHookPoint {
    Received,
    Started,
    Ran,
    Replied,
}

pub type LoopHookPointCallback =
    Arc<dyn Fn(LoopHookPoint, &str) -> Result<(), String> + Send + Sync>;
pub type LoopHookFaultCallback = Arc<dyn Fn(String) + Send + Sync>;

pub struct LoopHookHandOptions {
    pub transport: Arc<dyn Transport>,
    pub lane: String,
    pub run: LoopHookRunner,
    pub instance: Option<String>,
    pub on_point: Option<LoopHookPointCallback>,
    pub on_fault: Option<LoopHookFaultCallback>,
}

pub struct LoopHookHand {
    dead: Arc<AtomicBool>,
    ready_done: Arc<AtomicBool>,
    ready_notify: Arc<Notify>,
}

impl LoopHookHand {
    pub async fn ready(&self) {
        loop {
            let notified = self.ready_notify.notified();
            if self.ready_done.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    pub fn close(&self) {
        self.dead.store(true, Ordering::SeqCst);
    }
}

pub fn serve_loop_hooks(options: LoopHookHandOptions) -> Arc<LoopHookHand> {
    let instance = options
        .instance
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let answers: Arc<Mutex<HashMap<String, Option<LoopHookMessage>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let dead = Arc::new(AtomicBool::new(false));
    let ready_done = Arc::new(AtomicBool::new(false));
    let ready_notify = Arc::new(Notify::new());

    let transport = options.transport;
    let lane = options.lane;
    let run = options.run;
    let on_point = options.on_point;
    let on_fault = options.on_fault;

    let transport_frame = Arc::clone(&transport);
    let lane_frame = lane.clone();
    let dead_frame = Arc::clone(&dead);
    let answers_frame = Arc::clone(&answers);
    let instance_frame = instance.clone();
    let on_point_frame = on_point;
    let on_fault_frame = on_fault;

    let handler: FrameHandler = Arc::new(move |frame: TransportFrame| {
        let transport = Arc::clone(&transport_frame);
        let lane = lane_frame.clone();
        let dead = Arc::clone(&dead_frame);
        let answers = Arc::clone(&answers_frame);
        let instance = instance_frame.clone();
        let run = Arc::clone(&run);
        let on_point = on_point_frame.clone();
        let on_fault = on_fault_frame.clone();

        Box::pin(async move {
            if dead.load(Ordering::SeqCst) {
                return;
            }
            let Some(LoopHookMessage::Call {
                call_id,
                event,
                payload,
            }) = decode_loop_hook_message(&frame.payload)
            else {
                return;
            };

            let point = |at: LoopHookPoint, cid: &str| -> Result<(), String> {
                if dead.load(Ordering::SeqCst) {
                    return Err("this hand process was stopped".to_owned());
                }
                if let Some(hook) = &on_point
                    && let Err(err) = hook(at, cid)
                {
                    dead.store(true, Ordering::SeqCst);
                    return Err(err);
                }
                Ok(())
            };

            let res: Result<(), String> = async {
                let existing = {
                    let guard = answers.lock().unwrap_or_else(|p| p.into_inner());
                    guard.get(&call_id).cloned()
                };
                if let Some(opt_answer) = existing {
                    if let Some(answer) = opt_answer {
                        transport
                            .send(&lane, encode_loop_hook_message(&answer))
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                    return Ok(());
                }

                point(LoopHookPoint::Received, &call_id)?;
                answers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(call_id.clone(), None);
                transport
                    .send(
                        &lane,
                        encode_loop_hook_message(&LoopHookMessage::Started {
                            call_id: call_id.clone(),
                            instance: instance.clone(),
                        }),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                point(LoopHookPoint::Started, &call_id)?;

                let answer = match run(event, payload).await {
                    Ok(result) => LoopHookMessage::Reply {
                        call_id: call_id.clone(),
                        result,
                    },
                    Err(error) => LoopHookMessage::Failed {
                        call_id: call_id.clone(),
                        error,
                    },
                };
                point(LoopHookPoint::Ran, &call_id)?;
                answers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(call_id.clone(), Some(answer.clone()));
                transport
                    .send(&lane, encode_loop_hook_message(&answer))
                    .await
                    .map_err(|e| e.to_string())?;
                point(LoopHookPoint::Replied, &call_id)?;
                Ok(())
            }
            .await;

            if let Err(err) = res
                && let Some(fault) = &on_fault
            {
                fault(err);
            }
        })
    });

    transport.on_frame(handler);

    let ready_done_spawn = Arc::clone(&ready_done);
    let ready_notify_spawn = Arc::clone(&ready_notify);
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        rt.spawn(async move {
            let _ = transport
                .send(
                    &lane,
                    encode_loop_hook_message(&LoopHookMessage::Hello { instance }),
                )
                .await;
            ready_done_spawn.store(true, Ordering::SeqCst);
            ready_notify_spawn.notify_waiters();
        });
    } else {
        ready_done.store(true, Ordering::SeqCst);
        ready_notify.notify_waiters();
    }

    Arc::new(LoopHookHand {
        dead,
        ready_done,
        ready_notify,
    })
}
