//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/reasoning-level-notice.ts`.

use std::collections::HashMap;

use super::startup_tip::StartupTipLine;

/// Id under which the one-time notice is recorded in the settings tip history (`tipsHistory`),
/// the same store the rotating startup tips use. Once it has a timestamp there it is never shown
/// again from that agent directory.
pub const REASONING_LEVEL_TIP_ID: &str = "reasoning-level-key";

pub struct ReasoningLevelNoticeOptions<'a> {
    pub history: &'a HashMap<String, u64>,
    pub model_reasoning: bool,
    pub keys: &'a dyn Fn(&str) -> String,
}

/// The one-time "<key> changes the reasoning level" notice for a reasoning model. Shown only once
/// per agent directory (tracked in the tip history) and only when `app.thinking.cycle` is actually
/// bound, so the key named is the user's own.
pub fn resolve_reasoning_level_notice(
    options: ReasoningLevelNoticeOptions<'_>,
) -> Option<StartupTipLine> {
    if !options.model_reasoning {
        return None;
    }
    if options.history.contains_key(REASONING_LEVEL_TIP_ID) {
        return None;
    }
    let key = (options.keys)("app.thinking.cycle");
    if key.is_empty() {
        return None;
    }
    Some(StartupTipLine {
        tip_id: REASONING_LEVEL_TIP_ID.to_string(),
        line: format!("Tip: {key} changes the reasoning level"),
    })
}

#[cfg(test)]
mod tests {
    //! Port of the `resolveReasoningLevelNotice` cases of senpi `reasoning-level-notice.test.ts`.

    use super::*;

    fn keys(binding: &str) -> String {
        if binding == "app.thinking.cycle" {
            "Shift+Tab".to_string()
        } else {
            String::new()
        }
    }

    #[test]
    fn names_the_bound_key_and_the_reasoning_level_the_first_time() {
        let notice = resolve_reasoning_level_notice(ReasoningLevelNoticeOptions {
            history: &HashMap::new(),
            model_reasoning: true,
            keys: &keys,
        });
        assert_eq!(
            notice,
            Some(StartupTipLine {
                tip_id: REASONING_LEVEL_TIP_ID.to_string(),
                line: "Tip: Shift+Tab changes the reasoning level".to_string(),
            })
        );
    }

    #[test]
    fn is_never_shown_for_a_model_without_reasoning() {
        assert_eq!(
            resolve_reasoning_level_notice(ReasoningLevelNoticeOptions {
                history: &HashMap::new(),
                model_reasoning: false,
                keys: &keys,
            }),
            None
        );
    }

    #[test]
    fn is_not_shown_when_the_cycle_action_is_unbound() {
        assert_eq!(
            resolve_reasoning_level_notice(ReasoningLevelNoticeOptions {
                history: &HashMap::new(),
                model_reasoning: true,
                keys: &|_| String::new(),
            }),
            None
        );
    }
}
