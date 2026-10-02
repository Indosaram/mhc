use std::{collections::BTreeMap, path::Path};
pub fn run(mode: &str, env: &BTreeMap<String, String>) -> Result<i32, String> {
    let payload_path = env.get("FACTS_PAYLOAD_PATH").ok_or("facts paths are required")?;
    let extraction_path = env.get("FACTS_EXTRACTION_PATH").ok_or("facts paths are required")?;
    if env.get("SENPI_MEMORY_FACTS").is_none_or(|value| value != "1") { return Err("facts sentinel is required".into()); }
    let payload: serde_json::Value = super::super::run_artifacts::read_run_json(Path::new(payload_path)).map_err(|error| error.to_string())?;
    let output = match mode {
        "fact" => format!("{}\n", serde_json::json!({"scope":"project","text":format!("fixture consumed {} queue entries", payload["entries"].as_array().ok_or("entries must be an array")?.len()),"date":payload["today"]})),
        "person" => format!("{}\n", serde_json::json!({"scope":"person","person":{"name":"Mina","aliases":["Min"]},"text":"Mina prefers concise reviews.","date":payload["today"]})),
        "empty" => String::new(),
        "malformed" => "{\"scope\":\"project\",\"person\":{\"name\":\"Mina\",\"aliases\":[]},\"text\":\"bad\",\"date\":\"2026-08-10\"}\n".into(),
        "fail" => return Ok(7),
        "model-not-found" => {
            eprintln!("Error: Model \"extension-only/primary\" not found. Use --list-models to see available models.");
            return Ok(1);
        }
        _ => return Err(format!("unknown facts fixture mode: {mode}")),
    };
    std::fs::write(extraction_path, output).map_err(|error| error.to_string())?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_extraction_uses_payload_date_and_queue_count() {
        let root = tempfile::tempdir().unwrap();
        let payload = root.path().join("payload"); let extraction = root.path().join("extraction");
        super::super::super::run_artifacts::write_run_json_atomic(&payload, &serde_json::json!({"today":"2026-10-02","entries":[{},{}]}), 0o600).unwrap();
        let env = BTreeMap::from([("FACTS_PAYLOAD_PATH".into(),payload.to_string_lossy().into_owned()),("FACTS_EXTRACTION_PATH".into(),extraction.to_string_lossy().into_owned()),("SENPI_MEMORY_FACTS".into(),"1".into())]);
        assert_eq!(run("fact", &env).unwrap(), 0);
        let output: serde_json::Value = super::super::super::run_artifacts::read_run_json(&extraction).unwrap();
        assert_eq!(output["date"], "2026-10-02"); assert_eq!(output["scope"], "project");
        assert_eq!(output["text"], "fixture consumed 2 queue entries");
    }
}
