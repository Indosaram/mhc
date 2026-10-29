//! Process-wide team-core logger (`setTeamCoreLogger` / `log`).

use std::sync::{Arc, PoisonError, RwLock};

use serde_json::Value;

pub type TeamCoreLog = Arc<dyn Fn(&str, Option<Value>) + Send + Sync>;

static ACTIVE_LOGGER: RwLock<Option<TeamCoreLog>> = RwLock::new(None);

pub fn set_team_core_logger(logger: TeamCoreLog) {
    *ACTIVE_LOGGER
        .write()
        .unwrap_or_else(PoisonError::into_inner) = Some(logger);
}

pub fn log(message: &str, data: Option<Value>) {
    let logger = ACTIVE_LOGGER
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(logger) = logger {
        logger(message, data);
    }
}

/// The active logger as an injectable function value.
#[must_use]
pub fn log_fn() -> TeamCoreLog {
    Arc::new(log)
}
