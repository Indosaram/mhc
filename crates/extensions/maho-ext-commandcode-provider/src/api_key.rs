use crate::contract::ProviderEnv;
use serde_json::Value;
use std::fs;

pub fn get_configured_api_key(env: &ProviderEnv) -> Option<String> {
    std::env::var("COMMAND_CODE_API_KEY")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("COMMANDCODE_API_KEY")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .or_else(|| api_key_from_files(env))
}

pub fn api_key_from_files(env: &ProviderEnv) -> Option<String> {
    let paths = [
        env.home.join(".commandcode/auth.json"),
        env.home.join(".pi/agent/auth.json"),
        env.home.join(".omp/agent/auth.json"),
    ];
    for path in paths {
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&contents) else {
            continue;
        };
        let Some(object) = value.as_object() else {
            continue;
        };
        for field in ["apiKey", "commandcode"] {
            if let Some(key) = object
                .get(field)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                return Some(key.to_owned());
            }
        }
        for field in ["commandcode", "command-code"] {
            let Some(credential) = object.get(field).and_then(Value::as_object) else {
                continue;
            };
            let kind = credential.get("type").and_then(Value::as_str);
            let key = match kind {
                Some("oauth") => credential.get("access"),
                Some("api") => credential.get("key"),
                _ => credential.get("access").or_else(|| credential.get("key")),
            };
            if let Some(key) = key.and_then(Value::as_str).filter(|s| !s.is_empty()) {
                return Some(key.to_owned());
            }
        }
    }
    None
}

pub fn is_placeholder(key: &str) -> bool {
    matches!(
        key,
        "$COMMAND_CODE_API_KEY"
            | "COMMAND_CODE_API_KEY"
            | "$COMMANDCODE_API_KEY"
            | "COMMANDCODE_API_KEY"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn env(home: PathBuf) -> ProviderEnv {
        ProviderEnv {
            api_base: String::new(),
            models_url: String::new(),
            models_timeout_ms: 1,
            cache_path: PathBuf::new(),
            cwd: PathBuf::new(),
            home,
        }
    }

    #[test]
    fn reads_auth_keys_and_skips_malformed_files() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join(".commandcode");
        fs::create_dir_all(&first).unwrap();
        fs::write(first.join("auth.json"), "{").unwrap();
        let second = dir.path().join(".pi/agent");
        fs::create_dir_all(&second).unwrap();
        fs::write(
            second.join("auth.json"),
            r#"{"commandcode":{"type":"oauth","access":"b"}}"#,
        )
        .unwrap();
        let third = dir.path().join(".omp/agent");
        fs::create_dir_all(&third).unwrap();
        fs::write(
            third.join("auth.json"),
            r#"{"command-code":{"type":"api","key":"c"}}"#,
        )
        .unwrap();
        assert_eq!(
            api_key_from_files(&env(dir.path().to_owned())),
            Some("b".to_owned())
        );
    }

    #[test]
    fn first_file_wins() {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in [
            (".commandcode", r#"{"apiKey":"a"}"#),
            (".pi/agent", r#"{"apiKey":"later"}"#),
        ] {
            let folder = dir.path().join(path);
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("auth.json"), contents).unwrap();
        }
        assert_eq!(
            api_key_from_files(&env(dir.path().to_owned())),
            Some("a".to_owned())
        );
    }
}
