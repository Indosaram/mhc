use std::sync::LazyLock;

use regex::Regex;

use crate::retry_patterns::DELEGATE_TASK_ERROR_PATTERNS;
use crate::retry_patterns::DetectedError;

/// Mirrors the TS `/Available[^:]*:\s*(.+)$/m`; `R` makes `.`/`$` treat `\r` as a line end too.
#[expect(clippy::expect_used, reason = "static regex literal is valid")]
static AVAILABLE_LIST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?mR)Available[^:]*:\s*(.+)$").expect("valid regex"));

fn extract_available_list(output: &str) -> Option<&str> {
    AVAILABLE_LIST
        .captures(output)
        .and_then(|captures| captures.get(1))
        .map(|list| list.as_str().trim())
}

/// Builds the retry instructions injected after a failed `task` call.
#[must_use]
pub fn build_retry_guidance(error_info: &DetectedError) -> String {
    let Some(pattern) = DELEGATE_TASK_ERROR_PATTERNS
        .iter()
        .find(|entry| entry.error_type == error_info.error_type)
    else {
        return "[task ERROR] Fix the error and retry with correct parameters.".to_string();
    };

    let mut guidance = format!(
        "\n [task CALL FAILED - IMMEDIATE RETRY REQUIRED]\n\n **Error Type**: {}\n **Fix**: {}\n ",
        error_info.error_type, pattern.fix_hint
    );

    if let Some(available_list) = extract_available_list(&error_info.original_output) {
        guidance.push_str(&format!("\n**Available Options**: {available_list}\n"));
    }

    guidance.push_str(
        "\n **Action**: Retry task NOW with corrected parameters.\n\n Example of CORRECT call:\n ```\n task(\n   description=\"Task description\",\n   prompt=\"Detailed prompt...\",\n   category=\"unspecified-low\",  // OR subagent_type=\"explore\"\n   run_in_background=false,\n   load_skills=[]\n )\n ```\n ",
    );

    guidance
}
