//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/startup-tips.ts`.

use std::collections::{HashMap, HashSet};

use super::history_writer::record_tip_shown;
use super::reasoning_level_notice::{ReasoningLevelNoticeOptions, resolve_reasoning_level_notice};
use super::registry::TIP_DEFINITIONS;
use super::startup_tip::{StartupTipOptions, resolve_startup_tip_line};

/// senpi's `StartupTipsSettings`: the slice of `SettingsManager` the startup tips read and write.
/// `quiet_startup` is `getQuietStartup() !== false` (header-only quiet startup counts as quiet).
pub trait StartupTipsSettings {
    fn tips_enabled(&self) -> bool;
    fn quiet_startup(&self) -> bool;
    fn tips_history(&self) -> HashMap<String, u64>;
    fn set_tip_shown(&mut self, tip_id: &str, timestamp: u64);
}

pub struct StartupTipsOptions<'a> {
    pub settings: &'a mut dyn StartupTipsSettings,
    pub now: u64,
    pub model_reasoning: &'a dyn Fn() -> bool,
    pub has_command: &'a dyn Fn(&str) -> bool,
    pub keys: &'a dyn Fn(&str) -> String,
    pub display_keys: &'a dyn Fn(&str) -> String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupTips {
    pub text: String,
    pub tip_ids: Vec<String>,
}

fn record(settings: &mut dyn StartupTipsSettings, tip_id: &str, now: u64) {
    let next = record_tip_shown(&settings.tips_history(), tip_id, now);
    settings.set_tip_shown(tip_id, next.get(tip_id).copied().unwrap_or(now));
}

/// The tip block under the startup header, recorded in the settings tip history as it is resolved.
/// The one-time reasoning-level notice (senpi#3090) goes first; on the launch it shows, the
/// rotating tip skips its own `thinking-level` entry so the two do not repeat each other.
pub fn resolve_startup_tips(options: StartupTipsOptions<'_>) -> Option<StartupTips> {
    let settings = options.settings;
    let tips_enabled = settings.tips_enabled();
    let quiet_startup = settings.quiet_startup();
    let reasoning_notice = if tips_enabled && !quiet_startup {
        resolve_reasoning_level_notice(ReasoningLevelNoticeOptions {
            history: &settings.tips_history(),
            model_reasoning: (options.model_reasoning)(),
            keys: options.display_keys,
        })
    } else {
        None
    };
    if let Some(notice) = &reasoning_notice {
        record(settings, &notice.tip_id, options.now);
    }
    let exclude: HashSet<String> = ["thinking-level".to_string()].into_iter().collect();
    let history = settings.tips_history();
    let startup_tip = resolve_startup_tip_line(StartupTipOptions {
        tips_enabled,
        quiet_startup,
        history: &history,
        now: options.now,
        definitions: TIP_DEFINITIONS.as_slice(),
        keys: options.keys,
        has_command: Some(options.has_command),
        exclude: reasoning_notice.as_ref().map(|_| &exclude),
    });
    if let Some(tip) = &startup_tip {
        record(settings, &tip.tip_id, options.now);
    }
    let shown: Vec<_> = [reasoning_notice, startup_tip]
        .into_iter()
        .flatten()
        .collect();
    if shown.is_empty() {
        return None;
    }
    Some(StartupTips {
        text: shown
            .iter()
            .map(|tip| tip.line.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        tip_ids: shown.into_iter().map(|tip| tip.tip_id).collect(),
    })
}

#[cfg(test)]
mod tests {
    //! Port of the "one-time reasoning level notice through the startup header (senpi#3090)" cases
    //! of senpi `reasoning-level-notice.test.ts`. senpi persists through a real `SettingsManager`
    //! on a temp agent dir; the restart here carries the recorded history into a fresh settings
    //! value, which is what the reload observes.

    use super::super::reasoning_level_notice::REASONING_LEVEL_TIP_ID;
    use super::*;

    const NOTICE: &str = "Tip: Shift+Tab changes the reasoning level";

    #[derive(Default)]
    struct MemorySettings {
        tips_disabled: bool,
        quiet: bool,
        history: HashMap<String, u64>,
    }

    impl StartupTipsSettings for MemorySettings {
        fn tips_enabled(&self) -> bool {
            !self.tips_disabled
        }
        fn quiet_startup(&self) -> bool {
            self.quiet
        }
        fn tips_history(&self) -> HashMap<String, u64> {
            self.history.clone()
        }
        fn set_tip_shown(&mut self, tip_id: &str, timestamp: u64) {
            self.history.insert(tip_id.to_string(), timestamp);
        }
    }

    /// senpi's default bindings for the keys the tip catalog names; `app.thinking.cycle` is
    /// Shift+Tab.
    fn keys(binding: &str) -> String {
        match binding {
            "app.thinking.cycle" => "Shift+Tab".to_string(),
            _ => "Ctrl+X".to_string(),
        }
    }

    fn startup_tips(settings: &mut MemorySettings, reasoning: bool) -> Option<String> {
        resolve_startup_tips(StartupTipsOptions {
            settings,
            now: 1_700_000_000_000,
            model_reasoning: &|| reasoning,
            has_command: &|_| false,
            keys: &keys,
            display_keys: &keys,
        })
        .map(|tips| tips.text)
    }

    #[test]
    fn shows_the_notice_once_skips_the_rotating_thinking_level_tip_that_launch_and_never_repeats_after_a_restart()
     {
        let mut first = MemorySettings::default();
        let first_launch = startup_tips(&mut first, true).expect("first launch tips");
        assert!(first_launch.contains(NOTICE));
        assert!(!first_launch.contains("/thinking <level>"));
        assert!(first.history.contains_key(REASONING_LEVEL_TIP_ID));
        assert!(!first.history.contains_key("thinking-level"));

        let mut restarted = MemorySettings {
            history: first.history.clone(),
            ..MemorySettings::default()
        };
        let second_launch = startup_tips(&mut restarted, true).expect("second launch tips");
        assert!(!second_launch.contains(NOTICE));
    }

    #[test]
    fn shows_nothing_about_the_reasoning_level_for_a_model_without_reasoning() {
        let mut settings = MemorySettings::default();
        let tips = startup_tips(&mut settings, false).unwrap_or_default();
        assert!(!tips.contains(NOTICE));
        assert!(!settings.history.contains_key(REASONING_LEVEL_TIP_ID));
    }

    #[test]
    fn respects_tips_false_and_a_quiet_startup_and_records_nothing_then() {
        let mut tips_off = MemorySettings {
            tips_disabled: true,
            ..MemorySettings::default()
        };
        assert_eq!(startup_tips(&mut tips_off, true), None);
        assert!(!tips_off.history.contains_key(REASONING_LEVEL_TIP_ID));

        let mut quiet = MemorySettings {
            quiet: true,
            ..MemorySettings::default()
        };
        assert_eq!(startup_tips(&mut quiet, true), None);
        assert!(!quiet.history.contains_key(REASONING_LEVEL_TIP_ID));
    }
}
