//! Footer HUD (v7): location and session on the left, cost/context/model on the right, then the
//! ordered, foldable extension status line with memory pinned right. Diverges from senpi's
//! footer.ts by design; see docs/divergences.md (UX-2).
use super::footer_layout::{
    FoldableStatus, FooterGroup, compact_cwd, elide_path, flatten_groups, groups_width,
    join_groups, plan_status_fold, status_fold_width,
};
use crate::theme::theme::{Theme, ThemeColor};
use maho_tui::utils::{truncate_to_width, visible_width};
use std::collections::BTreeMap;
pub fn format_tokens(count: f64) -> String {
    let n = count.round();
    fn trim(n: f64) -> String {
        format!("{n:.1}").trim_end_matches(".0").into()
    }
    if n < 1000. {
        format!("{n:.0}")
    } else if n < 10000. {
        format!("{}K", trim(n / 1000.))
    } else if n < 1000000. {
        format!("{}K", (n / 1000.).round())
    } else if n < 10000000. {
        format!("{}M", trim(n / 1000000.))
    } else if n < 1000000000. {
        format!("{}M", (n / 1000000.).round())
    } else if n < 10000000000. {
        format!("{}B", trim(n / 1000000000.))
    } else {
        format!("{}B", (n / 1000000000.).round())
    }
}
pub fn format_cwd_for_footer(cwd: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|h| !h.is_empty()) else {
        return cwd.into();
    };
    fn normalize(s: &str) -> std::path::PathBuf {
        let mut p = std::path::PathBuf::new();
        for c in std::path::Path::new(s).components() {
            match c {
                std::path::Component::ParentDir => {
                    p.pop();
                }
                std::path::Component::CurDir => {}
                _ => p.push(c),
            }
        }
        p
    }
    let path = normalize(cwd);
    let home = normalize(home);
    match path.strip_prefix(home) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".into(),
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => cwd.into(),
    }
}
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FooterSnapshot {
    pub cwd: String,
    pub home: Option<String>,
    pub branch: Option<String>,
    pub session_name: Option<String>,
    pub cache_read: f64,
    pub cache_write: f64,
    pub cost: f64,
    #[serde(default)]
    pub cost_unknown: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_cost: Option<f64>,
    pub latest_cache_hit_rate: Option<f64>,
    pub context_window: f64,
    pub context_percent: Option<f64>,
    pub context_tokens: Option<f64>,
    pub model_id: Option<String>,
    pub provider: Option<String>,
    pub reasoning: bool,
    pub thinking_level: Option<String>,
    pub fast_mode: bool,
    pub subscription: bool,
    pub provider_count: usize,
    pub account_suffix: String,
    pub extension_statuses: BTreeMap<String, String>,
}
pub struct FooterComponent {
    pub snapshot: FooterSnapshot,
    pub auto_compact_enabled: bool,
    pub compaction_delegated: bool,
}
/// Status key of the native badge; always first on the status line and never folded.
const BADGE_STATUS_KEY: &str = "  omo-native";
/// Status key rendered right-aligned on the status line; never folded.
const MEMORY_STATUS_KEY: &str = "memory";
/// Known status keys in display order with their fold priority (`None` = never folded; lower folds
/// first). Unknown keys follow in alphabetical order with [`UNKNOWN_STATUS_FOLD_PRIORITY`].
const STATUS_ORDER: &[(&str, Option<u8>)] = &[
    (BADGE_STATUS_KEY, None),
    ("goal", None),
    ("goal-wait", Some(80)),
    ("todo-waiting", Some(85)),
    ("ulw-loop", Some(75)),
    ("loop", Some(60)),
    ("omo-task", Some(70)),
    ("omo-dag", Some(65)),
    ("monitors", Some(50)),
    ("eval-cells", Some(40)),
    ("lsp", Some(20)),
    ("ext:nested-agents:status", Some(10)),
];
const UNKNOWN_STATUS_FOLD_PRIORITY: u8 = 30;
const SYNC_BLOCKED_SUFFIX: &str = " sync blocked";
fn format_money(value: f64) -> String {
    if value != 0. && value.abs() < 0.01 {
        format!("${value:.3}")
    } else {
        format!("${value:.2}")
    }
}
fn sanitize_status(text: &str) -> String {
    text.replace(['\r', '\n', '\t'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
/// Splits a memory status body into `(text, flags)` where flags is the trailing
/// `*`, ` (+N)`, ` !N` run.
fn split_memory_flags(text: &str) -> (&str, &str) {
    let mut end = text.len();
    if let Some(i) = text[..end].rfind(" !")
        && i + 2 < end
        && text[i + 2..end].bytes().all(|b| b.is_ascii_digit())
    {
        end = i;
    }
    if text[..end].ends_with(')')
        && let Some(i) = text[..end].rfind(" (+")
        && i + 3 < end - 1
        && text[i + 3..end - 1]
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.')
    {
        end = i;
    }
    if text[..end].ends_with('*') {
        end -= 1;
    }
    (&text[..end], &text[end..])
}
/// `mem:<id> <age>[*][ (+N)][ !N][ sync blocked]` -> muted `mem <id> <age>`, warning flags, error
/// `sync blocked`. `drop_identity` removes `<id>` for narrow widths.
fn memory_group(text: &str, drop_identity: bool) -> FooterGroup {
    if text.contains('\x1b') {
        return FooterGroup::run(text, ThemeColor::Muted);
    }
    let (body, sync) = match text.strip_suffix(SYNC_BLOCKED_SUFFIX) {
        Some(body) => (body, SYNC_BLOCKED_SUFFIX),
        None => (text, ""),
    };
    let (body, flags) = split_memory_flags(body);
    // `mem:<id> <spinner> reflecting` shows progress: the spinner run is accent.
    let (body, progress) = match body
        .strip_suffix(" reflecting")
        .and_then(|b| b.rsplit_once(' '))
    {
        Some((head, spinner)) if spinner.chars().count() == 1 => {
            (head, format!(" {spinner} reflecting"))
        }
        _ => (body, String::new()),
    };
    let mut group = FooterGroup::default();
    match body.strip_prefix("mem:") {
        Some(rest) => {
            let rest = if drop_identity {
                rest.split_once(' ').map_or("", |(_, age)| age)
            } else {
                rest
            };
            group = group.with("mem", ThemeColor::Muted).with(
                if rest.is_empty() {
                    String::new()
                } else {
                    format!(" {rest}")
                },
                ThemeColor::Muted,
            );
        }
        None => group = group.with(body, ThemeColor::Muted),
    }
    group
        .with(progress, ThemeColor::Accent)
        .with(flags, ThemeColor::Warning)
        .with(sync, ThemeColor::Error)
}
fn status_color(key: &str) -> ThemeColor {
    match key {
        BADGE_STATUS_KEY | "goal" => ThemeColor::Text,
        "ulw-loop" => ThemeColor::Accent,
        _ => ThemeColor::Muted,
    }
}
fn status_group(key: &str, text: &str) -> FooterGroup {
    if text.contains('\x1b') {
        // Extension-colored text keeps its own escapes; wrapping it would reset them mid-run.
        return FooterGroup {
            runs: vec![(text.to_owned(), ThemeColor::Text)],
        };
    }
    // A trailing failure count is the one part that asks for action: `N failed` is error,
    // `⚠ N failed` (a degraded but non-fatal source) is warning.
    if let Some(head) = text.strip_suffix(" failed")
        && let Some((rest, count)) = head.rsplit_once(' ')
        && !count.is_empty()
        && count.bytes().all(|b| b.is_ascii_digit())
    {
        let (rest, failure, color) = match rest.strip_suffix(" ⚠") {
            Some(rest) => (rest, format!(" ⚠ {count} failed"), ThemeColor::Warning),
            None => (rest, format!(" {count} failed"), ThemeColor::Error),
        };
        return FooterGroup::run(rest, status_color(key)).with(failure, color);
    }
    FooterGroup::run(text, status_color(key))
}
fn render_group(group: &FooterGroup, theme: &Theme) -> String {
    group
        .runs
        .iter()
        .map(|(text, color)| {
            if text.contains('\x1b') {
                text.clone()
            } else {
                theme.fg(*color, text)
            }
        })
        .collect()
}
/// Gap between footer groups: 3 spaces, 2 on narrow terminals.
fn gap_for(width: usize) -> usize {
    if width >= 100 { 3 } else { 2 }
}
fn pad_line(
    left: &str,
    left_width: usize,
    right: &str,
    right_width: usize,
    width: usize,
) -> String {
    let padding = " ".repeat(width.saturating_sub(left_width + right_width));
    format!("{left}{padding}{right}")
}
impl FooterComponent {
    pub fn new(snapshot: FooterSnapshot) -> Self {
        Self {
            snapshot,
            auto_compact_enabled: true,
            compaction_delegated: false,
        }
    }
    pub fn set_auto_compact_enabled(&mut self, enabled: bool) {
        self.auto_compact_enabled = enabled;
    }
    pub fn set_compaction_delegated(&mut self, delegated: bool) {
        self.compaction_delegated = delegated;
    }
    fn cost_group(&self) -> Option<FooterGroup> {
        let s = &self.snapshot;
        let subagent_cost = s
            .subagent_cost
            .filter(|cost| cost.is_finite() && *cost > 0.0)
            .unwrap_or(0.0);
        let using_subscription = s.subscription || s.provider.as_deref() == Some("kimi-coding");
        if s.cost == 0. && !using_subscription && subagent_cost <= 0.0 {
            return None;
        }
        let subagent = if subagent_cost > 0.0 {
            format!(" +{}", &format_money(subagent_cost)[1..])
        } else {
            String::new()
        };
        Some(
            FooterGroup::run(format_money(s.cost), ThemeColor::Text)
                .with(if s.cost_unknown { "+?" } else { "" }, ThemeColor::Warning)
                .with(subagent, ThemeColor::Muted)
                .with(
                    if using_subscription { " sub" } else { "" },
                    ThemeColor::Muted,
                ),
        )
    }
    fn context_group(&self) -> FooterGroup {
        let s = &self.snapshot;
        let tokens = s
            .context_tokens
            .or_else(|| {
                s.context_percent
                    .map(|p| (s.context_window * p / 100.).round())
            })
            .map_or_else(|| "?".into(), format_tokens);
        let mut group = FooterGroup::run("ctx ", ThemeColor::Muted);
        let percent = s.context_percent.unwrap_or(0.);
        group = match s.context_percent {
            Some(p) => {
                let color = if p > 90. {
                    ThemeColor::Error
                } else if p > 70. {
                    ThemeColor::Warning
                } else {
                    ThemeColor::Text
                };
                group.with(format!("{}%", p.round() as i64), color)
            }
            None => group.with("?", ThemeColor::Muted),
        };
        group
            .with(
                format!(" {tokens}/{}", format_tokens(s.context_window)),
                ThemeColor::Muted,
            )
            .with(
                if percent > 90. { " compact soon" } else { "" },
                ThemeColor::Error,
            )
            .with(
                if self.auto_compact_enabled {
                    ""
                } else {
                    " manual"
                },
                ThemeColor::Muted,
            )
            .with(
                if self.compaction_delegated {
                    " sdk"
                } else {
                    ""
                },
                ThemeColor::Muted,
            )
    }
    fn cache_group(&self) -> Option<FooterGroup> {
        let s = &self.snapshot;
        if s.cache_read <= 0. && s.cache_write <= 0. {
            return None;
        }
        let rate = s.latest_cache_hit_rate.filter(|v| *v >= 10.)?;
        Some(
            FooterGroup::run("cache ", ThemeColor::Muted)
                .with(format!("{}%", rate.round() as i64), ThemeColor::Text),
        )
    }
    fn model_group(&self, with_provider: bool, strip_vendor: bool) -> FooterGroup {
        let s = &self.snapshot;
        let model = s
            .model_id
            .as_deref()
            .filter(|m| !m.is_empty())
            .unwrap_or("no-model");
        let model = if strip_vendor {
            model.strip_prefix("claude-").unwrap_or(model)
        } else {
            model
        };
        let provider_prefix = if with_provider
            && (s.provider_count > 1 || !s.account_suffix.is_empty())
            && s.model_id.is_some()
        {
            let prefix = format!(
                "{}{}",
                s.provider.as_deref().unwrap_or(""),
                s.account_suffix
            );
            if prefix.is_empty() {
                prefix
            } else {
                format!("{prefix} ")
            }
        } else {
            String::new()
        };
        let level = if s.reasoning {
            format!(
                " {}",
                s.thinking_level
                    .as_deref()
                    .filter(|l| !l.is_empty())
                    .unwrap_or("off")
            )
        } else {
            String::new()
        };
        FooterGroup::run(if s.fast_mode { "⚡ " } else { "" }, ThemeColor::Warning)
            .with(provider_prefix, ThemeColor::Muted)
            .with(model, ThemeColor::Muted)
            .with(level, ThemeColor::Muted)
    }
    fn location_group(cwd: &str, branch: Option<&str>) -> FooterGroup {
        let mut group = FooterGroup::run(cwd, ThemeColor::Muted);
        if let Some(branch) = branch {
            group = group
                .with("  ", ThemeColor::Muted)
                .with(branch, ThemeColor::Muted);
        }
        group
    }
    fn render_top(&self, width: usize, theme: &Theme) -> String {
        let s = &self.snapshot;
        let cwd = compact_cwd(&format_cwd_for_footer(&s.cwd, s.home.as_deref()));
        let branch = s.branch.as_deref().filter(|b| !b.is_empty());
        let session = s
            .session_name
            .as_deref()
            .filter(|n| !n.is_empty())
            .map(|n| FooterGroup::run("session ", ThemeColor::Muted).with(n, ThemeColor::Text));
        let cost = self.cost_group();
        let ctx = self.context_group();
        let cache = self.cache_group();
        let right_for = |with_cache: bool, with_provider: bool, strip_vendor: bool| {
            cost.iter()
                .cloned()
                .chain(std::iter::once(ctx.clone()))
                .chain(cache.iter().filter(|_| with_cache).cloned())
                .chain(std::iter::once(
                    self.model_group(with_provider, strip_vendor),
                ))
                .collect::<Vec<_>>()
        };
        let finish = |left: &[FooterGroup], right: &[FooterGroup], gap: usize| {
            let l = join_groups(left, gap, theme);
            let r = join_groups(right, gap, theme);
            pad_line(
                &l.colored,
                groups_width(left, gap),
                &r.colored,
                groups_width(right, gap),
                width,
            )
        };
        let fits = |left: &[FooterGroup], right: &[FooterGroup], gap: usize| {
            groups_width(left, gap) + gap + groups_width(right, gap) <= width
        };
        let location = Self::location_group(&cwd, branch);
        let gap = gap_for(width);
        // Drop session, then cache, then the provider prefix.
        for (with_session, with_cache, with_provider) in [
            (true, true, true),
            (false, true, true),
            (false, false, true),
            (false, false, false),
        ] {
            let mut left = vec![location.clone()];
            if with_session && let Some(session) = &session {
                left.push(session.clone());
            }
            let right = right_for(with_cache, with_provider, false);
            if fits(&left, &right, gap) {
                return finish(&left, &right, gap);
            }
        }
        let branch_width = branch.map_or(0, |b| 2 + visible_width(b));
        // Head-elide the path, then strip the vendor prefix from the model and elide again.
        // Before stripping the vendor, the path must keep at least its last component.
        let last_component = cwd.rsplit('/').next().unwrap_or(&cwd);
        let min_path = visible_width(&cwd).min(2 + visible_width(last_component));
        for (strip_vendor, min_path) in [(false, min_path), (true, 2)] {
            let right = right_for(false, false, strip_vendor);
            let budget = width.saturating_sub(gap + groups_width(&right, gap) + branch_width);
            if budget >= min_path.max(1) {
                let left = [Self::location_group(&elide_path(&cwd, budget), branch)];
                return finish(&left, &right, gap);
            }
        }
        // Extremely narrow: keep only an elided path, then only the right side, then truncate it.
        let right = right_for(false, false, true);
        let right_width = groups_width(&right, gap);
        let budget = width.saturating_sub(gap + right_width);
        if budget >= 1 {
            let left = [FooterGroup::run(
                elide_path(&cwd, budget),
                ThemeColor::Muted,
            )];
            return finish(&left, &right, gap);
        }
        if right_width <= width {
            return finish(&[], &right, gap);
        }
        let truncated = flatten_groups(&right, gap).truncated(width);
        pad_line("", 0, &truncated.render(theme), truncated.width(), width)
    }
    fn render_status(&self, width: usize, theme: &Theme) -> Option<String> {
        let statuses = &self.snapshot.extension_statuses;
        let mut entries: Vec<(FooterGroup, Option<u8>)> = Vec::new();
        let mut push = |key: &str, value: &str, priority: Option<u8>| {
            let text = sanitize_status(value);
            if !text.is_empty() {
                // Failures ask for attention, so they never fold (plan: "failed counts").
                let priority = priority.filter(|_| !text.contains(" failed"));
                entries.push((status_group(key, &text), priority));
            }
        };
        for (key, priority) in STATUS_ORDER {
            if let Some(value) = statuses.get(*key) {
                push(key, value.as_str(), *priority);
            }
        }
        for (key, value) in statuses {
            if key != MEMORY_STATUS_KEY && !STATUS_ORDER.iter().any(|(k, _)| *k == key.as_str()) {
                push(
                    key.as_str(),
                    value.as_str(),
                    Some(UNKNOWN_STATUS_FOLD_PRIORITY),
                );
            }
        }
        let memory = statuses
            .get(MEMORY_STATUS_KEY)
            .map(|v| sanitize_status(v))
            .filter(|v| !v.is_empty());
        if entries.is_empty() && memory.is_none() {
            return None;
        }
        let gap = gap_for(width);
        let foldable: Vec<FoldableStatus> = entries
            .iter()
            .map(|(group, priority)| FoldableStatus {
                text: group.plain(),
                fold_priority: *priority,
            })
            .collect();
        let memory_variants: Vec<FooterGroup> = memory
            .as_deref()
            .map(|m| vec![memory_group(m, false), memory_group(m, true)])
            .unwrap_or_default();
        // Prefer the full memory text; drop its identity when the pinned statuses would not fit.
        let right = memory_variants
            .iter()
            .find(|m| {
                let budget = width.saturating_sub(m.width() + gap);
                status_fold_width(&foldable, &plan_status_fold(&foldable, budget, gap), gap)
                    <= budget
            })
            .or(memory_variants.last())
            .cloned();
        let right_width = right.as_ref().map_or(0, FooterGroup::width);
        if right_width >= width {
            let truncated = right.unwrap_or_default().truncated(width);
            return Some(render_group(&truncated, theme));
        }
        let budget = if right.is_some() {
            width.saturating_sub(right_width + gap)
        } else {
            width
        };
        let fold = plan_status_fold(&foldable, budget, gap);
        let mut left: Vec<FooterGroup> = fold.kept.iter().map(|&i| entries[i].0.clone()).collect();
        if fold.folded > 0 {
            left.push(FooterGroup::run(
                format!("+{}", fold.folded),
                ThemeColor::Muted,
            ));
        }
        if groups_width(&left, gap) > budget {
            let mut flat = flatten_groups(&left, gap).truncated(budget.saturating_sub(1));
            flat = flat.with("…", ThemeColor::Muted);
            left = vec![flat];
        }
        let left_width = groups_width(&left, gap);
        let left_colored = left
            .iter()
            .map(|g| render_group(g, theme))
            .collect::<Vec<_>>()
            .join(&" ".repeat(gap));
        Some(match right {
            Some(right) => pad_line(
                &left_colored,
                left_width,
                &render_group(&right, theme),
                right_width,
                width,
            ),
            None => left_colored,
        })
    }
    pub fn render(&self, width: usize, theme: &Theme) -> Result<Vec<String>, std::io::Error> {
        let mut lines = vec![self.render_top(width, theme)];
        lines.extend(self.render_status(width, theme));
        Ok(lines)
    }
}

pub fn account_footer_suffix(
    credential: Option<&maho_ai::auth::pool::slots::PooledCredential>,
    session_id: &str,
) -> String {
    use maho_ai::auth::pool::slots::account_label;
    use sha2::{Digest, Sha256};
    let slots = credential.map_or_else(Vec::new, maho_ai::auth::pool::kinds::slots_of_own_kind);
    if slots.len() < 2 {
        return String::new();
    }
    let winner = credential
        .and_then(|c| c.pinned.as_ref())
        .and_then(|p| slots.iter().find(|s| &s.name == p))
        .or_else(|| {
            slots.iter().max_by_key(|s| {
                let digest = Sha256::digest(format!("{session_id}\0{}", s.name).as_bytes());
                let mut bytes = [0; 8];
                bytes.copy_from_slice(&digest[..8]);
                u64::from_be_bytes(bytes)
            })
        });
    winner.map_or_else(String::new, |s| {
        format!(
            "@{}",
            truncate_to_width(
                &account_label(&s.name, s.display_name.as_deref()),
                24,
                "…",
                false
            )
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(snapshot: &FooterSnapshot, width: usize) -> Vec<String> {
        let theme = Theme::builtin("dark", crate::theme::ColorMode::Truecolor).unwrap();
        FooterComponent::new(snapshot.clone())
            .render(width, &theme)
            .unwrap()
            .iter()
            .map(|l| maho_tui::utils::strip_terminal_sequences(l))
            .collect()
    }

    #[test]
    fn footer_cost_renders_unknown_and_subagent_cost() {
        let mut snapshot = FooterSnapshot {
            cost: 0.38,
            ..Default::default()
        };
        assert!(plain(&snapshot, 160)[0].contains("$0.38   ctx"));

        snapshot.cost_unknown = true;
        assert!(plain(&snapshot, 160)[0].contains("$0.38+?   ctx"));

        snapshot.subagent_cost = Some(1.27);
        assert!(plain(&snapshot, 160)[0].contains("$0.38+? +1.27   ctx"));

        snapshot.cost = 0.0;
        snapshot.cost_unknown = false;
        snapshot.subagent_cost = Some(0.5);
        assert!(plain(&snapshot, 160)[0].contains("$0.00 +0.50   ctx"));

        snapshot.cost = 0.004;
        snapshot.subagent_cost = None;
        assert!(plain(&snapshot, 160)[0].contains("$0.004   ctx"));
    }

    #[test]
    fn status_line_orders_known_keys_and_pins_memory_right() {
        let snapshot = FooterSnapshot {
            extension_statuses: [
                ("zeta", "Z"),
                ("lsp", "LSP 2"),
                ("goal", "Pursuing goal"),
                ("  omo-native", "Maho Code"),
                ("memory", "mem:agent 1m ago* (+12) !3 sync blocked"),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
            ..Default::default()
        };
        let lines = plain(&snapshot, 120);
        assert!(lines[1].starts_with("Maho Code   Pursuing goal   LSP 2   Z"));
        assert!(lines[1].ends_with("mem agent 1m ago* (+12) !3 sync blocked"));
        assert_eq!(maho_tui::utils::visible_width(&lines[1]), 120);
    }

    #[test]
    fn status_line_folds_lowest_priority_first() {
        let snapshot = FooterSnapshot {
            extension_statuses: [
                ("  omo-native", "Maho Code"),
                ("goal", "Pursuing goal"),
                ("loop", "Loop (watch): next in 4m"),
                ("lsp", "LSP 2 init 1"),
                ("ext:nested-agents:status", "AGENTS.md 2"),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
            ..Default::default()
        };
        let lines = plain(&snapshot, 60);
        assert_eq!(
            lines[1].trim_end(),
            "Maho Code  Pursuing goal  Loop (watch): next in 4m  +2"
        );
    }

    #[test]
    fn both_lines_are_one_row_at_every_width() {
        let snapshot = FooterSnapshot {
            cwd: "/home/u/work/project/ferryx".into(),
            home: Some("/home/u".into()),
            branch: Some("main".into()),
            session_name: Some("port".into()),
            cost: 1.234,
            subagent_cost: Some(1.27),
            cache_read: 1.,
            latest_cache_hit_rate: Some(80.),
            context_window: 400000.,
            context_percent: Some(93.),
            model_id: Some("claude-opus-5-5".into()),
            provider: Some("anthropic".into()),
            provider_count: 2,
            reasoning: true,
            thinking_level: Some("high".into()),
            extension_statuses: [
                ("  omo-native", "Maho Code"),
                ("goal", "Pursuing goal (14h 37m)"),
                ("omo-task", "8 subagents 1 failed"),
                ("memory", "mem:ferryx 2h ago* (+12) !3 sync blocked"),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
            ..Default::default()
        };
        for width in 40..=200 {
            for line in plain(&snapshot, width) {
                assert!(
                    maho_tui::utils::visible_width(&line) <= width,
                    "{width}: {line}"
                );
            }
            assert!(plain(&snapshot, width)[0].contains("ctx 93%"), "{width}");
        }
    }
}
