pub fn reflection_remediation(reason: Option<&str>, detail: Option<&str>) -> &'static str {
    let combined = format!("{} {}", reason.unwrap_or(""), detail.unwrap_or("")).to_lowercase();
    if combined.contains("budget_not_met") { return "run /dream again and trim or demote the largest system/ files until the committed estimate is below $SYSTEM_TOKEN_TARGET"; }
    if combined.contains("category_unavailable") || combined.contains("could not resolve a usable model") {
        return "no connected provider offers a model for the memory reflection category; run /login <provider>, or pin categories.<category>.model (or memory.reflection.category) in omo.json";
    }
    let quoted_model_miss = combined.match_indices("model").any(|(index, _)| {
        let rest = &combined[index + 5..];
        let trimmed = rest.trim_start();
        if trimmed.len() == rest.len() { return false; }
        let Some(quoted) = trimmed.strip_prefix('"') else { return false; };
        let Some(end) = quoted.find('"') else { return false; };
        if end == 0 { return false; }
        let rest = &quoted[end + 1..];
        rest.len() != rest.trim_start().len() && rest.trim_start().starts_with("not found")
    });
    if ["model-not-found", "model_not_visible", "model not found"].iter().any(|pattern| combined.contains(pattern)) || quoted_model_miss {
        return "the reflection child cannot see the configured category model; adjust memory.reflection category/model in your omo config";
    }
    if combined.contains("bwrap:") || combined.contains("setting up uid map") || combined.contains("setting up gid map") || combined.contains("setting up namespace") {
        return "the sandbox helper (bwrap) cannot create a user namespace on this host; set memory.reflection.sandbox to \"off\" in your omo config, or allow unprivileged user namespaces on the host";
    }
    // Model admission never reached an executable: the roster of tried models ends the detail.
    let bytes = combined.as_bytes();
    if combined.match_indices("attempted:").any(|(index, _)| index == 0 || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_')) {
        if combined.contains("provider_unavailable") { return "every model for the memory reflection category was refused by its provider (rate limit or outage); automatic reflection resumes at the next probe, or run /reflect once the provider recovers"; }
        if combined.contains("context_overflow") { return "the reflection prompt outgrew the context window of every candidate model; run /dream to trim system memory, or point memory.reflection at a larger-context model"; }
        if combined.contains("auth_missing") || combined.contains("api key") { return "run /login <provider>"; }
        return "no model for the memory reflection category could be admitted; the models that were tried are named in the failure detail";
    }
    if combined.contains("spawn") || combined.contains("enoent") {
        return "senpi executable not resolvable for the reflection child; set SENPI_BIN";
    }
    if combined.contains("api key") || combined.contains("auth_missing") { return "run /login <provider>"; }
    if combined.contains("cannot lock mhc") {
        return "an mhc session started before the last update is still running an old version; restart that session";
    }
    "inspect runtime/reflection/runs/<runId>/child-stderr.log"
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn category_failure_precedes_generic_spawn_detail(){assert_eq!(reflection_remediation(Some("category_unavailable"),Some("spawn failed")),reflection_remediation(Some("category_unavailable"),None));assert_ne!(reflection_remediation(Some("category_unavailable"),None),reflection_remediation(None,None));}
    #[test]fn quoted_child_model_miss_matches_taxonomy(){assert_eq!(reflection_remediation(Some("child_exit"),Some("Error: Model \"p/m\" not found. Use --list-models")),reflection_remediation(Some("model_not_visible"),None));}
    #[test]fn existing_model_miss_variants_select_same_remediation(){for reason in ["model-not-found","model_not_visible","Model not found: p/m"]{assert_eq!(reflection_remediation(Some(reason),None),reflection_remediation(Some("model_not_visible"),None));}assert_ne!(reflection_remediation(Some("model_not_visible"),None),reflection_remediation(None,None));}
    #[test]fn spawn_and_missing_executable_select_same_remediation(){assert_eq!(reflection_remediation(Some("spawn_failed"),None),reflection_remediation(Some("child_exit"),Some("execvp ENOENT")));assert_ne!(reflection_remediation(Some("spawn_failed"),None),reflection_remediation(None,None));}
    #[test]fn stale_version_lock_failure_selects_restart_remediation(){let stale=reflection_remediation(Some("child_exit"),Some("cannot lock mhc 2026.10.22: Read-only file system (os error 30)"));assert_ne!(stale,reflection_remediation(None,None));assert!(stale.contains("restart"));}
    #[test]fn generic_failure_points_at_the_reflection_run_directory(){assert!(reflection_remediation(None,None).contains("runtime/reflection/runs/<runId>/child-stderr.log"));}
    #[test]fn generic_failure_uses_default_and_auth_stays_distinct(){assert_eq!(reflection_remediation(Some("child_exit"),Some("exit code 1")),reflection_remediation(None,None));assert_eq!(reflection_remediation(Some("auth_missing"),None),reflection_remediation(None,Some("No API key")));assert_ne!(reflection_remediation(Some("auth_missing"),None),reflection_remediation(None,None));}
    #[test]fn budget_namespace_and_model_exhaustion_have_specific_hints(){assert!(reflection_remediation(Some("budget_not_met"),None).contains("$SYSTEM_TOKEN_TARGET"));assert!(reflection_remediation(None,Some("bwrap: setting up uid map")).contains("user namespace"));assert!(reflection_remediation(Some("spawn_failed"),Some("provider_unavailable:p/a; attempted:p/a")).contains("refused by its provider"));assert!(reflection_remediation(Some("spawn_failed"),Some("context_overflow:p/a; attempted:p/a")).contains("context window"));assert!(reflection_remediation(Some("spawn_failed"),Some("attempted:p/a,p/b")).contains("could be admitted"));assert_eq!(reflection_remediation(Some("spawn_failed"),Some("auth_missing:p; attempted:p/a")),"run /login <provider>");}
}
