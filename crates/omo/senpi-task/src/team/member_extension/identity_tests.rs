//! `team/member-extension/identity.test.ts`
use std::collections::HashMap;

use crate::team::member_extension::identity::{MEMBER_IDENTITY_ENV, is_team_member_process};
use pretty_assertions::assert_eq;

#[test]
fn given_an_env_carrying_the_member_identity_when_checked_then_it_reports_a_member_process() {
    let env = HashMap::from([(
        MEMBER_IDENTITY_ENV.to_string(),
        "3a80dbd1-3fd2-4e86-b110-596e645b6bd4::a1-incumbents".to_string(),
    )]);

    assert_eq!(is_team_member_process(&env), true);
}

#[test]
fn given_an_env_without_the_member_identity_when_checked_then_it_reports_a_non_member_process() {
    assert_eq!(is_team_member_process(&HashMap::new()), false);
}

#[test]
fn given_an_empty_member_identity_when_checked_then_it_reports_a_non_member_process() {
    let env = HashMap::from([(MEMBER_IDENTITY_ENV.to_string(), String::new())]);

    assert_eq!(is_team_member_process(&env), false);
}
