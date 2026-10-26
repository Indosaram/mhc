//! The transaction helpers one extension op may call.
//!
//! Upstream counterpart: `thread/gateway/extension-transaction.ts`. A helper joined to the op's
//! transaction holds its savepoint; a statement run while one is in flight is refused, because it
//! would land inside that savepoint and vanish with a data refusal's rollback.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::Value;

use super::store_extensions::{
    ExtensionAckReply, ExtensionAckRequest, ExtensionBindReply, ExtensionBindRequest, ExtensionBindingLookup, ExtensionEnqueueRequest, ExtensionOutboxRequest, ExtensionRebindReply,
    ExtensionRebindRequest, ExtensionUnbindReply, ExtensionUnbindRequest, StoreExtensionError, StoreExtensionTransaction,
};
use super::store_ops::{SqlAccess, StoreContext, SqlRow, v_str};
use super::store_relay_ops;
use super::types::{EnvelopeOrigin, GatewayDeliveryResult, Provenance};

/// A transaction helper called with arguments it cannot serve; the op fails as `invalid_arguments`.
pub fn argument_error(message: impl Into<String>) -> StoreExtensionError {
    StoreExtensionError::invalid_arguments(message)
}

/// The transaction ended (the op returned, or its budget ran out).
pub fn ended_error(extension: &str) -> StoreExtensionError {
    StoreExtensionError::failed(format!("The {extension} extension transaction has ended."))
}

/// A statement ran while a transaction helper was still in flight.
pub fn busy_error(extension: &str) -> StoreExtensionError {
    StoreExtensionError::failed(format!(
        "The {extension} extension ran a statement while a tx helper (enqueue, bind, ...) was still in flight; await the helper first."
    ))
}

/// Rejects a statement that carries more than one statement, so an extension cannot smuggle a second
/// one past the schema checks.
pub fn single_extension_statement(statement: &str) -> Result<(), StoreExtensionError> {
    let mut in_single = false;
    let mut in_double = false;
    let mut terminators = 0;
    for character in statement.chars() {
        match character {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ';' if !in_single && !in_double => terminators += 1,
            _ => {}
        }
    }
    let trimmed = statement.trim_end();
    let trailing = trimmed.ends_with(';');
    if terminators > usize::from(trailing) {
        return Err(StoreExtensionError { code: super::store_extensions::StoreExtensionRefusalCode::ExtensionSchemaViolation, message: "an extension statement must be exactly one SQL statement.".to_owned() });
    }
    Ok(())
}

/// The sender-owned provenance an extension enqueue records.
pub type SenderProvenance = Arc<dyn Fn() -> Provenance + Send + Sync>;

struct Guard {
    active: AtomicBool,
    running: AtomicUsize,
}

/// Runs one transaction helper: refused once the transaction ended, and counted as in flight from
/// the call until it settles.
fn run_joined<T>(guard: &Guard, owner: &str, body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    if !guard.active.load(Ordering::SeqCst) {
        return Err(ended_error(owner).message);
    }
    guard.running.fetch_add(1, Ordering::SeqCst);
    let result = body();
    guard.running.fetch_sub(1, Ordering::SeqCst);
    result
}

/// Builds the helpers of one op's transaction; `finish` marks the transaction ended.
pub struct ExtensionTransaction<'a> {
    pub tx: StoreExtensionTransaction<'a>,
    guard: Arc<Guard>,
}

impl ExtensionTransaction<'_> {
    pub fn finish(&self) {
        self.guard.active.store(false, Ordering::SeqCst);
    }
}

pub fn extension_transaction<'a>(ctx: &'a StoreContext, name: &str, now: i64, sender_provenance: Option<SenderProvenance>) -> ExtensionTransaction<'a> {
    let guard = Arc::new(Guard { active: AtomicBool::new(true), running: AtomicUsize::new(0) });
    let owner = name.to_owned();

    let sql_guard = {
        let guard = guard.clone();
        let owner = owner.clone();
        move || -> Result<(), StoreExtensionError> {
            if !guard.active.load(Ordering::SeqCst) {
                return Err(ended_error(&owner));
            }
            if guard.running.load(Ordering::SeqCst) > 0 {
                return Err(busy_error(&owner));
            }
            Ok(())
        }
    };

    let all = {
        let sql_guard = sql_guard.clone();
        std::rc::Rc::new(move |columns: &[String], statement: &str, params: &[Value], order_by: Option<&str>| -> Result<Vec<SqlRow>, String> {
            sql_guard().map_err(|error| error.message)?;
            single_extension_statement(statement).map_err(|error| error.message)?;
            for column in columns {
                single_extension_statement(column).map_err(|error| error.message)?;
            }
            let order = order_by.map(|order| format!(" ORDER BY {order}")).unwrap_or_default();
            let sql = format!("{statement}{order}");
            ctx.all(&sql, &values_to_params(params)).map_err(|error| error.message())
        }) as std::rc::Rc<dyn Fn(&[String], &str, &[Value], Option<&str>) -> Result<Vec<SqlRow>, String> + 'a>
    };
    let one = {
        let sql_guard = sql_guard.clone();
        std::rc::Rc::new(move |columns: &[String], statement: &str, params: &[Value]| -> Result<Option<SqlRow>, String> {
            sql_guard().map_err(|error| error.message)?;
            single_extension_statement(statement).map_err(|error| error.message)?;
            for column in columns {
                single_extension_statement(column).map_err(|error| error.message)?;
            }
            ctx.one(statement, &values_to_params(params)).map_err(|error| error.message())
        }) as std::rc::Rc<dyn Fn(&[String], &str, &[Value]) -> Result<Option<SqlRow>, String> + 'a>
    };
    let exec = {
        let sql_guard = sql_guard.clone();
        std::rc::Rc::new(move |statement: &str, params: &[Value]| -> Result<usize, String> {
            sql_guard().map_err(|error| error.message)?;
            single_extension_statement(statement).map_err(|error| error.message)?;
            ctx.run(statement, &values_to_params(params)).map_err(|error| error.message())
        }) as std::rc::Rc<dyn Fn(&str, &[Value]) -> Result<usize, String> + 'a>
    };

    // Every helper joins the op's transaction through the same guard: in flight from the call until it
    // settles, so a statement run in that gap is refused instead of landing inside its savepoint.

    let enqueue = {
        let guard = guard.clone();
        let owner = owner.clone();
        let provenance = sender_provenance.clone();
        std::rc::Rc::new(move |request: ExtensionEnqueueRequest| -> Result<GatewayDeliveryResult, String> {
            let value = run_joined(&guard, &owner, || {
                let binding = store_relay_ops::select_bindings(ctx, "binding_id = ?", &[v_str(&request.binding_id)])
                    .map_err(|error| error.message())?
                    .into_iter()
                    .next()
                    .ok_or_else(|| "No binding has this id.".to_owned())?;
                let delivery_id = super::store_ops::new_id("ext");
                let provenance = provenance
                    .as_ref()
                    .map(|provenance| provenance())
                    .unwrap_or(Provenance { workspace: "unclassified".to_owned(), project: "extension".to_owned(), lane: None, session: None });
                let enqueue_request = super::types::EnqueueRequest {
                    provenance,
                    now,
                    delivery_id: delivery_id.clone(),
                    target_durable_id: binding.session_durable_id.clone(),
                    sender_principal: format!("binding:{}", binding.binding_id),
                    rate_principal: None,
                    sender_node: format!("binding:{}", binding.binding_id),
                    sender_turn: None,
                    cause_delivery_id: None,
                    claimed_root_id: None,
                    origin: EnvelopeOrigin::External {
                        external: super::types::ExternalOrigin {
                            platform: binding.platform.clone(),
                            account_id: binding.account_id.clone(),
                            chat_id: binding.chat_id.clone(),
                            thread_id: binding.thread_id.clone(),
                            message_id: request.event_id.clone(),
                            author: None,
                        },
                    },
                    actor: binding.account_id.clone(),
                    body: request.text.clone(),
                    mode: super::types::GatewayDeliveryMode::parse(&request.mode).unwrap_or(super::types::GatewayDeliveryMode::Auto),
                    expected_turn_id: None,
                    binding: Some(super::types::BindingRef { binding_id: binding.binding_id.clone(), revision: binding.revision }),
                    receipt: super::types::ReceiptRef { idempotency_key: format!("event:{}", request.event_id), args_hash: store_relay_ops::hash_args(&Value::String(request.text.clone())) },
                    endpoint_kind: None,
                };
                let outcome = super::store_ops::enqueue(ctx, &enqueue_request).map_err(|error| error.message())?;
                match outcome {
                    super::types::EnqueueOutcome::Inserted { row, queue_position } => {
                        Ok(serde_json::to_value(super::result::result_from_row(&row, queue_position, None, false, true)).unwrap_or(Value::Null))
                    }
                    super::types::EnqueueOutcome::Replay { result } => Ok(result),
                    super::types::EnqueueOutcome::Refused { message, .. } => Err(message),
                    super::types::EnqueueOutcome::Busy => Err("the gateway store is locked".to_owned()),
                }
            });
            serde_json::from_value::<GatewayDeliveryResult>(value?).map_err(|error| error.to_string())
        }) as std::rc::Rc<dyn Fn(ExtensionEnqueueRequest) -> Result<GatewayDeliveryResult, String> + 'a>
    };

    let outbox_ack = {
        let guard = guard.clone();
        let owner = owner.clone();
        std::rc::Rc::new(move |request: ExtensionAckRequest| -> Result<ExtensionAckReply, String> {

            run_joined(&guard, &owner, || {
                let outcome = store_relay_ops::ack_outbox(ctx, now, &request.binding_id, request.cursor, request.provider_message_id.as_deref()).map_err(|error| error.message())?;
                match outcome {
                    store_relay_ops::RelayOutcome::Ok { value: (acked_cursor, changed) } => {
                        Ok(ExtensionAckReply { binding_id: request.binding_id.clone(), acked_cursor, changed })
                    }
                    store_relay_ops::RelayOutcome::Refused { message, .. } => Err(message),
                }
            })
        }) as std::rc::Rc<dyn Fn(ExtensionAckRequest) -> Result<ExtensionAckReply, String> + 'a>
    };

    let bind = {
        let guard = guard.clone();
        let owner = owner.clone();
        std::rc::Rc::new(move |request: ExtensionBindRequest| -> Result<ExtensionBindReply, String> {

            run_joined(&guard, &owner, || {
                let input: store_relay_ops::BindInput = serde_json::from_value(request.input.clone()).map_err(|error| error.to_string())?;
                match store_relay_ops::bind_thread(ctx, now, &input).map_err(|error| error.message())? {
                    store_relay_ops::RelayOutcome::Ok { value } => Ok(ExtensionBindReply { binding: serde_json::to_value(&value).unwrap_or(Value::Null), deduplicated: false }),
                    store_relay_ops::RelayOutcome::Refused { message, .. } => Err(message),
                }
            })
        }) as std::rc::Rc<dyn Fn(ExtensionBindRequest) -> Result<ExtensionBindReply, String> + 'a>
    };

    let unbind = {
        let guard = guard.clone();
        let owner = owner.clone();
        std::rc::Rc::new(move |request: ExtensionUnbindRequest| -> Result<ExtensionUnbindReply, String> {

            run_joined(&guard, &owner, || {
                match store_relay_ops::unbind_thread(ctx, now, &request.binding_id, request.expected_revision).map_err(|error| error.message())? {
                    store_relay_ops::RelayOutcome::Ok { value: (binding, already_closed, in_flight) } => Ok(ExtensionUnbindReply {
                        binding: serde_json::to_value(&binding).unwrap_or(Value::Null),
                        already_closed,
                        in_flight,
                    }),
                    store_relay_ops::RelayOutcome::Refused { message, .. } => Err(message),
                }
            })
        }) as std::rc::Rc<dyn Fn(ExtensionUnbindRequest) -> Result<ExtensionUnbindReply, String> + 'a>
    };

    let rebind = {
        let guard = guard.clone();
        let owner = owner.clone();
        std::rc::Rc::new(move |request: ExtensionRebindRequest| -> Result<ExtensionRebindReply, String> {

            run_joined(&guard, &owner, || {
                match store_relay_ops::rebind_thread(ctx, now, &request.binding_id, request.expected_revision, &request.session_durable_id).map_err(|error| error.message())? {
                    store_relay_ops::RelayOutcome::Ok { value: (binding, closed) } => Ok(ExtensionRebindReply { binding: serde_json::to_value(&binding).unwrap_or(Value::Null), closed }),
                    store_relay_ops::RelayOutcome::Refused { message, .. } => Err(message),
                }
            })
        }) as std::rc::Rc<dyn Fn(ExtensionRebindRequest) -> Result<ExtensionRebindReply, String> + 'a>
    };

    let binding_for = {
        let guard = guard.clone();
        let owner = owner.clone();
        std::rc::Rc::new(move |lookup: ExtensionBindingLookup| -> Result<Option<Value>, String> {
            let value = run_joined(&guard, &owner, || store_relay_ops::binding_for(ctx, &lookup).map(|binding| binding.map(|binding| serde_json::to_value(binding).unwrap_or(Value::Null))).map_err(|error| error.message()))?;
            Ok(value)
        }) as std::rc::Rc<dyn Fn(ExtensionBindingLookup) -> Result<Option<Value>, String> + 'a>
    };

    let bindings_for_session = {
        let guard = guard.clone();
        let owner = owner.clone();
        std::rc::Rc::new(move |session_durable_id: &str| -> Result<Vec<Value>, String> {
            if session_durable_id.is_empty() {
                return Err(argument_error("bindingsForSession needs a non-empty session durable id.").message);
            }
            run_joined(&guard, &owner, || {
                let bindings = store_relay_ops::bindings_for_session(ctx, session_durable_id, now).map_err(|error| error.message())?;
                Ok(bindings.into_iter().map(|binding| serde_json::to_value(binding).unwrap_or(Value::Null)).collect())
            })
        }) as std::rc::Rc<dyn Fn(&str) -> Result<Vec<Value>, String> + 'a>
    };

    let outbox_pending = {
        let guard = guard.clone();
        let owner = owner.clone();
        std::rc::Rc::new(move |request: ExtensionOutboxRequest| -> Result<Value, String> {
            run_joined(&guard, &owner, || {
                match store_relay_ops::read_outbox(ctx, now, &request.binding_id, request.after_cursor, request.limit, true).map_err(|error| error.message())? {
                    store_relay_ops::RelayOutcome::Ok { value } => Ok(serde_json::to_value(&value).unwrap_or(Value::Null)),
                    store_relay_ops::RelayOutcome::Refused { message, .. } => Err(message),
                }
            })
        }) as std::rc::Rc<dyn Fn(ExtensionOutboxRequest) -> Result<Value, String> + 'a>
    };

    ExtensionTransaction {
        tx: StoreExtensionTransaction { all, one, exec, enqueue, outbox_ack, bind, unbind, rebind, binding_for, bindings_for_session, outbox_pending },
        guard,
    }
}

fn values_to_params(values: &[Value]) -> Vec<rusqlite::types::Value> {
    values
        .iter()
        .map(|value| match value {
            Value::Null => rusqlite::types::Value::Null,
            Value::Bool(flag) => rusqlite::types::Value::Integer(i64::from(*flag)),
            Value::Number(number) => number.as_i64().map_or_else(|| rusqlite::types::Value::Real(number.as_f64().unwrap_or(0.0)), rusqlite::types::Value::Integer),
            Value::String(text) => rusqlite::types::Value::Text(text.clone()),
            other => rusqlite::types::Value::Text(other.to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_statement_is_refused() {
        assert!(single_extension_statement("SELECT 1").is_ok());
        assert!(single_extension_statement("SELECT 1;").is_ok());
        assert!(single_extension_statement("SELECT 1; DROP TABLE bindings").is_err());
        assert!(single_extension_statement("SELECT ';'").is_ok());
    }
}
