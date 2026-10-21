use super::plugin_identity::LOG_FILENAME;
use serde_json::Value;
use std::{path::PathBuf, sync::LazyLock};
pub use utils::logging::LoggerTestOverrides;
use utils::logging::{BoundLogger, LoggerOptions, create_logger};

static LOGGER: LazyLock<BoundLogger> =
    LazyLock::new(|| create_logger(LoggerOptions::new(LOG_FILENAME)));

pub fn log(message: &str, data: Option<&Value>) {
    LOGGER.log(message, data);
}
pub fn get_log_file_path() -> PathBuf {
    LOGGER.get_log_file_path()
}
pub fn set_logger_for_testing(overrides: &LoggerTestOverrides) {
    LOGGER.set_logger_for_testing(overrides);
}
pub fn reset_logger_for_testing() {
    LOGGER.reset_logger_for_testing();
}
pub fn flush_for_testing() {
    LOGGER.flush_for_testing();
}
