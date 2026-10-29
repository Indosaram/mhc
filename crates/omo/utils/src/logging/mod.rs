//! Buffered, rotating file logger and product identity records.

mod logger;
mod product_identity;

pub use logger::{
    BoundLogger, DEFAULT_LOG_BUFFER_SIZE_LIMIT, DEFAULT_LOG_FLUSH_INTERVAL_MS,
    DEFAULT_MAX_LOG_FILE_BACKUPS, DEFAULT_MAX_LOG_FILE_SIZE_BYTES, LogPathResolver, LoggerOptions,
    LoggerTestOverrides, create_logger,
};
pub use product_identity::{ProductIdentity, ProductIdentityInput, create_product_identity};
