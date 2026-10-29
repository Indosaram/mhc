//! Layout selection and main-pane sizing.

use std::sync::Arc;

use serde_json::json;

use crate::runner::{RunTmuxOptions, run_tmux_command};
use crate::tmux_utils::deps::{EnforceMainPaneWidthDeps, strings};
use crate::types::TmuxLayout;

/// Runs `[tmux, args...]` with ignored output and returns its exit code.
pub type SpawnCommandFn = Arc<dyn Fn(&[String]) -> i32 + Send + Sync>;

#[derive(Clone, Default)]
pub struct LayoutDeps {
    pub spawn_command: Option<SpawnCommandFn>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MainPaneWidthOptions {
    pub main_pane_size: Option<u32>,
    pub main_pane_min_width: Option<u32>,
    pub agent_pane_min_width: Option<u32>,
}

impl From<u32> for MainPaneWidthOptions {
    fn from(main_pane_size: u32) -> Self {
        Self {
            main_pane_size: Some(main_pane_size),
            ..Self::default()
        }
    }
}

fn calculate_main_pane_width(window_width: u32, options: MainPaneWidthOptions) -> u32 {
    let divider_width = 1;
    let size_percent = f64::from(options.main_pane_size.unwrap_or(50).clamp(20, 80));
    let usable = f64::from(window_width.saturating_sub(divider_width));
    // Preserve JavaScript's division-before-multiplication rounding (100 * .29 < 29).
    // The floored value is nonnegative and at most 80% of u32::MAX.
    let desired = (usable * (size_percent / 100.0)).floor() as u32;
    let max_main = window_width
        .saturating_sub(divider_width)
        .saturating_sub(options.agent_pane_min_width.unwrap_or(0));
    desired
        .max(options.main_pane_min_width.unwrap_or(0))
        .min(max_main)
}

fn default_spawn_command(args: &[String]) -> i32 {
    let (tmux, rest) = args
        .split_first()
        .map_or(("", &[][..]), |(tmux, rest)| (tmux.as_str(), rest));
    run_tmux_command(tmux, rest, &RunTmuxOptions::default()).exit_code
}

pub fn apply_layout(tmux: &str, layout: TmuxLayout, main_pane_size: u32, deps: &LayoutDeps) {
    let spawn_command = |args: &[&str]| match &deps.spawn_command {
        Some(spawn) => spawn(&strings(args)),
        None => default_spawn_command(&strings(args)),
    };
    spawn_command(&[tmux, "select-layout", layout.as_str()]);

    let dimension = match layout {
        TmuxLayout::MainHorizontal => "main-pane-height",
        TmuxLayout::MainVertical => "main-pane-width",
        TmuxLayout::Tiled | TmuxLayout::EvenHorizontal | TmuxLayout::EvenVertical => return,
    };
    spawn_command(&[
        tmux,
        "set-window-option",
        dimension,
        &format!("{main_pane_size}%"),
    ]);
}

pub fn enforce_main_pane_width(
    main_pane_id: &str,
    window_width: u32,
    options: impl Into<MainPaneWidthOptions>,
    deps: &EnforceMainPaneWidthDeps,
) {
    let Some(tmux) = (deps.get_tmux_path)() else {
        return;
    };
    let options = options.into();
    let main_width = calculate_main_pane_width(window_width, options);
    deps.run(
        &tmux,
        &strings(&[
            "resize-pane",
            "-t",
            main_pane_id,
            "-x",
            &main_width.to_string(),
        ]),
    );
    deps.log(
        "[enforceMainPaneWidth] main pane resized",
        Some(json!({
            "mainPaneId": main_pane_id,
            "mainWidth": main_width,
            "windowWidth": window_width,
            "mainPaneSize": options.main_pane_size,
            "mainPaneMinWidth": options.main_pane_min_width,
            "agentPaneMinWidth": options.agent_pane_min_width,
        })),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn width_preserves_javascript_rounding_and_minimums() {
        for (width, options, expected) in [
            (101, MainPaneWidthOptions::from(29), 28),
            (100, MainPaneWidthOptions::from(95), 79),
            (100, MainPaneWidthOptions {
                main_pane_size: Some(50),
                main_pane_min_width: Some(90),
                agent_pane_min_width: Some(20),
            }, 79),
            (0, MainPaneWidthOptions::default(), 0),
        ] {
            assert_eq!(calculate_main_pane_width(width, options), expected);
        }
    }

    #[test]
    fn enforce_resizes_with_clamped_width_and_skips_missing_tmux() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&calls);
        let mut deps = EnforceMainPaneWidthDeps {
            get_tmux_path: Arc::new(|| Some("tmux".to_owned())),
            run_tmux_command: Arc::new(move |_, args| {
                sink.lock().expect("recorded calls lock").push(args.to_vec());
                crate::runner::TmuxCommandResult::new("", "", 0)
            }),
            ..EnforceMainPaneWidthDeps::default()
        };
        enforce_main_pane_width("%1", 100, 95, &deps);
        enforce_main_pane_width("%1", 100, MainPaneWidthOptions {
            main_pane_size: Some(50),
            main_pane_min_width: Some(90),
            agent_pane_min_width: Some(20),
        }, &deps);
        deps.get_tmux_path = Arc::new(|| None);
        enforce_main_pane_width("%1", 100, 50, &deps);
        assert_eq!(*calls.lock().expect("recorded calls lock"), vec![
            strings(&["resize-pane", "-t", "%1", "-x", "79"]),
            strings(&["resize-pane", "-t", "%1", "-x", "79"]),
        ]);
    }
}
