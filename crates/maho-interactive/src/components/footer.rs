//! Port of footer.ts; the host supplies its O(1) session/footer snapshot.
use super::footer_layout::{
    FooterLayout, FooterLayoutInput, FooterRightLabel, FooterSegment, plan_footer_layout,
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
    pub fn render(&self, width: usize, theme: &Theme) -> Result<Vec<String>, std::io::Error> {
        let s = &self.snapshot;
        let segment = |plain: String, c: ThemeColor| FooterSegment {
            colored: theme.fg(c, &plain),
            plain,
        };
        let pwd = format_cwd_for_footer(&s.cwd, s.home.as_deref());
        let mut anchor = vec![segment(pwd, ThemeColor::Accent)];
        if let Some(b) = &s.branch
            && !b.is_empty()
        {
            anchor.push(segment(b.clone(), ThemeColor::Warning));
        }
        let mut middle = Vec::new();
        if let Some(n) = &s.session_name
            && !n.is_empty()
        {
            middle.push(segment(n.clone(), ThemeColor::Muted));
        }
        if (s.cache_read > 0. || s.cache_write > 0.)
            && let Some(rate) = s.latest_cache_hit_rate.filter(|v| *v >= 10.)
        {
            middle.push(segment(format!("CH{rate:.1}%"), ThemeColor::Dim));
        }
        if s.cost != 0. || s.subscription || s.provider.as_deref() == Some("kimi-coding") {
            middle.push(segment(
                format!(
                    "$ {:.3}{}",
                    s.cost,
                    if s.subscription || s.provider.as_deref() == Some("kimi-coding") {
                        " (sub)"
                    } else {
                        ""
                    }
                )
                .replacen("$ ", "$", 1),
                ThemeColor::Success,
            ));
        }
        let percent = s.context_percent.unwrap_or(0.);
        let percent_text = s
            .context_percent
            .map_or_else(|| "?".into(), |p| format!("{p:.1}"));
        let tokens = s
            .context_tokens
            .or_else(|| {
                s.context_percent
                    .map(|p| (s.context_window * p / 100.).round())
            })
            .map_or_else(|| "?".into(), format_tokens);
        let auto = if self.auto_compact_enabled {
            " (auto)"
        } else {
            ""
        };
        let sdk = if self.compaction_delegated {
            " (SDK)"
        } else {
            ""
        };
        let base = format!(
            "{tokens}/{} ({}{}){auto}",
            format_tokens(s.context_window),
            percent_text,
            if s.context_percent.is_some() { "%" } else { "" }
        );
        let color = if percent > 90. {
            ThemeColor::Error
        } else if percent > 70. {
            ThemeColor::Warning
        } else {
            ThemeColor::Muted
        };
        let make_tail = |marker: bool| FooterSegment {
            plain: format!("{base}{}", if marker { sdk } else { "" }),
            colored: format!(
                "{}{}",
                theme.fg(color, &base),
                if marker {
                    theme.fg(ThemeColor::Muted, sdk)
                } else {
                    String::new()
                }
            ),
        };
        let mut tail = make_tail(!sdk.is_empty());
        let model = s
            .model_id
            .as_deref()
            .filter(|m| !m.is_empty())
            .unwrap_or("no-model");
        let fast = if s.fast_mode { "⚡ " } else { "" };
        let thinking = if s.reasoning {
            format!(
                ":{}",
                s.thinking_level
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("off")
            )
        } else {
            String::new()
        };
        let minimal_plain = format!("{fast}{model}{thinking}");
        let mut runs = Vec::new();
        if !fast.is_empty() {
            runs.push((fast, ThemeColor::Warning));
        }
        runs.push((model, ThemeColor::Accent));
        if !thinking.is_empty() {
            runs.push((&thinking, ThemeColor::Dim));
        }
        let color_runs = |runs: &[(&str, ThemeColor)], plain: &str| {
            let text = maho_tui::utils::strip_terminal_sequences(plain);
            let mut remaining = text.as_str();
            let mut colored = String::new();
            for (text, c) in runs {
                let count = text.chars().count().min(remaining.chars().count());
                let end = remaining
                    .char_indices()
                    .nth(count)
                    .map_or(remaining.len(), |(i, _)| i);
                colored.push_str(&theme.fg(*c, &remaining[..end]));
                remaining = &remaining[end..];
                if remaining.is_empty() {
                    break;
                }
            }
            colored
        };
        let minimal = FooterSegment {
            colored: color_runs(&runs, &minimal_plain),
            plain: minimal_plain,
        };
        let provider_prefix =
            if (s.provider_count > 1 || !s.account_suffix.is_empty()) && s.model_id.is_some() {
                format!(
                    "({}{}) ",
                    s.provider.as_deref().unwrap_or(""),
                    s.account_suffix
                )
            } else {
                String::new()
            };
        let full = if provider_prefix.is_empty() {
            None
        } else {
            let mut full_runs = vec![(provider_prefix.as_str(), ThemeColor::Muted)];
            full_runs.extend(runs.iter().copied());
            let plain = format!("{provider_prefix}{}", minimal.plain);
            Some(FooterSegment {
                colored: color_runs(&full_runs, &plain),
                plain,
            })
        };
        let right_labels = FooterRightLabel { minimal, full };
        let marker = segment("…".into(), ThemeColor::Dim);
        let plan_for = |t: &FooterSegment| {
            plan_footer_layout(&FooterLayoutInput {
                width,
                anchor: &anchor,
                pwd_index: 0,
                middle: &middle,
                tail: t,
                right: &right_labels,
                separator: " • ",
                min_padding: 2,
                ellipsis_marker: &marker,
            })
        };
        let mut plan = plan_for(&tail)?;
        if let FooterLayout::LeftElided { left_plain } = &plan
            && !sdk.is_empty()
            && !left_plain.contains(sdk.trim_start())
        {
            tail = make_tail(false);
            plan = plan_for(&tail)?;
        }
        let mut left_segments = anchor.clone();
        let (kept, show, use_full, pwd, left_plain, right_plain) = match &plan {
            FooterLayout::Full { use_full_right } => {
                (middle.len(), false, *use_full_right, None, None, None)
            }
            FooterLayout::MiddleElided {
                kept_middle_count,
                show_marker,
                use_full_right,
            } => (
                *kept_middle_count,
                *show_marker,
                *use_full_right,
                None,
                None,
                None,
            ),
            FooterLayout::PwdElided {
                pwd_plain,
                kept_middle_count,
                show_marker,
                use_full_right,
            } => (
                *kept_middle_count,
                *show_marker,
                *use_full_right,
                Some(pwd_plain),
                None,
                None,
            ),
            FooterLayout::LeftElided { left_plain } => {
                (0, false, false, None, Some(left_plain), None)
            }
            FooterLayout::RightTruncated { right_plain } => {
                (0, false, false, None, None, Some(right_plain))
            }
        };
        if let Some(p) = pwd {
            left_segments[0] = segment(p.clone(), ThemeColor::Accent);
        }
        left_segments.extend(middle.iter().take(kept).cloned());
        if show {
            left_segments.push(marker);
        }
        left_segments.push(tail);
        let (left, left_width) = if let Some(p) = left_plain {
            (theme.fg(ThemeColor::Muted, p), visible_width(p))
        } else if right_plain.is_some() {
            (String::new(), 0)
        } else {
            (
                left_segments
                    .iter()
                    .map(|s| s.colored.as_str())
                    .collect::<Vec<_>>()
                    .join(&theme.fg(ThemeColor::BorderMuted, " • ")),
                visible_width(
                    &left_segments
                        .iter()
                        .map(|s| s.plain.as_str())
                        .collect::<Vec<_>>()
                        .join(" • "),
                ),
            )
        };
        let right = if let Some(p) = right_plain {
            FooterSegment {
                plain: p.clone(),
                colored: color_runs(&runs, p),
            }
        } else if use_full {
            right_labels.full.unwrap_or(right_labels.minimal)
        } else {
            right_labels.minimal
        };
        let padding = " ".repeat(width.saturating_sub(left_width + visible_width(&right.plain)));
        let mut lines = vec![format!("{left}{padding}{}", right.colored)];
        if !s.extension_statuses.is_empty() {
            let text = s
                .extension_statuses
                .values()
                .map(|v| {
                    v.replace(['\r', '\n', '\t'], " ")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(truncate_to_width(
                &text,
                width,
                &theme.fg(ThemeColor::Dim, "..."),
                false,
            ));
        }
        Ok(lines)
    }
}

pub fn account_footer_suffix(
    credential: Option<&maho_ai::auth::pool::slots::PooledCredential>,
    session_id: &str,
) -> String {
    use maho_ai::auth::pool::slots::{account_label, list_slots};
    use sha2::{Digest, Sha256};
    let slots = list_slots(credential);
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
