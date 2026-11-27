//! Consecutive run error-streak detector (`thread/run-alerts/error-streak.ts`).

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use super::super::completion::{AgentEndFacts, CompletionOutcome, completion_outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunErrorClass {
    RateLimit,
    Auth,
    Overloaded,
    Network,
    Unknown,
}

impl RunErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RateLimit => "rate_limit",
            Self::Auth => "auth",
            Self::Overloaded => "overloaded",
            Self::Network => "network",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub enum RunEnd {
    Error {
        error_class: RunErrorClass,
        provider: Option<String>,
        model: Option<String>,
    },
    Aborted,
    Completed,
}

#[derive(Debug, Clone)]
pub enum ErrorStreakEvent {
    ErrorStreak {
        session_id: String,
        count: usize,
        classes: HashMap<RunErrorClass, usize>,
        aborted: usize,
        first_at: i64,
        last_at: i64,
        provider: Option<String>,
        model: Option<String>,
        repeat: bool,
    },
    Recovered {
        session_id: String,
        count: usize,
        classes: HashMap<RunErrorClass, usize>,
        aborted: usize,
        first_at: i64,
        last_at: i64,
        recovered_at: i64,
    },
}

static PATTERNS: LazyLock<[(RunErrorClass, Regex); 4]> = LazyLock::new(|| {
    [
        (
            RunErrorClass::RateLimit,
            Regex::new(r"(?i)rate.?limit|\b429\b|too many requests|usage.?limit|quota")
                .expect("valid"),
        ),
        (
            RunErrorClass::Auth,
            Regex::new(r"(?i)\b40[13]\b|unauthori[sz]ed|forbidden|authenticat|api.?key|credential|permission")
                .expect("valid"),
        ),
        (
            RunErrorClass::Overloaded,
            Regex::new(r"(?i)overloaded|\b5(?:29|03)\b|unavailable|capacity").expect("valid"),
        ),
        (
            RunErrorClass::Network,
            Regex::new(r"(?i)fetch failed|network|socket hang up|timed? ?out|\bE(?:CONNRESET|CONNREFUSED|TIMEDOUT|NOTFOUND|AI_AGAIN|PIPE)\b")
                .expect("valid"),
        ),
    ]
});

pub fn classify_run_error(error_message: Option<&str>) -> RunErrorClass {
    let Some(msg) = error_message else {
        return RunErrorClass::Unknown;
    };
    PATTERNS
        .iter()
        .find(|(_, re)| re.is_match(msg))
        .map(|(cls, _)| *cls)
        .unwrap_or(RunErrorClass::Unknown)
}

pub fn run_end_of(facts: &AgentEndFacts) -> RunEnd {
    let outcome = completion_outcome(facts);
    if outcome == CompletionOutcome::Cancelled {
        return RunEnd::Aborted;
    }
    if outcome == CompletionOutcome::Completed {
        return RunEnd::Completed;
    }
    RunEnd::Error {
        error_class: classify_run_error(facts.last_error_message.as_deref()),
        provider: facts.provider.clone(),
        model: facts.model.clone(),
    }
}

struct Streak {
    count: usize,
    classes: HashMap<RunErrorClass, usize>,
    aborted: usize,
    first_at: i64,
    last_at: i64,
    provider: Option<String>,
    model: Option<String>,
    alerted_at: Option<i64>,
}

pub struct ErrorStreakDetector {
    threshold: usize,
    realert_ms: i64,
    streaks: HashMap<String, Streak>,
}

impl ErrorStreakDetector {
    pub fn new(threshold: usize, realert_ms: i64) -> Self {
        Self {
            threshold,
            realert_ms,
            streaks: HashMap::new(),
        }
    }

    pub fn forget(&mut self, session_id: &str) {
        self.streaks.remove(session_id);
    }

    pub fn observe(&mut self, session_id: &str, end: RunEnd, now: i64) -> Option<ErrorStreakEvent> {
        match end {
            RunEnd::Aborted => {
                if let Some(s) = self.streaks.get_mut(session_id) {
                    s.aborted += 1;
                }
                None
            }
            RunEnd::Completed => {
                let s = self.streaks.remove(session_id)?;
                s.alerted_at?;
                Some(ErrorStreakEvent::Recovered {
                    session_id: session_id.to_owned(),
                    count: s.count,
                    classes: s.classes,
                    aborted: s.aborted,
                    first_at: s.first_at,
                    last_at: s.last_at,
                    recovered_at: now,
                })
            }
            RunEnd::Error {
                error_class,
                provider,
                model,
            } => {
                let s = self.streaks.entry(session_id.to_owned()).or_insert(Streak {
                    count: 0,
                    classes: HashMap::new(),
                    aborted: 0,
                    first_at: now,
                    last_at: now,
                    provider: None,
                    model: None,
                    alerted_at: None,
                });
                s.count += 1;
                *s.classes.entry(error_class).or_insert(0) += 1;
                s.last_at = now;
                if provider.is_some() {
                    s.provider = provider;
                }
                if model.is_some() {
                    s.model = model;
                }
                if s.count < self.threshold {
                    return None;
                }
                let repeat = s.alerted_at.is_some();
                if repeat
                    && (self.realert_ms <= 0
                        || s.alerted_at.is_some_and(|a| now - a < self.realert_ms))
                {
                    return None;
                }
                s.alerted_at = Some(now);
                Some(ErrorStreakEvent::ErrorStreak {
                    session_id: session_id.to_owned(),
                    count: s.count,
                    classes: s.classes.clone(),
                    aborted: s.aborted,
                    first_at: s.first_at,
                    last_at: s.last_at,
                    provider: s.provider.clone(),
                    model: s.model.clone(),
                    repeat,
                })
            }
        }
    }
}
