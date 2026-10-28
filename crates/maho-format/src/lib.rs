//! Format after edit (spec `docs/cleanroom/specs/post-edit-format.md`).
//!
//! After `write`, `edit` or `apply_patch` succeeds, every file the call changed is run through the
//! project's formatter; when the file content changed, a block telling the model so is appended to
//! the tool result.
//!
//! - [`paths`]: which files a tool call changed, resolved and normalized.
//! - [`settings`]: the `formatOnMutation` keys.
//! - [`detect`]: formatter choice from the markers in the session cwd, Bun substitution.
//! - [`run`]: binary lookup, running a formatter, line counts, shared runs per path.
//! - [`lsp`]: the optional language-server formatting path.
//! - [`engine`]: one tool result end to end.
//! - [`extension`]: the `tool_result` handler on the maho extension API.

pub mod detect;
pub mod engine;
pub mod extension;
pub mod lsp;
pub mod paths;
pub mod run;
pub mod settings;

pub use detect::FormatterKind;
pub use engine::{FormatEngine, FormatReport, FormatRequest};
pub use extension::FormatExtension;
pub use lsp::{LspAnswer, LspFormat};
pub use run::RunOutcome;
pub use settings::{Mode, Settings};

/// Tools whose results are formatted.
pub const MUTATION_TOOLS: [&str; 3] = ["write", "edit", "apply_patch"];
