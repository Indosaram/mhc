//! senpi `modes/rpc/tool-child/job-policy.ts` (pin 26acd9353).
//!
//! The tool child's job policy (senpi#3566, plan todo 6; DESIGN PART 1 (c)). Pure decisions over an injected clock:
//! per-kind server backstop deadlines and retry rules, the cancel ladder, control liveness and the crash loop. The
//! client applies them; nothing here starts a timer of its own.
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use serde_json::Value;

/// Server backstop in ms for a job of this kind, from its arguments; `None` = no backstop (liveness bounds it).
pub type BackstopMs = Arc<dyn Fn(Option<&Value>) -> Option<f64> + Send + Sync>;

#[derive(Clone)]
pub struct KindPolicy {
    pub backstop_ms: BackstopMs,
    pub idempotent: bool,
    /// What the user reads when the backstop fires.
    pub user_text: String,
}

const MB10: f64 = (10 * 1024 * 1024) as f64;

fn number_field(args: Option<&Value>, field: &str) -> Option<f64> {
    let value = args?.as_object()?.get(field)?.as_f64()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn fixed(ms: f64) -> BackstopMs {
    Arc::new(move |_| Some(ms))
}

fn per_ten_mb(base_ms: f64, field: &'static str) -> BackstopMs {
    Arc::new(move |args| {
        Some(base_ms + 1_000.0 * (number_field(args, field).unwrap_or(0.0) / MB10).ceil())
    })
}

fn policy(backstop_ms: BackstopMs, idempotent: bool, user_text: &str) -> KindPolicy {
    KindPolicy {
        backstop_ms,
        idempotent,
        user_text: user_text.to_owned(),
    }
}

/// The per-kind table, in upstream order.
pub static KIND_POLICIES: LazyLock<Vec<(&'static str, KindPolicy)>> = LazyLock::new(|| {
    vec![
        (
            "tool.grep",
            policy(
                fixed(120_000.0),
                true,
                "The search took too long and was stopped.",
            ),
        ),
        (
            "tool.find",
            policy(
                fixed(120_000.0),
                true,
                "The file search took too long and was stopped.",
            ),
        ),
        (
            "tool.glob",
            policy(
                fixed(120_000.0),
                true,
                "The file search took too long and was stopped.",
            ),
        ),
        (
            "tool.ls",
            policy(
                fixed(120_000.0),
                true,
                "Listing the directory took too long and was stopped.",
            ),
        ),
        (
            "tool.read",
            policy(
                fixed(60_000.0),
                true,
                "Reading the file took too long and was stopped.",
            ),
        ),
        (
            "tool.edit",
            policy(
                fixed(120_000.0),
                false,
                "The edit took too long and was stopped.",
            ),
        ),
        (
            "tool.write",
            policy(
                fixed(120_000.0),
                false,
                "Writing the file took too long and was stopped.",
            ),
        ),
        (
            "tool.apply_patch",
            policy(
                fixed(120_000.0),
                false,
                "Applying the patch took too long and was stopped.",
            ),
        ),
        (
            "session.parse",
            policy(
                per_ten_mb(30_000.0, "bytes"),
                true,
                "Reading the session took too long and was stopped.",
            ),
        ),
        (
            "history.spool",
            policy(
                per_ten_mb(60_000.0, "lineBytes"),
                true,
                "Saving the session history took too long and was stopped.",
            ),
        ),
        (
            "compaction.prepare",
            policy(
                fixed(180_000.0),
                true,
                "Preparing the compaction took too long and was stopped.",
            ),
        ),
        (
            "eval.cell",
            policy(Arc::new(|_| None), false, "The cell was stopped."),
        ),
    ]
});

/// The longest a `module` / `offload.*` job may run, on the child and in-process alike.
pub const OFFLOAD_MAX_MS: f64 = 300_000.0;
const MODULE_KIND: &str = "module";

/// What `policy_for` takes besides the kind: the caller's idempotency, deadline and the environment to read
/// `qa.*` deadlines from (`None` = the process environment).
#[derive(Clone, Debug, Default)]
pub struct PolicyOptions {
    pub idempotent: Option<bool>,
    pub deadline_ms: Option<f64>,
    pub env: Option<HashMap<String, String>>,
}

/// The policy for one job: the table, `offload.<id>` / `module` (caller value, max 300 s), or a `qa.*` fixture kind.
pub fn policy_for(kind: &str, options: &PolicyOptions) -> KindPolicy {
    if let Some((_, known)) = KIND_POLICIES.iter().find(|(name, _)| *name == kind) {
        return known.clone();
    }
    if kind == MODULE_KIND || kind.starts_with("offload.") {
        let deadline = options
            .deadline_ms
            .unwrap_or(OFFLOAD_MAX_MS)
            .min(OFFLOAD_MAX_MS);
        return KindPolicy {
            backstop_ms: Arc::new(move |_| Some(deadline)),
            idempotent: options.idempotent == Some(true),
            user_text: "The tool took too long and was stopped.".to_owned(),
        };
    }
    let from_env = match &options.env {
        Some(env) => fixture_deadline(kind, &|name| env.get(name).cloned()),
        None => fixture_deadline(kind, &|name| std::env::var(name).ok()),
    };
    let deadline_ms = options.deadline_ms;
    KindPolicy {
        backstop_ms: Arc::new(move |_| from_env.or(deadline_ms)),
        idempotent: options.idempotent == Some(true),
        user_text: format!("The {kind} job took too long and was stopped."),
    }
}

/// `SENPI_TOOL_CHILD_DEADLINE_<KIND>_MS` for a `qa.*` fixture kind (`qa.sleep` -> `..._QA_SLEEP_MS`).
/// `env` reads one variable by name (`env[name]`).
pub fn fixture_deadline(kind: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<f64> {
    if !kind.starts_with("qa.") && !kind.starts_with("qa_") {
        return None;
    }
    let name: String = kind
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    let raw = env(&format!("SENPI_TOOL_CHILD_DEADLINE_{name}_MS"))?;
    // `Number(raw)`: surrounding whitespace is trimmed and an empty string is 0.
    let trimmed = raw.trim();
    let value = if trimmed.is_empty() {
        0.0
    } else {
        trimmed.parse::<f64>().unwrap_or(f64::NAN)
    };
    (value.is_finite() && value >= 0.0).then_some(value)
}

pub const CANCEL_ACK_MS: f64 = 5_000.0;
pub const TERM_GRACE_MS: f64 = 2_000.0;
pub const PING_INTERVAL_MS: f64 = 5_000.0;
pub const MISSED_PONGS_LIMIT: usize = 3;
pub const READY_MS: f64 = 10_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelStep {
    Term,
    Kill,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CancelRung {
    pub at_ms: f64,
    pub step: CancelStep,
}

/// The cancel ladder: cancel, wait 5 s for the child to settle the job, then TERM, then 2 s later KILL.
pub const CANCEL_LADDER: [CancelRung; 2] = [
    CancelRung {
        at_ms: CANCEL_ACK_MS,
        step: CancelStep::Term,
    },
    CancelRung {
        at_ms: CANCEL_ACK_MS + TERM_GRACE_MS,
        step: CancelStep::Kill,
    },
];

pub const CRASH_LOOP_EXITS: usize = 3;
pub const CRASH_LOOP_WINDOW_MS: f64 = 5_000.0;
pub const CRASH_LOOP_COOLDOWN_MS: f64 = 60_000.0;

/// Three exits within 5 s make the child `unavailable` for 60 s; within that time no spawn is attempted.
pub struct CrashLoop {
    now: Box<dyn Fn() -> f64 + Send + Sync>,
    exits: Vec<f64>,
    unavailable_until: f64,
}

impl CrashLoop {
    pub fn new(now: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        Self {
            now: Box::new(now),
            exits: Vec::new(),
            unavailable_until: 0.0,
        }
    }

    pub fn record_exit(&mut self) {
        let now = (self.now)();
        self.exits.retain(|at| now - at < CRASH_LOOP_WINDOW_MS);
        self.exits.push(now);
        if self.exits.len() >= CRASH_LOOP_EXITS {
            self.unavailable_until = now + CRASH_LOOP_COOLDOWN_MS;
            self.exits.clear();
        }
    }

    pub fn unavailable(&self) -> bool {
        (self.now)() < self.unavailable_until
    }
}

/// Control liveness: a ping per interval while work exists; three unanswered pings in a row mean the child is gone.
#[derive(Debug, Default)]
pub struct Liveness {
    outstanding: usize,
}

impl Liveness {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ping_sent(&mut self) {
        self.outstanding += 1;
    }

    pub fn pong_received(&mut self) {
        self.outstanding = 0;
    }

    pub fn unresponsive(&self) -> bool {
        self.outstanding >= MISSED_PONGS_LIMIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    // senpi#3566 todo 6: the tool child's job policy (DESIGN PART 1 (c)) - per-kind backstops and retry rules, the
    // cancel ladder, control liveness and the crash loop - decided over a fake clock.

    fn backstop(kind: &str, args: Option<Value>) -> Option<f64> {
        (policy_for(kind, &PolicyOptions::default()).backstop_ms)(args.as_ref())
    }

    #[test]
    fn follows_the_table_including_the_per_10_mb_growth_of_spool_and_parse_backstops() {
        assert_eq!(backstop("tool.grep", None), Some(120_000.0));
        assert_eq!(backstop("tool.read", None), Some(60_000.0));
        assert_eq!(backstop("compaction.prepare", None), Some(180_000.0));
        assert_eq!(
            backstop("history.spool", Some(json!({ "lineBytes": 0 }))),
            Some(60_000.0)
        );
        assert_eq!(
            backstop(
                "history.spool",
                Some(json!({ "lineBytes": 25 * 1024 * 1024 }))
            ),
            Some(63_000.0)
        );
        assert_eq!(
            backstop("session.parse", Some(json!({ "bytes": 10 * 1024 * 1024 }))),
            Some(31_000.0)
        );
        assert_eq!(backstop("eval.cell", None), None);
    }

    #[test]
    fn gives_every_table_kind_a_user_text_and_marks_exactly_the_retry_safe_kinds_idempotent() {
        for (_, policy) in KIND_POLICIES.iter() {
            assert!(!policy.user_text.is_empty());
        }
        let mut idempotent: Vec<&str> = KIND_POLICIES
            .iter()
            .filter(|(_, policy)| policy.idempotent)
            .map(|(kind, _)| *kind)
            .collect();
        idempotent.sort_unstable();
        let mut expected = vec![
            "compaction.prepare",
            "history.spool",
            "session.parse",
            "tool.find",
            "tool.glob",
            "tool.grep",
            "tool.ls",
            "tool.read",
        ];
        expected.sort_unstable();
        assert_eq!(idempotent, expected);
    }

    #[test]
    fn caps_module_and_offload_jobs_at_300_s_and_takes_the_callers_idempotency() {
        assert_eq!(backstop("module", None), Some(300_000.0));
        let with_deadline = |deadline_ms: f64| PolicyOptions {
            deadline_ms: Some(deadline_ms),
            ..PolicyOptions::default()
        };
        assert_eq!(
            (policy_for("offload.grep", &with_deadline(2_000.0)).backstop_ms)(None),
            Some(2_000.0)
        );
        assert_eq!(
            (policy_for("offload.grep", &with_deadline(900_000.0)).backstop_ms)(None),
            Some(300_000.0)
        );
        let idempotent = PolicyOptions {
            idempotent: Some(true),
            ..PolicyOptions::default()
        };
        assert!(policy_for("offload.grep", &idempotent).idempotent);
        assert!(!policy_for("module", &PolicyOptions::default()).idempotent);
    }

    #[test]
    fn lets_the_deadline_env_override_a_qa_fixture_kind_and_only_a_qa_kind() {
        let env: HashMap<String, String> = [
            (
                "SENPI_TOOL_CHILD_DEADLINE_QA_SLEEP_MS".to_owned(),
                "50".to_owned(),
            ),
            (
                "SENPI_TOOL_CHILD_DEADLINE_TOOL_GREP_MS".to_owned(),
                "1".to_owned(),
            ),
        ]
        .into();
        let options = PolicyOptions {
            env: Some(env.clone()),
            ..PolicyOptions::default()
        };
        assert_eq!(
            (policy_for("qa.sleep", &options).backstop_ms)(None),
            Some(50.0)
        );
        assert_eq!(
            fixture_deadline("tool.grep", &|name| env.get(name).cloned()),
            None
        );
        assert_eq!(
            (policy_for("tool.grep", &options).backstop_ms)(None),
            Some(120_000.0)
        );
    }

    fn clocked(now: &Arc<AtomicU64>) -> CrashLoop {
        let now = now.clone();
        CrashLoop::new(move || now.load(Ordering::SeqCst) as f64)
    }

    #[test]
    fn crash_loop_is_unavailable_for_60_s_after_three_exits_within_5_s_and_only_then() {
        let now = Arc::new(AtomicU64::new(0));
        let mut crash_loop = clocked(&now);
        crash_loop.record_exit();
        now.store(2_000, Ordering::SeqCst);
        crash_loop.record_exit();
        assert!(!crash_loop.unavailable());
        now.store(4_999, Ordering::SeqCst);
        crash_loop.record_exit();
        assert!(crash_loop.unavailable());
        now.store(4_999 + 59_999, Ordering::SeqCst);
        assert!(crash_loop.unavailable());
        now.store(4_999 + 60_000, Ordering::SeqCst);
        assert!(!crash_loop.unavailable());
    }

    #[test]
    fn crash_loop_does_not_count_exits_that_fall_out_of_the_5_s_window() {
        let now = Arc::new(AtomicU64::new(0));
        let mut crash_loop = clocked(&now);
        for at in [0, 3_000, 6_000, 9_000] {
            now.store(at, Ordering::SeqCst);
            crash_loop.record_exit();
        }
        assert!(!crash_loop.unavailable());
    }

    #[test]
    fn liveness_is_unresponsive_after_three_unanswered_pings_in_a_row_and_a_pong_resets_the_count()
    {
        let mut liveness = Liveness::new();
        for _ in 0..MISSED_PONGS_LIMIT - 1 {
            liveness.ping_sent();
        }
        assert!(!liveness.unresponsive());
        liveness.pong_received();
        for _ in 0..MISSED_PONGS_LIMIT {
            liveness.ping_sent();
        }
        assert!(liveness.unresponsive());
    }

    #[test]
    fn cancel_ladder_is_term_at_5_s_and_kill_at_7_s_after_the_cancel() {
        assert_eq!(
            CANCEL_LADDER,
            [
                CancelRung {
                    at_ms: 5_000.0,
                    step: CancelStep::Term
                },
                CancelRung {
                    at_ms: 7_000.0,
                    step: CancelStep::Kill
                },
            ]
        );
    }
}
