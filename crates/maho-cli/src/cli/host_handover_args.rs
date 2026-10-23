//! Port of senpi `cli/host-handover-args.ts`.
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleHandoverTerms {
    pub operation_id: String,
    pub if_instance_id: String,
    pub if_generation: u64,
    pub target_runtime_build_id: String,
}

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// The conditional handover's terms: all five flags together, or a usage error naming what is wrong.
pub fn handover_terms(flags: &BTreeMap<String, String>) -> Result<IdleHandoverTerms, String> {
    let app = maho_core::config::app_name();
    let get = |name: &str| flags.get(name).filter(|value| !value.is_empty());
    if flags.get("--when").map(String::as_str) != Some("idle") {
        return Err(format!(
            "Error: \"{app} host handoff\" supports only --when idle."
        ));
    }
    let (Some(operation), Some(instance), Some(generation), Some(build)) = (
        get("--operation"),
        get("--if-instance"),
        get("--if-generation"),
        get("--target-build"),
    ) else {
        return Err(format!(
            "Error: \"{app} host handoff --when idle\" needs --operation, --if-instance, --if-generation and --target-build."
        ));
    };
    let if_generation = generation
        .chars()
        .all(|c| c.is_ascii_digit())
        .then(|| generation.parse::<u64>().ok())
        .flatten()
        .filter(|value| *value <= MAX_SAFE_INTEGER)
        .ok_or_else(|| "Error: --if-generation must be a non-negative integer.".to_owned())?;
    Ok(IdleHandoverTerms {
        operation_id: operation.clone(),
        if_instance_id: instance.clone(),
        if_generation,
        target_runtime_build_id: build.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    const FULL: [(&str, &str); 5] = [
        ("--when", "idle"),
        ("--operation", "op"),
        ("--if-instance", "i1"),
        ("--if-generation", "3"),
        ("--target-build", "sha256:x"),
    ];

    #[test]
    fn full_terms_parse() {
        let terms = handover_terms(&flags(&FULL)).ok();
        assert_eq!(terms.map(|t| t.if_generation), Some(3));
    }

    #[test]
    fn rejects_other_when_missing_terms_and_bad_generation() {
        let mut other = flags(&FULL);
        other.insert("--when".into(), "now".into());
        assert!(handover_terms(&other).is_err_and(|e| e.contains("supports only --when idle")));
        let mut missing = flags(&FULL);
        missing.remove("--operation");
        assert!(handover_terms(&missing).is_err_and(|e| e.contains("needs --operation")));
        let mut bad = flags(&FULL);
        bad.insert("--if-generation".into(), "-1".into());
        assert!(handover_terms(&bad).is_err_and(|e| e.contains("non-negative integer")));
    }
}
