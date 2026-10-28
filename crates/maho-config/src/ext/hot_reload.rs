//! Hot reload of the harness configuration (spec §14): watch targets, a sticky validation
//! verdict, and the registration lifecycle against `maho-ext-config-reload`.

use crate::diagnostics::{Diagnostic, DiagnosticKind, DiagnosticTarget};
use crate::env::{Env, account_home, home_dir_with};
use crate::fs::{ConfigFs, RealFs};
use crate::load::{LoadResult, load_native};
use crate::paths::project_paths_farthest_first;
use maho_ext_api::{
    BusSubscription, ConfigWatchTargetKind, ConfigWatchTargetSpec, ConfigWatchValidation, EventBus,
    EventKind, EventResult, Extension, ExtensionApi, RegisteredConfigWatch,
};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Registration id the host keys on (kept for compatibility).
pub const REGISTRATION_ID: &str = "omo";
/// Display name of the registration.
pub const DISPLAY_NAME: &str = ".omo config";
/// Maximum ancestors watched for project files.
pub const MAX_PROJECT_WATCH_DEPTH: usize = 128;
/// Re-publish attempts per distinct target set.
pub const MAX_REPUBLISH: usize = 3;
/// Linux `statfs` type of 9P mounts (used by WSL for Windows drives).
pub const PLAN9_MAGIC: i64 = 0x0102_1997;

pub const CONFIG_WATCH_READY: &str = "config-watch:ready";
pub const CONFIG_WATCH_RELOADED: &str = "config-watch:reloaded";
pub const CONFIG_WATCH_REJECTED: &str = "config-watch:rejected";

const FILE_FILTERS: [&str; 2] = ["/omo.jsonc", "/omo.json"];
const DIR_FILTERS: [&str; 3] = ["/.omo", "/.omo/omo.jsonc", "/.omo/omo.json"];

/// Whether `path` sits on a 9P filesystem (events are unreliable there).
pub fn is_plan9_mount(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        #[allow(clippy::unnecessary_cast)]
        let plan9 = nix::sys::statfs::statfs(path)
            .is_ok_and(|fs| fs.filesystem_type().0 as i64 == PLAN9_MAGIC);
        plan9
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

fn dir_watch(path: &Path, filters: &[&str]) -> ConfigWatchTargetSpec {
    ConfigWatchTargetSpec {
        path: path.to_path_buf(),
        kind: ConfigWatchTargetKind::Dir,
        filter_globs: filters.iter().map(|f| (*f).to_owned()).collect(),
    }
}

/// Whether a target is inside (or would match inside) a protected agent-dir path.
pub fn touches_protected(target: &ConfigWatchTargetSpec, agent_dir: &Path) -> bool {
    let agent = maho_support::normalize_lexically(agent_dir);
    let protected: Vec<PathBuf> = ["auth.json", "sessions", "logs"]
        .iter()
        .map(|n| agent.join(n))
        .collect();
    let base = maho_support::normalize_lexically(&target.path);
    if protected.iter().any(|p| base.starts_with(p)) {
        return true;
    }
    target.filter_globs.iter().any(|filter| {
        let matched = maho_support::normalize_lexically(&base.join(filter.trim_start_matches('/')));
        protected
            .iter()
            .any(|p| matched.starts_with(p) || p.starts_with(&matched))
    })
}

/// The computed watch targets.
#[derive(Clone, Debug, PartialEq)]
pub struct WatchPlan {
    pub targets: Vec<ConfigWatchTargetSpec>,
    /// Whether a user-level target could be registered.
    pub user_watched: bool,
}

/// Inputs of [`watch_targets`].
pub struct WatchInputs<'a> {
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub agent_dir: &'a Path,
    pub account_home: Option<&'a Path>,
    pub fs: &'a dyn ConfigFs,
    pub skip_mount: &'a dyn Fn(&Path) -> bool,
}

/// Directory watches for the user and project configuration files (§14.1).
pub fn watch_targets(inputs: &WatchInputs<'_>) -> WatchPlan {
    let fs = inputs.fs;
    let mut targets: Vec<ConfigWatchTargetSpec> = Vec::new();
    let push = |target: ConfigWatchTargetSpec, targets: &mut Vec<ConfigWatchTargetSpec>| -> bool {
        if (inputs.skip_mount)(&target.path) || touches_protected(&target, inputs.agent_dir) {
            return false;
        }
        if !targets.contains(&target) {
            targets.push(target);
        }
        true
    };
    let user_omo = inputs.home.join(".omo");
    let user_watched = if fs.is_dir(&user_omo) {
        push(dir_watch(&user_omo, &FILE_FILTERS), &mut targets)
    } else if fs.is_dir(inputs.home) {
        push(dir_watch(inputs.home, &DIR_FILTERS), &mut targets)
    } else {
        false
    };
    let known: Vec<PathBuf> =
        project_paths_farthest_first(inputs.cwd, inputs.home, fs, inputs.account_home)
            .into_iter()
            .filter_map(|file| file.parent().map(Path::to_path_buf))
            .collect();
    let cwd = maho_support::normalize_lexically(inputs.cwd);
    let home = maho_support::normalize_lexically(inputs.home);
    let inside_home = cwd.starts_with(&home);
    let mut current = Some(cwd.as_path());
    let mut visited = 0;
    while let Some(dir) = current {
        if visited >= MAX_PROJECT_WATCH_DEPTH {
            break;
        }
        visited += 1;
        let omo = dir.join(".omo");
        if known.contains(&omo) || (fs.is_dir(&omo) && !fs.is_symlink(&omo)) {
            push(dir_watch(&omo, &FILE_FILTERS), &mut targets);
        }
        if fs.is_dir(dir) {
            push(dir_watch(dir, &DIR_FILTERS), &mut targets);
        }
        if inside_home && dir == home {
            break;
        }
        current = dir.parent();
    }
    WatchPlan {
        targets,
        user_watched,
    }
}

fn fingerprint(diagnostic: &Diagnostic) -> String {
    let target = match &diagnostic.target {
        DiagnosticTarget::File(path) => format!("file:{}", path.display()),
        DiagnosticTarget::Merged => "merged".into(),
        DiagnosticTarget::Config(path) => format!("config:{path}"),
    };
    let mut hasher = Sha256::new();
    hasher.update(diagnostic.kind.as_str());
    hasher.update([0]);
    hasher.update(target);
    hasher.update([0]);
    hasher.update(&diagnostic.message);
    format!("{:x}", hasher.finalize())
}

/// The nearest `.omo` directory at or above `path`.
fn omo_dir(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == ".omo"))
        .map(Path::to_path_buf)
}

fn attributable(diagnostic: &Diagnostic, changed: &[PathBuf]) -> bool {
    if diagnostic.target == DiagnosticTarget::Merged
        || diagnostic.kind == DiagnosticKind::CatalogCycle
    {
        return true;
    }
    let Some(file) = diagnostic.file() else {
        return false;
    };
    let file_omo = omo_dir(file);
    changed.iter().any(|changed| {
        file.starts_with(changed)
            || (file_omo.is_some()
                && file_omo == omo_dir(changed).or_else(|| Some(changed.join(".omo"))))
    })
}

pub type ReloadLoader = Arc<dyn Fn() -> LoadResult + Send + Sync>;

/// The sticky reload verdict (§14.2).
pub struct ReloadValidator {
    loader: ReloadLoader,
    baseline: HashSet<String>,
    unresolved: Vec<(String, String)>,
}

fn blocking(result: &LoadResult) -> Vec<(String, &Diagnostic)> {
    result
        .diagnostics
        .iter()
        .filter(|d| d.kind != DiagnosticKind::DeprecatedKeys)
        .map(|d| (fingerprint(d), d))
        .collect()
}

impl ReloadValidator {
    /// Loads once and records the baseline.
    pub fn new(loader: ReloadLoader) -> Self {
        let first = loader();
        let baseline = blocking(&first).into_iter().map(|(fp, _)| fp).collect();
        Self {
            loader,
            baseline,
            unresolved: Vec::new(),
        }
    }

    /// Validates a change set.
    pub fn validate(&mut self, changed: &[PathBuf]) -> ConfigWatchValidation {
        let result = (self.loader)();
        let current = blocking(&result);
        let present: HashSet<&String> = current.iter().map(|(fp, _)| fp).collect();
        self.unresolved.retain(|(fp, _)| present.contains(fp));
        for (fp, diagnostic) in &current {
            if !self.baseline.contains(fp)
                && attributable(diagnostic, changed)
                && !self.unresolved.iter().any(|(seen, _)| seen == fp)
            {
                self.unresolved
                    .push((fp.clone(), diagnostic.message.clone()));
            }
        }
        if self.unresolved.is_empty() {
            self.baseline = current.into_iter().map(|(fp, _)| fp).collect();
            ConfigWatchValidation::Ok
        } else {
            ConfigWatchValidation::Rejected {
                errors: self
                    .unresolved
                    .iter()
                    .map(|(_, message)| message.clone())
                    .collect(),
            }
        }
    }
}

/// Counts re-publishes per distinct target set.
#[derive(Default)]
pub struct RepublishBudget {
    targets: Option<Vec<ConfigWatchTargetSpec>>,
    used: usize,
}

impl RepublishBudget {
    /// Whether another re-publish is allowed for `targets` (and records it).
    pub fn allow(&mut self, targets: &[ConfigWatchTargetSpec]) -> bool {
        if self.targets.as_deref() != Some(targets) {
            self.targets = Some(targets.to_vec());
            self.used = 0;
        }
        if self.used >= MAX_REPUBLISH {
            return false;
        }
        self.used += 1;
        true
    }
}

fn log_line(message: &str) {
    maho_support::log(message, None);
    maho_ext_api::diagnostic(&format!("[config-hot-reload] {message}"));
}

struct Shared {
    owner: String,
    events: EventBus,
    registration: Mutex<RegisteredConfigWatch>,
    budget: Mutex<RepublishBudget>,
    stood_down: AtomicBool,
    shut_down: AtomicBool,
    subscriptions: Mutex<Vec<BusSubscription>>,
    compute: Arc<dyn Fn() -> WatchPlan + Send + Sync>,
}

impl Shared {
    fn active(&self) -> bool {
        !self.stood_down.load(Ordering::SeqCst) && !self.shut_down.load(Ordering::SeqCst)
    }

    fn stop(&self) {
        self.subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    fn publish_now(self: &Arc<Self>) {
        if !self.active() {
            return;
        }
        let registration = self
            .registration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let shared = Arc::clone(self);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            shared
                .events
                .publish_config_watch(&shared.owner, registration);
        }));
    }

    /// Re-publishes on a fresh task (never synchronously).
    fn publish_later(self: &Arc<Self>) {
        let shared = Arc::clone(self);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    tokio::task::yield_now().await;
                    shared.publish_now();
                });
            }
            Err(_) => {
                std::thread::spawn(move || shared.publish_now());
            }
        }
    }
}

/// The hot-reload extension.
#[derive(Clone, Default)]
pub struct ConfigHotReload {
    pub env: Option<Env>,
}

fn paths_of(payload: &serde_json::Value) -> Vec<String> {
    payload["paths"]
        .as_array()
        .map(|p| {
            p.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn on_rejected(shared: &Arc<Shared>, payload: &serde_json::Value) {
    if payload["registrationId"].as_str() != Some(REGISTRATION_ID) || !shared.active() {
        return;
    }
    let errors: Vec<String> = payload["errors"]
        .as_array()
        .map(|e| {
            e.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    log_line(&format!(
        "configuration change rejected ({}): {}",
        paths_of(payload).join(", "),
        errors.join("; ")
    ));
    let targets = (shared.compute)().targets;
    let allowed = shared
        .budget
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .allow(&targets);
    if !allowed {
        log_line("giving up re-registering the .omo configuration watch after repeated rejections");
        return;
    }
    shared
        .registration
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .targets = targets;
    shared.publish_later();
}

impl Extension for ConfigHotReload {
    fn register(&self, api: &mut ExtensionApi) {
        let env = self.env.clone().unwrap_or_else(crate::env::process_env);
        let cwd = api.cwd.clone();
        let home = home_dir_with(&env, &cwd);
        let agent_dir = PathBuf::from(maho_core::config::get_agent_dir());
        let account = account_home();
        let compute: Arc<dyn Fn() -> WatchPlan + Send + Sync> = {
            let (cwd, home) = (cwd.clone(), home.clone());
            Arc::new(move || {
                watch_targets(&WatchInputs {
                    cwd: &cwd,
                    home: &home,
                    agent_dir: &agent_dir,
                    account_home: account.as_deref(),
                    fs: &RealFs,
                    skip_mount: &is_plan9_mount,
                })
            })
        };
        let plan = compute();
        if !plan.user_watched {
            log_line(
                "the user configuration directory cannot be watched; creating ~/.omo/omo.jsonc later requires a reload",
            );
        }
        let validator = {
            let (cwd, env) = (cwd.clone(), env.clone());
            Arc::new(Mutex::new(ReloadValidator::new(Arc::new(move || {
                load_native(&cwd, &env)
            }))))
        };
        let registration = RegisteredConfigWatch {
            id: REGISTRATION_ID.into(),
            display_name: DISPLAY_NAME.into(),
            targets: plan.targets,
            validate: Arc::new(move |changed| {
                let validator = Arc::clone(&validator);
                Box::pin(async move {
                    Ok(validator
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .validate(&changed))
                })
            }),
        };
        let shared = Arc::new(Shared {
            owner: api.registered.identity.path.clone(),
            events: api.events.clone(),
            registration: Mutex::new(registration.clone()),
            budget: Mutex::new(RepublishBudget::default()),
            stood_down: AtomicBool::new(false),
            shut_down: AtomicBool::new(false),
            subscriptions: Mutex::new(Vec::new()),
            compute,
        });
        // Subscribe before publishing: the host may reject synchronously.
        let mut subscriptions = Vec::new();
        let weak = Arc::downgrade(&shared);
        subscriptions.push(api.events.on(
            CONFIG_WATCH_READY,
            Arc::new(move |_| {
                if let Some(shared) = weak.upgrade() {
                    shared.publish_later();
                }
            }),
        ));
        let weak = Arc::downgrade(&shared);
        subscriptions.push(api.events.on(
            CONFIG_WATCH_REJECTED,
            Arc::new(move |payload| {
                if let Some(shared) = weak.upgrade() {
                    on_rejected(&shared, payload);
                }
            }),
        ));
        let weak = Arc::downgrade(&shared);
        subscriptions.push(api.events.on(
            CONFIG_WATCH_RELOADED,
            Arc::new(move |payload| {
                if weak.upgrade().is_some()
                    && payload["registrationId"].as_str() == Some(REGISTRATION_ID)
                {
                    log_line(&format!(
                        "configuration reloaded: {}",
                        paths_of(payload).join(", ")
                    ));
                }
            }),
        ));
        let weak = Arc::downgrade(&shared);
        subscriptions.push(api.on_config_watch_registration(Arc::new(
            move |owner, registration| {
                let Some(shared) = weak.upgrade() else { return };
                if registration.id == REGISTRATION_ID && owner != shared.owner && shared.active() {
                    log_line(
                        "another extension registered the .omo configuration watch; standing down",
                    );
                    shared.stood_down.store(true, Ordering::SeqCst);
                    shared.stop();
                }
            },
        )));
        *shared
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = subscriptions;
        let on_shutdown = Arc::clone(&shared);
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, _| {
                let shared = Arc::clone(&on_shutdown);
                Box::pin(async move {
                    shared.shut_down.store(true, Ordering::SeqCst);
                    shared.stop();
                    Ok(EventResult::None)
                })
            }),
        );
        api.register_config_watch(registration);
    }
}
