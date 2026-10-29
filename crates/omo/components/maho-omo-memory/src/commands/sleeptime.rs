//! `/sleeptime -- display the resolved memory settings for the bound identity.
//! Port of `components/memory/commands/sleeptime.ts` at pin 77f3067f1.

use std::sync::Arc;

use maho_ext_api::ExtensionApi;
use serde_json::Value;

use super::types::{
    command_context_from, finish, require_identity, respond, CommandContext, CommandResponse,
    MemoryCommandDeps, NotifyLevel,
};

pub const SLEEPTIME_OVERRIDE_MARK: &str = " [agent override]";

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedSleeptimeValue<T> {
    pub value: T,
    pub overridden: bool,
}

fn resolved<T>(value: T, overridden: bool) -> ResolvedSleeptimeValue<T> {
    ResolvedSleeptimeValue { value, overridden }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedSleeptimeSettings {
    pub reflection: ResolvedReflection,
    pub nudge: ResolvedNudge,
    pub facts: ResolvedFacts,
    pub dream: ResolvedDream,
    pub people: ResolvedPeople,
    pub soul: ResolvedSoul,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedReflection {
    pub enabled: ResolvedSleeptimeValue<bool>,
    pub step_count: ResolvedSleeptimeValue<i64>,
    pub on_compaction: ResolvedSleeptimeValue<bool>,
    pub merge: ResolvedSleeptimeValue<String>,
    pub category: ResolvedSleeptimeValue<String>,
    pub timeout_minutes: ResolvedSleeptimeValue<i64>,
    pub sandbox: ResolvedSleeptimeValue<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedNudge {
    pub enabled: ResolvedSleeptimeValue<bool>,
    pub every_user_turns: ResolvedSleeptimeValue<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedFacts {
    pub enabled: ResolvedSleeptimeValue<bool>,
    pub debounce_settles: ResolvedSleeptimeValue<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedDream {
    pub enabled: ResolvedSleeptimeValue<bool>,
    pub idle_minutes: ResolvedSleeptimeValue<i64>,
    pub min_hours_between: ResolvedSleeptimeValue<i64>,
    pub shutdown_launch: ResolvedSleeptimeValue<bool>,
    pub auto_select_max: ResolvedSleeptimeValue<i64>,
    pub auto_select_max_chars: ResolvedSleeptimeValue<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPeople {
    pub enabled: ResolvedSleeptimeValue<bool>,
    pub max_entries: ResolvedSleeptimeValue<i64>,
    pub max_entry_chars: ResolvedSleeptimeValue<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedSoul {
    pub edit_notice: ResolvedSleeptimeValue<bool>,
}

fn bool_of(value: &Value) -> Option<bool> {
    value.as_bool()
}

fn int_of(value: &Value) -> Option<i64> {
    value.as_i64()
}

fn str_of(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

pub fn resolve_sleeptime_settings(settings: &Value, agent_id: &str) -> Result<ResolvedSleeptimeSettings, String> {
    let agent_override = settings.get("agents").and_then(|agents| agents.get(agent_id));
    let reflection_override = agent_override.and_then(|agent| agent.get("reflection"));
    let nudge_override = agent_override.and_then(|agent| agent.get("nudge"));
    let facts_override = agent_override.and_then(|agent| agent.get("facts"));
    let dream_override = agent_override.and_then(|agent| agent.get("dream"));
    let people_override = agent_override.and_then(|agent| agent.get("people"));
    let soul_override = agent_override.and_then(|agent| agent.get("soul"));

    let effective = crate::reflection_settings::resolve_agent_reflection_settings(Some(settings), agent_id)?;
    let effective_trigger = effective.get("trigger");

    let reflection = ResolvedReflection {
        enabled: resolved(
            bool_of(&effective["enabled"]).unwrap_or(false),
            reflection_override.and_then(|value| value.get("enabled")).is_some(),
        ),
        step_count: resolved(
            effective_trigger.and_then(|trigger| int_of(&trigger["step_count"])).unwrap_or(0),
            reflection_override
                .and_then(|value| value.get("trigger"))
                .and_then(|trigger| trigger.get("step_count"))
                .is_some(),
        ),
        on_compaction: resolved(
            effective_trigger
                .and_then(|trigger| bool_of(&trigger["on_compaction"]))
                .unwrap_or(false),
            reflection_override
                .and_then(|value| value.get("trigger"))
                .and_then(|trigger| trigger.get("on_compaction"))
                .is_some(),
        ),
        merge: resolved(
            str_of(&effective["merge"]).unwrap_or_default(),
            reflection_override.and_then(|value| value.get("merge")).is_some(),
        ),
        category: resolved(
            str_of(&effective["category"]).unwrap_or_default(),
            reflection_override.and_then(|value| value.get("category")).is_some(),
        ),
        timeout_minutes: resolved(
            int_of(&effective["timeout_minutes"]).unwrap_or(0),
            reflection_override.and_then(|value| value.get("timeout_minutes")).is_some(),
        ),
        sandbox: resolved(
            str_of(&effective["sandbox"]).unwrap_or_default(),
            reflection_override.and_then(|value| value.get("sandbox")).is_some(),
        ),
    };

    let base_nudge = settings.get("nudge");
    let nudge = ResolvedNudge {
        enabled: resolved(
            nudge_override
                .and_then(|value| bool_of(&value["enabled"]))
                .or_else(|| base_nudge.and_then(|value| bool_of(&value["enabled"])))
                .unwrap_or(false),
            nudge_override.and_then(|value| value.get("enabled")).is_some(),
        ),
        every_user_turns: resolved(
            nudge_override
                .and_then(|value| int_of(&value["every_user_turns"]))
                .or_else(|| base_nudge.and_then(|value| int_of(&value["every_user_turns"])))
                .unwrap_or(0),
            nudge_override.and_then(|value| value.get("every_user_turns")).is_some(),
        ),
    };

    let base_facts = settings.get("facts");
    let facts = ResolvedFacts {
        enabled: resolved(
            facts_override
                .and_then(|value| bool_of(&value["enabled"]))
                .or_else(|| base_facts.and_then(|value| bool_of(&value["enabled"])))
                .unwrap_or(false),
            facts_override.and_then(|value| value.get("enabled")).is_some(),
        ),
        debounce_settles: resolved(
            facts_override
                .and_then(|value| int_of(&value["debounce_settles"]))
                .or_else(|| base_facts.and_then(|value| int_of(&value["debounce_settles"])))
                .unwrap_or(0),
            facts_override.and_then(|value| value.get("debounce_settles")).is_some(),
        ),
    };

    let base_dream = settings.get("dream");
    let dream_value = |key: &str| -> Option<i64> {
        dream_override
            .and_then(|value| int_of(&value[key]))
            .or_else(|| base_dream.and_then(|value| int_of(&value[key])))
    };
    let dream_flag = |key: &str| -> Option<bool> {
        dream_override
            .and_then(|value| bool_of(&value[key]))
            .or_else(|| base_dream.and_then(|value| bool_of(&value[key])))
    };
    let dream = ResolvedDream {
        enabled: resolved(dream_flag("enabled").unwrap_or(false), dream_override.and_then(|v| v.get("enabled")).is_some()),
        idle_minutes: resolved(dream_value("idle_minutes").unwrap_or(0), dream_override.and_then(|v| v.get("idle_minutes")).is_some()),
        min_hours_between: resolved(dream_value("min_hours_between").unwrap_or(0), dream_override.and_then(|v| v.get("min_hours_between")).is_some()),
        shutdown_launch: resolved(dream_flag("shutdown_launch").unwrap_or(false), dream_override.and_then(|v| v.get("shutdown_launch")).is_some()),
        auto_select_max: resolved(dream_value("auto_select_max").unwrap_or(0), dream_override.and_then(|v| v.get("auto_select_max")).is_some()),
        auto_select_max_chars: resolved(dream_value("auto_select_max_chars").unwrap_or(0), dream_override.and_then(|v| v.get("auto_select_max_chars")).is_some()),
    };

    let base_people = settings.get("people");
    let people_value = |key: &str| -> Option<i64> {
        people_override
            .and_then(|value| int_of(&value[key]))
            .or_else(|| base_people.and_then(|value| int_of(&value[key])))
    };
    let people = ResolvedPeople {
        enabled: resolved(
            people_override
                .and_then(|value| bool_of(&value["enabled"]))
                .or_else(|| base_people.and_then(|value| bool_of(&value["enabled"])))
                .unwrap_or(false),
            people_override.and_then(|value| value.get("enabled")).is_some(),
        ),
        max_entries: resolved(people_value("max_entries").unwrap_or(0), people_override.and_then(|v| v.get("max_entries")).is_some()),
        max_entry_chars: resolved(people_value("max_entry_chars").unwrap_or(0), people_override.and_then(|v| v.get("max_entry_chars")).is_some()),
    };

    let base_soul = settings.get("soul");
    let soul = ResolvedSoul {
        edit_notice: resolved(
            soul_override
                .and_then(|value| bool_of(&value["edit_notice"]))
                .or_else(|| base_soul.and_then(|value| bool_of(&value["edit_notice"])))
                .unwrap_or(false),
            soul_override.and_then(|value| value.get("edit_notice")).is_some(),
        ),
    };

    Ok(ResolvedSleeptimeSettings { reflection, nudge, facts, dream, people, soul })
}

fn mark(setting: &ResolvedSleeptimeValue<impl Sized>) -> &'static str {
    if setting.overridden {
        SLEEPTIME_OVERRIDE_MARK
    } else {
        ""
    }
}

pub async fn handle_sleeptime(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    _args: &str,
) -> CommandResponse {
    let identity = match require_identity(deps, ctx) {
        Ok(identity) => identity,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    let settings = match (deps.settings)() {
        Ok(settings) => settings,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    let agent_id = identity.identity.clone();
    let values = match resolve_sleeptime_settings(&settings, &agent_id) {
        Ok(values) => values,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    let config_path = deps
        .config_path_value()
        .unwrap_or_else(|| "your omo config file".to_owned());

    let reflection = &values.reflection;
    let nudge = &values.nudge;
    let facts = &values.facts;
    let dream = &values.dream;
    let people = &values.people;
    let soul = &values.soul;

    let lines = vec![
        format!("# Sleeptime reflection: {agent_id}"),
        String::new(),
        format!(
            "Reflection: {}{}",
            if reflection.enabled.value { "on" } else { "off" },
            mark(&reflection.enabled)
        ),
        format!(
            "Step trigger: {}{}",
            if reflection.step_count.value > 0 {
                format!("every {} steps", reflection.step_count.value)
            } else {
                "off".to_owned()
            },
            mark(&reflection.step_count)
        ),
        format!(
            "On compaction: {}{}",
            if reflection.on_compaction.value { "on" } else { "off" },
            mark(&reflection.on_compaction)
        ),
        format!("Merge policy: {}{}", reflection.merge.value, mark(&reflection.merge)),
        format!("Category: {}{}", reflection.category.value, mark(&reflection.category)),
        format!(
            "Timeout: {} minutes{}",
            reflection.timeout_minutes.value,
            mark(&reflection.timeout_minutes)
        ),
        format!("Sandbox: {}{}", reflection.sandbox.value, mark(&reflection.sandbox)),
        String::new(),
        format!("Nudge: {}{}", if nudge.enabled.value { "on" } else { "off" }, mark(&nudge.enabled)),
        format!(
            "Nudge every: every {} turns{}",
            nudge.every_user_turns.value,
            mark(&nudge.every_user_turns)
        ),
        String::new(),
        format!("Facts: {}{}", if facts.enabled.value { "on" } else { "off" }, mark(&facts.enabled)),
        format!(
            "Facts debounce: debounce {} settles{}",
            facts.debounce_settles.value,
            mark(&facts.debounce_settles)
        ),
        String::new(),
        format!("Dream: {}{}", if dream.enabled.value { "on" } else { "off" }, mark(&dream.enabled)),
        format!("Dream idle: idle {} minutes{}", dream.idle_minutes.value, mark(&dream.idle_minutes)),
        format!(
            "Dream spacing: min {}h between{}",
            dream.min_hours_between.value,
            mark(&dream.min_hours_between)
        ),
        format!(
            "Dream shutdown: shutdown launch {}{}",
            if dream.shutdown_launch.value { "on" } else { "off" },
            mark(&dream.shutdown_launch)
        ),
        format!("Dream select: select max {}{}", dream.auto_select_max.value, mark(&dream.auto_select_max)),
        format!(
            "Dream select chars: max {} chars{}",
            dream.auto_select_max_chars.value,
            mark(&dream.auto_select_max_chars)
        ),
        String::new(),
        format!("People: {}{}", if people.enabled.value { "on" } else { "off" }, mark(&people.enabled)),
        format!("People entries: max {} entries{}", people.max_entries.value, mark(&people.max_entries)),
        format!(
            "People chars: max {} chars{}",
            people.max_entry_chars.value,
            mark(&people.max_entry_chars)
        ),
        String::new(),
        format!("Soul: edit notice {}{}", if soul.edit_notice.value { "on" } else { "off" }, mark(&soul.edit_notice)),
        String::new(),
        format!(
            "Edit {config_path} under memory.reflection, or memory.agents.{agent_id} for this identity only."
        ),
        "Reflect now: /reflect [--recent N | --conversation <ids>] [focus]".to_owned(),
        "Dream now: /dream [--auto|--recent N|--conversation <ids>] [focus]".to_owned(),
    ];
    respond(ctx, lines.join("\n"), NotifyLevel::Info)
}

pub fn register_sleeptime_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "sleeptime",
        Some("Show the resolved sleeptime memory settings for this identity.".to_owned()),
        Some(String::new()),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_sleeptime(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;

    fn expected_default() -> ResolvedSleeptimeSettings {
        ResolvedSleeptimeSettings {
            reflection: ResolvedReflection {
                enabled: resolved(true, false),
                step_count: resolved(25, false),
                on_compaction: resolved(true, false),
                merge: resolved("auto".to_owned(), false),
                category: resolved("quick".to_owned(), false),
                timeout_minutes: resolved(15, false),
                sandbox: resolved("auto".to_owned(), false),
            },
            nudge: ResolvedNudge {
                enabled: resolved(true, false),
                every_user_turns: resolved(10, false),
            },
            facts: ResolvedFacts {
                enabled: resolved(true, false),
                debounce_settles: resolved(4, false),
            },
            dream: ResolvedDream {
                enabled: resolved(true, false),
                idle_minutes: resolved(30, false),
                min_hours_between: resolved(24, false),
                shutdown_launch: resolved(true, false),
                auto_select_max: resolved(5, false),
                auto_select_max_chars: resolved(150000, false),
            },
            people: ResolvedPeople {
                enabled: resolved(true, false),
                max_entries: resolved(40, false),
                max_entry_chars: resolved(200, false),
            },
            soul: ResolvedSoul { edit_notice: resolved(true, false) },
        }
    }

    #[tokio::test]
    async fn given_default_settings_when_resolved_and_invoked_then_machine_values_use_defaults_without_override_flags() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let resolved = resolve_sleeptime_settings(&memory_settings(), TEST_IDENTITY).expect("resolved");
        let response = handle_sleeptime(&fake.deps, &context.ctx, "").await;

        assert_eq!(resolved, expected_default());
        assert!(response.text.contains("Reflection: on"));
        assert!(response.text.contains("Dream select chars: max 150000 chars"));
        assert_eq!(
            context.ui.notifications(),
            vec![(response.text.clone(), NotifyLevel::Info)]
        );
    }

    #[tokio::test]
    async fn given_per_agent_overrides_when_resolved_and_invoked_then_override_values_and_flags_win_field_by_field() {
        let (_root, identity) = temp_identity();
        let mut settings = memory_settings();
        settings["agents"] = serde_json::json!({
            TEST_IDENTITY: {
                "reflection": {
                    "enabled": false,
                    "trigger": { "step_count": 5, "on_compaction": false },
                    "merge": "integration",
                    "category": "deep",
                    "timeout_minutes": 30,
                    "sandbox": "required"
                },
                "nudge": { "enabled": false, "every_user_turns": 20 },
                "facts": { "enabled": false, "debounce_settles": 8 },
                "dream": {
                    "enabled": false, "idle_minutes": 60, "min_hours_between": 48,
                    "shutdown_launch": false, "auto_select_max": 3, "auto_select_max_chars": 25000
                },
                "people": { "enabled": false, "max_entries": 20, "max_entry_chars": 100 },
                "soul": { "edit_notice": false }
            }
        });
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { settings: Some(settings.clone()), ..Default::default() },
        );
        let context = fake_command_context(FakeContextOptions::default());

        let actual = resolve_sleeptime_settings(&settings, TEST_IDENTITY).expect("resolved");
        let response = handle_sleeptime(&fake.deps, &context.ctx, "").await;

        let expected = ResolvedSleeptimeSettings {
            reflection: ResolvedReflection {
                enabled: resolved(false, true),
                step_count: resolved(5, true),
                on_compaction: resolved(false, true),
                merge: resolved("integration".to_owned(), true),
                category: resolved("deep".to_owned(), true),
                timeout_minutes: resolved(30, true),
                sandbox: resolved("required".to_owned(), true),
            },
            nudge: ResolvedNudge { enabled: resolved(false, true), every_user_turns: resolved(20, true) },
            facts: ResolvedFacts { enabled: resolved(false, true), debounce_settles: resolved(8, true) },
            dream: ResolvedDream {
                enabled: resolved(false, true),
                idle_minutes: resolved(60, true),
                min_hours_between: resolved(48, true),
                shutdown_launch: resolved(false, true),
                auto_select_max: resolved(3, true),
                auto_select_max_chars: resolved(25000, true),
            },
            people: ResolvedPeople {
                enabled: resolved(false, true),
                max_entries: resolved(20, true),
                max_entry_chars: resolved(100, true),
            },
            soul: ResolvedSoul { edit_notice: resolved(false, true) },
        };
        assert_eq!(actual, expected);
        for line in [
            "Reflection: off [agent override]",
            "On compaction: off [agent override]",
            "Category: deep [agent override]",
            "Sandbox: required [agent override]",
            "Dream shutdown: shutdown launch off [agent override]",
            "Dream select: select max 3 [agent override]",
            "Dream select chars: max 25000 chars [agent override]",
        ] {
            assert!(response.text.contains(line), "missing line: {line}");
        }
    }

    #[tokio::test]
    async fn given_an_unbound_session_when_invoked_then_the_command_surfaces_a_structured_error_notification() {
        let fake = fake_deps(None, FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_sleeptime(&fake.deps, &context.ctx, "").await;

        assert_eq!(
            context.ui.notifications(),
            vec![(response.text.clone(), NotifyLevel::Error)]
        );
    }
}
