use maho_core::agent_session::AgentSession;
use std::sync::{Arc, Mutex, OnceLock};

type SessionSource = Arc<dyn Fn() -> Option<AgentSession> + Send + Sync>;
#[derive(Default)]
struct Registry {
    sources: Vec<SessionSource>,
    signal_installed: bool,
}
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}
pub fn enabled() -> bool {
    maho_core::brand::env_value("MEMORY_REPORT", &maho_core::config::current_env()).as_deref()
        == Some("1")
}
pub fn sessions() -> Vec<AgentSession> {
    let mut registry = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut sessions = Vec::new();
    registry.sources.retain(|read| match read() {
        Some(session) => {
            sessions.push(session);
            true
        }
        None => false,
    });
    sessions
}
pub fn register_session(session: &AgentSession) {
    if !enabled() {
        return;
    }
    let mut state = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let source = session.weak_accessor();
    state.sources.push(source.clone());
    if !state.signal_installed {
        match super::memory_report_write::install_memory_report_signal() {
            Ok(()) => state.signal_installed = true,
            Err(error) => {
                maho_core::output_guard::maho_write_stderr(&format!(
                    "mhc memory report: {error}\n"
                ));
            }
        }
    }
    drop(state);
    session.add_tool_context_disposer(move || {
        registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sources
            .retain(|registered| !Arc::ptr_eq(registered, &source));
    });
}

#[cfg(windows)]
pub struct MemoryReportCommand;

#[cfg(windows)]
impl maho_ext_api::Extension for MemoryReportCommand {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        api.register_command(
            "memory-report",
            Some("Write memory reports for the current registered sessions (MEMORY_REPORT opt-in)".into()),
            None,
            Arc::new(|_, context| Box::pin(async move {
                use maho_ext_api::NotificationType;

                if !enabled() {
                    let names = maho_core::brand::brand_env_names(
                        "MEMORY_REPORT",
                        Some(&maho_core::brand::brand_profile()),
                    );
                    context.ui.notify(
                        &format!(
                            "Memory reporting is disabled: the effective MEMORY_REPORT setting must be exactly 1. Before starting mhc, set the first defined variable in this precedence list to 1 (or define the first variable as 1): {}. Restart mhc; sessions are registered only when opted in.",
                            names.join(", ")
                        ),
                        NotificationType::Warning,
                    );
                    return Ok(());
                }
                let sessions = sessions();
                if sessions.is_empty() {
                    context.ui.notify("No live registered session to report.", NotificationType::Warning);
                    return Ok(());
                }
                let outcome = super::memory_report_write::write_memory_reports(&sessions);
                for path in &outcome.paths {
                    context.ui.notify(
                        &format!("Memory report written: {}", path.display()),
                        NotificationType::Info,
                    );
                }
                let failed = sessions.len() - outcome.paths.len();
                if failed != 0 {
                    context.ui.notify(
                        &format!("{failed} memory report(s) not written; see the memory report errors on stderr."),
                        NotificationType::Error,
                    );
                }
                Ok(())
            })),
        );
    }
}
