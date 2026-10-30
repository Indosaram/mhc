//! Port of senpi `packages/coding-agent/src/modes/interactive/model-change-notice.ts`.
//!
//! senpi's `ModelChangeSource` is a string union (`"command"`, `"picker"`, `"fallback"`, ...);
//! the Rust session records carry it as the same string, so sources are `&str` here.

/// A durable transcript row for a model switch that landed during a streaming turn (senpi#2870).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChangeNotice {
    pub text: String,
}

/// senpi `ModelChangeOrigin`: who or what changed the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChangeOrigin {
    pub source: String,
    pub actor: Option<String>,
}

/// The `model_changed` event fields [`live_model_change_notice`] reads.
pub struct LiveModelChange<'a> {
    pub during_turn: bool,
    pub origin: &'a ModelChangeOrigin,
    pub model_id: &'a str,
    pub previous_model_id: Option<&'a str>,
}

/// The `model_change` session entry fields [`replayed_model_change_notice`] reads.
#[derive(Debug, Clone, Default)]
pub struct ModelChangeEntryFields {
    pub model_id: String,
    pub original_model_id: Option<String>,
    pub during_turn: Option<bool>,
    pub source: Option<String>,
    pub actor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCommandArgument {
    pub search_term: String,
    pub as_default: bool,
}

const DEFAULT_FLAG: &str = "--default";

/// `/model <search> [--default]`: the flag asks for the selection to become the default for new
/// sessions too.
pub fn parse_model_command_argument(argument: Option<&str>) -> ModelCommandArgument {
    let words: Vec<&str> = argument.unwrap_or("").split_whitespace().collect();
    ModelCommandArgument {
        as_default: words.contains(&DEFAULT_FLAG),
        search_term: words
            .into_iter()
            .filter(|word| *word != DEFAULT_FLAG)
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Fallback switches already announce themselves with their own notice box.
fn is_announced_elsewhere(source: &str) -> bool {
    source == "fallback" || source == "fallback-revert"
}

fn describe_origin(origin: &ModelChangeOrigin) -> String {
    match &origin.actor {
        None => origin.source.clone(),
        Some(actor) => format!("{}: {actor}", origin.source),
    }
}

pub fn model_change_notice_text(
    from: Option<&str>,
    to: &str,
    origin: &ModelChangeOrigin,
) -> String {
    let route = match from {
        None => to.to_string(),
        Some(from) => format!("{from} → {to}"),
    };
    format!(
        "⇄ Model changed mid-turn · {route} ({})",
        describe_origin(origin)
    )
}

pub fn live_model_change_notice(event: LiveModelChange<'_>) -> Option<ModelChangeNotice> {
    if !event.during_turn || is_announced_elsewhere(&event.origin.source) {
        return None;
    }
    Some(ModelChangeNotice {
        text: model_change_notice_text(event.previous_model_id, event.model_id, event.origin),
    })
}

pub fn replayed_model_change_notice(entry: &ModelChangeEntryFields) -> Option<ModelChangeNotice> {
    let source = entry.source.as_deref()?;
    if entry.during_turn != Some(true) || is_announced_elsewhere(source) {
        return None;
    }
    let origin = ModelChangeOrigin {
        source: source.to_string(),
        actor: entry.actor.clone(),
    };
    Some(ModelChangeNotice {
        text: model_change_notice_text(
            entry.original_model_id.as_deref(),
            &entry.model_id,
            &origin,
        ),
    })
}

#[cfg(test)]
mod tests {
    //! Port of senpi `test/model-change-notice.test.ts`.

    use super::*;

    fn entry() -> ModelChangeEntryFields {
        ModelChangeEntryFields {
            model_id: "next".to_string(),
            ..ModelChangeEntryFields::default()
        }
    }

    fn origin(source: &str, actor: Option<&str>) -> ModelChangeOrigin {
        ModelChangeOrigin {
            source: source.to_string(),
            actor: actor.map(str::to_string),
        }
    }

    fn parsed(search_term: &str, as_default: bool) -> ModelCommandArgument {
        ModelCommandArgument {
            search_term: search_term.to_string(),
            as_default,
        }
    }

    #[test]
    fn parses_model_default_anywhere_in_the_argument_and_leaves_the_search_term_intact() {
        assert_eq!(
            parse_model_command_argument(Some("sonnet")),
            parsed("sonnet", false)
        );
        assert_eq!(
            parse_model_command_argument(Some("--default anthropic/claude")),
            parsed("anthropic/claude", true)
        );
        assert_eq!(
            parse_model_command_argument(Some("claude --default")),
            parsed("claude", true)
        );
        assert_eq!(parse_model_command_argument(None), parsed("", false));
    }

    #[test]
    fn announces_a_mid_turn_switch_with_both_models_and_its_source_live_and_on_replay_alike() {
        let picker = origin("picker", Some("favorites"));
        let live = live_model_change_notice(LiveModelChange {
            during_turn: true,
            origin: &picker,
            model_id: "next",
            previous_model_id: Some("main"),
        });
        let replayed = replayed_model_change_notice(&ModelChangeEntryFields {
            during_turn: Some(true),
            source: Some("picker".to_string()),
            actor: Some("favorites".to_string()),
            original_model_id: Some("main".to_string()),
            ..entry()
        });
        assert_eq!(
            live.as_ref().map(|notice| notice.text.as_str()),
            Some("⇄ Model changed mid-turn · main → next (picker: favorites)")
        );
        assert_eq!(replayed, live);
    }

    #[test]
    fn adds_nothing_for_a_switch_between_turns_a_fallback_or_a_legacy_entry() {
        assert_eq!(
            live_model_change_notice(LiveModelChange {
                during_turn: false,
                origin: &origin("command", None),
                model_id: "next",
                previous_model_id: None,
            }),
            None
        );
        assert_eq!(
            live_model_change_notice(LiveModelChange {
                during_turn: true,
                origin: &origin("fallback", None),
                model_id: "next",
                previous_model_id: None,
            }),
            None
        );
        assert_eq!(
            replayed_model_change_notice(&ModelChangeEntryFields {
                during_turn: Some(true),
                source: Some("fallback-revert".to_string()),
                ..entry()
            }),
            None
        );
        assert_eq!(replayed_model_change_notice(&entry()), None);
    }
}
