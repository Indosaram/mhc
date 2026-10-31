//! Port of senpi `terminal/tools/monitor-rpc.ts`: the monitor tool's rpc branch. It resolves the
//! source (`type` is inferred from the one source field given), parses the target, arms the watch
//! and answers with every matched session as it is now, so nothing before arming is missed and
//! nothing before arming is injected as an event.
//!
//! Native shape: the registry and manager live behind std mutexes the async tool must not hold
//! across an await, so `create_rpc_monitor` takes the registry's rpc source (a shared handle) and
//! returns the spec to record; the caller binds the `mon_` id and hands the spec to the writer.
use super::context::{TerminalToolResult, error_result, text_result};
use super::monitor_schema::{compile_filter, resolve_timeout_ms};
use crate::rpc_monitor_source::{RegisterRpcMonitorOptions, RpcMonitorSource};
use crate::rpc_monitor_targets::{RpcEvents, default_events, parse_rpc_target};
use crate::shared::DURABLE_MONITOR_EXPIRY_MS;
use crate::terminal_manifest_model::{MonitorRegistration, MonitorSpec, RecordedRegistration};
use serde_json::{Value, json};

/// Upstream `MonitorSource`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorSource {
    Command,
    Path,
    Rpc,
}

impl MonitorSource {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "command" => Some(Self::Command),
            "path" => Some(Self::Path),
            "rpc" => Some(Self::Rpc),
            _ => None,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Path => "path",
            Self::Rpc => "rpc",
        }
    }
    fn field(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Path => "path",
            Self::Rpc => "target",
        }
    }
}

fn given(input: &Value, field: &str) -> bool {
    input
        .get(field)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
}

/// Upstream `monitorSourceOf`: which source a create call names; an error when it names none,
/// several, or contradicts `type`.
pub fn monitor_source_of(input: &Value) -> Result<MonitorSource, String> {
    let named = [
        (MonitorSource::Command, "command"),
        (MonitorSource::Path, "path"),
        (MonitorSource::Rpc, "target"),
    ]
    .into_iter()
    .filter(|(_, field)| given(input, field))
    .map(|(source, _)| source)
    .collect::<Vec<_>>();
    if named.len() > 1 {
        return Err(format!(
            "monitor takes one source; this call names {}.",
            named
                .iter()
                .map(|source| source.name())
                .collect::<Vec<_>>()
                .join(" and ")
        ));
    }
    let inferred = named.first().copied();
    let typed = input
        .get("type")
        .and_then(Value::as_str)
        .and_then(MonitorSource::parse);
    if let (Some(typed), Some(inferred)) = (typed, inferred)
        && typed != inferred
    {
        return Err(format!(
            "monitor type \"{}\" needs {}, but this call gives {}.",
            typed.name(),
            typed.field(),
            inferred.field()
        ));
    }
    let Some(source) = typed.or(inferred) else {
        return Err(
            "monitor requires description and command, path or target to start a watcher."
                .to_owned(),
        );
    };
    if inferred.is_none() {
        return Err(format!(
            "monitor type \"{}\" needs {}; {}",
            source.name(),
            source.field(),
            if source == MonitorSource::Rpc {
                "pass target: \"children\", \"all\" or a session id."
            } else {
                "pass it."
            }
        ));
    }
    Ok(source)
}

/// Upstream monitor.ts:189-200: the pure checks a create of `source` passes before persistence is
/// bound: `wakes_per_hour` only on a persistent command watch and within 1..=3600, and no
/// `human_input` on an rpc watch.
pub fn precheck_create(input: &Value, source: MonitorSource) -> Option<TerminalToolResult> {
    if let Some(rate) = input.get("wakes_per_hour") {
        if source != MonitorSource::Command
            || input.get("persistent").and_then(Value::as_bool) != Some(true)
        {
            return Some(error_result(
                "wakes_per_hour applies to persistent command monitors (command with persistent: true); other watches are not wake-paced.",
            ));
        }
        if rate
            .as_u64()
            .is_none_or(|value| !(1..=3600).contains(&value))
        {
            return Some(error_result("wakes_per_hour must be an integer from 1 to 3600."));
        }
    }
    if source == MonitorSource::Rpc && input.get("human_input").and_then(Value::as_bool) == Some(true)
    {
        return Some(error_result(
            "human_input applies to command monitors; an rpc watch reports session status, not a person's text.",
        ));
    }
    None
}

/// What the rpc branch needs from the tool context.
pub struct RpcMonitorContext {
    /// The registry's rpc source (`ctx.monitorRegistry.registerRpc`).
    pub source: RpcMonitorSource,
    /// `ctx.getSessionContext()?.sessionManager?.getSessionId()`: what a `children` target names
    /// and the durability key the checkpoint is handed to.
    pub session_id: Option<String>,
    pub agent_dir: std::path::PathBuf,
    /// Endpoint/observer overrides (tests); `None` in production.
    pub endpoints: Option<crate::rpc_monitor_watch::EndpointLister>,
    pub opener: Option<crate::rpc_monitor_watch::ObserverOpener>,
    pub now_ms: f64,
}

/// A created rpc watch: its ids and the spec to hand to the session's writer.
pub struct CreatedRpcMonitor {
    pub runtime_id: String,
    pub monitor_id: String,
    pub registration: RecordedRegistration,
}

/// Upstream `createRpcMonitor`. On success the caller binds `monitor_id` to `runtime_id` and hands
/// the registration to the manifest writer (`handMonitorSpec`).
pub async fn create_rpc_monitor(
    ctx: RpcMonitorContext,
    input: &Value,
) -> (TerminalToolResult, Option<CreatedRpcMonitor>) {
    let description = input
        .get("description")
        .and_then(Value::as_str)
        .filter(|description| !description.is_empty());
    let Some(description) = description else {
        return (
            error_result("monitor requires description to start a watcher."),
            None,
        );
    };
    let filter_text = input.get("filter").and_then(Value::as_str);
    let filter = match compile_filter(filter_text) {
        Ok(filter) => filter,
        Err(_) => {
            return (
                error_result(format!(
                    "Invalid monitor filter regex: {}",
                    filter_text.unwrap_or_default()
                )),
                None,
            );
        }
    };
    let target_text = input
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let target = match parse_rpc_target(&target_text, ctx.session_id.as_deref()) {
        Ok(target) => target,
        Err(error) => return (error_result(error), None),
    };
    let events = match input.get("events") {
        None | Some(Value::Null) => None,
        Some(value) => match serde_json::from_value::<RpcEvents>(value.clone()) {
            Ok(events) => Some(events),
            Err(_) => {
                return (
                    error_result("monitor events must be \"all\" or a list of record kinds."),
                    None,
                );
            }
        },
    };
    let exclude = match input.get("exclude") {
        None | Some(Value::Null) => None,
        Some(value) => match serde_json::from_value::<Vec<String>>(value.clone()) {
            Ok(exclude) => Some(exclude),
            Err(_) => {
                return (
                    error_result("monitor exclude must be a list of record kinds."),
                    None,
                );
            }
        },
    };
    let persistent = input.get("persistent").and_then(Value::as_bool) == Some(true);
    let timeout_ms =
        (!persistent).then(|| resolve_timeout_ms(input.get("timeout_ms").and_then(Value::as_f64)));
    let on_checkpoint = persistent.then(|| {
        let session_id = ctx.session_id.clone();
        std::sync::Arc::new(move |monitor_id: &str, checkpoint| {
            super::monitor_manifest_binding::hand_rpc_checkpoint(
                session_id.as_deref(),
                monitor_id,
                checkpoint,
            );
        }) as crate::rpc_monitor_source::RpcCheckpointSink
    });
    let registered = ctx
        .source
        .register(RegisterRpcMonitorOptions {
            description: description.to_owned(),
            events: events
                .clone()
                .unwrap_or_else(|| RpcEvents::Kinds(default_events(&target))),
            target,
            target_text: target_text.clone(),
            exclude: exclude.clone().unwrap_or_default(),
            filter: filter.map(|regex| (filter_text.unwrap_or_default().to_owned(), regex)),
            persistent,
            timeout_ms,
            expires_at: persistent.then_some(ctx.now_ms + DURABLE_MONITOR_EXPIRY_MS as f64),
            monitor_id: None,
            agent_dir: ctx.agent_dir,
            endpoints: ctx.endpoints,
            opener: ctx.opener,
            restored_from: None,
            on_checkpoint,
        })
        .await;
    let (runtime_id, monitor_id, rows) = match registered {
        Ok(registered) => registered,
        Err(error) => return (error_result(error.to_string()), None),
    };
    let registration = MonitorRegistration {
        monitor_id: monitor_id.clone(),
        spec: MonitorSpec::Rpc {
            description: description.to_owned(),
            target: target_text,
            events,
            exclude,
            filter: filter_text.map(str::to_owned),
            persistent,
        },
    }
    .with_deadline_ms(timeout_ms.map(|timeout_ms| ctx.now_ms + timeout_ms as f64));
    let listing = if rows.is_empty() {
        "No matching sessions yet.".to_owned()
    } else {
        format!("Watching:\n{}", rows.join("\n"))
    };
    let mut result = text_result(format!("Monitor started with ID: {monitor_id}\n{listing}"));
    result.details = json!({"monitor_id":monitor_id,"bash_id":runtime_id,"monitor":true})
        .as_object()
        .cloned();
    (
        result,
        Some(CreatedRpcMonitor {
            runtime_id,
            monitor_id,
            registration,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor_registry::MonitorRegistry;
    use crate::rpc_monitor_observer::{RpcObserver, RpcObserverHandlers};
    use crate::rpc_monitor_watch::BoxFuture;
    use std::sync::{Arc, Mutex};

    #[test]
    fn the_source_is_inferred_from_the_one_field_given() {
        let source = |input: Value| monitor_source_of(&input);
        assert_eq!(
            source(json!({"command":"make"})),
            Ok(MonitorSource::Command)
        );
        assert_eq!(source(json!({"path":"out"})), Ok(MonitorSource::Path));
        assert_eq!(source(json!({"target":"all"})), Ok(MonitorSource::Rpc));
        assert_eq!(
            source(json!({"type":"rpc","target":"children"})),
            Ok(MonitorSource::Rpc)
        );
        assert_eq!(
            source(json!({"command":"make","path":"out"})),
            Err("monitor takes one source; this call names command and path.".to_owned())
        );
        assert_eq!(
            source(json!({"type":"path","command":"make"})),
            Err("monitor type \"path\" needs path, but this call gives command.".to_owned())
        );
        assert_eq!(
            source(json!({"type":"rpc"})),
            Err("monitor type \"rpc\" needs target; pass target: \"children\", \"all\" or a session id.".to_owned())
        );
        assert_eq!(
            source(json!({"type":"command"})),
            Err("monitor type \"command\" needs command; pass it.".to_owned())
        );
        assert_eq!(
            source(json!({"description":"x"})),
            Err(
                "monitor requires description and command, path or target to start a watcher."
                    .to_owned()
            )
        );
    }

    type Handlers = Arc<Mutex<Vec<RpcObserverHandlers>>>;
    fn context(registry: &MonitorRegistry, handlers: Handlers) -> RpcMonitorContext {
        RpcMonitorContext {
            source: registry.rpc_source(),
            session_id: Some("owner".into()),
            agent_dir: std::env::temp_dir(),
            endpoints: Some(Arc::new(|| -> BoxFuture<Vec<String>> {
                Box::pin(async { vec!["/tmp/h.sock".to_owned()] })
            })),
            opener: Some(Arc::new(
                move |_socket: String,
                      _subscription: Value,
                      observer: RpcObserverHandlers|
                      -> BoxFuture<Result<RpcObserver, String>> {
                    handlers.lock().unwrap().push(observer);
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = tx.send(Ok(json!({"data":{"hostInstanceId":"i","hostGeneration":"1",
                        "snapshot":[{"sessionId":"s1","status":"busy","parentSessionId":"owner"}]}})));
                    Box::pin(async move { Ok(RpcObserver::new(rx, || {})) })
                },
            )),
            now_ms: 1_000.0,
        }
    }

    /// monitor-rpc.ts: a persistent children watch answers with the matched sessions as they are
    /// now, records an rpc spec with no deadline, and checkpoints through the bound writer.
    #[tokio::test]
    async fn a_persistent_children_watch_lists_rows_and_records_its_spec() {
        let registry = MonitorRegistry::new(|_| {});
        let handlers: Handlers = Arc::default();
        let (result, created) = create_rpc_monitor(
            context(&registry, handlers.clone()),
            &json!({"type":"rpc","target":"children","description":"children status","persistent":true}),
        )
        .await;
        assert!(result.is_error.is_none(), "{:?}", result.content);
        let created = created.unwrap();
        assert_eq!(created.runtime_id, "rpc_1");
        assert_eq!(
            result.content[0].text,
            format!(
                "Monitor started with ID: {}\nWatching:\ns1 busy (0 attached)",
                created.monitor_id
            )
        );
        assert_eq!(created.registration.deadline_ms, None);
        match &created.registration.registration.spec {
            MonitorSpec::Rpc {
                target,
                persistent,
                events,
                ..
            } => {
                assert_eq!(target, "children");
                assert!(*persistent);
                assert_eq!(*events, None);
            }
            other => panic!("rpc spec: {other:?}"),
        }
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].expires_at, Some(1_000.0 + DURABLE_MONITOR_EXPIRY_MS as f64));
        assert_eq!(snapshot[0].deadline_ms, None);
        assert!(registry.stop_rpc("rpc_1"));
    }

    #[tokio::test]
    async fn an_ephemeral_watch_records_its_deadline_and_bad_inputs_arm_nothing() {
        let registry = MonitorRegistry::new(|_| {});
        let handlers: Handlers = Arc::default();
        let (result, created) = create_rpc_monitor(
            context(&registry, handlers.clone()),
            &json!({"target":"all","description":"all","timeout_ms":5000,"events":"all","exclude":["text_delta"]}),
        )
        .await;
        assert!(result.is_error.is_none());
        let created = created.unwrap();
        assert_eq!(created.registration.deadline_ms, Some(6_000.0));
        assert!(matches!(
            &created.registration.registration.spec,
            MonitorSpec::Rpc { events: Some(RpcEvents::All), exclude: Some(exclude), .. } if exclude == &["text_delta".to_owned()]
        ));
        assert!(registry.stop_rpc(&created.runtime_id));
        for (input, message) in [
            (json!({"target":"all"}), "monitor requires description to start a watcher."),
            (json!({"target":"all","description":"d","filter":"("}), "Invalid monitor filter regex: ("),
            (json!({"target":"   ","description":"d"}), "target is empty; pass \"children\", \"all\" or a session id, path or st_ task id"),
        ] {
            let (result, created) = create_rpc_monitor(context(&registry, handlers.clone()), &input).await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(result.content[0].text, message);
            assert!(created.is_none());
        }
        assert!(registry.snapshot().is_empty());
    }
}
