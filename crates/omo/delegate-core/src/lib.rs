//! Rust port of the `@oh-my-opencode/delegate-core` package (SUL-1.0, internal use only).
//!
//! Delegated-task model selection (built on `model-core`) and `task` tool retry guidance.

mod model_selection;
mod retry_guidance;
mod retry_patterns;

pub use model_selection::DelegateFallbackEntry;
pub use model_selection::DelegateLog;
pub use model_selection::DelegateModelResolutionDeps;
pub use model_selection::DelegateModelResolutionInput;
pub use model_selection::DelegateModelResolutionResult;
pub use model_selection::ResolvedDelegateModel;
pub use model_selection::resolve_model_for_delegate_task;
pub use model_selection::transform_model_for_provider;
pub use retry_guidance::build_retry_guidance;
pub use retry_patterns::DELEGATE_TASK_ERROR_PATTERNS;
pub use retry_patterns::DelegateTaskErrorPattern;
pub use retry_patterns::DetectedError;
pub use retry_patterns::detect_delegate_task_error;
