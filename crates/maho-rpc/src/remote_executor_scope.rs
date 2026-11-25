//! Port of senpi `packages/coding-agent/src/modes/rpc/remote-executor-scope.ts`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use maho_agent::execution::plane::NestedRequestFn;
use maho_agent::execution::remote_executor::{HandCall, HandScope, OpenHandScope};
use maho_agent::execution::tool_host::ExecutionScope;
use maho_agent::types::AgentToolResult;
use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use maho_core::execution::hand_wake_route::HandWake;
use maho_core::settings_manager::SettingsManager;
use maho_ext_api::{PromptSurface, ToolExecutionMode};
use serde_json::Value;

use crate::hand_file_ops::{HandRunBinding, create_hand_file_ops};
use crate::hand_flock_binding::load_flock;
use crate::hand_run_allowlist::HandRunContext;
use crate::loopless_brain_calls::LooplessCallFacts;
use crate::loopless_execute_grant::grant_loopless_execute_to_remote_executor;
use crate::loopless_runtime::{LooplessRuntime, LooplessRuntimeFactory, LooplessRuntimeProfile};
use crate::loopless_runtime_settings::LooplessCliConfiguration;
use crate::loopless_tool_handlers::TwoSidedHome;
use crate::runtime_factory_loopless::{
    LooplessExtensionLoader, LooplessRuntimeFactoryOptions, create_loopless_runtime_factory,
};

fn call_facts(call: &HandCall) -> LooplessCallFacts {
    let prompt_surface = match call.prompt_surface.as_deref() {
        Some("terminal") => Some(PromptSurface::Terminal),
        Some("app") => Some(PromptSurface::App),
        Some("chat") => Some(PromptSurface::Chat),
        _ => None,
    };
    LooplessCallFacts { prompt_surface }
}

fn not_found(name: &str) -> AgentToolResult {
    let mut res = AgentToolResult::text(format!("Tool {name} not found"));
    res.is_error = Some(true);
    res
}

/// A hand's durable wake outbox, per scope (brain/hand split H3): what a scope's work produces
/// for the loop after the call that started it ended.
pub trait HandWakes: Send + Sync {
    fn offer(&self, scope: &str, wake: HandWake);
    /// The scope was disposed: its un-acked wakes are kept for the retention, then reported
    /// dropped to the loop.
    fn close_scope(&self, scope: &str);
}

#[derive(Clone, Default)]
pub struct LooplessHandOptions {
    pub extension_loader: Option<Arc<dyn LooplessExtensionLoader>>,
    pub startup_settings_manager: Option<Arc<SettingsManager>>,
    pub two_sided_home: TwoSidedHome,
    pub wakes: Option<Arc<dyn HandWakes>>,
}

struct ScopeEntry {
    root: String,
    runtime: Arc<tokio::sync::OnceCell<Result<LooplessRuntime, String>>>,
}

struct LooplessHandInner {
    factory: LooplessRuntimeFactory,
    wakes: Option<Arc<dyn HandWakes>>,
    hand_run: Arc<HandRunBinding>,
    runtimes: Mutex<HashMap<String, ScopeEntry>>,
    disposed: Mutex<HashSet<String>>,
    disposals: Mutex<HashSet<String>>,
    closed: std::sync::atomic::AtomicBool,
}

impl LooplessHandInner {
    async fn runtime_of(
        &self,
        scope: &ExecutionScope,
        reopen: bool,
    ) -> Result<LooplessRuntime, String> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("this hand is closed".to_owned());
        }
        if !Path::new(&scope.root).is_absolute() {
            return Err(format!(
                "scope root must be an absolute path: {:?}",
                scope.root
            ));
        }
        let cell = {
            let mut map = self.runtimes.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(known) = map.get(&scope.id) {
                if known.root != scope.root {
                    return Err(format!(
                        "scope_binding_mismatch: scope {:?} is bound to another root",
                        scope.id
                    ));
                }
                Arc::clone(&known.runtime)
            } else {
                let mut disposed = self.disposed.lock().unwrap_or_else(PoisonError::into_inner);
                if !reopen && disposed.contains(&scope.id) {
                    return Err(format!(
                        "scope {:?} was disposed; its extensions' loop-hook state is gone",
                        scope.id
                    ));
                }
                disposed.remove(&scope.id);
                let cell = Arc::new(tokio::sync::OnceCell::new());
                map.insert(
                    scope.id.clone(),
                    ScopeEntry {
                        root: scope.root.clone(),
                        runtime: Arc::clone(&cell),
                    },
                );
                cell
            }
        };

        let factory = Arc::clone(&self.factory);
        let wakes = self.wakes.clone();
        let scope_id = scope.id.clone();
        let scope_root = scope.root.clone();
        let res = cell
            .get_or_init(|| async move {
                let mut profile = LooplessRuntimeProfile::new(scope_root);
                if let Some(wakes) = wakes {
                    let sid = scope_id.clone();
                    profile.wake = Some(Arc::new(move |wake| wakes.offer(&sid, wake)));
                }
                factory(profile).await
            })
            .await
            .clone();
        if res.is_err() {
            self.runtimes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&scope.id);
        }
        res
    }

    async fn dispose_scope_runtime(
        &self,
        scope_id: &str,
        runtime: &LooplessRuntime,
    ) -> Result<(), String> {
        {
            let mut disposals = self
                .disposals
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !disposals.insert(scope_id.to_owned()) {
                return Ok(());
            }
        }
        runtime.dispose().await
    }
}

struct LooplessHandScope {
    scope: ExecutionScope,
    runtime: LooplessRuntime,
    hand: Arc<LooplessHandInner>,
    file_ops: Option<crate::hand_file_ops::HandFileOpFn>,
}

impl HandScope for LooplessHandScope {
    fn run(&self, call: HandCall) -> BoxFuture<'_, Result<AgentToolResult, String>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            if !runtime.definitions.contains_key(&call.name) {
                return Ok(not_found(&call.name));
            }
            runtime
                .execute(
                    call.name.clone(),
                    call.tool_call_id.clone(),
                    call.arguments.clone(),
                    Some(call.signal.clone()),
                    Some(call.nested.clone()),
                    Some(call_facts(&call)),
                )
                .await
        })
    }

    fn is_sequential(&self, name: &str) -> bool {
        self.runtime
            .definitions
            .get(name)
            .is_some_and(|e| e.definition.execution_mode == Some(ToolExecutionMode::Sequential))
    }

    fn file(
        &self,
        op: &str,
        args: &Value,
        signal: AbortSignal,
        nested: NestedRequestFn,
    ) -> Option<BoxFuture<'_, Result<AgentToolResult, String>>> {
        let ops = self.file_ops.as_ref()?.clone();
        let op = op.to_owned();
        let args = args.clone();
        Some(Box::pin(async move {
            Ok(ops(op, args, signal, Some(nested)).await)
        }))
    }

    fn dispose(&self) -> BoxFuture<'_, Result<(), String>> {
        let hand = Arc::clone(&self.hand);
        let scope_id = self.scope.id.clone();
        let runtime = self.runtime.clone();
        Box::pin(async move {
            hand.runtimes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&scope_id);
            hand.disposed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(scope_id.clone());
            if let Some(ref wakes) = hand.wakes {
                wakes.close_scope(&scope_id);
            }
            hand.dispose_scope_runtime(&scope_id, &runtime).await
        })
    }
}

/// A hand on loopless runtimes: its scopes for the remote executor, and its loop hooks for a
/// loop that runs elsewhere.
pub struct LooplessHand {
    inner: Arc<LooplessHandInner>,
}

impl LooplessHand {
    pub async fn open_scope(&self, scope: ExecutionScope) -> Result<Arc<dyn HandScope>, String> {
        let runtime = self.inner.runtime_of(&scope, true).await?;
        let file_ops = runtime.file_guards.as_ref().map(|guards| {
            create_hand_file_ops(
                &scope.root,
                Arc::clone(guards),
                Some(Arc::clone(&self.inner.hand_run)),
            )
        });
        Ok(Arc::new(LooplessHandScope {
            scope,
            runtime,
            hand: Arc::clone(&self.inner),
            file_ops,
        }))
    }

    pub async fn run_loop_hooks(
        &self,
        scope: &ExecutionScope,
        event: &str,
        payload: Value,
    ) -> Result<Value, String> {
        let runtime = self.inner.runtime_of(scope, false).await?;
        runtime.run_loop_hooks(event, payload).await
    }

    pub async fn loop_hooks(&self, event: &str, payload: Value) -> Result<Value, String> {
        let scope = scope_of(&payload)?;
        self.run_loop_hooks(&scope, event, payload).await
    }

    pub async fn close(&self) -> Result<(), String> {
        self.inner
            .closed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        type RuntimeCell = Arc<tokio::sync::OnceCell<Result<LooplessRuntime, String>>>;
        let open: Vec<(String, RuntimeCell)> = {
            let mut map = self
                .inner
                .runtimes
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            map.drain().map(|(k, v)| (k, v.runtime)).collect()
        };
        let mut errors = Vec::new();
        for (scope_id, cell) in open {
            if let Some(Ok(runtime)) = cell.get()
                && let Err(err) = self.inner.dispose_scope_runtime(&scope_id, runtime).await
            {
                errors.push(err);
            }
        }
        match errors.len() {
            0 => Ok(()),
            1 => Err(errors.remove(0)),
            _ => Err(format!(
                "disposing this hand's runtimes failed: {}",
                errors.join("; ")
            )),
        }
    }
}

fn scope_of(payload: &Value) -> Result<ExecutionScope, String> {
    let scope = payload.as_object().and_then(|o| o.get("scope"));
    let Some(obj) = scope.and_then(Value::as_object) else {
        return Err("a loop-hook call names no scope".to_owned());
    };
    let (Some(id), Some(root)) = (
        obj.get("id").and_then(Value::as_str),
        obj.get("root").and_then(Value::as_str),
    ) else {
        return Err("a loop-hook call names no scope".to_owned());
    };
    Ok(ExecutionScope {
        id: id.to_owned(),
        root: root.to_owned(),
    })
}

/// Opens a hand's execution scopes on loopless runtimes (brain/hand split, S27a): one runtime per
/// scope, bound to the scope's root as its cwd, built with the remote tool executor's grant.
pub fn create_loopless_hand(
    configuration: LooplessCliConfiguration,
    local: LooplessHandOptions,
) -> Arc<LooplessHand> {
    let factory = create_loopless_runtime_factory(
        configuration,
        LooplessRuntimeFactoryOptions {
            extension_loader: local.extension_loader,
            startup_settings_manager: local.startup_settings_manager,
            execute_grant: Some(grant_loopless_execute_to_remote_executor()),
            two_sided_home: local.two_sided_home,
        },
    );
    let inner = Arc::new(LooplessHandInner {
        factory,
        wakes: local.wakes,
        hand_run: Arc::new(HandRunBinding {
            context: HandRunContext::current(),
            flock: load_flock(),
        }),
        runtimes: Mutex::new(HashMap::new()),
        disposed: Mutex::new(HashSet::new()),
        disposals: Mutex::new(HashSet::new()),
        closed: std::sync::atomic::AtomicBool::new(false),
    });
    Arc::new(LooplessHand { inner })
}

/// The scopes of `create_loopless_hand`, for a hand that serves no loop hooks.
pub fn create_loopless_hand_scopes(
    configuration: LooplessCliConfiguration,
    local: LooplessHandOptions,
) -> OpenHandScope {
    let hand = create_loopless_hand(configuration, local);
    Arc::new(move |scope| {
        let hand = Arc::clone(&hand);
        Box::pin(async move { hand.open_scope(scope).await })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_agent::execution::remote_executor::HandCall;
    use maho_ai::utils::abort::AbortController;
    use serde_json::json;

    #[tokio::test]
    async fn opens_disposes_and_guards_scope_roots_and_closed_hand() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let cfg = LooplessCliConfiguration {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: agent_dir.to_string_lossy().into_owned(),
            no_extensions: true,
            no_skills: true,
            ..Default::default()
        };
        let hand = create_loopless_hand(cfg, LooplessHandOptions::default());

        let rel_err = hand
            .open_scope(ExecutionScope {
                id: "s1".into(),
                root: "relative".into(),
            })
            .await
            .err()
            .expect("relative root");
        assert!(rel_err.contains("absolute"));

        let scope_root = dir.path().to_string_lossy().into_owned();
        let scope = hand
            .open_scope(ExecutionScope {
                id: "s1".into(),
                root: scope_root.clone(),
            })
            .await
            .expect("open scope");

        let mismatch = hand
            .open_scope(ExecutionScope {
                id: "s1".into(),
                root: "/other".into(),
            })
            .await
            .err()
            .expect("mismatch");
        assert!(mismatch.contains("scope_binding_mismatch"));

        let ctrl = AbortController::new();
        let missing_tool = scope
            .run(HandCall {
                tool_call_id: "tc1".into(),
                name: "nonexistent_tool".into(),
                arguments: json!({}),
                signal: ctrl.signal(),
                nested: Arc::new(|_, _, _| Box::pin(async { Ok(Value::Null) })),
                prompt_surface: Some("terminal".into()),
            })
            .await
            .expect("not found returns error result");
        assert_eq!(missing_tool.is_error, Some(true));

        scope.dispose().await.expect("dispose");
        let after_dispose = hand
            .run_loop_hooks(
                &ExecutionScope {
                    id: "s1".into(),
                    root: scope_root,
                },
                "describe_tools",
                json!({"sessionId": "x", "idle": true}),
            )
            .await
            .expect_err("loop hooks do not reopen disposed scope");
        assert!(after_dispose.contains("disposed"));

        hand.close().await.expect("close");
    }
}
