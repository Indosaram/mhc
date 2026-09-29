//! Port of src/missing-tmux.test.ts

mod common;

use std::sync::Arc;

use common::{Recorder, enabled_config, healthy_deps};
use pretty_assertions::assert_eq;
use tmux_core::{SpawnPaneResult, SpawnTmuxPaneRequest, SplitDirection, TmuxDeps, spawn_tmux_pane};

#[test]
fn tmux_resolver_returns_none_spawn_fails_without_running_tmux() {
    // given
    let recorder = Recorder::new(Vec::new());
    let deps = TmuxDeps {
        get_tmux_path: Arc::new(|| None),
        ..healthy_deps(&recorder, "unused")
    };
    let config = enabled_config();

    // when
    let result = spawn_tmux_pane(
        &SpawnTmuxPaneRequest {
            session_id: "session-1",
            description: "worker",
            config: &config,
            server_url: "http://127.0.0.1:4096",
            directory: "/tmp/project",
            target_pane_id: Some("%0"),
            split_direction: SplitDirection::Horizontal,
        },
        &deps,
    );

    // then
    assert_eq!(result, SpawnPaneResult::failure());
    assert!(recorder.calls().is_empty());
}
