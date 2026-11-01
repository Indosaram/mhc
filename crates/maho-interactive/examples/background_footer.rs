//! Render deterministic animation frames through the production footer for terminal QA.
use maho_ext_api::BackgroundTaskStatus;
use maho_interactive::{components::footer::{FooterComponent, FooterSnapshot}, theme::{ColorMode, Theme}};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let width = std::env::args().nth(1).map_or(Ok(120), |arg| arg.parse::<usize>())?;
    let theme_name = std::env::args().nth(2).unwrap_or_else(|| "dark".into());
    let theme = Theme::builtin(&theme_name, ColorMode::Truecolor)?;
    for state in ["running", "paused", "failed", "empty"] {
        for now_ms in [0, 150, 300, 450] {
            let mut footer = FooterComponent::new(FooterSnapshot {
                cwd: "/home/user/project".into(), branch: Some("main".into()),
                model_id: Some("model".into()), context_percent: Some(38.0),
                ..Default::default()
            });
            footer.now_ms = now_ms;
            let counts = match state {
                "running" => BackgroundTaskStatus { running: 3, completed: 4, total: 9, queued: 2, ..Default::default() },
                "paused" => BackgroundTaskStatus { paused: 2, completed: 4, total: 6, ..Default::default() },
                "failed" => BackgroundTaskStatus { failed: 1, completed: 4, total: 5, ..Default::default() },
                _ => BackgroundTaskStatus::default(),
            };
            if state != "empty" {
                footer.snapshot.extension_statuses.insert("omo-task".into(), serde_json::to_string(&counts)?);
                footer.snapshot.extension_statuses.insert("monitors".into(), if state == "paused" {
                    "◉ 1 monitor muted · build.log (3m)".into()
                } else { "◉ 1 monitor watching · build.log (3m)".into() });
            }
            println!("{state} / {now_ms}ms\r");
            for line in footer.render(width, &theme)? { println!("{line}\r"); }
        }
    }
    Ok(())
}
