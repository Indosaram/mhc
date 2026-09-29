/// One known `task` tool failure signature and the hint that fixes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelegateTaskErrorPattern {
    pub pattern: &'static str,
    pub error_type: &'static str,
    pub fix_hint: &'static str,
}

/// Known `task` tool error signatures, checked in order.
pub const DELEGATE_TASK_ERROR_PATTERNS: &[DelegateTaskErrorPattern] = &[
    DelegateTaskErrorPattern {
        pattern: "run_in_background",
        error_type: "missing_run_in_background",
        fix_hint: "Add run_in_background=false (for delegation) or run_in_background=true (for parallel exploration)",
    },
    DelegateTaskErrorPattern {
        pattern: "load_skills",
        error_type: "missing_load_skills",
        fix_hint: "Add load_skills=[] parameter (empty array if no skills needed). Note: Calling Skill tool does NOT populate this.",
    },
    DelegateTaskErrorPattern {
        pattern: "category OR subagent_type",
        error_type: "mutual_exclusion",
        fix_hint: "Provide ONLY one of: category (e.g., 'general', 'quick') OR subagent_type (e.g., 'oracle', 'explore')",
    },
    DelegateTaskErrorPattern {
        pattern: "Must provide either category or subagent_type",
        error_type: "missing_category_or_agent",
        fix_hint: "Add either category='general' OR subagent_type='explore'",
    },
    DelegateTaskErrorPattern {
        pattern: "Unknown category",
        error_type: "unknown_category",
        fix_hint: "Use a valid category from the Available list in the error message",
    },
    DelegateTaskErrorPattern {
        pattern: "Agent name cannot be empty",
        error_type: "empty_agent",
        fix_hint: "Provide a non-empty subagent_type value",
    },
    DelegateTaskErrorPattern {
        pattern: "Unknown agent",
        error_type: "unknown_agent",
        fix_hint: "Use a valid agent from the Available agents list in the error message",
    },
    DelegateTaskErrorPattern {
        pattern: "Cannot call primary agent",
        error_type: "primary_agent",
        fix_hint: "Primary agents cannot be called via task. Use a subagent like 'explore', 'oracle', or 'librarian'",
    },
    DelegateTaskErrorPattern {
        pattern: "Skills not found",
        error_type: "unknown_skills",
        fix_hint: "Use valid skill names from the Available list in the error message",
    },
];

/// `{ errorType; originalOutput }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedError {
    pub error_type: String,
    pub original_output: String,
}

/// Classifies a failed `task` tool output; `None` when it is not a recognised error.
#[must_use]
pub fn detect_delegate_task_error(output: &str) -> Option<DetectedError> {
    if !output.contains("[ERROR]") && !output.contains("Invalid arguments") {
        return None;
    }

    DELEGATE_TASK_ERROR_PATTERNS
        .iter()
        .find(|entry| output.contains(entry.pattern))
        .map(|entry| DetectedError {
            error_type: entry.error_type.to_string(),
            original_output: output.to_string(),
        })
}
