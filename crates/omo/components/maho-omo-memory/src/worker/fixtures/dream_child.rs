use std::{collections::BTreeMap, path::Path};
pub async fn run(env: &BTreeMap<String, String>) -> Result<(), String> {
    let required = |name: &str| env.get(name).filter(|value| !value.is_empty()).ok_or_else(|| format!("{name} is required"));
    let memory = Path::new(required("MEMORY_DIR")?);
    let transcript: serde_json::Value = super::super::run_artifacts::read_run_json(Path::new(required("TRANSCRIPT_PATH")?)).map_err(|error| error.to_string())?;
    let policy: serde_json::Value = super::super::run_artifacts::read_run_json(Path::new(required("DREAM_POLICY_PATH")?)).map_err(|error| error.to_string())?;
    if env.get("SENPI_MEMORY_REFLECTION").is_none_or(|value| value != "1") { return Err("reflection sentinel is required".into()); }
    if transcript["request"]["trigger"] != "dream" || transcript["request"]["origin"] != "manual" { return Err("dream request payload is required".into()); }
    let empty = env.get("DREAM_FIXTURE_EMPTY_LEDGERS").is_some_and(|value| value == "1");
    let skills: serde_json::Value = super::super::run_artifacts::read_run_json(Path::new(required("SKILLS_USAGE_PATH")?)).map_err(|error| error.to_string())?;
    let state: serde_json::Value = super::super::run_artifacts::read_run_json(Path::new(required("DREAM_STATE_PATH")?)).map_err(|error| error.to_string())?;
    let expected_skills = if empty { serde_json::json!({}) } else { serde_json::json!({"review":{"count":2,"lastUsedAt":"2026-08-01T00:00:00.000Z"}}) };
    let expected_state = if empty { serde_json::json!({}) } else { serde_json::json!({"last_dream_at":"2026-07-01T00:00:00.000Z","lastRunId":"prior-run"}) };
    if skills != expected_skills { return Err("skills usage payload mismatch".into()); }
    if state != expected_state { return Err("dream state payload mismatch".into()); }
    let mut changed = vec![];
    if let Some(target) = env.get("DREAM_TARGET_PATH") {
        let target = Path::new(target);
        std::fs::create_dir_all(target.parent().unwrap_or(Path::new("."))).map_err(|error| error.to_string())?;
        std::fs::write(target, "---\ndescription: Style\n---\nMaintained by dream.\n").map_err(|error| error.to_string())?;
        changed.push(target.to_string_lossy().into_owned());
    } else {
        std::fs::create_dir_all(memory.join("system")).map_err(|error| error.to_string())?;
        super::super::run_artifacts::write_run_json_atomic(&memory.join("system/dream-dispatch.json"), &policy, 0o600).map_err(|error| error.to_string())?;
        changed.push("system/dream-dispatch.json".into());
        if policy["people"]["enabled"] == true {
            let count = policy["people"]["max_entries"].as_u64().and_then(|count| usize::try_from(count).ok()).ok_or("invalid max_entries")?;
            let chars = policy["people"]["max_entry_chars"].as_u64().and_then(|chars| usize::try_from(chars).ok()).ok_or("invalid max_entry_chars")?;
            let entries: Vec<String> = (0..count).map(|index| format!("ATTRIBUTE: {:02} {}", index + 1, "x".repeat(chars)).chars().take(chars).collect()).collect();
            std::fs::create_dir_all(memory.join("people/fixture")).map_err(|error| error.to_string())?;
            std::fs::write(memory.join("people/fixture/card.md"), format!("{}\n", entries.join("\n"))).map_err(|error| error.to_string())?;
            changed.push("people/fixture/card.md".into());
        }
    }
    let output = tokio::process::Command::new("git").arg("add").arg("--").args(changed).current_dir(memory).output().await.map_err(|error| error.to_string())?;
    if !output.status.success() { return Err(String::from_utf8_lossy(&output.stderr).into_owned()); }
    let output = tokio::process::Command::new("git").args(["commit", "-m", "chore(dream): verify dispatch fixture"]).current_dir(memory).output().await.map_err(|error| error.to_string())?;
    if !output.status.success() { return Err(String::from_utf8_lossy(&output.stderr).into_owned()); }
    Ok(())
}
