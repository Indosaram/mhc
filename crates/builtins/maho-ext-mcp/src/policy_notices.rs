use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};
#[derive(Default)]
pub struct PolicyNotices {
    state: Mutex<(BTreeSet<String>, BTreeMap<String, String>)>,
}
impl PolicyNotices {
    /// Security notices are keyed by declaration identity; warning logs deduplicate by text.
    pub fn notice(
        &self,
        identity: &str,
        server: &str,
        skill: &str,
        trusted: bool,
        names: &[String],
    ) -> (serde_json::Value, Option<String>) {
        let message = if names.is_empty() {
            String::new()
        } else if trusted {
            format!(
                "Project MCP server {server} (skill {skill}) withheld {} environment name(s): {}.",
                names.len(),
                names.join(", ")
            )
        } else {
            format!(
                "Skill '{skill}' MCP server '{server}': {} stays literal because the skill comes from an untrusted project; trust the project or move the server into your own mcp.json to expand it.",
                names
                    .iter()
                    .map(|name| format!("${{{name}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let warning = if names.is_empty() {
            state.1.remove(identity);
            None
        } else if state.0.insert(message.clone()) {
            state.1.insert(identity.into(), message.clone());
            Some(message.clone())
        } else {
            None
        };
        (
            serde_json::json!({"category":"mcp-env-withheld","path":identity,"message":message,"remove":names.is_empty()}),
            warning,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_identity_reuses_notice_and_grant_removes_without_relogging() {
        let notices = PolicyNotices::default();
        let names = vec!["TOKEN".into()];
        let (first, warning) = notices.notice("id", "server", "skill", false, &names);
        assert!(warning.is_some());
        assert_eq!(
            notices.notice("id", "server", "skill", false, &names),
            (first, None)
        );
        assert_eq!(
            notices.notice("id", "server", "skill", false, &[]).0["remove"],
            true
        );
        assert!(
            notices
                .notice("id", "server", "skill", false, &names)
                .1
                .is_none()
        );
    }
}
