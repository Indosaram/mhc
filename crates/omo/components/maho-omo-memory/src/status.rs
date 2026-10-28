use crate::context::MemoryIdentityContext;
use crate::sync_status_cached::{SyncStatusReader, SyncStatusReport};
use memory_core::git::{GitMemoryRepo, errors::GitError};
pub const MEMORY_STATUS_KEY: &str = "memory";
pub const MEMORY_STATUS_MAX_WIDTH: usize = 60;
pub const MEMORY_HEALTH_SCAN_LIMIT: usize = 20;
/// Shown while a sync failure is reported for this repository (#10637); never trimmed from the line.
pub const SYNC_BLOCKED_SEGMENT: &str = " sync blocked";
pub trait MemoryStatusUi {
    fn set_status(&mut self, key: &str, text: Option<&str>);
    fn notify(&mut self, message: &str, level: &str);
}
#[derive(Default)]
pub struct MemoryStatusSegments {
    pub dirty: bool,
    pub backlog_steps: f64,
    pub streak: usize,
    pub sync_blocked: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub struct MemoryStatusResult {
    pub notified: bool,
    pub footer_shown: bool,
}
pub struct RefreshMemoryStatusInput<'a> {
    pub context: &'a MemoryIdentityContext,
    pub compile_warn_tokens: usize,
    pub already_notified: bool,
    pub now_ms: i64,
    pub show_footer: bool,
    pub check_advisory: bool,
    pub session_id: Option<&'a str>,
    /// Reads the repository's sync status for the ` sync blocked` segment; omitted means no segment.
    pub sync_status: Option<&'a SyncStatusReader>,
}
pub fn format_relative_age(committed_at: f64, now: f64) -> Option<String> {
    let age = now - committed_at;
    if !age.is_finite() || age < 0.0 {
        return None;
    }
    Some(if age < 60_000.0 {
        "just now".into()
    } else if age < 3_600_000.0 {
        format!("{}m ago", (age / 60_000.0).floor())
    } else if age < 86_400_000.0 {
        format!("{}h ago", (age / 3_600_000.0).floor())
    } else {
        format!("{}d ago", (age / 86_400_000.0).floor())
    })
}
/// Assembles `mem:<identity> <age>[*][ (+N)][ !N][ sync blocked]`, dropping the optional segments
/// right-to-left to fit. ` sync blocked` is never dropped: it is the one segment that asks the person to act.
pub fn format_memory_status_line(
    identity: &str,
    age: &str,
    segments: &MemoryStatusSegments,
) -> String {
    let base = format!("mem:{identity} {age}");
    let sync = if segments.sync_blocked {
        SYNC_BLOCKED_SEGMENT
    } else {
        ""
    };
    let optional = [
        if segments.dirty {
            "*".to_owned()
        } else {
            String::new()
        },
        if segments.backlog_steps >= 1.0 {
            format!(" (+{})", segments.backlog_steps)
        } else {
            String::new()
        },
        if segments.streak >= 3 {
            format!(" !{}", segments.streak)
        } else {
            String::new()
        },
    ];
    for keep in (1..=optional.len()).rev() {
        let candidate = format!("{base}{}{sync}", optional[..keep].join(""));
        if candidate.encode_utf16().count() <= MEMORY_STATUS_MAX_WIDTH {
            return candidate;
        }
    }
    format!("{base}{sync}")
}
pub fn read_memory_status_segments(
    repo: &GitMemoryRepo,
    context: &MemoryIdentityContext,
    session_id: Option<&str>,
    now: i64,
    sync_status: Option<&SyncStatusReader>,
) -> MemoryStatusSegments {
    let dirty = repo
        .status(&[] as &[&str])
        .is_ok_and(|status| !status.trim().is_empty());
    let backlog_steps = session_id
        .filter(|id| !id.is_empty())
        .and_then(|id| {
            std::fs::read(
                context
                    .identity_paths
                    .transcripts
                    .join(id)
                    .join("state.json"),
            )
            .ok()
        })
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("steps_since_last_successful_reflection")
                .and_then(serde_json::Value::as_f64)
        })
        .filter(|steps| steps.is_finite() && *steps > 0.0)
        .map(f64::floor)
        .unwrap_or(0.0);
    let streak = crate::worker::health::read_reflection_health(
        &context.identity_paths.reflection.join("completions"),
        MEMORY_HEALTH_SCAN_LIMIT,
        now,
    )
    .streak;
    let sync_blocked = read_sync_blocked(context, sync_status);
    MemoryStatusSegments {
        dirty,
        backlog_steps,
        streak,
        sync_blocked,
    }
}
/// A failing sync cycle is the one segment that asks the person to act; a read that fails says nothing.
fn read_sync_blocked(
    context: &MemoryIdentityContext,
    sync_status: Option<&SyncStatusReader>,
) -> bool {
    let Some(sync_status) = sync_status else {
        return false;
    };
    matches!(
        (sync_status)(&context.identity_paths.repo),
        Some(SyncStatusReport::Failing(_))
    )
}
pub fn refresh_memory_status(
    repo: &GitMemoryRepo,
    ui: &mut dyn MemoryStatusUi,
    input: &RefreshMemoryStatusInput<'_>,
) -> Result<MemoryStatusResult, GitError> {
    let Some(head) = repo.head()? else {
        return Ok(MemoryStatusResult {
            notified: false,
            footer_shown: false,
        });
    };
    let mut footer_shown = false;
    if input.show_footer
        && let Some(committed) = repo.head_commit_timestamp()?
        && let Some(age) = format_relative_age(
            committed.to_string().parse::<f64>().unwrap_or(f64::NAN) * 1000.0,
            input.now_ms.to_string().parse().unwrap_or(f64::NAN),
        )
    {
        let segments = read_memory_status_segments(
            repo,
            input.context,
            input.session_id,
            input.now_ms,
            input.sync_status,
        );
        ui.set_status(
            MEMORY_STATUS_KEY,
            Some(&format_memory_status_line(
                &input.context.identity,
                &age,
                &segments,
            )),
        );
        footer_shown = true;
    }
    if !input.check_advisory || input.already_notified {
        return Ok(MemoryStatusResult {
            notified: false,
            footer_shown,
        });
    }
    let paths = repo.ls_tree(Some(&head), None)?;
    let mut total_bytes = 0;
    for path in paths
        .iter()
        .filter(|path| path.starts_with("system/") && path.ends_with(".md"))
    {
        total_bytes += repo.show(&head, path)?.len();
    }
    let estimate = total_bytes / 4;
    if estimate < input.compile_warn_tokens {
        return Ok(MemoryStatusResult {
            notified: false,
            footer_shown,
        });
    }
    ui.notify(
        &format!(
            "system memory ~{estimate} tokens exceeds advisory {}; consider /doctor",
            input.compile_warn_tokens
        ),
        "warning",
    );
    Ok(MemoryStatusResult {
        notified: true,
        footer_shown,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn age_boundaries() {
        for (age, text) in [
            (0.0, "just now"),
            (59_999.0, "just now"),
            (60_000.0, "1m ago"),
            (3_600_000.0, "1h ago"),
            (86_400_000.0, "1d ago"),
        ] {
            assert_eq!(format_relative_age(0.0, age).as_deref(), Some(text));
        }
        assert_eq!(format_relative_age(1.0, 0.0), None);
        assert_eq!(format_relative_age(f64::NAN, 0.0), None);
    }
    #[test]
    fn segments_drop_right_to_left_with_utf16_width() {
        let segments = MemoryStatusSegments {
            dirty: true,
            backlog_steps: 12.0,
            streak: 3,
            sync_blocked: false,
        };
        assert_eq!(
            format_memory_status_line("agent", "1m ago", &segments),
            "mem:agent 1m ago* (+12) !3"
        );
        let id = "a".repeat(42);
        assert_eq!(
            format_memory_status_line(&id, "1m ago", &segments),
            format!("mem:{id} 1m ago* (+12)")
        );
        let id = "😀".repeat(24);
        assert_eq!(
            format_memory_status_line(&id, "1m ago", &segments),
            format!("mem:{id} 1m ago*")
        );
    }
    #[derive(Default)]
    struct Ui {
        lines: Vec<String>,
        notifications: usize,
    }
    #[test]
    fn zero_backlog_and_short_streak_are_hidden() {
        assert_eq!(
            format_memory_status_line(
                "agent",
                "just now",
                &MemoryStatusSegments { sync_blocked: false,
                    dirty: false,
                    backlog_steps: 0.0,
                    streak: 2
                }
            ),
            "mem:agent just now"
        );
    }
    #[test]
    fn dirty_marker_follows_age_without_space() {
        assert_eq!(
            format_memory_status_line(
                "agent",
                "1h ago",
                &MemoryStatusSegments {
                    dirty: true,
                    ..Default::default()
                }
            ),
            "mem:agent 1h ago*"
        );
    }
    #[test]
    fn backlog_without_dirty_has_space() {
        assert_eq!(
            format_memory_status_line(
                "agent",
                "1h ago",
                &MemoryStatusSegments {
                    backlog_steps: 14.0,
                    ..Default::default()
                }
            ),
            "mem:agent 1h ago (+14)"
        );
    }
    #[test]
    fn streak_at_threshold_renders() {
        assert_eq!(
            format_memory_status_line(
                "agent",
                "1h ago",
                &MemoryStatusSegments {
                    streak: 3,
                    ..Default::default()
                }
            ),
            "mem:agent 1h ago !3"
        );
    }
    #[test]
    fn a_sync_blocked_segment_is_appended_and_never_dropped() {
        let segments = MemoryStatusSegments {
            sync_blocked: true,
            ..Default::default()
        };
        assert_eq!(
            format_memory_status_line("agent", "just now", &segments),
            "mem:agent just now sync blocked"
        );
        // Even when the line is already over budget, the segment that asks the person to act stays.
        let identity = "a".repeat(80);
        assert_eq!(
            format_memory_status_line(&identity, "just now", &segments),
            format!("mem:{identity} just now sync blocked")
        );
        let dirty = MemoryStatusSegments {
            dirty: true,
            backlog_steps: 12.0,
            streak: 3,
            sync_blocked: true,
        };
        assert!(format_memory_status_line("agent", "1m ago", &dirty).ends_with("sync blocked"));
    }
    #[test]
    fn a_failing_sync_cycle_marks_the_segment_and_an_absent_reader_does_not() {
        let root = tempfile::tempdir().unwrap();
        let (engine, context, _) = fixture(root.path());
        assert!(
            !read_memory_status_segments(&engine.repo, &context, Some("session"), 0, None)
                .sync_blocked
        );
        let healthy: crate::sync_status_cached::SyncStatusReader = std::sync::Arc::new(|_| {
            Some(crate::sync_status_cached::SyncStatusReport::Status(
                crate::sync_runner::SyncRepoStatus::Unavailable {
                    reason: "no account".into(),
                },
            ))
        });
        assert!(
            !read_memory_status_segments(
                &engine.repo,
                &context,
                Some("session"),
                0,
                Some(&healthy)
            )
            .sync_blocked
        );
        let failing: crate::sync_status_cached::SyncStatusReader = std::sync::Arc::new(|_| {
            Some(crate::sync_status_cached::SyncStatusReport::Failing(
                crate::sync_status_cached::SyncFailure {
                    kind: "auth".into(),
                    reason: "Token expired.".into(),
                    action: "Run /login.".into(),
                },
            ))
        });
        assert!(
            read_memory_status_segments(&engine.repo, &context, Some("session"), 0, Some(&failing))
                .sync_blocked
        );
    }
    #[test]
    fn over_budget_base_drops_every_optional_segment() {
        let identity = "a".repeat(80);
        assert_eq!(
            format_memory_status_line(
                &identity,
                "just now",
                &MemoryStatusSegments { sync_blocked: false,
                    dirty: true,
                    backlog_steps: 14.0,
                    streak: 3
                }
            ),
            format!("mem:{identity} just now")
        );
    }
    #[test]
    fn infinite_timestamp_suppresses_age() {
        assert!(format_relative_age(f64::INFINITY, 0.0).is_none());
        assert!(format_relative_age(0.0, f64::INFINITY).is_none());
    }
    #[test]
    fn minute_bucket_floors_partial_minutes() {
        assert_eq!(
            format_relative_age(0.0, 90_000.0).as_deref(),
            Some("1m ago")
        );
    }
    #[test]
    fn hour_bucket_floors_partial_hours() {
        assert_eq!(
            format_relative_age(0.0, 5_400_000.0).as_deref(),
            Some("1h ago")
        );
    }
    #[test]
    fn day_bucket_floors_partial_days() {
        assert_eq!(
            format_relative_age(0.0, 129_600_000.0).as_deref(),
            Some("1d ago")
        );
    }
    impl MemoryStatusUi for Ui {
        fn set_status(&mut self, key: &str, text: Option<&str>) {
            assert_eq!(key, MEMORY_STATUS_KEY);
            self.lines.push(text.unwrap_or("").into());
        }
        fn notify(&mut self, _: &str, level: &str) {
            assert_eq!(level, "warning");
            self.notifications += 1;
        }
    }
    fn fixture(
        root: &std::path::Path,
    ) -> (
        crate::engine_session::MemoryEngineSession,
        MemoryIdentityContext,
        i64,
    ) {
        let paths = memory_core::identity::layout::build_identity_paths(root, "agent");
        let engine = crate::engine_session::prepare_memory_engine_session(
            "agent",
            &paths,
            Default::default(),
        )
        .unwrap();
        let now = engine.repo.head_commit_timestamp().unwrap().unwrap() * 1000;
        let context = MemoryIdentityContext::new(
            "agent".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        (engine, context, now)
    }
    fn refresh(
        engine: &crate::engine_session::MemoryEngineSession,
        context: &MemoryIdentityContext,
        now: i64,
        footer: bool,
        advisory: bool,
        notified: bool,
        limit: usize,
    ) -> Ui {
        let mut ui = Ui::default();
        refresh_memory_status(
            &engine.repo,
            &mut ui,
            &RefreshMemoryStatusInput {
                context,
                compile_warn_tokens: limit,
                already_notified: notified,
                now_ms: now,
                show_footer: footer,
                check_advisory: advisory,
                session_id: Some("session"),
                sync_status: None,
            },
        )
        .unwrap();
        ui
    }
    #[test]
    fn real_committed_age_uses_system_clock() {
        let root = tempfile::tempdir().unwrap();
        let (engine, context, now) = fixture(root.path());
        assert_eq!(
            refresh(&engine, &context, now + 90000, true, false, false, 1).lines,
            ["mem:agent 1m ago"]
        );
    }
    #[test]
    fn footer_disabled_still_checks_advisory() {
        let root = tempfile::tempdir().unwrap();
        let (engine, context, now) = fixture(root.path());
        let ui = refresh(&engine, &context, now, false, true, false, 1);
        assert!(ui.lines.is_empty());
        assert_eq!(ui.notifications, 1);
    }
    #[test]
    fn advisory_disabled_still_renders_footer() {
        let root = tempfile::tempdir().unwrap();
        let (engine, context, now) = fixture(root.path());
        let ui = refresh(&engine, &context, now, true, false, false, 1);
        assert_eq!(ui.lines.len(), 1);
        assert_eq!(ui.notifications, 0);
    }
    #[test]
    fn already_notified_suppresses_advisory() {
        let root = tempfile::tempdir().unwrap();
        let (engine, context, now) = fixture(root.path());
        let ui = refresh(&engine, &context, now, true, true, true, 1);
        assert_eq!(ui.lines.len(), 1);
        assert_eq!(ui.notifications, 0);
    }
    #[test]
    fn under_threshold_is_silent() {
        let root = tempfile::tempdir().unwrap();
        let (engine, context, now) = fixture(root.path());
        assert_eq!(
            refresh(&engine, &context, now, true, true, false, usize::MAX).notifications,
            0
        );
    }
    #[test]
    fn future_committed_timestamp_suppresses_footer() {
        let root = tempfile::tempdir().unwrap();
        let (engine, context, now) = fixture(root.path());
        assert!(
            refresh(&engine, &context, now - 1, true, false, false, 1)
                .lines
                .is_empty()
        );
    }
    #[test]
    fn real_dirty_repository_adds_marker() {
        let root = tempfile::tempdir().unwrap();
        let (engine, context, now) = fixture(root.path());
        std::fs::write(context.identity_paths.repo.join("new.md"), "dirty").unwrap();
        assert_eq!(
            refresh(&engine, &context, now, true, false, false, 1).lines,
            ["mem:agent just now*"]
        );
    }
    #[test]
    fn real_journal_backlog_is_floored() {
        let root = tempfile::tempdir().unwrap();
        let (engine, context, _) = fixture(root.path());
        let dir = context.identity_paths.transcripts.join("session");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("state.json"),
            r#"{"steps_since_last_successful_reflection":14.8}"#,
        )
        .unwrap();
        assert_eq!(
            read_memory_status_segments(&engine.repo, &context, Some("session"), 0, None)
                .backlog_steps,
            14.0
        );
    }
    #[test]
    fn corrupt_journal_uses_zero_backlog() {
        let root = tempfile::tempdir().unwrap();
        let (engine, context, _) = fixture(root.path());
        let dir = context.identity_paths.transcripts.join("session");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("state.json"), "corrupt").unwrap();
        assert_eq!(
            read_memory_status_segments(&engine.repo, &context, Some("session"), 0, None)
                .backlog_steps,
            0.0
        );
    }
    #[test]
    fn negative_journal_backlog_is_zero() {
        let root = tempfile::tempdir().unwrap();
        let (engine, context, _) = fixture(root.path());
        let dir = context.identity_paths.transcripts.join("session");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("state.json"),
            r#"{"steps_since_last_successful_reflection":-3}"#,
        )
        .unwrap();
        assert_eq!(
            read_memory_status_segments(&engine.repo, &context, Some("session"), 0, None)
                .backlog_steps,
            0.0
        );
    }
    #[test]
    fn missing_session_does_not_read_other_journal() {
        let root = tempfile::tempdir().unwrap();
        let (engine, context, _) = fixture(root.path());
        let dir = context.identity_paths.transcripts.join("session");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("state.json"),
            r#"{"steps_since_last_successful_reflection":14}"#,
        )
        .unwrap();
        assert_eq!(
            read_memory_status_segments(&engine.repo, &context, None, 0, None).backlog_steps,
            0.0
        );
    }
    #[test]
    fn real_repo_footer_and_advisory_once() {
        let dir = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(dir.path(), "agent");
        let engine = crate::engine_session::prepare_memory_engine_session(
            "agent",
            &paths,
            Default::default(),
        )
        .unwrap();
        let context = MemoryIdentityContext::new(
            "agent".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 1.0,
            },
        );
        let now = engine.repo.head_commit_timestamp().unwrap().unwrap() * 1000;
        let mut ui = Ui::default();
        let mut input = RefreshMemoryStatusInput {
            context: &context,
            compile_warn_tokens: 1,
            already_notified: false,
            now_ms: now,
            show_footer: true,
            check_advisory: true,
            session_id: None,
            sync_status: None,
        };
        assert_eq!(
            refresh_memory_status(&engine.repo, &mut ui, &input).unwrap(),
            MemoryStatusResult {
                notified: true,
                footer_shown: true
            }
        );
        input.already_notified = true;
        assert!(
            !refresh_memory_status(&engine.repo, &mut ui, &input)
                .unwrap()
                .notified
        );
        assert_eq!(ui.notifications, 1);
        assert_eq!(ui.lines, ["mem:agent just now", "mem:agent just now"]);
    }
}
