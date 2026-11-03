//! `warm`: load what the next `open_session` needs without opening a session (senpi `rpc/host-warm.ts`,
//! senpi#2314).
//!
//! A fresh host pays a one-time cost on its first session: the extension graph loads and the extension
//! factories build their runtimes. `warm` builds the cwd-bound services an open would build for the same
//! cwd, kind and context, and drops them. No session exists, nothing is registered or listed, and the
//! connection that asked stays an observer (`host_observe_request.rs`).
//!
//! A profile is warmed once per host: a repeat answers `already_warm` without running factories again,
//! and concurrent warms of one profile share a single load.
use crate::rpc_types::RPC_ERROR_WARM_FAILED;
use futures::future::{BoxFuture, FutureExt, Shared};
use maho_ai::node::provider_scope::{ProviderScope, run_with_provider_scope_async};
use maho_core::agent_session_runtime::AgentSessionLaunchProfile;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What a runtime factory needs to build a session's services without the session.
#[derive(Clone, Debug)]
pub struct PrepareRuntimeOptions {
    pub cwd: String,
    pub agent_dir: PathBuf,
    pub launch_profile: AgentSessionLaunchProfile,
    pub session_kind: Option<String>,
    pub session_context: Option<BTreeMap<String, String>>,
}

/// senpi `PreparableRuntimeFactory.prepare`: builds the cwd-bound services alone, for `warm`.
pub type PrepareRuntime =
    Arc<dyn Fn(PrepareRuntimeOptions) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// The launch inputs that decide which extensions load, and so what a warm prepares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostWarmProfile {
    pub cwd: String,
    pub session_kind: Option<String>,
    pub session_context: Option<BTreeMap<String, String>>,
}

/// `Warmed`: this call (or one it joined) loaded the profile. `AlreadyWarm`: an earlier one had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostWarmState {
    Warmed,
    AlreadyWarm,
}
impl HostWarmState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warmed => "warmed",
            Self::AlreadyWarm => "already_warm",
        }
    }
}

pub type HostWarm =
    Arc<dyn Fn(HostWarmProfile) -> BoxFuture<'static, Result<HostWarmState, String>> + Send + Sync>;

/// Distinct profiles a host remembers as warm; the oldest is forgotten past this.
const MAX_WARM_PROFILES: usize = 64;

fn profile_key(profile: &HostWarmProfile) -> String {
    // BTreeMap iterates sorted by key, matching the TS sort before `JSON.stringify`.
    let context: Vec<(&String, &String)> = profile
        .session_context
        .as_ref()
        .map(|context| context.iter().collect())
        .unwrap_or_default();
    json!([
        profile.cwd,
        profile.session_kind.as_deref().unwrap_or("interactive"),
        context
    ])
    .to_string()
}

type Loader = Arc<dyn Fn(HostWarmProfile) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;
type PendingLoad = Shared<BoxFuture<'static, Result<(), String>>>;

#[derive(Default)]
struct WarmerState {
    /// Insertion-ordered warm keys; the front is the oldest.
    warm: VecDeque<String>,
    loading: HashMap<String, PendingLoad>,
}

/// Coalesces concurrent warms of one profile and answers repeats without loading again.
#[derive(Clone)]
pub struct HostWarmer {
    state: Arc<Mutex<WarmerState>>,
    load: Loader,
}

impl HostWarmer {
    pub fn new(
        load: impl Fn(HostWarmProfile) -> BoxFuture<'static, Result<(), String>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: Arc::default(),
            load: Arc::new(load),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WarmerState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub async fn run(&self, profile: HostWarmProfile) -> Result<HostWarmState, String> {
        let key = profile_key(&profile);
        // The guard is released before the shared load is awaited.
        let pending = {
            let mut state = self.lock();
            if state.warm.contains(&key) {
                return Ok(HostWarmState::AlreadyWarm);
            }
            if let Some(pending) = state.loading.get(&key).cloned() {
                pending
            } else {
                let pending = self.load_once(key.clone(), profile).boxed().shared();
                state.loading.insert(key, pending.clone());
                pending
            }
        };
        pending.await?;
        Ok(HostWarmState::Warmed)
    }

    fn load_once(
        &self,
        key: String,
        profile: HostWarmProfile,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send + 'static {
        let state = self.state.clone();
        let load = self.load.clone();
        // The loader runs on the first poll, after `run` has released the state guard.
        async move {
            let result = load(profile).await;
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if result.is_ok() {
                state.warm.push_back(key.clone());
                if state.warm.len() > MAX_WARM_PROFILES {
                    state.warm.pop_front();
                }
            }
            state.loading.remove(&key);
            result
        }
    }
}

/// The in-process registry's `warm`. The services are built inside their own provider scope, exactly
/// like an open's, and the scope closes when they are dropped, so nothing an extension registered while
/// loading outlives the warm.
pub fn create_registry_warm(prepare: PrepareRuntime, agent_dir: PathBuf) -> HostWarm {
    let warmer = HostWarmer::new(move |profile: HostWarmProfile| {
        let prepare = prepare.clone();
        let agent_dir = agent_dir.clone();
        async move {
            tokio::fs::metadata(&profile.cwd)
                .await
                .map_err(|error| error.to_string())?;
            let launch_profile = AgentSessionLaunchProfile {
                cwd: profile.cwd.clone(),
                ..Default::default()
            };
            let scope = ProviderScope::new();
            let prepared = run_with_provider_scope_async(
                &scope,
                prepare(PrepareRuntimeOptions {
                    cwd: profile.cwd,
                    agent_dir,
                    launch_profile,
                    session_kind: profile.session_kind,
                    session_context: profile.session_context,
                }),
            )
            .await;
            scope.close();
            prepared.map_err(|error| error.to_string())?
        }
        .boxed()
    });
    Arc::new(move |profile: HostWarmProfile| {
        let warmer = warmer.clone();
        async move { warmer.run(profile).await }.boxed()
    })
}

fn refusal(command: &Value, code: &str) -> Value {
    let mut response = Map::new();
    if let Some(id) = command.get("id") {
        response.insert("id".into(), id.clone());
    }
    response.insert("type".into(), "response".into());
    response.insert("command".into(), "warm".into());
    response.insert("success".into(), false.into());
    response.insert("error".into(), code.into());
    Value::Object(response)
}

fn success(command: &Value, state: &str) -> Value {
    let mut response = Map::new();
    if let Some(id) = command.get("id") {
        response.insert("id".into(), id.clone());
    }
    response.insert("type".into(), "response".into());
    response.insert("command".into(), "warm".into());
    response.insert("success".into(), true.into());
    response.insert("data".into(), json!({ "state": state }));
    Value::Object(response)
}

/// The host facts `answer_warm` reads.
pub struct WarmHost<'a> {
    pub draining: bool,
    pub warm: Option<&'a HostWarm>,
    pub cwd: &'a str,
    pub host_context: Option<&'a BTreeMap<String, String>>,
}

/// Answers one `warm`. The router calls this OUTSIDE its request accounting: a warm holds no session
/// and no path, so neither a drain nor an idle window waits for it.
pub async fn answer_warm(command: &Value, host: WarmHost<'_>) -> Value {
    use crate::rpc_input_validation::{session_context_error, session_kind_error};
    use crate::rpc_types::{
        RPC_ERROR_INVALID_PATH, RPC_ERROR_INVALID_SESSION_CONTEXT, RPC_ERROR_INVALID_SESSION_KIND,
    };
    if host.draining {
        return refusal(command, "host_draining");
    }
    if let Some(error) = session_kind_error(command.get("kind")) {
        return refusal(
            command,
            &format!("{RPC_ERROR_INVALID_SESSION_KIND}: {error}"),
        );
    }
    if let Some(error) = session_context_error(command.get("context")) {
        return refusal(
            command,
            &format!("{RPC_ERROR_INVALID_SESSION_CONTEXT}: {error}"),
        );
    }
    let Some(warm) = host.warm else {
        return success(command, "unsupported");
    };
    // The same context an open on this host would hand its extensions: the host's identity wins.
    let requested: Option<BTreeMap<String, String>> = command
        .get("context")
        .and_then(Value::as_object)
        .map(|context| {
            context
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|text| (key.clone(), text.to_owned()))
                })
                .collect()
        });
    let session_context = match (requested, host.host_context) {
        (Some(mut requested), Some(own)) => {
            requested.extend(own.iter().map(|(key, value)| (key.clone(), value.clone())));
            Some(requested)
        }
        (None, Some(own)) => Some(own.clone()),
        (requested, None) => requested,
    };
    let cwd = command
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or(host.cwd);
    // node `path.isAbsolute`: a rooted path without a drive ("/work") is absolute on win32 too.
    if !Path::new(cwd).has_root() {
        return refusal(command, RPC_ERROR_INVALID_PATH);
    }
    match warm(HostWarmProfile {
        cwd: cwd.to_owned(),
        session_kind: command
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_owned),
        session_context,
    })
    .await
    {
        Ok(state) => success(command, state.as_str()),
        Err(cause) => refusal(command, &format!("{RPC_ERROR_WARM_FAILED}: {cause}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counting_warmer(
        runs: Arc<AtomicUsize>,
        gate: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> HostWarmer {
        HostWarmer::new(move |_profile| {
            let runs = runs.clone();
            let gate = gate.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                if let Some(mut gate) = gate {
                    gate.wait_for(|open| *open)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Ok::<(), String>(())
            }
            .boxed()
        })
    }

    fn profile(role: &str) -> HostWarmProfile {
        HostWarmProfile {
            cwd: "/work".into(),
            session_kind: Some("worker".into()),
            session_context: Some(BTreeMap::from([("role".into(), role.into())])),
        }
    }

    #[tokio::test]
    async fn answers_a_repeat_as_already_warm_without_running_the_factories_again() {
        let runs = Arc::new(AtomicUsize::new(0));
        let warmer = counting_warmer(runs.clone(), None);
        assert_eq!(
            warmer.run(profile("child")).await,
            Ok(HostWarmState::Warmed)
        );
        assert_eq!(
            warmer.run(profile("child")).await,
            Ok(HostWarmState::AlreadyWarm)
        );
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(
            warmer.run(profile("dag_child")).await,
            Ok(HostWarmState::Warmed)
        );
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn shares_one_load_between_concurrent_warms_of_one_profile() {
        let runs = Arc::new(AtomicUsize::new(0));
        let (open, gate) = tokio::sync::watch::channel(false);
        let warmer = counting_warmer(runs.clone(), Some(gate));
        // `join!` polls in order: the first warm starts the load and parks on the gate, the second joins
        // that pending load, and only then does the gate open.
        let (first, second, ()) = tokio::join!(
            warmer.run(profile("member")),
            warmer.run(profile("member")),
            async {
                open.send_replace(true);
            }
        );
        assert_eq!(first, Ok(HostWarmState::Warmed));
        assert_eq!(second, Ok(HostWarmState::Warmed));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn does_not_remember_a_failed_load() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        let warmer = HostWarmer::new(move |_profile| {
            let attempt = counted.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt == 0 {
                    Err("missing cwd".to_owned())
                } else {
                    Ok(())
                }
            }
            .boxed()
        });
        assert_eq!(
            warmer.run(profile("child")).await,
            Err("missing cwd".to_owned())
        );
        assert_eq!(
            warmer.run(profile("child")).await,
            Ok(HostWarmState::Warmed)
        );
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn profile_key_sorts_context_and_defaults_the_kind() {
        let mut left = profile("child");
        left.session_kind = None;
        left.session_context = Some(BTreeMap::from([
            ("b".into(), "2".into()),
            ("a".into(), "1".into()),
        ]));
        assert_eq!(
            profile_key(&left),
            r#"["/work","interactive",[["a","1"],["b","2"]]]"#
        );
    }

    #[tokio::test]
    async fn refuses_bad_inputs_with_the_open_session_codes() {
        let warm: HostWarm = Arc::new(|_profile: HostWarmProfile| {
            async { Ok::<_, String>(HostWarmState::Warmed) }.boxed()
        });
        let host = |draining| WarmHost {
            draining,
            warm: Some(&warm),
            cwd: "/work",
            host_context: None,
        };
        let draining = answer_warm(&json!({"id":"w","type":"warm"}), host(true)).await;
        assert_eq!(draining["error"], "host_draining");
        let kind = answer_warm(&json!({"type":"warm","kind":"daemon"}), host(false)).await;
        assert!(
            kind["error"]
                .as_str()
                .unwrap()
                .starts_with("invalid_session_kind: ")
        );
        let path = answer_warm(&json!({"type":"warm","cwd":"relative"}), host(false)).await;
        assert_eq!(path["error"], "invalid_path");
        let ok = answer_warm(&json!({"id":"w","type":"warm"}), host(false)).await;
        assert_eq!(
            ok,
            json!({"id":"w","type":"response","command":"warm","success":true,"data":{"state":"warmed"}})
        );
        let unsupported = answer_warm(
            &json!({"type":"warm"}),
            WarmHost {
                draining: false,
                warm: None,
                cwd: "/work",
                host_context: None,
            },
        )
        .await;
        assert_eq!(unsupported["data"]["state"], "unsupported");
        let failing: HostWarm = Arc::new(|_profile: HostWarmProfile| {
            async { Err::<HostWarmState, _>("boom".to_owned()) }.boxed()
        });
        let failed = answer_warm(
            &json!({"type":"warm"}),
            WarmHost {
                draining: false,
                warm: Some(&failing),
                cwd: "/work",
                host_context: None,
            },
        )
        .await;
        assert_eq!(failed["error"], "warm_failed: boom");
    }

    #[tokio::test]
    async fn the_host_context_wins_over_the_requested_one() {
        let seen = Arc::new(Mutex::new(None));
        let captured = seen.clone();
        let warm: HostWarm = Arc::new(move |profile: HostWarmProfile| {
            *captured.lock().unwrap() = profile.session_context;
            async { Ok::<_, String>(HostWarmState::Warmed) }.boxed()
        });
        let own = BTreeMap::from([("role".to_owned(), "host".to_owned())]);
        answer_warm(
            &json!({"type":"warm","context":{"role":"child","lane":"a"}}),
            WarmHost {
                draining: false,
                warm: Some(&warm),
                cwd: "/work",
                host_context: Some(&own),
            },
        )
        .await;
        assert_eq!(
            seen.lock().unwrap().clone(),
            Some(BTreeMap::from([
                ("lane".to_owned(), "a".to_owned()),
                ("role".to_owned(), "host".to_owned()),
            ]))
        );
    }
}
