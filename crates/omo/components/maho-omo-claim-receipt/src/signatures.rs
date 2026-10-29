//! Port of `claim-receipt/signatures.ts`: judge a check's output, a structured result and whether an
//! edited path is code at all.

use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;

use crate::evidence::CheckOutcome;

fn pattern(source: &str) -> FancyRegex {
    FancyRegex::new(source).expect("valid signature regex")
}

static FAIL_SIGNATURES: LazyLock<Vec<FancyRegex>> = LazyLock::new(|| {
    [
        r"(?i)(?<![0-9])[1-9]\d*\s+fail(?:ed|ures?)?\b(?![=:])",
        r"(?i)\bfail(?:ed|ures?)?=(?:[1-9]|\[[^\]\s])",
        r"(?m)^\(fail\)",
        r"\bFAIL\b",
        r"\berror TS\d+",
        r"AssertionError",
        r"(?im)^\s*error(?:\[[^\]]*\])?:",
        r"\b[A-Z_]*EXIT[=:]\s*[1-9]\d*\b",
        r"(?i)\bexit(?:ed)?(?: with)?(?: code| status)?\s*[:=]?\s*[1-9]\d*\b",
        r"\bTests?:?\s+[1-9]\d* failed",
        r"\bTest Files\s+[1-9]\d* failed",
        r#""exitCode":\s*[1-9]"#,
        r"\bFound [1-9]\d* errors?\b",
        r"Files with errors:\s*[1-9]",
        "\tfail\t",
    ]
    .into_iter()
    .map(pattern)
    .collect()
});

static UNSETTLED_SIGNATURES: LazyLock<Vec<FancyRegex>> = LazyLock::new(|| {
    [r"\t(?:pending|queued|in_progress)\t"]
        .into_iter()
        .map(pattern)
        .collect()
});

static PASS_SIGNATURES: LazyLock<Vec<FancyRegex>> = LazyLock::new(|| {
    [
        r"(?i)(?<![0-9])0\s+fail(?:ed|ures?)?\b",
        r"(?i)\bfail(?:ed|ures?)?=(?:0\b|\[\])",
        r"(?i)(?<![0-9])[1-9]\d*\s+pass(?:ed)?\b",
        r"\bTests?:?\s+\d+ passed",
        r"\bTest Files\s+\d+ passed",
        r#""exitCode":\s*0\b"#,
        r"(?i)\bAll tests passed\b",
        r"\b[A-Z_]*EXIT[=:]\s*0\b",
        r"(?i)\bexit(?:ed)?(?: with)?(?: code| status)?\s*[:=]?\s*0\b",
        r"\bFound 0 errors\b",
        r"\bNo diagnostics found\b",
        r"Files with errors:\s*0\b",
        r"\bNo fixes applied\b",
        "\tpass\t",
    ]
    .into_iter()
    .map(pattern)
    .collect()
});

static HARNESS_REFUSAL: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern(r"(?i)\bTool \S+ not found\b|\bpermission denied by\b|\b(?:was|were) denied\b|\bblocked by (?:policy|the user|a hook)\b|\brequires approval\b|\b(?:aborted|cancelled) by the user\b")
});

static NON_CODE_PATH: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern(r"(?i)(?:^|/)(?:\.omo|omo-runs|tmp|evidence|notepads?)/|^/(?:private/)?tmp/|\.(?:md|txt|log|jsonl|csv)$")
});

fn matches_any(patterns: &[FancyRegex], text: &str) -> bool {
    patterns
        .iter()
        .any(|pattern| pattern.is_match(text).unwrap_or(false))
}

/// A structured run that errored is a failed check only if it actually ran: a harness refusal is no
/// evidence.
#[must_use]
pub fn judge_structured(errored: bool, text: &str) -> CheckOutcome {
    if !errored {
        return judge_output(text);
    }
    if HARNESS_REFUSAL.is_match(text).unwrap_or(false) {
        CheckOutcome::Unknown
    } else {
        CheckOutcome::Failed
    }
}

#[must_use]
pub fn judge_output(text: &str) -> CheckOutcome {
    if matches_any(FAIL_SIGNATURES.as_slice(), text) {
        return CheckOutcome::Failed;
    }
    if matches_any(UNSETTLED_SIGNATURES.as_slice(), text) {
        return CheckOutcome::Unknown;
    }
    if matches_any(PASS_SIGNATURES.as_slice(), text) {
        return CheckOutcome::Passed;
    }
    CheckOutcome::Unknown
}

#[must_use]
pub fn is_relevant_path(path: &str) -> bool {
    !NON_CODE_PATH.is_match(path).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_outranks_pending_which_outranks_pass() {
        assert_eq!(
            judge_output(" Test Files  3 passed (3)\n      Tests  8 passed (8)"),
            CheckOutcome::Passed
        );
        assert_eq!(
            judge_output("build\tpass\t1m\thttps://x\ntest\tpending\t0\thttps://y"),
            CheckOutcome::Unknown
        );
        assert_eq!(
            judge_output("CI_SETTLED total=46 fail=0"),
            CheckOutcome::Passed
        );
        assert_eq!(
            judge_output("CI_SETTLED #1197 fail=[lint] #1198 fail=[]"),
            CheckOutcome::Failed
        );
    }

    #[test]
    fn a_harness_refusal_is_unknown_while_a_real_failure_is_failed() {
        assert_eq!(
            judge_structured(true, "Tool bash not found. Run bash inside an eval cell."),
            CheckOutcome::Unknown
        );
        assert_eq!(
            judge_structured(true, "Permission denied by policy: bash"),
            CheckOutcome::Unknown
        );
        assert_eq!(judge_structured(true, " 2 pass\n 1 fail"), CheckOutcome::Failed);
    }

    #[test]
    fn notes_evidence_and_scratch_paths_are_not_code() {
        assert!(is_relevant_path("src/a.ts"));
        assert!(!is_relevant_path(".omo/evidence/run/summary.md"));
        assert!(!is_relevant_path("/Users/Shared/omo-runs/lane/rerun.sh"));
        assert!(!is_relevant_path("notes/RESULT.md"));
    }
}
