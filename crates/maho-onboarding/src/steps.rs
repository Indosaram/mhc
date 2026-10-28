//! Versioned setup steps and who sees them (spec §1–2; step model ported from oh-my-pi
//! `packages/tui/src/setup/wizard.ts` `selectSetupScenes` and `setup-version.ts`).

/// One setup step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub id: &'static str,
    pub min_version: u32,
}

pub const PROVIDER: Step = Step {
    id: "provider",
    min_version: 1,
};
pub const MODEL: Step = Step {
    id: "model",
    min_version: 1,
};

/// The steps in order.
pub const ALL_STEPS: [Step; 2] = [PROVIDER, MODEL];

/// The version a finished setup records: the newest step version.
pub const CURRENT_SETUP_VERSION: u32 = 1;

/// Environment variable that skips setup when truthy.
pub const SKIP_ENV: &str = "MAHO_SKIP_SETUP";

/// Gates of [`select`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectOptions {
    /// stdin and stdout are terminals.
    pub is_tty: bool,
    /// The run resumes or forks a session.
    pub resuming: bool,
    /// Value of [`SKIP_ENV`].
    pub skip_env: Option<String>,
    /// `setup.enabled` from `omo.jsonc` (`None` = default true).
    pub enabled: Option<bool>,
    /// Run every step regardless of the gates and versions (except the TTY gate for prompts).
    pub force: bool,
}

/// Whether a skip variable value is set: anything but empty, `0`, `false`, `no`.
pub fn skip_value_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let value = value.trim().to_ascii_lowercase();
        !value.is_empty() && value != "0" && value != "false" && value != "no"
    })
}

/// The steps newer than `stored_version`, honouring the gates.
pub fn select(stored_version: u32, options: &SelectOptions) -> Vec<Step> {
    if !options.is_tty && !options.force {
        return Vec::new();
    }
    if !options.force
        && (options.resuming
            || skip_value_enabled(options.skip_env.as_deref())
            || options.enabled == Some(false))
    {
        return Vec::new();
    }
    ALL_STEPS
        .iter()
        .copied()
        .filter(|step| options.force || step.min_version > stored_version)
        .collect()
}

/// `setup.enabled` of a merged config view.
pub fn enabled_in(view: &serde_json::Value) -> Option<bool> {
    view.get("setup")?.get("enabled")?.as_bool()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tty() -> SelectOptions {
        SelectOptions {
            is_tty: true,
            ..SelectOptions::default()
        }
    }

    #[test]
    fn current_version_is_the_newest_step() {
        assert_eq!(
            ALL_STEPS.iter().map(|s| s.min_version).max(),
            Some(CURRENT_SETUP_VERSION)
        );
    }

    #[test]
    fn a_fresh_install_on_a_terminal_runs_every_step() {
        assert_eq!(select(0, &tty()), ALL_STEPS.to_vec());
    }

    #[test]
    fn a_current_install_runs_nothing_unless_forced() {
        assert!(select(1, &tty()).is_empty());
        let forced = SelectOptions {
            force: true,
            ..tty()
        };
        assert_eq!(select(1, &forced), ALL_STEPS.to_vec());
    }

    #[test]
    fn gates_hold_unless_forced() {
        let cases = [
            SelectOptions::default(),
            SelectOptions {
                resuming: true,
                ..tty()
            },
            SelectOptions {
                skip_env: Some("1".into()),
                ..tty()
            },
            SelectOptions {
                enabled: Some(false),
                ..tty()
            },
        ];
        for case in cases {
            assert!(select(0, &case).is_empty(), "{case:?}");
            let forced = SelectOptions {
                force: true,
                ..case.clone()
            };
            assert_eq!(select(0, &forced).len(), 2, "{case:?}");
        }
    }

    #[test]
    fn skip_values() {
        for (value, expected) in [
            (None, false),
            (Some(""), false),
            (Some(" 0 "), false),
            (Some("FALSE"), false),
            (Some("no"), false),
            (Some("1"), true),
            (Some("yes"), true),
        ] {
            assert_eq!(skip_value_enabled(value), expected, "{value:?}");
        }
    }

    #[test]
    fn enabled_reads_the_setup_section() {
        assert_eq!(enabled_in(&serde_json::json!({})), None);
        assert_eq!(
            enabled_in(&serde_json::json!({"setup": {"enabled": false}})),
            Some(false)
        );
    }
}
