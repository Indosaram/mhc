//! Process-wide shared sub-unit logger hook (no-op until configured).

use std::sync::{Arc, LazyLock, RwLock};

use serde_json::Value;

pub type SharedSubunitLogger = Arc<dyn Fn(&str, Option<&Value>) + Send + Sync>;

static SHARED_LOGGER: LazyLock<RwLock<Option<SharedSubunitLogger>>> =
    LazyLock::new(|| RwLock::new(None));

pub fn configure_shared_subunit_logger(logger: Option<SharedSubunitLogger>) {
    *SHARED_LOGGER
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = logger;
}

pub fn log(message: &str, data: Option<&Value>) {
    let logger = SHARED_LOGGER
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(logger) = logger {
        logger(message, data);
    }
}
