use crate::{
    constants::{FILE_VISIBLE_ROWS, TOOL_VISIBLE_ROWS},
    format::{truncate_visible, truncate_visible_start, visible_width},
    links::PanelAction,
    rows::PanelRowsInput,
    runtime::PanelSnapshot,
    store::PanelChildStatus,
    types::*,
};

fn heading(name: &str, summary: &str) -> PanelRow {
    PanelRow::colored(
        if summary.is_empty() {
            name.into()
        } else {
            format!("{name}  {summary}")
        },
        PanelColor::Accent,
    )
}
fn field(label: &str, value: impl std::fmt::Display, color: PanelColor) -> PanelRow {
    PanelRow::colored(format!("{label:<8}{value}"), color)
}
fn tokens(value: f64) -> String {
    if !value.is_finite() || value < 0.0 {
        return "0".into();
    }
    let rounded = value.round();
    if rounded < 1000.0 {
        return format!("{rounded:.0}");
    }
    for (scale, suffix) in [(1000.0, "K"), (1_000_000.0, "M"), (1_000_000_000.0, "B")] {
        let n = (rounded / scale * 10.0).round() / 10.0;
        if n < 1000.0 || suffix == "B" {
            let text = format!("{n:.1}");
            return format!(
                "{}{suffix}",
                text.trim_end_matches('0').trim_end_matches('.')
            );
        }
    }
    String::new()
}
fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    let minutes = seconds / 60;
    if seconds < 60 {
        format!("{seconds}s")
    } else if minutes < 60 {
        format!("{minutes}m{:02}", seconds % 60)
    } else {
        format!("{}h{:02}", minutes / 60, minutes % 60)
    }
}
fn cost(value: f64) -> String {
    if !value.is_finite() || value <= 0.0 {
        "$0".into()
    } else if value < 10.0 {
        format!("${value:.2}")
    } else if value < 1000.0 {
        format!("${value:.1}")
    } else {
        format!("${value:.0}")
    }
}
fn bar(ratio: f64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let ratio = if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let unit = 1.0 / f64::from(u32::try_from(width).unwrap_or(u32::MAX));
    let mut remaining = ratio;
    let mut cells = Vec::new();
    for _ in 0..width {
        let eighths = (remaining / unit * 8.0).round().clamp(0.0, 8.0);
        cells.push(if eighths >= 8.0 {
            '█'
        } else if eighths >= 7.0 {
            '▉'
        } else if eighths >= 6.0 {
            '▊'
        } else if eighths >= 5.0 {
            '▋'
        } else if eighths >= 4.0 {
            '▌'
        } else if eighths >= 3.0 {
            '▍'
        } else if eighths >= 2.0 {
            '▎'
        } else if eighths >= 1.0 {
            '▏'
        } else {
            '░'
        });
        remaining = (remaining - unit).max(0.0);
    }
    if ratio > 0.0 && cells.iter().all(|c| *c == '░') {
        cells[0] = '▏';
    }
    cells.into_iter().collect()
}
fn number(value: &serde_json::Value, key: &str) -> f64 {
    value
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0)
}

pub fn build_snapshot_sections(
    snapshot: &PanelSnapshot,
    width: usize,
    home: Option<&str>,
) -> PanelRowsInput {
    let s = snapshot;
    let mut input = PanelRowsInput {
        sections: s.settings.sections.clone(),
        ..PanelRowsInput::default()
    };
    if width == 0 {
        return input;
    }
    let totals = s.facts.value.get("totals");
    let spend = totals.map_or(0.0, |v| number(v, "cost")) + s.state.child_spend;
    let total_tokens = totals.map_or(0.0, |v| number(v, "input") + number(v, "output"));
    let summary = if spend == 0.0 && total_tokens == 0.0 {
        String::new()
    } else {
        format!("{} · {}", cost(spend), tokens(total_tokens))
    };
    input.session.push(heading("SESSION", &summary));
    if let Some(model) = s.facts.model.as_deref().filter(|s| !s.is_empty()) {
        input.session.push(field("model", model, PanelColor::Text));
    }
    input.session.push(field(
        "elapsed",
        duration(s.now.saturating_sub(s.started_at)),
        PanelColor::Muted,
    ));
    if let Some(totals) = totals.filter(|_| total_tokens > 0.0) {
        input.session.push(field(
            "tokens",
            format!(
                "in {} · out {}",
                tokens(number(totals, "input")),
                tokens(number(totals, "output"))
            ),
            PanelColor::Muted,
        ));
        if let Some(hit) = totals
            .get("latestCacheHitRate")
            .and_then(serde_json::Value::as_f64)
        {
            input
                .session
                .push(field("cache", format!("{hit:.0}% hit"), PanelColor::Muted));
        }
    }
    if s.state.child_spend > 0.0 {
        input.session.push(field(
            "agents",
            cost(s.state.child_spend),
            PanelColor::Muted,
        ));
    }
    if let Some(goal) = &s.goal {
        let (label, color) = match goal.status {
            PanelGoalStatus::Active => ("active", PanelColor::Accent),
            PanelGoalStatus::Paused => ("paused", PanelColor::Muted),
            PanelGoalStatus::Blocked => ("blocked", PanelColor::Warning),
            PanelGoalStatus::BudgetLimited => ("limited by budget", PanelColor::Warning),
            PanelGoalStatus::Complete => ("complete", PanelColor::Success),
        };
        let elapsed = if goal.time_used_seconds.is_finite() && goal.time_used_seconds > 0.0 {
            let millis = std::time::Duration::try_from_secs_f64(goal.time_used_seconds)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            format!(" · {}", duration(millis))
        } else {
            String::new()
        };
        input
            .goal
            .push(PanelRow::colored(format!("GOAL  {label}{elapsed}"), color));
        input.goal.push(PanelRow {
            text: truncate_visible(&goal.objective, width),
            color: Some(PanelColor::Text),
            action: Some(PanelAction::Goal),
        });
        if goal.tokens_used > 0.0 {
            input
                .goal
                .push(field("tokens", tokens(goal.tokens_used), PanelColor::Muted));
        }
        if let Some(budget) = goal.token_budget.filter(|b| *b > 0.0) {
            let percent = goal.tokens_used / budget * 100.0;
            let rendered = bar(percent / 100.0, width.saturating_sub(13));
            if !rendered.is_empty() {
                input.goal.push(field(
                    "budget",
                    format!("{rendered} {:.0}%", percent.round()),
                    PanelColor::Accent,
                ));
            }
        }
        if goal.consecutive_continuations > 0 || goal.unattended_continuations > 0 {
            input.goal.push(field(
                "loops",
                format!(
                    "{} · {} unattended",
                    goal.consecutive_continuations, goal.unattended_continuations
                ),
                PanelColor::Muted,
            ));
        }
    }
    if let Some(usage) = s.facts.value.get("usage") {
        if let Some(used) = usage.get("tokens").and_then(serde_json::Value::as_f64) {
            let window = number(usage, "contextWindow");
            let summary = if window > 0.0 {
                format!("{}/{}", tokens(used), tokens(window))
            } else {
                tokens(used)
            };
            input.context.push(heading("CONTEXT", &summary));
            if window > 0.0 {
                let percent = usage
                    .get("percent")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(used / window * 100.0);
                let rendered = bar(percent / 100.0, width.saturating_sub(13));
                if !rendered.is_empty() {
                    input.context.push(field(
                        "used",
                        format!("{rendered} {:.0}%", percent.round()),
                        PanelColor::Accent,
                    ));
                }
            }
        } else {
            input.context.push(heading("CONTEXT", ""));
            input
                .context
                .push(field("window", "measuring", PanelColor::Dim));
        }
    }
    if !s.state.children.is_empty() {
        let running = s
            .state
            .children
            .iter()
            .filter(|c| c.status.running())
            .count();
        let parked = s
            .state
            .children
            .iter()
            .filter(|c| {
                matches!(
                    c.status,
                    PanelChildStatus::Resuming | PanelChildStatus::Ending
                )
            })
            .count();
        let mut parts = Vec::new();
        if running > 0 {
            parts.push(format!("{running} running"));
        }
        if parked > 0 {
            parts.push(format!("{parked} parked"));
        }
        parts.push(format!(
            "{} done",
            s.state.children.len() - running - parked
        ));
        input.agents.push(heading("AGENTS", &parts.join(" · ")));
        for child in &s.state.children {
            let (glyph, color) = match child.status {
                PanelChildStatus::Queued => ("◦", PanelColor::Dim),
                PanelChildStatus::Running => ("●", PanelColor::Text),
                PanelChildStatus::Resuming | PanelChildStatus::Ending => ("‖", PanelColor::Muted),
                PanelChildStatus::Finished => ("✓", PanelColor::Muted),
                PanelChildStatus::Failed => ("✗", PanelColor::Error),
                PanelChildStatus::Cancelled => ("-", PanelColor::Dim),
            };
            let mut text = format!("{glyph} {}", child.name);
            if !matches!(
                child.status,
                PanelChildStatus::Resuming | PanelChildStatus::Ending
            ) {
                text.push_str(&format!(
                    "  {}",
                    duration(
                        child
                            .finished_at
                            .unwrap_or(s.now)
                            .saturating_sub(child.started_at)
                    )
                ));
            }
            if child.status == PanelChildStatus::Running
                && let Some(activity) = &child.activity
            {
                text.push_str(&format!("  {activity}"));
            }
            input.agents.push(PanelRow {
                text: truncate_visible(&text, width),
                color: Some(color),
                action: Some(PanelAction::Agent {
                    id: child.id.clone(),
                }),
            });
        }
    }
    if !s.state.tools.is_empty() {
        let hidden = s.state.tools.len().saturating_sub(TOOL_VISIBLE_ROWS);
        let summary = if hidden > 0 {
            format!("{} · +{hidden} earlier", s.state.tools.len())
        } else {
            s.state.tools.len().to_string()
        };
        input.tools.push(heading("TOOLS", &summary));
        for call in s.state.tools.iter().skip(hidden) {
            let text = match call.detail.as_deref().filter(|d| !d.is_empty()) {
                Some(detail) => format!("{}  {detail}", call.name),
                None => call.name.clone(),
            };
            input.tools.push(PanelRow::colored(
                truncate_visible(&text, width),
                PanelColor::Muted,
            ));
        }
    }
    if let Some(git) = s.git.as_ref().filter(|g| !g.files.is_empty()) {
        let hidden = git.files.len().saturating_sub(FILE_VISIBLE_ROWS);
        let summary = format!(
            "{} changed{}",
            git.files.len(),
            if hidden > 0 {
                format!(" · +{hidden} more")
            } else {
                String::new()
            }
        );
        input.files.push(heading("FILES", &summary));
        for file in git.files.iter().take(FILE_VISIBLE_ROWS) {
            let trimmed = file.path.strip_suffix('/').unwrap_or(&file.path);
            let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
            let directory = if file.path.ends_with('/') { "/" } else { "" };
            let delta = if file.added.is_none() && file.removed.is_none() {
                String::new()
            } else {
                format!(
                    "  +{}/-{}",
                    file.added.unwrap_or(0),
                    file.removed.unwrap_or(0)
                )
            };
            let color = if file.xy == "??" {
                PanelColor::Dim
            } else if file.xy.contains('U') || file.xy == "AA" || file.xy == "DD" {
                PanelColor::Error
            } else if file.xy.as_bytes().get(1) == Some(&b' ') {
                PanelColor::Success
            } else {
                PanelColor::Text
            };
            input.files.push(PanelRow {
                text: truncate_visible(&format!("{} {name}{directory}{delta}", file.xy), width),
                color: Some(color),
                action: Some(PanelAction::File {
                    path: file.path.clone(),
                }),
            });
        }
    }
    if let Some(memory) = &s.memory {
        input.memory.push(heading(
            "MEMORY",
            &truncate_visible(&memory.identity, width.saturating_sub(8)),
        ));
        input.memory.extend(memory.rows.clone());
    }
    let mut directory = s.cwd.clone();
    if let Some(home) = home.filter(|h| !h.is_empty())
        && let Some(rest) = s
            .cwd
            .strip_prefix(home)
            .filter(|r| r.is_empty() || r.starts_with(['/', '\\']))
    {
        directory = format!("~{rest}");
    }
    let branch = s.branch.as_deref().filter(|b| !b.is_empty());
    let suffix = branch.map_or_else(String::new, |b| format!(" · {b}"));
    let room = width.saturating_sub(visible_width(&suffix));
    let location = if room == 0 {
        truncate_visible_start(branch.unwrap_or(&directory), width)
    } else {
        format!("{}{suffix}", truncate_visible_start(&directory, room))
    };
    input
        .location
        .push(PanelRow::colored(location, PanelColor::Muted));
    input
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_child_uses_ascii_hyphen() {
        let mut state = crate::store::PanelState::default();
        state.children.push(crate::store::PanelChild {
            id: "cancelled".into(),
            name: "explore".into(),
            status: PanelChildStatus::Cancelled,
            started_at: 0,
            finished_at: Some(1000),
            activity: None,
            cost: None,
            card: vec![],
            category: None,
            parked_reason: None,
            host: None,
            turns: None,
            tokens: None,
        });
        let snapshot = PanelSnapshot {
            settings: Default::default(),
            facts: Default::default(),
            state,
            cwd: "/repo".into(),
            branch: None,
            started_at: 0,
            now: 1000,
            git: None,
            goal: None,
            memory: None,
        };
        let rows = build_snapshot_sections(&snapshot, 80, None).agents;
        assert!(rows[1].text.starts_with("- explore"));
    }
}
