use super::{
    env_grant_store::{EnvGrantSnapshot, GrantStore},
    env_grants_store::{Grant, GrantLabel, Observation},
    project_env_policy::{
        declaration_digest, policy_hash, resolve_project_environment, uses_project_environment,
    },
    types::{Environment, SkillMcpClientInfo},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use tokio::sync::{Mutex, watch};
#[derive(Clone)]
struct PendingGrant {
    digest: String,
    names: Vec<String>,
    info: SkillMcpClientInfo,
    raw: Value,
}
struct Live {
    names: Vec<String>,
    version: u64,
    controller: watch::Sender<bool>,
}
#[derive(Clone)]
pub struct PreparedPolicy {
    pub identity: String,
    pub digest: String,
    pub version: u64,
    pub config: Value,
    pub environment: Environment,
    pub signal: watch::Receiver<bool>,
}
#[derive(Default)]
struct PolicyState {
    snapshot: Option<EnvGrantSnapshot>,
    pending: BTreeMap<String, PendingGrant>,
    order: VecDeque<String>,
    live: BTreeMap<String, Live>,
    notices: BTreeMap<String, String>,
    messages: BTreeMap<String, String>,
    declarations: BTreeMap<String, String>,
    watcher: Option<Observation>,
}
pub type Notice = Arc<dyn Fn(&str, &SkillMcpClientInfo) + Send + Sync>;
pub type Invalidation = Arc<
    dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;
pub struct EnvironmentGrantPolicy {
    store: Arc<dyn GrantStore>,
    state: Mutex<PolicyState>,
    reconciliation: Mutex<()>,
    notify: Option<Notice>,
    invalidate: Invalidation,
}
fn declaration_key(info: &SkillMcpClientInfo) -> String {
    format!(
        "{}:{}:{}",
        info.session_id, info.skill_name, info.server_name
    )
}
fn drop_pending(state: &mut PolicyState, id: &str) {
    if let Some(pending) = state.pending.remove(id) {
        state.notices.remove(&pending.digest);
        state.messages.remove(&pending.digest);
    }
    state.order.retain(|key| key != id);
}
fn invalidate_live(live: &mut Live) {
    live.version += 1;
    live.controller.send_replace(true);
    live.controller = watch::channel(false).0;
}
impl EnvironmentGrantPolicy {
    pub fn new(
        store: Arc<dyn GrantStore>,
        notify: Option<Notice>,
        invalidate: Invalidation,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            state: Mutex::new(PolicyState::default()),
            reconciliation: Mutex::new(()),
            notify,
            invalidate,
        })
    }
    pub async fn refresh(&self, conservative: bool) {
        let _serial = self.reconciliation.lock().await;
        let next = self.store.read();
        let mut invalidated = Vec::new();
        {
            let mut state = self.state.lock().await;
            let gap = conservative
                || state.snapshot.as_ref().is_some_and(|previous| {
                    next.snapshot.generation < previous.snapshot.generation
                        || next.snapshot.generation > previous.snapshot.generation.saturating_add(1)
                        || (next.revision != previous.revision
                            && next.snapshot.generation <= previous.snapshot.generation)
                });
            for (identity, live) in &mut state.live {
                let names = next
                    .snapshot
                    .grants
                    .get(identity)
                    .map(|g| g.names.clone())
                    .unwrap_or_default();
                if gap || names != live.names {
                    live.names = names;
                    invalidate_live(live);
                    invalidated.push(identity.clone());
                }
            }
            state.snapshot = Some(next);
        }
        for identity in invalidated {
            (self.invalidate)(identity).await;
        }
    }
    pub async fn prepare(
        self: &Arc<Self>,
        info: &SkillMcpClientInfo,
        raw: &Value,
        explicit: &Environment,
    ) -> Option<PreparedPolicy> {
        if !uses_project_environment(info) {
            return None;
        }
        {
            let mut state = self.state.lock().await;
            if state.watcher.is_none() {
                let weak = Arc::downgrade(self);
                state.watcher = self.store.watch(Box::new(move |event| {
                    if let Some(policy) = weak.upgrade() {
                        tokio::spawn(async move {
                            policy.refresh(event.conservative).await;
                        });
                    }
                }));
            }
        }
        self.state
            .lock()
            .await
            .declarations
            .insert(declaration_key(info), declaration_digest(info, raw));
        self.refresh(false).await;
        let identity = declaration_digest(info, raw);
        let mut state = self.state.lock().await;
        let names = state
            .snapshot
            .as_ref()
            .and_then(|s| s.snapshot.grants.get(&identity))
            .map(|g| g.names.clone())
            .unwrap_or_default();
        let policy = resolve_project_environment(
            raw,
            &names,
            explicit,
            &std::env::vars().collect(),
            cfg!(windows),
        );
        let mut authority = json!({"identity":identity,"names":names,"explicit":explicit,"withheld":policy.withheld});
        if let Some(provenance) = &info.provenance {
            authority["provenance"] =
                serde_json::to_value(provenance).expect("provenance serializes");
        }
        let digest = policy_hash(&authority);
        let live = state.live.entry(identity.clone()).or_insert_with(|| Live {
            names: names.clone(),
            version: 0,
            controller: watch::channel(false).0,
        });
        let version = live.version;
        let signal = live.controller.subscribe();
        let mut notice = None;
        if policy.withheld.is_empty() {
            let ids: Vec<_> = state
                .pending
                .iter()
                .filter(|(_, p)| p.digest == identity)
                .map(|(id, _)| id.clone())
                .collect();
            for id in ids {
                drop_pending(&mut state, &id);
            }
            state.notices.remove(&identity);
            state.messages.remove(&identity);
        } else if state.notices.get(&identity) != Some(&digest) {
            let ids: Vec<_> = state
                .pending
                .iter()
                .filter(|(_, p)| {
                    p.digest == identity || declaration_key(&p.info) == declaration_key(info)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in ids {
                drop_pending(&mut state, &id);
            }
            while state.pending.len() >= 128 {
                if let Some(id) = state.order.front().cloned() {
                    drop_pending(&mut state, &id);
                } else {
                    break;
                }
            }
            let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_owned();
            let message = format!(
                "Project MCP server {} (skill {}) withheld {} environment names: {}. Grant and save permanently: /omo-mcp-env-grant {} (all displayed names), or /omo-mcp-env-grant {} NAME [NAME...].",
                info.server_name,
                info.skill_name,
                policy.withheld.len(),
                policy.withheld.join(", "),
                id,
                id
            );
            state.pending.insert(
                id.clone(),
                PendingGrant {
                    digest: identity.clone(),
                    names: policy.withheld,
                    info: info.clone(),
                    raw: raw.clone(),
                },
            );
            state.order.push_back(id);
            state.notices.insert(identity.clone(), digest.clone());
            state.messages.insert(identity.clone(), message.clone());
            notice = Some(message);
        }
        drop(state);
        if let Some(message) = notice
            && let Some(notify) = &self.notify {
                notify(&message, info);
            }
        Some(PreparedPolicy {
            identity,
            digest,
            version,
            config: policy.config,
            environment: policy.env,
            signal,
        })
    }
    pub async fn version(&self, identity: &str) -> u64 {
        self.state
            .lock()
            .await
            .live
            .get(identity)
            .map(|l| l.version)
            .unwrap_or(0)
    }
    pub async fn invalidate_identity(&self, identity: &str) {
        if let Some(live) = self.state.lock().await.live.get_mut(identity) {
            invalidate_live(live);
        }
    }
    pub async fn notice(&self, info: &SkillMcpClientInfo) -> Option<String> {
        let state = self.state.lock().await;
        state
            .declarations
            .get(&declaration_key(info))
            .and_then(|identity| state.messages.get(identity))
            .cloned()
    }
    pub async fn grant(&self, id: &str, subset: Option<&[String]>) -> super::Result<String> {
        let pending = self
            .state
            .lock()
            .await
            .pending
            .get(id)
            .cloned()
            .ok_or("Unknown or expired MCP environment grant handle.")?;
        let mut names = subset
            .filter(|n| !n.is_empty())
            .unwrap_or(&pending.names)
            .to_vec();
        names.sort();
        names.dedup();
        if names.iter().any(|n| !pending.names.contains(n)) {
            return Err("Grant contains a name outside the displayed set.".into());
        }
        let current = if let Some(revalidate) = &pending.info.revalidate_declaration {
            revalidate().await?
        } else {
            Some(super::types::Declaration {
                config: pending.raw.clone(),
                source_path: pending.info.source_path.clone().unwrap_or_default(),
                source_kind: pending
                    .info
                    .source_kind
                    .clone()
                    .unwrap_or_else(|| "frontmatter".into()),
            })
        };
        let mut info = pending.info.clone();
        let valid = if let Some(current) = current {
            if pending.info.revalidate_declaration.is_some() {
                info.source_path = Some(current.source_path);
                info.source_kind = Some(current.source_kind);
            }
            declaration_digest(&info, &current.config) == pending.digest
        } else {
            false
        };
        let mut state = self.state.lock().await;
        if !state.pending.contains_key(id) {
            return Err("Unknown or expired MCP environment grant handle.".into());
        }
        if !valid
            || state.declarations.get(&declaration_key(&pending.info)) != Some(&pending.digest)
        {
            drop_pending(&mut state, id);
            return Err("The MCP declaration changed. Start the server again to obtain a new grant command.".into());
        }
        self.store.add(
            &pending.digest,
            Grant {
                label: GrantLabel {
                    project: info
                        .provenance
                        .as_ref()
                        .and_then(|p| p.project_root.clone())
                        .or(info.directory)
                        .unwrap_or_default(),
                    skill: info.skill_name,
                    server: info.server_name.clone(),
                },
                names: names.clone(),
            },
        )?;
        drop_pending(&mut state, id);
        drop(state);
        self.refresh(false).await;
        Ok(format!(
            "Saved permanent MCP environment grant for {}: {}.",
            info.server_name,
            names.join(", ")
        ))
    }
    pub async fn close(&self) {
        let mut state = self.state.lock().await;
        state.watcher = None;
        for live in state.live.values_mut() {
            invalidate_live(live);
        }
    }
}
