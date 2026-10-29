//! `team/shutdown-helpers.test.ts`

use pretty_assertions::assert_eq;
use team_core::types::MemberStatus;

use crate::team::shutdown_helpers::{DELETABLE_MEMBER_STATUSES, is_member_deletable};

fn status_name(status: &MemberStatus) -> String {
    serde_json::to_value(status)
        .expect("serialize status")
        .as_str()
        .expect("status string")
        .to_string()
}

fn member_statuses() -> Vec<MemberStatus> {
    vec![
        MemberStatus::Pending,
        MemberStatus::Running,
        MemberStatus::Idle,
        MemberStatus::Errored,
        MemberStatus::Completed,
        MemberStatus::ShutdownApproved,
    ]
}

#[test]
fn given_the_omo_shutdown_helpers_parity_set_when_read_then_it_is_exactly_completed_shutdown_approved_errored() {
    let mut deletable: Vec<String> = DELETABLE_MEMBER_STATUSES.iter().map(status_name).collect();
    deletable.sort();

    assert_eq!(
        deletable,
        vec!["completed".to_string(), "errored".to_string(), "shutdown_approved".to_string()]
    );
}

#[test]
fn given_each_member_status_when_is_member_deletable_is_applied_then_only_terminal_safe_statuses_are_deletable() {
    let verdicts: Vec<(String, bool)> = member_statuses()
        .iter()
        .map(|status| (status_name(status), is_member_deletable(status)))
        .collect();

    assert_eq!(
        verdicts,
        vec![
            ("pending".to_string(), false),
            ("running".to_string(), false),
            ("idle".to_string(), false),
            ("errored".to_string(), true),
            ("completed".to_string(), true),
            ("shutdown_approved".to_string(), true),
        ]
    );
}
