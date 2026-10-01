pub fn reflection_remediation(reason: Option<&str>, detail: Option<&str>) -> &'static str {
    let combined = format!("{} {}", reason.unwrap_or(""), detail.unwrap_or("")).to_lowercase();
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
    if combined.contains("spawn") || combined.contains("enoent") {
        return "senpi executable not resolvable for the reflection child; set SENPI_BIN";
    }
    if combined.contains("api key") || combined.contains("auth_missing") { return "run /login <provider>"; }
    "inspect runtime/reflection-sessions/<runId>/child-stderr.log"
}
