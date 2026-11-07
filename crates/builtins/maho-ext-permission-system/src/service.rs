use crate::{
    evaluate::{evaluate_external_spelling, evaluate_patterns},
    events::{PermissionEventEmitter, PermissionRepliedEvent},
    external_spellings::{Decision, decide_external_paths},
    types::{
        Action, PermissionDecision, PermissionError, Reply, ReplyInput, Request, Rule, Ruleset,
    },
};
use std::{
    cell::{Cell, RefCell},
    fs,
    future::Future,
    path::Path,
};
use tokio::sync::oneshot;

/// How one request is decided (TS `DecisionOptions` plus the `ask` options that shape it).
#[derive(Clone, Debug, Default)]
pub struct DecisionOptions {
    /// Every lexical spelling of the target: for `external_directory` they restrict the resolved
    /// decision (most restrictive wins); for other permissions they replace the pattern.
    pub rule_aliases: Option<Vec<String>>,
    /// Raw inputs, for display only (senpi#4311); never widen a decision.
    pub original_paths: Option<Vec<String>>,
}
/// Options for [`PermissionService::ask_with`].
#[derive(Clone, Debug, Default)]
pub struct AskOptions {
    /// Only a deny decides; an ask is approved without prompting (unparsed-bash deny-only check).
    pub auto_approve_ask: bool,
    pub rule_aliases: Option<Vec<String>>,
}

struct PendingEntry {
    info: Request,
    options: DecisionOptions,
    sender: oneshot::Sender<Result<(), PermissionError>>,
}
pub struct PermissionService {
    pending: Vec<PendingEntry>,
    approved: RefCell<Ruleset>,
    static_ruleset: Ruleset,
    agent_dir: Option<std::path::PathBuf>,
    project_dir: Option<std::path::PathBuf>,
    protected_store_path: Option<std::path::PathBuf>,
    approved_version: RefCell<Option<String>>,
    persisted_approved_count: Cell<usize>,
    emitter: PermissionEventEmitter,
    id_counter: u64,
}
fn target_is_protected_store(target: &str, store: &Path) -> bool {
    let target = Path::new(target);
    fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf())
        == fs::canonicalize(store).unwrap_or_else(|_| store.to_path_buf())
}
impl PermissionService {
    pub fn new(
        static_ruleset: Ruleset,
        approved: Ruleset,
        emitter: PermissionEventEmitter,
    ) -> Self {
        Self {
            pending: Vec::new(),
            persisted_approved_count: Cell::new(approved.len()),
            approved_version: RefCell::new(None),
            protected_store_path: None,
            agent_dir: None,
            project_dir: None,
            approved: RefCell::new(approved),
            static_ruleset,
            emitter,
            id_counter: 0,
        }
    }
    /// The one decision for a pattern, used when it is asked and again whenever an "Always" reply
    /// re-checks the requests still pending (TS `dispatchDecision`). External paths are decided
    /// separately for the absolute lexical path, the physical-prefix spellings from the component
    /// walk and their proven on-disk case; the most restrictive decision wins (deny > ask > allow),
    /// and more than 16 spellings or 40 hops asks instead (senpi#4351). Remembered approvals match
    /// only the resolved target, so they cannot override a deny from any spelling.
    pub fn decide(&self, permission: &str, pattern: &str, options: &DecisionOptions) -> Decision {
        self.refresh_approved();
        let aliases = options.rule_aliases.clone().unwrap_or_default();
        if permission == "external_directory" {
            return decide_external_paths(
                &[pattern],
                &aliases,
                &mut |spelling: &str, resolved: &str, canonical_case: &dyn Fn(&str) -> String| {
                    self.decide_target(
                        permission,
                        &[spelling],
                        resolved,
                        (spelling != resolved).then_some(canonical_case),
                    )
                },
            );
        }
        let targets: Vec<&str> = if aliases.is_empty() {
            vec![pattern]
        } else {
            aliases.iter().map(String::as_str).collect()
        };
        self.decide_target(permission, &targets, pattern, None)
    }
    /// Last match wins over the static rules; a configured deny is final, otherwise a matching
    /// approval (allow or deny) decides. A spelling that differs from its resolved target keeps its lexical
    /// ancestry, so a resolved rule cannot erase a lexical symlink deny.
    fn decide_target(
        &self,
        permission: &str,
        targets: &[&str],
        approved_target: &str,
        ancestry: Option<&dyn Fn(&str) -> String>,
    ) -> Decision {
        if self.protected_store_path.as_deref().is_some_and(|store| {
            targets
                .iter()
                .any(|target| target_is_protected_store(target, store))
        }) {
            return Decision {
                action: Action::Deny,
                rules: vec![Rule {
                    permission: permission.into(),
                    pattern: "*".into(),
                    action: Action::Deny,
                }],
            };
        }
        let configured = match ancestry {
            Some(canonical_case) => {
                evaluate_external_spelling(targets, &self.static_ruleset, Some(canonical_case))
            }
            None => evaluate_patterns(permission, targets, &[&self.static_ruleset]),
        };
        // A configured deny on this spelling is final: an approval for the resolved target must not
        // override it (senpi#4351, HIGH 2).
        if configured.action == Action::Deny {
            return Decision {
                action: Action::Deny,
                rules: vec![configured],
            };
        }
        let approved_targets: Vec<&str> =
            if ancestry.is_some() || permission == "external_directory" {
                vec![approved_target]
            } else {
                targets.to_vec()
            };
        let remembered = {
            let approved = self.approved.borrow();
            let remembered = evaluate_patterns(permission, &approved_targets, &[&approved]);
            approved.contains(&remembered).then_some(remembered)
        };
        if let Some(remembered) = remembered {
            return Decision {
                action: remembered.action,
                rules: vec![remembered],
            };
        }
        Decision {
            action: configured.action,
            rules: vec![configured],
        }
    }
    pub fn ask(
        &mut self,
        info: Request,
    ) -> impl Future<Output = Result<(), PermissionError>> + use<> {
        self.ask_with(info, AskOptions::default())
    }
    /// Request permission for a tool call. Resolves if allowed, errors on denial.
    pub fn ask_with(
        &mut self,
        info: Request,
        ask: AskOptions,
    ) -> impl Future<Output = Result<(), PermissionError>> + use<> {
        let mut info = info;
        if info.id.is_empty() {
            self.id_counter += 1;
            info.id = format!("permission-{}", self.id_counter);
        }
        let options = DecisionOptions {
            rule_aliases: ask.rule_aliases,
            original_paths: info.original_paths.clone(),
        };
        let mut denied = Vec::new();
        let mut needs_ask = false;
        for pattern in &info.patterns {
            match self.decide(&info.permission, pattern, &options).action {
                Action::Deny => denied.push(pattern.clone()),
                Action::Ask => needs_ask |= !ask.auto_approve_ask,
                Action::Allow => {}
            }
        }
        let (sender, receiver) = oneshot::channel();
        if !denied.is_empty() {
            if sender.send(Err(PermissionError::Denied(denied))).is_err() {
                unreachable!("receiver exists");
            }
        } else if !needs_ask {
            self.emit_replied(&info, PermissionDecision::Allow);
            if sender.send(Ok(())).is_err() {
                unreachable!("receiver exists");
            }
        } else {
            self.pending.push(PendingEntry {
                info: info.clone(),
                options,
                sender,
            });
            if let Err(error) = self.emitter.emit_asked(&info) {
                maho_ext_api::diagnostic(&format!("Error in permission_asked handler: {error}"));
            }
        }
        async move { receiver.await.unwrap_or(Err(PermissionError::Rejected)) }
    }
    fn emit_replied(&self, info: &Request, reply: PermissionDecision) {
        if let Err(error) = self.emitter.emit_replied(PermissionRepliedEvent {
            request_id: info.id.clone(),
            session_id: info.session_id.clone(),
            reply,
        }) {
            maho_ext_api::diagnostic(&format!("Error in permission_replied handler: {error}"));
        }
    }
    pub fn reply(&mut self, input: ReplyInput) {
        let Some(index) = self
            .pending
            .iter()
            .position(|entry| entry.info.id == input.request_id)
        else {
            return;
        };
        let entry = self.pending.remove(index);
        let decision = match input.reply {
            Reply::Once => PermissionDecision::Once,
            Reply::Always => PermissionDecision::Always,
            Reply::Reject => PermissionDecision::Reject,
        };
        self.emit_replied(&entry.info, decision);
        match input.reply {
            Reply::Reject => {
                let error = match input.message.filter(|message| !message.is_empty()) {
                    Some(message) => PermissionError::Corrected(message),
                    None => PermissionError::Rejected,
                };
                if entry.sender.send(Err(error)).is_err() {
                    maho_ext_api::diagnostic("Permission requester dropped before rejection");
                }
                let mut index = 0;
                while index < self.pending.len() {
                    if self.pending[index].info.session_id == entry.info.session_id {
                        let other = self.pending.remove(index);
                        self.emit_replied(&other.info, PermissionDecision::Reject);
                        if other.sender.send(Err(PermissionError::Rejected)).is_err() {
                            maho_ext_api::diagnostic(
                                "Permission requester dropped before rejection",
                            );
                        }
                    } else {
                        index += 1;
                    }
                }
            }
            Reply::Once => {
                if entry.sender.send(Ok(())).is_err() {
                    maho_ext_api::diagnostic("Permission requester dropped before approval");
                }
            }
            Reply::Always => {
                for pattern in input.always.as_ref().unwrap_or(&entry.info.always) {
                    self.approved.borrow_mut().push(Rule {
                        permission: entry.info.permission.clone(),
                        pattern: pattern.clone(),
                        action: Action::Allow,
                    });
                }
                if entry.sender.send(Ok(())).is_err() {
                    maho_ext_api::diagnostic("Permission requester dropped before approval");
                }
                let mut index = 0;
                while index < self.pending.len() {
                    let other = &self.pending[index];
                    if other.info.session_id == entry.info.session_id
                        && other.info.patterns.iter().all(|pattern| {
                            self.decide(&other.info.permission, pattern, &other.options)
                                .action
                                == Action::Allow
                        })
                    {
                        let other = self.pending.remove(index);
                        self.emit_replied(&other.info, PermissionDecision::Always);
                        if other.sender.send(Ok(())).is_err() {
                            maho_ext_api::diagnostic(
                                "Permission requester dropped before approval",
                            );
                        }
                    } else {
                        index += 1;
                    }
                }
            }
        }
    }
    pub fn list(&self) -> Vec<Request> {
        self.pending
            .iter()
            .map(|entry| entry.info.clone())
            .collect()
    }
    pub fn with_store_paths(
        mut self,
        agent_dir: &std::path::Path,
        project_dir: &std::path::Path,
    ) -> Self {
        self.agent_dir = Some(agent_dir.to_path_buf());
        self.project_dir = Some(project_dir.to_path_buf());
        self.protected_store_path = Some(crate::storage::approved_store_real_path(
            agent_dir,
            project_dir,
        ));
        let approved_version = crate::storage::approved_file_version(agent_dir, project_dir);
        *self.approved_version.borrow_mut() = approved_version;
        self
    }
    fn refresh_approved(&self) {
        let (Some(agent_dir), Some(project_dir)) = (&self.agent_dir, &self.project_dir) else {
            return;
        };
        let version = crate::storage::approved_file_version(agent_dir, project_dir);
        if version == *self.approved_version.borrow() {
            return;
        }
        let fresh = crate::storage::load_approved(agent_dir, project_dir);
        let persisted = self.persisted_approved_count.get();
        let current = self.approved.borrow();
        let local = current.get(persisted..).unwrap_or_default().to_vec();
        drop(current);
        let updated = [fresh.clone(), local].concat();
        *self.approved.borrow_mut() = updated;
        self.persisted_approved_count.set(fresh.len());
        *self.approved_version.borrow_mut() = version;
    }
    pub fn get_approved(&self) -> Ruleset {
        self.approved.borrow().clone()
    }
}
