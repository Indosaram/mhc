//! The `omo.jsonc` half of applying a default model (spec §4.2), through the comment-preserving
//! `maho-config` writer.

use maho_config::{Edit, Env, WriteOptions, WriteOutcome, WriteTarget, update_config};
use serde_json::json;

/// Name of the model profile setup writes.
pub const PROFILE_NAME: &str = "setup";

/// The edits that make `provider/model` the session-start model profile.
pub fn model_profile_edits(provider: &str, model: &str) -> Vec<Edit> {
    vec![
        Edit::set(
            &["model_profiles", PROFILE_NAME, "models"],
            json!([format!("{provider}/{model}")]),
        ),
        Edit::set(&["model_profile"], json!(PROFILE_NAME)),
    ]
}

/// Writes the edits into the user `omo.jsonc` of `env`'s home.
pub fn write_model_profile(env: &Env, provider: &str, model: &str) -> Result<WriteOutcome, String> {
    update_config(
        &model_profile_edits(provider, model),
        &WriteOptions::new(WriteTarget::User, env.clone()),
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_for(home: &std::path::Path) -> Env {
        let mut env = Env::new();
        env.insert("HOME".into(), home.to_string_lossy().into_owned());
        env
    }

    #[test]
    fn writes_profile_into_a_new_file() {
        let dir = tempfile::tempdir().expect("temp");
        let outcome = write_model_profile(&env_for(dir.path()), "offline", "m1").expect("write");
        assert!(outcome.backup.is_none());
        let text = std::fs::read_to_string(dir.path().join(".omo/omo.jsonc")).expect("file");
        let value = maho_support::parse_jsonc_lenient(&text).value.expect("parse");
        assert_eq!(value["model_profile"], "setup");
        assert_eq!(value["model_profiles"]["setup"]["models"][0], "offline/m1");
    }

    #[test]
    fn keeps_comments_and_other_keys_and_backs_up() {
        let dir = tempfile::tempdir().expect("temp");
        std::fs::create_dir_all(dir.path().join(".omo")).expect("dir");
        std::fs::write(
            dir.path().join(".omo/omo.jsonc"),
            "{\n  // keep me\n  \"telemetry\": { \"enabled\": true }\n}\n",
        )
        .expect("seed");
        let outcome = write_model_profile(&env_for(dir.path()), "p", "m").expect("write");
        assert!(outcome.backup.is_some());
        let text = std::fs::read_to_string(dir.path().join(".omo/omo.jsonc")).expect("file");
        assert!(text.contains("// keep me"));
        let value = maho_support::parse_jsonc_lenient(&text).value.expect("parse");
        assert_eq!(value["telemetry"]["enabled"], true);
        assert_eq!(value["model_profiles"]["setup"]["models"][0], "p/m");
    }
}
