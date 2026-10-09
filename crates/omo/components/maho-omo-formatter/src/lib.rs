//! Root module for `maho-omo-formatter`.

pub mod formatter;

pub use formatter::{
    create_formatter_step, default_packaged_daemon_format, detect_formatter, format_one,
    is_git_ignored, language_for_path, normalize_resolved_path, resolve_format_mode,
    DaemonFormatFn, DaemonFormatResult, FormatMode, FormatMutationOutcome, FormatOnMutationConfig,
    FormatResult, FormatStatus, Formatter, FormatterLogger, FormatterStep, FormatterStepOptions,
    FormatterTool, MarkersFn, ReadMarkerFn, ResolveBinaryFn, DEFAULT_FORMAT_MODE,
    DEFAULT_MAX_FILE_BYTES, DEFAULT_TIMEOUT_MS,
};
