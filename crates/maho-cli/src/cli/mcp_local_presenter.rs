//! Port of senpi `modes/interactive/mcp-local-presenter.ts`: with `--mcp-presenter-enforcement` the
//! terminal itself is the `local_tui` principal of its own [`McpPresenterAuthority`]. Private MCP forms
//! are FIFO-queued per binding with a deadline-preserving signal, walked through the terminal's
//! ephemeral dialogs by `collect_mrtr_form`, and settled through the authority so only a presenter
//! action is human admission. An RPC attachment never receives this handle.
use maho_ai::utils::abort::{AbortController, AbortSignal, ListenerId};
use maho_core::mcp_form_types as core_forms;
use maho_ext_api::ExtensionUi;
use maho_rpc::mcp_form_types as rpc_forms;
use maho_rpc::mcp_presenter_authority::{McpPresenterAuthority, McpPresenterConnection};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// senpi `AbortSignal.any(sources)`; the listeners it adds come off its sources when it drops.
struct AnySignal {
    controller: AbortController,
    listeners: Vec<(AbortSignal, ListenerId)>,
}
impl AnySignal {
    fn new(sources: &[AbortSignal]) -> Self {
        let controller = AbortController::new();
        let listeners = sources
            .iter()
            .map(|source| {
                let target = controller.clone();
                let id = source.add_abort_listener(move |_| target.abort(None));
                (source.clone(), id)
            })
            .collect();
        if sources.iter().any(AbortSignal::aborted) {
            controller.abort(None);
        }
        Self {
            controller,
            listeners,
        }
    }
    fn signal(&self) -> AbortSignal {
        self.controller.signal()
    }
}
impl Drop for AnySignal {
    fn drop(&mut self) {
        for (source, id) in self.listeners.drain(..) {
            source.remove_abort_listener(id);
        }
    }
}

/// The form the authority is presenting right now, and how to fail it.
struct ActiveInput {
    input: rpc_forms::McpFormInput,
    failure: AbortController,
    token: u64,
}

fn to_rpc_input(input: &core_forms::McpFormInput) -> rpc_forms::McpFormInput {
    rpc_forms::McpFormInput {
        wire: rpc_forms::McpFormInputWire {
            operation_id: input.operation_id.clone(),
            input_id: input.input_id.clone(),
            round: f64::from(input.round),
            server: serde_json::to_value(&input.server).unwrap_or(Value::Null),
            message: input.message.clone(),
            schema: input.schema.clone(),
            deadline_ms: input.deadline_ms,
        },
        operation_signal: input.operation_signal.clone(),
        operation_current: input.operation_current.clone(),
        presentation_signal: input.presentation_signal.clone(),
    }
}

fn to_core_outcome(outcome: rpc_forms::McpFormOutcome) -> core_forms::McpFormOutcome {
    core_forms::McpFormOutcome {
        kind: match outcome.kind {
            rpc_forms::McpFormOutcomeKind::Result(result) => {
                core_forms::McpFormOutcomeKind::Result(result)
            }
            rpc_forms::McpFormOutcomeKind::ProcessingFailure => {
                core_forms::McpFormOutcomeKind::ProcessingFailure
            }
            rpc_forms::McpFormOutcomeKind::OperationAborted => {
                core_forms::McpFormOutcomeKind::OperationAborted
            }
        },
        source: match outcome.source {
            rpc_forms::McpFormSource::PresenterAction => {
                core_forms::McpFormOutcomeSource::PresenterAction
            }
            rpc_forms::McpFormSource::HostTermination => {
                core_forms::McpFormOutcomeSource::HostTermination
            }
        },
    }
}

fn host_termination(kind: core_forms::McpFormOutcomeKind) -> core_forms::McpFormOutcome {
    core_forms::McpFormOutcome {
        kind,
        source: core_forms::McpFormOutcomeSource::HostTermination,
    }
}

/// The envelope fields every presenter frame echoes back.
fn envelope(frame: &Value) -> serde_json::Map<String, Value> {
    [
        "sessionId",
        "bindingEpoch",
        "attachmentEpoch",
        "grantId",
        "dialogId",
        "dialogToken",
        "formRevision",
    ]
    .into_iter()
    .filter_map(|key| frame.get(key).map(|value| (key.to_owned(), value.clone())))
    .collect()
}

/// senpi `registerLocalMcpPresenter`. `admit` runs once per presenter action (the authority's dialog
/// commit), never per primitive dialog.
pub fn register_local_mcp_presenter(
    ui: Arc<dyn ExtensionUi>,
    session_id: &str,
    lifetime: AbortSignal,
    admit: Arc<dyn Fn() + Send + Sync>,
) -> core_forms::McpFormBinding {
    let authority = McpPresenterAuthority::new(None);
    let binding = authority.bind(session_id, session_id, admit);
    let active: Arc<Mutex<Option<ActiveInput>>> = Arc::default();
    let drafts: Arc<Mutex<BTreeMap<String, AbortController>>> = Arc::default();
    let sender_cell: Arc<OnceLock<McpPresenterConnection>> = Arc::default();
    let send = {
        let (authority, active, drafts, sender_cell, lifetime) = (
            authority.clone(),
            Arc::clone(&active),
            Arc::clone(&drafts),
            Arc::clone(&sender_cell),
            lifetime.clone(),
        );
        Arc::new(move |frame: &Value| -> bool {
            let dialog_id = frame["dialogId"].as_str().unwrap_or_default().to_owned();
            if frame["type"] != "mcp_form_request" {
                if let Some(draft) = lock(&drafts).remove(&dialog_id) {
                    draft.abort(None);
                }
                return true;
            }
            let Some((input, failure)) = lock(&active)
                .as_ref()
                .map(|active| (active.input.clone(), active.failure.clone()))
            else {
                return false;
            };
            let Some(sender) = sender_cell.get().cloned() else {
                return false;
            };
            let draft = AbortController::new();
            lock(&drafts).insert(dialog_id.clone(), draft.clone());
            let envelope = envelope(frame);
            let label = format!(
                "{} ({})",
                frame["server"]["label"].as_str().unwrap_or_default(),
                frame["server"]["source"].as_str().unwrap_or_default()
            );
            let message = frame["message"].as_str().unwrap_or_default().to_owned();
            let schema = frame["schema"].clone();
            let (authority, drafts, ui, lifetime) = (
                authority.clone(),
                Arc::clone(&drafts),
                Arc::clone(&ui),
                lifetime.clone(),
            );
            tokio::spawn(async move {
                let signal = AnySignal::new(&[
                    input.operation_signal.clone(),
                    input.presentation_signal.clone(),
                    lifetime,
                    draft.signal(),
                ]);
                let presented = {
                    let (authority, sender, mut record) =
                        (authority.clone(), sender.clone(), envelope.clone());
                    record.insert("type".into(), "mcp_form_presented".into());
                    let record = Value::Object(record);
                    Arc::new(move || drop(authority.receive(&sender, &record)))
                };
                let presentation = maho_core::mcp_mrtr_presentations::McpMrtrPresentation {
                    ui,
                    signal: signal.signal(),
                    admit: None,
                    presented: Some(presented),
                    private_forms: None,
                };
                // A panicking or failing collector settles its form (senpi's `() => active.fail()`).
                let collected = tokio::spawn(async move {
                    maho_ext_mcp::modern::elicitation::collect_mrtr_form(
                        &presentation,
                        &label,
                        &message,
                        &schema,
                        &maho_ext_api::AbortSignal::default(),
                    )
                    .await
                })
                .await;
                match collected {
                    Ok(Ok(result)) if !signal.signal().aborted() => {
                        let mut record = envelope;
                        record.insert("type".into(), "mcp_form_response".into());
                        if let Ok(Value::Object(result)) = serde_json::to_value(&result) {
                            record.extend(result);
                        }
                        drop(authority.receive(&sender, &Value::Object(record)));
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(_)) | Err(_) => failure.abort(None),
                }
                lock(&drafts).remove(&dialog_id);
            });
            true
        })
    };
    let sender = authority.connect("local_tui", send);
    drop(sender_cell.set(sender.clone()));
    authority.attach(&sender, &binding);
    let owner = authority.enroll(None);
    if let Err(reason) = owner
        .register(&binding, &sender, rpc_forms::McpPresenterKind::LocalTui)
        .and_then(|registration| owner.acquire(&registration))
    {
        // A fresh authority with this connection attached cannot refuse; if it ever does, the
        // binding captures no presentation and private forms fail closed.
        maho_ext_api::diagnostic(&format!("local MCP presenter was not granted: {reason}"));
    }
    let dispose: Arc<dyn Fn() + Send + Sync> = {
        let (authority, drafts) = (authority.clone(), Arc::clone(&drafts));
        Arc::new(move || {
            authority.dispose();
            let drafts = std::mem::take(&mut *lock(&drafts));
            for draft in drafts.into_values() {
                draft.abort(None);
            }
        })
    };
    let on_lifetime = Arc::clone(&dispose);
    lifetime.add_abort_listener(move |_| on_lifetime());
    let next_token = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let capture_binding = Arc::clone(&binding);
    let capture_lifetime = lifetime.clone();
    let capture: Arc<dyn Fn() -> Option<core_forms::McpFormPresentation> + Send + Sync> =
        Arc::new(move || {
            let presentation = capture_binding.capture()?;
            let ready = Arc::clone(&presentation.ready);
            let signal = presentation.signal.clone();
            let (binding, active, lifetime, next_token) = (
                Arc::clone(&capture_binding),
                Arc::clone(&active),
                capture_lifetime.clone(),
                Arc::clone(&next_token),
            );
            let present = Arc::new(move |input: core_forms::McpFormInput| {
                let (presentation, binding, active, lifetime, next_token) = (
                    presentation.clone(),
                    Arc::clone(&binding),
                    Arc::clone(&active),
                    lifetime.clone(),
                    Arc::clone(&next_token),
                );
                Box::pin(async move {
                    present_queued(
                        presentation,
                        &binding,
                        &active,
                        &lifetime,
                        &next_token,
                        input,
                    )
                    .await
                }) as maho_ai::types::BoxFuture<'static, core_forms::McpFormOutcome>
            });
            Some(core_forms::McpFormPresentation {
                signal,
                ready,
                present,
                synchronize: None,
            })
        });
    let forms = core_forms::McpFormBinding {
        epoch: binding.epoch.clone(),
        signal: binding.signal.clone(),
        capture,
        dispose: Arc::clone(&dispose),
        version: None,
        subscribe: None,
    };
    if lifetime.aborted() {
        dispose();
    }
    forms
}

/// The private-forms `present`: one form at a time for this binding, then the authority's dialog
/// with the remaining deadline.
async fn present_queued(
    presentation: rpc_forms::McpFormPresentation,
    binding: &Arc<rpc_forms::McpFormBinding>,
    active: &Arc<Mutex<Option<ActiveInput>>>,
    lifetime: &AbortSignal,
    next_token: &std::sync::atomic::AtomicU64,
    input: core_forms::McpFormInput,
) -> core_forms::McpFormOutcome {
    let deadline_ms = input.deadline_ms.max(0.);
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs_f64(deadline_ms / 1000.);
    let timeout = AbortController::new();
    let expired = timeout.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep_until(deadline).await;
        expired.abort(None);
    });
    let signal = AnySignal::new(&[
        input.operation_signal.clone(),
        input.presentation_signal.clone(),
        presentation.signal.clone(),
        lifetime.clone(),
        timeout.signal(),
    ]);
    let queue_signal =
        maho_ext_mcp::wrap::McpLinkedSignal::with_host(Vec::new(), vec![signal.signal()]);
    let rpc_input = to_rpc_input(&input);
    let queued = maho_ext_mcp::modern::consent_queue::queued_form(
        usize::try_from(binding.id).unwrap_or(usize::MAX),
        &queue_signal.signal,
        || async {
            let failure = AbortController::new();
            let presentation_signal = AnySignal::new(&[signal.signal(), failure.signal()]);
            let mut current = rpc_input.clone();
            current.presentation_signal = presentation_signal.signal();
            current.wire.deadline_ms = deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .as_secs_f64()
                * 1000.;
            let token = next_token.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *lock(active) = Some(ActiveInput {
                input: current.clone(),
                failure,
                token,
            });
            let outcome = (presentation.present)(current).await;
            let mut slot = lock(active);
            if slot.as_ref().is_some_and(|active| active.token == token) {
                *slot = None;
            }
            Ok::<_, maho_ext_mcp::modern::errors::McpMrtrError>(outcome)
        },
    )
    .await;
    timer.abort();
    match queued {
        Ok(outcome) => to_core_outcome(outcome),
        Err(_) => host_termination(
            if input.operation_signal.aborted()
                || binding.signal.aborted()
                || input
                    .operation_current
                    .as_ref()
                    .is_some_and(|current| !current())
            {
                core_forms::McpFormOutcomeKind::OperationAborted
            } else {
                core_forms::McpFormOutcomeKind::ProcessingFailure
            },
        ),
    }
}
