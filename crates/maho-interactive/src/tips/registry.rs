//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/registry.ts`.

use std::sync::LazyLock;

use super::catalog::cli_tips::CLI_TIPS;
use super::catalog::dag_tips::DAG_TIPS;
use super::catalog::ethos_tips::ETHOS_TIPS;
use super::catalog::input_tips::INPUT_TIPS;
use super::catalog::memory_tips::MEMORY_TIPS;
use super::catalog::model_tips::MODEL_TIPS;
use super::catalog::session_tips::SESSION_TIPS;
use super::catalog::settings_tips::SETTINGS_TIPS;
use super::catalog::subagent_tips::SUBAGENT_TIPS;
use super::catalog::types::TipDefinition;
use super::catalog::workspace_tips::WORKSPACE_TIPS;

pub use super::catalog::types::TipDefinition as Tip;

pub static TIP_DEFINITIONS: LazyLock<Vec<&'static TipDefinition>> = LazyLock::new(|| {
    let mut definitions = Vec::new();
    for group in [
        MODEL_TIPS,
        INPUT_TIPS,
        SESSION_TIPS,
        WORKSPACE_TIPS,
        SETTINGS_TIPS,
        CLI_TIPS,
        SUBAGENT_TIPS,
        MEMORY_TIPS,
        DAG_TIPS,
        ETHOS_TIPS,
    ] {
        definitions.extend(group.iter());
    }
    definitions
});
