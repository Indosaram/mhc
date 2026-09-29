use pretty_assertions::assert_eq;

use std::collections::BTreeMap;

use crate::journal::cursor::{ReflectionSnapshot, ReflectionTranscriptState};
use crate::journal::entries::{TextTranscriptEntry, TranscriptEntry};

use crate::reflection::machine::{
    CapturedConversation, DreamOrigin, EvaluationAction, JournalSnapshot, MachineState,
    ReflectionEvent, ReflectionOutcome, ReflectionRequest, ReflectionTrigger, ReservationState,
    TriggerConfig, complete_transition, evaluate_transitions, reserve_transition,
};

fn journal(steps: usize, pending_compaction: Option<bool>) -> JournalSnapshot {
    let mut entries = Vec::new();
    for i in 0..steps {
        entries.push(TranscriptEntry::Text(TextTranscriptEntry {
            kind: "assistant".to_string(),
            text: format!("Step {i}"),
            captured_at: "2026-03-30T00:00:00.000Z".to_string(),
            source_line_id: format!("line-{i}"),
            source_message_id: format!("msg-{i}"),
        }));
    }

    JournalSnapshot {
        conversation_id: "conversation-a".to_string(),
        state: ReflectionTranscriptState {
            schema_version: "v3_assistant_steps".to_string(),
            reflected_through_message_id: None,
            total_completed_steps: steps,
            reflected_completed_steps: 0,
            steps_since_last_successful_reflection: steps,
            last_reflection_started_at: None,
            last_reflection_succeeded_at: None,
            pending_compaction,
        },
        snapshot: Some(ReflectionSnapshot {
            start_message_id: "msg-0".to_string(),
            end_message_id: format!("msg-{}", steps.saturating_sub(1)),
            start_line: 0,
            end_snapshot_line: steps,
            entries,
        }),
    }
}

fn request(
    trigger: ReflectionTrigger,
    conversation_ids: Option<Vec<String>>,
    origin: Option<DreamOrigin>,
) -> ReflectionRequest {
    let ids = conversation_ids.unwrap_or_else(|| vec!["conversation-a".to_string()]);
    let snapshots = ids
        .iter()
        .map(|id| CapturedConversation {
            conversation_id: id.clone(),
            snapshot: ReflectionSnapshot {
                start_message_id: "msg-0".to_string(),
                end_message_id: "msg-0".to_string(),
                start_line: 0,
                end_snapshot_line: 1,
                entries: Vec::new(),
            },
        })
        .collect();

    ReflectionRequest {
        trigger,
        origin,
        conversation_ids: ids,
        snapshots,
        focus: None,
        recent_n: None,
        target_doc: None,
    }
}

#[test]
fn given_step_count_below_threshold_when_settled_event_arrives_then_no_transition_is_requested() {
    let state = MachineState {
        journal: journal(4, None),
        reservation: ReservationState::default(),
        config: TriggerConfig {
            step_count: Some(5),
            on_compaction: Some(true),
        },
    };

    let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
    assert!(matches!(result.action, EvaluationAction::None));
}

#[test]
fn given_step_count_reaching_threshold_when_settled_event_arrives_then_a_step_count_reflection_run_is_requested()
 {
    let state = MachineState {
        journal: journal(5, None),
        reservation: ReservationState::default(),
        config: TriggerConfig {
            step_count: Some(5),
            on_compaction: Some(true),
        },
    };

    let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
    match result.action {
        EvaluationAction::Reserve(req) => {
            assert_eq!(req.trigger, ReflectionTrigger::StepCount);
            assert_eq!(req.conversation_ids, vec!["conversation-a"]);
        }
        _ => panic!("Expected Reserve action"),
    }
}

#[test]
fn given_compaction_accepted_followed_by_successful_settled_event_when_evaluated_in_order_then_compaction_reflection_takes_precedence()
 {
    let state = MachineState {
        journal: journal(6, None),
        reservation: ReservationState::default(),
        config: TriggerConfig {
            step_count: Some(5),
            on_compaction: Some(true),
        },
    };

    let compaction_result = evaluate_transitions(state, ReflectionEvent::CompactionAccepted);
    assert_eq!(
        compaction_result.state.journal.state.pending_compaction,
        Some(true)
    );

    let settled_result = evaluate_transitions(
        compaction_result.state,
        ReflectionEvent::Settled { success: true },
    );
    match settled_result.action {
        EvaluationAction::Reserve(req) => {
            assert_eq!(req.trigger, ReflectionTrigger::Compaction);
            assert_eq!(req.conversation_ids, vec!["conversation-a"]);
        }
        _ => panic!("Expected Reserve action for compaction"),
    }
}

#[test]
fn given_a_manual_request_event_when_evaluated_then_it_immediately_emits_a_manual_reflection_request_with_custom_scope()
 {
    let state = MachineState {
        journal: journal(1, None),
        reservation: ReservationState::default(),
        config: TriggerConfig {
            step_count: Some(5),
            on_compaction: Some(true),
        },
    };

    let result = evaluate_transitions(
        state,
        ReflectionEvent::Manual {
            focus: Some("Focus on security".to_string()),
            recent_n: Some(3),
            conversation_ids: Some(vec!["convo-1".to_string(), "convo-2".to_string()]),
        },
    );

    match result.action {
        EvaluationAction::Reserve(req) => {
            assert_eq!(req.trigger, ReflectionTrigger::Manual);
            assert_eq!(req.focus, Some("Focus on security".to_string()));
            assert_eq!(req.recent_n, Some(3));
            assert_eq!(req.conversation_ids, vec!["convo-1", "convo-2"]);
        }
        _ => panic!("Expected Reserve action for manual"),
    }
}

#[test]
fn given_an_active_reflection_run_when_a_manual_run_arrives_then_manual_work_is_queued_into_pending()
 {
    let initial = ReservationState::default();
    let (active_state, outcome1) = reserve_transition(
        initial,
        request(ReflectionTrigger::StepCount, None, None),
        "run-active".to_string(),
    );
    assert_eq!(outcome1, "active");
    assert_eq!(active_state.active.as_ref().unwrap().run_id, "run-active");

    let (queued_state, outcome2) = reserve_transition(
        active_state,
        request(ReflectionTrigger::Manual, None, None),
        "run-manual".to_string(),
    );
    assert_eq!(outcome2, "pending");
    assert_eq!(queued_state.pending.as_ref().unwrap().run_id, "run-manual");
    assert_eq!(
        queued_state.pending.as_ref().unwrap().request.trigger,
        ReflectionTrigger::Manual
    );
}

#[test]
fn given_active_reflection_and_queued_compaction_when_manual_reflection_arrives_then_manual_overrides_compaction_in_pending()
 {
    let initial = ReservationState::default();
    let (state1, _) = reserve_transition(
        initial,
        request(ReflectionTrigger::StepCount, None, None),
        "active".to_string(),
    );
    let (state2, _) = reserve_transition(
        state1,
        request(ReflectionTrigger::Compaction, None, None),
        "compaction".to_string(),
    );
    assert_eq!(
        state2.pending.as_ref().unwrap().request.trigger,
        ReflectionTrigger::Compaction
    );

    let (state3, _) = reserve_transition(
        state2,
        request(ReflectionTrigger::Manual, None, None),
        "manual".to_string(),
    );
    assert_eq!(
        state3.pending.as_ref().unwrap().request.trigger,
        ReflectionTrigger::Manual
    );
}

#[test]
fn given_active_reflection_and_queued_step_count_when_an_idle_dream_arrives_then_dream_overrides_step_count_in_pending()
 {
    let initial = ReservationState::default();
    let (state1, _) = reserve_transition(
        initial,
        request(ReflectionTrigger::Compaction, None, None),
        "active".to_string(),
    );
    let (state2, _) = reserve_transition(
        state1,
        request(ReflectionTrigger::StepCount, None, None),
        "step".to_string(),
    );
    let (state3, _) = reserve_transition(
        state2,
        request(ReflectionTrigger::Dream, None, Some(DreamOrigin::Idle)),
        "dream".to_string(),
    );

    assert_eq!(
        state3.pending.as_ref().unwrap().request.trigger,
        ReflectionTrigger::Dream
    );
    assert_eq!(
        state3.pending.as_ref().unwrap().request.origin,
        Some(DreamOrigin::Idle)
    );
}

#[test]
fn given_active_reflection_and_queued_idle_dream_when_a_newer_idle_dream_arrives_then_newer_dream_updates_focus_and_merges_conversations()
 {
    let initial = ReservationState::default();
    let (state1, _) = reserve_transition(
        initial,
        request(ReflectionTrigger::StepCount, None, None),
        "active".to_string(),
    );

    let mut first_dream = request(
        ReflectionTrigger::Dream,
        Some(vec!["conversation-a".to_string()]),
        Some(DreamOrigin::Idle),
    );
    first_dream.focus = Some("older dream".to_string());

    let (state2, _) = reserve_transition(state1, first_dream, "dream-1".to_string());

    let mut second_dream = request(
        ReflectionTrigger::Dream,
        Some(vec!["conversation-b".to_string()]),
        Some(DreamOrigin::Idle),
    );
    second_dream.focus = Some("newest dream".to_string());

    let (state3, _) = reserve_transition(state2, second_dream, "dream-2".to_string());
    let pending = state3.pending.unwrap();
    assert_eq!(pending.request.trigger, ReflectionTrigger::Dream);
    assert_eq!(pending.request.origin, Some(DreamOrigin::Idle));
    assert_eq!(pending.request.focus, Some("newest dream".to_string()));
    assert_eq!(
        pending.request.conversation_ids,
        vec!["conversation-a", "conversation-b"]
    );
}

#[test]
fn given_interleaved_reflection_and_dream_requests_when_every_bounded_event_sequence_is_applied_then_at_most_one_active_and_one_pending_run_exist()
 {
    let candidates = vec![
        request(ReflectionTrigger::StepCount, None, None),
        request(ReflectionTrigger::Compaction, None, None),
        request(ReflectionTrigger::Manual, None, None),
        request(ReflectionTrigger::Dream, None, Some(DreamOrigin::Idle)),
        request(ReflectionTrigger::Dream, None, Some(DreamOrigin::Shutdown)),
        request(ReflectionTrigger::Dream, None, Some(DreamOrigin::Manual)),
    ];

    for first in &candidates {
        for second in &candidates {
            for third in &candidates {
                let (s1, _) = reserve_transition(
                    ReservationState::default(),
                    first.clone(),
                    "run-1".to_string(),
                );
                let (s2, _) = reserve_transition(s1, second.clone(), "run-2".to_string());
                let (s3, _) = reserve_transition(s2, third.clone(), "run-3".to_string());

                assert!(s3.active.is_some());
                if let Some(p) = &s3.pending {
                    assert_ne!(p.run_id, s3.active.as_ref().unwrap().run_id);
                }
            }
        }
    }
}

#[test]
fn given_every_completion_outcome_when_the_active_run_completes_then_only_merged_and_no_changes_advance_its_captured_cursor()
 {
    let outcomes = [
        ReflectionOutcome::Merged,
        ReflectionOutcome::NoChanges,
        ReflectionOutcome::ParentDirty,
        ReflectionOutcome::MergeConflict,
        ReflectionOutcome::DirtyUncommitted,
        ReflectionOutcome::Failed,
        ReflectionOutcome::TimedOut,
    ];

    for outcome in outcomes {
        let (active_state, _) = reserve_transition(
            ReservationState::default(),
            request(ReflectionTrigger::StepCount, None, None),
            "run".to_string(),
        );

        let mut journals = BTreeMap::new();
        journals.insert("conversation-a".to_string(), journal(6, None));

        let config = TriggerConfig {
            step_count: Some(5),
            on_compaction: Some(true),
        };

        let result = complete_transition(active_state, "run", outcome, &journals, &config).unwrap();
        let expected_len =
            if outcome == ReflectionOutcome::Merged || outcome == ReflectionOutcome::NoChanges {
                1
            } else {
                0
            };
        assert_eq!(result.finalize.len(), expected_len);
        assert!(result.state.active.is_none());
    }
}

#[test]
fn given_a_failed_active_run_and_a_queued_request_when_completed_then_the_cursor_stays_unmoved_and_pending_is_promoted()
 {
    let (s1, _) = reserve_transition(
        ReservationState::default(),
        request(ReflectionTrigger::Manual, None, None),
        "active".to_string(),
    );
    let (s2, _) = reserve_transition(
        s1,
        request(ReflectionTrigger::StepCount, None, None),
        "pending".to_string(),
    );

    let mut journals = BTreeMap::new();
    journals.insert("conversation-a".to_string(), journal(6, None));
    let config = TriggerConfig {
        step_count: Some(5),
        on_compaction: None,
    };

    let result =
        complete_transition(s2, "active", ReflectionOutcome::Failed, &journals, &config).unwrap();
    assert!(result.finalize.is_empty());
    assert_eq!(result.launch.as_ref().unwrap().run_id, "pending");
    assert_eq!(result.state.active.as_ref().unwrap().run_id, "pending");
}

#[test]
fn given_pending_automatic_work_became_stale_when_dequeued_then_it_is_dropped_while_manual_work_survives()
 {
    let (s1, _) = reserve_transition(
        ReservationState::default(),
        request(ReflectionTrigger::Manual, None, None),
        "active".to_string(),
    );
    let (s2, _) = reserve_transition(
        s1,
        request(ReflectionTrigger::StepCount, None, None),
        "stale".to_string(),
    );

    let mut journals = BTreeMap::new();
    journals.insert("conversation-a".to_string(), journal(1, None));
    let config = TriggerConfig {
        step_count: Some(5),
        on_compaction: None,
    };

    let stale_result =
        complete_transition(s2, "active", ReflectionOutcome::Failed, &journals, &config).unwrap();
    assert!(stale_result.launch.is_none());

    let (s3, _) = reserve_transition(
        ReservationState::default(),
        request(ReflectionTrigger::StepCount, None, None),
        "active".to_string(),
    );
    let (s4, _) = reserve_transition(
        s3,
        request(ReflectionTrigger::Manual, None, None),
        "manual".to_string(),
    );

    let mut journals_zero = BTreeMap::new();
    journals_zero.insert("conversation-a".to_string(), journal(0, None));

    let manual_result = complete_transition(
        s4,
        "active",
        ReflectionOutcome::Failed,
        &journals_zero,
        &config,
    )
    .unwrap();
    assert_eq!(
        manual_result.launch.as_ref().unwrap().request.trigger,
        ReflectionTrigger::Manual
    );
}
