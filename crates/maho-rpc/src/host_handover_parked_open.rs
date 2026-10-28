//! The bridge between a handoff-parked session and the host's one open implementation (senpi
//! `modes/rpc/host-handover-parked-open.ts`, senpi#3368). A park records the client's `open_session` choices from
//! the session's launch profile; the successor reopens it by replaying exactly those choices as an
//! `open_session`, so the reopened session gets everything any client's open gets: validation, the host-context
//! merge, its kind registered with the event writer, its binding, and the drain's settle wake-up.
use crate::host_handover_parked::HandoverParkedEntry;
use crate::host_handover_parked::ParkedModel;
use crate::session_registry::RpcSessionLaunchProfile;
use serde_json::{Map, Value, json};

const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Launch choices a caller knows beyond the profile (`promptSurface`, `browserEngine`, `retryFallback`); a value
/// here overrides the profile's own, an absent one falls back to it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParkedLaunchExtras {
    pub prompt_surface: Option<String>,
    pub browser_engine: Option<String>,
    pub retry_fallback: Option<Value>,
}

/// The session fields `parkedEntryFromProfile` reads off the live session.
#[derive(Debug, Clone, Default)]
pub struct ParkedSession<'a> {
    pub session_path: &'a str,
    pub durable_session_id: Option<&'a str>,
    pub context_overlay: Option<&'a Value>,
}

fn kind_name(kind: maho_ext_api::SessionKind) -> &'static str {
    match kind {
        maho_ext_api::SessionKind::Interactive => "interactive",
        maho_ext_api::SessionKind::Worker => "worker",
    }
}

/// The park record of a retained session. `host_context_keys` are the context keys THIS host merged in on
/// open: they describe this generation and are stripped, so the successor merges its own.
pub fn parked_entry_from_profile(
    session: &ParkedSession<'_>,
    profile: Option<&RpcSessionLaunchProfile>,
    extras: &ParkedLaunchExtras,
    host_context_keys: &[&str],
) -> HandoverParkedEntry {
    let client_context = profile
        .and_then(|profile| profile.session_context.as_ref())
        .map(|context| {
            context
                .iter()
                .filter(|(key, _)| !host_context_keys.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                .collect::<Map<String, Value>>()
        });
    let runtime = profile.map(|profile| &profile.runtime);
    HandoverParkedEntry {
        session_path: session.session_path.to_owned(),
        durable_session_id: session.durable_session_id.map(str::to_owned),
        retain_on_disconnect: true,
        context_overlay: session.context_overlay.cloned(),
        // `profile?.cwd`: the profile's cwd is a required string, so it is present whenever a profile is.
        cwd: runtime.map(|runtime| runtime.cwd.clone()),
        model: runtime
            .and_then(|runtime| runtime.creation_model.as_ref())
            .map(|(provider, model_id)| ParkedModel {
                provider: provider.clone(),
                model_id: model_id.clone(),
            }),
        thinking_level: runtime.and_then(|runtime| runtime.initial_thinking_level.clone()),
        permission_preset: runtime.and_then(|runtime| runtime.permission_preset.clone()),
        prompt_surface: extras
            .prompt_surface
            .clone()
            .or_else(|| profile.and_then(|profile| profile.prompt_surface.clone())),
        browser_engine: extras
            .browser_engine
            .clone()
            .or_else(|| profile.and_then(|profile| profile.browser_engine.clone())),
        session_kind: profile
            .and_then(|profile| profile.session_kind)
            .map(|kind| kind_name(kind).to_owned()),
        session_context: client_context
            .filter(|context| !context.is_empty())
            .map(Value::Object),
        auto_title: runtime.and_then(|runtime| runtime.auto_title),
        retry_fallback: extras
            .retry_fallback
            .clone()
            .or_else(|| profile.and_then(|profile| profile.retry_fallback.clone())),
        parked_by: None,
    }
}

/// The `open_session` a parked entry replays on the successor, in its wire form.
pub fn parked_open_command(entry: &HandoverParkedEntry) -> Value {
    let thinking_level = entry
        .thinking_level
        .as_deref()
        .filter(|level| THINKING_LEVELS.contains(level));
    if let Some(level) = entry.thinking_level.as_deref()
        && thinking_level.is_none()
    {
        maho_core::output_guard::maho_write_stderr(&format!(
            "senpi rpc successor reopens {} without its unknown thinking level \"{level}\"\n",
            entry.session_path
        ));
    }
    let mut command = Map::new();
    command.insert("type".into(), json!("open_session"));
    command.insert("sessionPath".into(), json!(entry.session_path));
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            command.insert(key.into(), value);
        }
    };
    put("cwd", entry.cwd.as_ref().map(|cwd| json!(cwd)));
    put(
        "durableSessionId",
        entry.durable_session_id.as_ref().map(|id| json!(id)),
    );
    put(
        "provider",
        entry.model.as_ref().map(|model| json!(model.provider)),
    );
    put(
        "modelId",
        entry.model.as_ref().map(|model| json!(model.model_id)),
    );
    put("thinkingLevel", thinking_level.map(|level| json!(level)));
    put(
        "permissionPreset",
        entry.permission_preset.as_ref().map(|preset| json!(preset)),
    );
    put(
        "promptSurface",
        entry.prompt_surface.as_ref().map(|surface| json!(surface)),
    );
    put(
        "browserEngine",
        entry.browser_engine.as_ref().map(|engine| json!(engine)),
    );
    put("kind", entry.session_kind.as_ref().map(|kind| json!(kind)));
    put("context", entry.session_context.clone());
    put("auto_title", entry.auto_title.map(|title| json!(title)));
    put("retryFallback", entry.retry_fallback.clone());
    command.insert(
        "retain_on_disconnect".into(),
        json!(entry.retain_on_disconnect),
    );
    Value::Object(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn profile() -> RpcSessionLaunchProfile {
        RpcSessionLaunchProfile {
            runtime: maho_core::agent_session_runtime::AgentSessionLaunchProfile {
                cwd: "/work".into(),
                permission_preset: Some("ask".into()),
                creation_model: Some(("anthropic".into(), "m".into())),
                initial_thinking_level: Some("high".into()),
                auto_title: Some(false),
                prompt_size_inventory: None,
            },
            session_path: None,
            durable_session_id: None,
            session_kind: Some(maho_ext_api::SessionKind::Worker),
            session_context: Some(BTreeMap::from([
                ("lane".to_owned(), "l1".to_owned()),
                ("host_instance".to_owned(), "old-host".to_owned()),
                ("host_socket".to_owned(), "/old.sock".to_owned()),
            ])),
            attended: None,
            prompt_surface: Some("app".into()),
            browser_engine: Some("builtin".into()),
            retry_fallback: None,
            holder_endpoint: None,
        }
    }

    fn extras() -> ParkedLaunchExtras {
        ParkedLaunchExtras {
            prompt_surface: Some("app".into()),
            browser_engine: Some("builtin".into()),
            retry_fallback: None,
        }
    }

    fn session() -> ParkedSession<'static> {
        ParkedSession {
            session_path: "/s.jsonl",
            durable_session_id: Some("d-1"),
            context_overlay: None,
        }
    }

    const HOST_KEYS: [&str; 2] = ["host_instance", "host_socket"];

    #[test]
    fn records_the_clients_choices_and_strips_this_hosts_own_context_keys() {
        let entry = parked_entry_from_profile(&session(), Some(&profile()), &extras(), &HOST_KEYS);
        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            json!({
                "sessionPath": "/s.jsonl",
                "durableSessionId": "d-1",
                "retainOnDisconnect": true,
                "cwd": "/work",
                "model": {"provider": "anthropic", "modelId": "m"},
                "thinkingLevel": "high",
                "permissionPreset": "ask",
                "promptSurface": "app",
                "browserEngine": "builtin",
                "sessionKind": "worker",
                "sessionContext": {"lane": "l1"},
                "autoTitle": false,
            })
        );
    }

    #[test]
    fn replays_them_as_the_open_session_a_client_would_send() {
        let entry = parked_entry_from_profile(&session(), Some(&profile()), &extras(), &HOST_KEYS);
        assert_eq!(
            parked_open_command(&entry),
            json!({
                "type": "open_session",
                "sessionPath": "/s.jsonl",
                "cwd": "/work",
                "durableSessionId": "d-1",
                "provider": "anthropic",
                "modelId": "m",
                "thinkingLevel": "high",
                "permissionPreset": "ask",
                "promptSurface": "app",
                "browserEngine": "builtin",
                "kind": "worker",
                "context": {"lane": "l1"},
                "auto_title": false,
                "retain_on_disconnect": true,
            })
        );
    }

    #[test]
    fn drops_a_context_left_with_only_host_owned_keys_and_an_unknown_thinking_level() {
        let mut bare = profile();
        bare.runtime = maho_core::agent_session_runtime::AgentSessionLaunchProfile {
            cwd: "/w".into(),
            initial_thinking_level: Some("turbo".into()),
            ..Default::default()
        };
        bare.session_context = Some(BTreeMap::from([(
            "host_instance".to_owned(),
            "old".to_owned(),
        )]));
        let entry = parked_entry_from_profile(
            &ParkedSession {
                session_path: "/s.jsonl",
                ..Default::default()
            },
            Some(&bare),
            &ParkedLaunchExtras::default(),
            &["host_instance"],
        );
        assert_eq!(entry.session_context, None);
        assert_eq!(parked_open_command(&entry).get("thinkingLevel"), None);
    }

    #[test]
    fn carries_the_contexts_overlay_into_the_entry() {
        let overlay = json!({"task_status": "running"});
        let entry = parked_entry_from_profile(
            &ParkedSession {
                session_path: "/s.jsonl",
                context_overlay: Some(&overlay),
                ..Default::default()
            },
            None,
            &ParkedLaunchExtras::default(),
            &[],
        );
        assert_eq!(entry.context_overlay, Some(overlay));
        assert_eq!(entry.cwd, None);
    }
}
