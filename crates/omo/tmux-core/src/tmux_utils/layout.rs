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
    let size_percent = u64::from(options.main_pane_size.unwrap_or(50).clamp(20, 80));
    let usable = u64::from(window_width.saturating_sub(divider_width));
    let desired = u32::try_from(usable * size_percent / 100).unwrap_or(u32::MAX);
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
