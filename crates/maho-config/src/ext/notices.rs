//! Startup notices (spec §13): run the startup migration and a load once per extension instance,
//! then show what happened on the first `session_start`.

use crate::env::Env;
use crate::load::{LoadResult, load_native};
use crate::migrate::plans::{StartupMigration, run_startup_migration};
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, NotificationType};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Product prefix of every notice.
pub const PRODUCT: &str = "mhc";

/// Notice severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
}

/// One startup notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupNotice {
    pub level: NoticeLevel,
    pub text: String,
}

fn notice(level: NoticeLevel, text: String) -> StartupNotice {
    StartupNotice { level, text }
}

/// The notices for one startup, in delivery order.
pub fn compute_notices(migration: &StartupMigration, load: &LoadResult) -> Vec<StartupNotice> {
    let mut out = Vec::new();
    if let Some(error) = &migration.error {
        out.push(notice(
            NoticeLevel::Warning,
            format!("{PRODUCT}: the .omo configuration migration did not finish: {error}"),
        ));
    } else if !migration.migrated_from.is_empty() {
        let paths: Vec<String> = migration
            .migrated_from
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        out.push(notice(
            NoticeLevel::Info,
            format!(
                "{PRODUCT}: migrated older configuration into omo.jsonc from {}",
                paths.join(", ")
            ),
        ));
    } else if migration.journal_resumed {
        out.push(notice(
            NoticeLevel::Info,
            format!("{PRODUCT}: finished a configuration migration that was interrupted earlier"),
        ));
    }
    let plan_diagnostics: Vec<&str> = migration
        .plans
        .iter()
        .flat_map(|plan| plan.diagnostics.iter().map(String::as_str))
        .collect();
    if !plan_diagnostics.is_empty() {
        out.push(notice(
            NoticeLevel::Warning,
            format!(
                "{PRODUCT}: configuration migration notes: {}",
                plan_diagnostics.join("; ")
            ),
        ));
    }
    if !load.diagnostics.is_empty() {
        let messages: Vec<&str> = load
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        out.push(notice(
            NoticeLevel::Warning,
            format!("{PRODUCT}: configuration problems: {}", messages.join("; ")),
        ));
    }
    out
}

/// Hands each notice to the UI when there is one, otherwise to the diagnostic log.
pub fn deliver(
    notices: &[StartupNotice],
    has_ui: bool,
    notify: &dyn Fn(&str, NotificationType),
    log: &dyn Fn(&str),
) {
    for item in notices {
        if has_ui {
            let kind = match item.level {
                NoticeLevel::Info => NotificationType::Info,
                NoticeLevel::Warning => NotificationType::Warning,
            };
            notify(&item.text, kind);
        } else {
            log(&item.text);
        }
    }
}

/// Holds the computed notices until the first `session_start` takes them.
#[derive(Default)]
pub struct NoticeQueue {
    pending: Mutex<Option<Vec<StartupNotice>>>,
}

impl NoticeQueue {
    pub fn new(notices: Vec<StartupNotice>) -> Self {
        Self {
            pending: Mutex::new(Some(notices)),
        }
    }

    /// The notices on the first call, nothing afterwards.
    pub fn take(&self) -> Vec<StartupNotice> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap_or_default()
    }
}

pub type MigrationRunner =
    Arc<dyn Fn(&Path, &Env, Option<&Path>) -> StartupMigration + Send + Sync>;
pub type ConfigLoader = Arc<dyn Fn(&Path, &Env) -> LoadResult + Send + Sync>;

/// The env handed to the loader, with `HOME` and `USERPROFILE` both set when a home is known.
pub fn startup_env(mut env: Env) -> (Env, Option<PathBuf>) {
    let home = env
        .get("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| env.get("USERPROFILE").filter(|v| !v.is_empty()))
        .cloned();
    if let Some(home) = &home {
        env.entry("HOME".into()).or_insert_with(|| home.clone());
        env.entry("USERPROFILE".into())
            .or_insert_with(|| home.clone());
    }
    (env, home.map(PathBuf::from))
}

/// Computes the startup notices for `cwd` with an injectable migration runner and loader.
pub fn startup_notices(
    cwd: &Path,
    env: Env,
    migrate: &MigrationRunner,
    load: &ConfigLoader,
) -> Vec<StartupNotice> {
    let (env, home) = startup_env(env);
    let migration = migrate(cwd, &env, home.as_deref());
    let loaded = load(cwd, &env);
    compute_notices(&migration, &loaded)
}

/// The startup-notices extension.
#[derive(Clone)]
pub struct ConfigNotices {
    pub migrate: MigrationRunner,
    pub load: ConfigLoader,
    pub env: Option<Env>,
}

impl Default for ConfigNotices {
    fn default() -> Self {
        Self {
            migrate: Arc::new(run_startup_migration),
            load: Arc::new(load_native),
            env: None,
        }
    }
}

impl Extension for ConfigNotices {
    fn register(&self, api: &mut ExtensionApi) {
        let env = self.env.clone().unwrap_or_else(crate::env::process_env);
        let notices = startup_notices(&api.cwd, env, &self.migrate, &self.load);
        let queue = Arc::new(NoticeQueue::new(notices));
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                let queue = Arc::clone(&queue);
                let ui = Arc::clone(&ctx.ui);
                let has_ui = ctx.has_ui;
                Box::pin(async move {
                    let notices = queue.take();
                    deliver(
                        &notices,
                        has_ui,
                        &|text, kind| ui.notify(text, kind),
                        &|text| maho_ext_api::diagnostic(text),
                    );
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
