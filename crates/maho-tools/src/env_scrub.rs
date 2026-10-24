//! Secret scrubbing for model-run bash environments; the implementation lives in
//! `maho-process-env` so `maho-codemode` can share it without depending on `maho-tools`.
pub use maho_process_env::env_scrub::{is_secret_key, scrub_secrets};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn reexport_scrubs_secrets_and_honors_passthrough() {
        let mut env: BTreeMap<String, String> =
            [("GITHUB_TOKEN", "a"), ("NPM_TOKEN", "b"), ("KEEP", "c")]
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect();
        scrub_secrets(&mut env, &["NPM_TOKEN".to_owned()]);
        assert_eq!(
            env.keys().map(String::as_str).collect::<Vec<_>>(),
            ["KEEP", "NPM_TOKEN"]
        );
        assert!(is_secret_key("MY_API_KEY", &[]));
    }
}
