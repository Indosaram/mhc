//! Ordered turn-end decision table. This module observes; it never queues work.
use maho_core::agent_abort_provenance::AgentEndEvent;
use serde::{Deserialize, Serialize};
pub const TURN_END_LIVENESS_EVENT: &str = "turn_end_liveness";
pub const TURN_END_LIVENESS_ENTRY_TYPE: &str = "turn-end-liveness";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WakeExpectation {
    WakeSource { source: String },
    ArmedTimer { timer: String },
    RecoveryStaged,
    PendingMessages,
    UserInputQueued,
    QuestionToUser,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum OpenWork {
    ActiveGoal {
        #[serde(rename = "goalId")]
        goal_id: String,
    },
    OpenTodos {
        count: usize,
    },
    None,
}
#[derive(Clone, Debug, Serialize)]
pub struct TurnEndLivenessInput {
    pub is_child_session: bool,
    #[serde(skip)]
    pub mode: maho_ext_api::ExtensionMode,
    pub ended_by: String,
    pub ledger_hold: Option<String>,
    pub will_retry: bool,
    pub deadline_stop: bool,
    pub goal_stopped: bool,
    pub open_work: OpenWork,
    pub continuation_queued: Option<String>,
    pub expectations: Vec<WakeExpectation>,
    pub budget_spent: bool,
    pub provider_retry_at_ms: Option<f64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TurnEndLivenessVerdict {
    Continuing {
        via: String,
    },
    Held {
        reason: String,
    },
    Waiting {
        on: Vec<WakeExpectation>,
        #[serde(rename = "heldBy", skip_serializing_if = "Option::is_none")]
        held_by: Option<String>,
    },
    Done,
    Stalled {
        reason: String,
        #[serde(rename = "retryAtMs", skip_serializing_if = "Option::is_none")]
        retry_at_ms: Option<f64>,
        #[serde(rename = "abortSource", skip_serializing_if = "Option::is_none")]
        abort_source: Option<String>,
    },
}
pub fn classify_turn_end(event: &AgentEndEvent) -> String {
    use maho_ext_api::AbortSource;
    (if event.aborted && event.abort_source == Some(AbortSource::User) {
        "user-abort"
    } else if event.aborted && event.abort_source == Some(AbortSource::System) {
        "system-abort"
    } else if event.abort_source == Some(AbortSource::Provider) {
        "provider-abort"
    } else if event.aborted {
        "unknown-abort"
    } else if crate::terminal_provider_error::did_terminal_policy_rejection_end_turn(event) {
        "policy-rejection"
    } else if crate::terminal_provider_error::did_terminal_provider_error_end_turn(event) {
        "provider-error"
    } else if crate::continuation::did_agent_end_cleanly(&event.messages) {
        "clean"
    } else {
        "unknown-abort"
    })
    .into()
}
pub fn effective_wakes(input: &TurnEndLivenessInput) -> Vec<WakeExpectation> {
    input
        .expectations
        .iter()
        .filter(|wake| {
            !(matches!(wake, WakeExpectation::QuestionToUser) && input.ended_by != "clean"
                || input.deadline_stop
                    && matches!(
                        wake,
                        WakeExpectation::ArmedTimer { .. } | WakeExpectation::RecoveryStaged
                    ))
        })
        .cloned()
        .collect()
}
fn held_reason(input: &TurnEndLivenessInput) -> Option<String> {
    if input.deadline_stop {
        Some("deadline-stop".into())
    } else if input.ended_by == "user-abort" {
        Some("user-abort".into())
    } else if input.ledger_hold.is_some() {
        input.ledger_hold.clone()
    } else if input.is_child_session {
        Some("child-session".into())
    } else if input.goal_stopped {
        Some("goal-stopped".into())
    } else {
        None
    }
}
pub const TURN_END_RULES: &[&str] = &[
    "retry",
    "ledger-hold",
    "continuation-queued",
    "non-interactive",
    "wake",
    "held",
    "provider-error",
    "aborted",
    "budget",
    "no-wake",
    "done",
];
pub fn evaluate_turn_end_liveness(i: &TurnEndLivenessInput) -> TurnEndLivenessVerdict {
    use TurnEndLivenessVerdict::*;
    if i.will_retry {
        return Continuing {
            via: "retry".into(),
        };
    }
    if let Some(reason) = &i.ledger_hold
        && !i.expectations.contains(&WakeExpectation::UserInputQueued)
    {
        return Held {
            reason: reason.clone(),
        };
    }
    if let Some(via) = &i.continuation_queued {
        return Continuing { via: via.clone() };
    }
    if matches!(
        i.mode,
        maho_ext_api::ExtensionMode::Print | maho_ext_api::ExtensionMode::Json
    ) {
        return Held {
            reason: "non-interactive".into(),
        };
    }
    let wakes = effective_wakes(i);
    if !wakes.is_empty() {
        return Waiting {
            on: wakes,
            held_by: held_reason(i),
        };
    }
    if let Some(reason) = held_reason(i) {
        return Held { reason };
    }
    if matches!(i.ended_by.as_str(), "provider-error" | "policy-rejection") {
        return Stalled {
            reason: "provider-error".into(),
            retry_at_ms: i.provider_retry_at_ms,
            abort_source: None,
        };
    }
    if matches!(
        i.ended_by.as_str(),
        "system-abort" | "provider-abort" | "unknown-abort"
    ) {
        return Stalled {
            reason: "aborted".into(),
            retry_at_ms: None,
            abort_source: match i.ended_by.as_str() {
                "system-abort" => Some("system".into()),
                "provider-abort" => Some("provider".into()),
                _ => None,
            },
        };
    }
    if i.open_work != OpenWork::None {
        return Stalled {
            reason: if i.budget_spent { "budget" } else { "no-wake" }.into(),
            retry_at_ms: None,
            abort_source: None,
        };
    }
    Done
}
pub fn repeats_last_stall(
    branch: &[maho_ext_api::SessionEntry],
    verdict: &TurnEndLivenessVerdict,
) -> bool {
    let value = serde_json::to_value(verdict).expect("serializable verdict");
    for entry in branch.iter().rev() {
        if entry.kind == "message" && entry.data["message"]["role"] == "user" {
            return false;
        }
        if entry.kind != "custom" || entry.data["customType"] != TURN_END_LIVENESS_ENTRY_TYPE {
            continue;
        }
        let recorded = &entry.data["data"]["verdict"];
        return ["reason", "retryAtMs", "abortSource"]
            .iter()
            .all(|key| recorded[*key] == value[*key]);
    }
    false
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_upstream_decision_table() {
        let rows: Vec<(String, String, serde_json::Value, serde_json::Value)> =
            serde_json::from_str(include_str!("../tests/fixtures/turn-end-rules.json")).unwrap();
        let mut covered = std::collections::BTreeSet::new();
        for (rule, label, overrides, expected) in rows {
            let mut i = input();
            // goal-turn-end-liveness.test.ts starts each table row with an active goal.
            i.open_work = OpenWork::ActiveGoal {
                goal_id: "goal-1".into(),
            };
            for (key, value) in overrides.as_object().unwrap() {
                match key.as_str() {
                    "isChildSession" => i.is_child_session = value.as_bool().unwrap(),
                    "mode" => {
                        i.mode = match value.as_str().unwrap() {
                            "print" => maho_ext_api::ExtensionMode::Print,
                            "json" => maho_ext_api::ExtensionMode::Json,
                            _ => maho_ext_api::ExtensionMode::Tui,
                        }
                    }
                    "endedBy" => i.ended_by = value.as_str().unwrap().into(),
                    "ledgerHold" => i.ledger_hold = Some(value.as_str().unwrap().into()),
                    "willRetry" => i.will_retry = value.as_bool().unwrap(),
                    "deadlineStop" => i.deadline_stop = value.as_bool().unwrap(),
                    "goalStopped" => i.goal_stopped = value.as_bool().unwrap(),
                    "openWork" => i.open_work = serde_json::from_value(value.clone()).unwrap(),
                    "continuationQueued" => {
                        i.continuation_queued = Some(value.as_str().unwrap().into())
                    }
                    "expectations" => {
                        i.expectations = serde_json::from_value(value.clone()).unwrap()
                    }
                    "budgetSpent" => i.budget_spent = value.as_bool().unwrap(),
                    "providerRetryAtMs" => i.provider_retry_at_ms = value.as_f64(),
                    other => panic!("untranslated upstream input {other}"),
                }
            }
            // The upstream decision table compares JavaScript numbers. Deserialize
            // the fixture so integer JSON and f64 retry timestamps compare numerically.
            assert_eq!(
                evaluate_turn_end_liveness(&i),
                serde_json::from_value::<TurnEndLivenessVerdict>(expected).unwrap(),
                "{rule}: {label}"
            );
            covered.insert(rule);
        }
        assert_eq!(
            covered,
            TURN_END_RULES.iter().map(|rule| rule.to_string()).collect()
        );
    }
    fn input() -> TurnEndLivenessInput {
        TurnEndLivenessInput {
            is_child_session: false,
            mode: maho_ext_api::ExtensionMode::Tui,
            ended_by: "clean".into(),
            ledger_hold: None,
            will_retry: false,
            deadline_stop: false,
            goal_stopped: false,
            open_work: OpenWork::None,
            continuation_queued: None,
            expectations: vec![],
            budget_spent: false,
            provider_retry_at_ms: None,
        }
    }
    #[test]
    fn ordered_rules_hold_engine_wakes_but_not_person_input() {
        let mut i = input();
        i.ledger_hold = Some("session_hold".into());
        i.continuation_queued = Some("goal".into());
        assert_eq!(
            evaluate_turn_end_liveness(&i),
            TurnEndLivenessVerdict::Held {
                reason: "session_hold".into()
            }
        );
        i.will_retry = true;
        assert_eq!(
            evaluate_turn_end_liveness(&i),
            TurnEndLivenessVerdict::Continuing {
                via: "retry".into()
            }
        );
        i.will_retry = false;
        i.expectations.push(WakeExpectation::UserInputQueued);
        assert_eq!(
            evaluate_turn_end_liveness(&i),
            TurnEndLivenessVerdict::Continuing { via: "goal".into() }
        );
    }
    #[test]
    fn deadline_filters_only_goal_owned_wakes_and_errors_filter_questions() {
        let mut i = input();
        i.deadline_stop = true;
        i.ended_by = "provider-error".into();
        i.expectations = vec![
            WakeExpectation::ArmedTimer {
                timer: "monitor".into(),
            },
            WakeExpectation::RecoveryStaged,
            WakeExpectation::QuestionToUser,
            WakeExpectation::PendingMessages,
        ];
        assert_eq!(effective_wakes(&i), vec![WakeExpectation::PendingMessages]);
        assert!(matches!(
            evaluate_turn_end_liveness(&i),
            TurnEndLivenessVerdict::Waiting {
                held_by: Some(_),
                ..
            }
        ));
    }
    #[test]
    fn errors_and_aborts_stall_even_without_open_work() {
        for ended in [
            "provider-error",
            "policy-rejection",
            "system-abort",
            "provider-abort",
            "unknown-abort",
        ] {
            let mut i = input();
            i.ended_by = ended.into();
            assert!(matches!(
                evaluate_turn_end_liveness(&i),
                TurnEndLivenessVerdict::Stalled { .. }
            ));
        }
        let mut i = input();
        assert_eq!(evaluate_turn_end_liveness(&i), TurnEndLivenessVerdict::Done);
        i.open_work = OpenWork::OpenTodos { count: 1 };
        assert!(
            matches!(evaluate_turn_end_liveness(&i),TurnEndLivenessVerdict::Stalled {reason,..} if reason=="no-wake")
        );
        i.budget_spent = true;
        assert!(
            matches!(evaluate_turn_end_liveness(&i),TurnEndLivenessVerdict::Stalled {reason,..} if reason=="budget")
        );
    }
}

#[cfg(test)]
mod upstream_invariants {
    use super::*;
    #[test]
    fn every_stop_hold_work_wake_and_start_combination() {
        for stop in [
            "clean",
            "user-abort",
            "system-abort",
            "provider-abort",
            "unknown-abort",
            "provider-error",
            "policy-rejection",
        ] {
            for hold in 0..7 {
                for work in [
                    OpenWork::None,
                    OpenWork::ActiveGoal {
                        goal_id: "g".into(),
                    },
                    OpenWork::OpenTodos { count: 1 },
                ] {
                    for wake in [
                        None,
                        Some(WakeExpectation::WakeSource {
                            source: "senpi-task".into(),
                        }),
                        Some(WakeExpectation::ArmedTimer {
                            timer: "monitor".into(),
                        }),
                        Some(WakeExpectation::QuestionToUser),
                        Some(WakeExpectation::UserInputQueued),
                        Some(WakeExpectation::PendingMessages),
                    ] {
                        for start in 0..3 {
                            let i = TurnEndLivenessInput {
                                is_child_session: hold == 3,
                                mode: if hold == 5 {
                                    maho_ext_api::ExtensionMode::Print
                                } else {
                                    maho_ext_api::ExtensionMode::Tui
                                },
                                ended_by: stop.into(),
                                ledger_hold: (hold == 6).then(|| "unattended".into()),
                                will_retry: start == 1,
                                deadline_stop: hold == 1,
                                goal_stopped: hold == 2,
                                open_work: work.clone(),
                                continuation_queued: (start == 2).then(|| "goal".into()),
                                expectations: wake.clone().into_iter().collect(),
                                budget_spent: hold == 4,
                                provider_retry_at_ms: None,
                            };
                            let verdict = evaluate_turn_end_liveness(&i);
                            if verdict == TurnEndLivenessVerdict::Done {
                                assert_eq!(stop, "clean");
                                assert_eq!(work, OpenWork::None);
                            }
                            let person = i.expectations.contains(&WakeExpectation::UserInputQueued);
                            if i.ledger_hold.is_some() && !i.will_retry && !person {
                                assert_eq!(
                                    verdict,
                                    TurnEndLivenessVerdict::Held {
                                        reason: "unattended".into()
                                    }
                                );
                            }
                            let starting = i.will_retry
                                || (i.continuation_queued.is_some()
                                    && (i.ledger_hold.is_none() || person));
                            assert_eq!(
                                matches!(verdict, TurnEndLivenessVerdict::Continuing { .. }),
                                starting
                            );
                            if let TurnEndLivenessVerdict::Waiting { on, .. } = verdict {
                                assert!(!on.is_empty());
                                if i.deadline_stop {
                                    assert!(!on.iter().any(|wake| matches!(
                                        wake,
                                        WakeExpectation::ArmedTimer { .. }
                                    )));
                                }
                                if stop != "clean" {
                                    assert!(!on.contains(&WakeExpectation::QuestionToUser));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn last_stall_deduplication_resets_at_user_and_matches_retry_time() {
        let stall = |reason: &str, retry: Option<f64>| maho_ext_api::SessionEntry {
            id: "s".into(),
            parent_id: None,
            timestamp: String::new(),
            kind: "custom".into(),
            data: serde_json::json!({"customType":TURN_END_LIVENESS_ENTRY_TYPE,"data":{"verdict":{"kind":"stalled","reason":reason,"retryAtMs":retry}}}),
        };
        let verdict = TurnEndLivenessVerdict::Stalled {
            reason: "provider-error".into(),
            retry_at_ms: Some(1000.0),
            abort_source: None,
        };
        assert!(repeats_last_stall(
            &[stall("provider-error", Some(1000.0))],
            &verdict
        ));
        assert!(!repeats_last_stall(
            &[stall("provider-error", Some(2000.0))],
            &verdict
        ));
        let user = maho_ext_api::SessionEntry {
            id: "u".into(),
            parent_id: None,
            timestamp: String::new(),
            kind: "message".into(),
            data: serde_json::json!({"message":{"role":"user"}}),
        };
        assert!(!repeats_last_stall(
            &[stall("provider-error", Some(1000.0)), user],
            &verdict
        ));
        assert!(!repeats_last_stall(&[], &verdict));
    }
}
