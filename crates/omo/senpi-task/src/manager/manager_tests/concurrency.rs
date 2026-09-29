//! `concurrency.test.ts`

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::manager::concurrency::{Grant, TaskConcurrency, TaskConcurrencyConfig};

const CLAUDE: &str = "anthropic/claude";

fn limited(limit: usize) -> TaskConcurrency {
    TaskConcurrency::new(TaskConcurrencyConfig {
        default_concurrency: Some(limit),
        ..TaskConcurrencyConfig::default()
    })
}

/// The TS grant runs synchronously inside `release`; the port hands it back to run.
fn release(concurrency: &mut TaskConcurrency, model: &str) {
    if let Some(grant) = concurrency.release(model) {
        grant();
    }
}

type GrantLog = Arc<Mutex<Vec<String>>>;

fn recorder() -> (GrantLog, impl Fn(&str) -> Grant) {
    let log: GrantLog = Arc::default();
    let sink = Arc::clone(&log);
    let make = move |label: &str| -> Grant {
        let sink = Arc::clone(&sink);
        let label = label.to_string();
        Box::new(move || sink.lock().expect("log").push(label))
    };
    (log, make)
}

#[test]
fn given_default_settings_when_nothing_acquired_then_a_fresh_model_has_a_free_slot() {
    assert!(limited(5).has_free_slot(CLAUDE));
}

#[test]
fn given_limit_reached_when_checking_free_slot_then_it_reports_full_and_exposes_queue_position() {
    let mut concurrency = limited(1);
    concurrency.acquire(CLAUDE, "st_00000001");
    let free = concurrency.has_free_slot(CLAUDE);
    let position = concurrency.enqueue(CLAUDE, "st_00000002", Box::new(|| {}));
    assert!(!free);
    assert_eq!(position, 1);
}

#[test]
fn given_a_waiter_enqueued_when_the_holder_releases_then_the_waiter_callback_fires_fifo_handoff() {
    let mut concurrency = limited(1);
    concurrency.acquire(CLAUDE, "st_00000001");
    let (log, make) = recorder();
    concurrency.enqueue(CLAUDE, "st_00000002", make("granted"));
    release(&mut concurrency, CLAUDE);
    assert_eq!(*log.lock().expect("log"), ["granted"]);
}

#[test]
fn given_two_waiters_when_slots_free_one_at_a_time_then_they_are_granted_in_fifo_order() {
    let mut concurrency = limited(1);
    concurrency.acquire("openai/gpt", "st_00000001");
    let (log, make) = recorder();
    concurrency.enqueue("openai/gpt", "st_00000002", make("second"));
    concurrency.enqueue("openai/gpt", "st_00000003", make("third"));
    release(&mut concurrency, "openai/gpt");
    release(&mut concurrency, "openai/gpt");
    assert_eq!(*log.lock().expect("log"), ["second", "third"]);
}

#[test]
fn given_model_and_provider_overrides_when_resolving_a_key_then_model_override_wins_over_provider()
{
    let concurrency = TaskConcurrency::new(TaskConcurrencyConfig {
        default_concurrency: Some(5),
        model_concurrency: Some(BTreeMap::from([("anthropic/opus".to_string(), 2)])),
        provider_concurrency: Some(BTreeMap::from([("anthropic".to_string(), 3)])),
    });
    assert_eq!(concurrency.get_key("anthropic/opus"), "anthropic/opus");
    assert_eq!(concurrency.get_key("anthropic/sonnet"), "anthropic");
    assert_eq!(concurrency.get_limit("anthropic/opus"), Some(2));
    assert_eq!(concurrency.get_limit("anthropic/sonnet"), Some(3));
}

#[test]
fn given_different_models_when_both_acquire_under_a_shared_default_limit_then_each_keeps_its_own_count()
 {
    let mut concurrency = limited(1);
    concurrency.acquire(CLAUDE, "st_00000001");
    assert!(concurrency.has_free_slot("openai/gpt"));
}

#[test]
fn given_an_enqueued_waiter_when_removed_then_its_queue_position_becomes_undefined() {
    let mut concurrency = limited(1);
    concurrency.acquire(CLAUDE, "st_00000001");
    concurrency.enqueue(CLAUDE, "st_00000002", Box::new(|| {}));
    assert_eq!(concurrency.queue_position(CLAUDE, "st_00000002"), Some(1));
    assert!(concurrency.remove(CLAUDE, "st_00000002"));
    assert_eq!(concurrency.queue_position(CLAUDE, "st_00000002"), None);
}

#[test]
fn given_three_queued_waiters_when_the_middle_is_removed_then_survivors_renumber() {
    let mut concurrency = limited(1);
    concurrency.acquire(CLAUDE, "st_00000001");
    for id in ["st_00000002", "st_00000003", "st_00000004"] {
        concurrency.enqueue(CLAUDE, id, Box::new(|| {}));
    }
    assert_eq!(concurrency.queue_position(CLAUDE, "st_00000004"), Some(3));
    assert!(concurrency.remove(CLAUDE, "st_00000003"));
    assert_eq!(concurrency.queue_position(CLAUDE, "st_00000002"), Some(1));
    assert_eq!(concurrency.queue_position(CLAUDE, "st_00000004"), Some(2));
}

#[test]
fn given_head_waiter_removed_when_release_fires_then_it_grants_the_next_survivor_not_the_removed_one()
 {
    let mut concurrency = limited(1);
    concurrency.acquire(CLAUDE, "st_00000001");
    let (log, make) = recorder();
    concurrency.enqueue(CLAUDE, "st_00000002", make("head"));
    concurrency.enqueue(CLAUDE, "st_00000003", make("survivor"));
    concurrency.remove(CLAUDE, "st_00000002");
    release(&mut concurrency, CLAUDE);
    assert_eq!(*log.lock().expect("log"), ["survivor"]);
}

#[test]
fn given_no_such_queued_task_when_remove_called_then_it_returns_false() {
    let mut concurrency = limited(1);
    concurrency.acquire(CLAUDE, "st_00000001");
    concurrency.enqueue(CLAUDE, "st_00000002", Box::new(|| {}));
    assert!(!concurrency.remove(CLAUDE, "st_99999999"));
    assert_eq!(concurrency.queue_position(CLAUDE, "st_00000002"), Some(1));
}

#[test]
fn given_an_acquired_slot_when_a_queued_waiter_is_removed_then_get_count_is_unchanged() {
    let mut concurrency = limited(1);
    concurrency.acquire(CLAUDE, "st_00000001");
    concurrency.enqueue(CLAUDE, "st_00000002", Box::new(|| {}));
    assert_eq!(concurrency.get_count(CLAUDE), 1);
    concurrency.remove(CLAUDE, "st_00000002");
    assert_eq!(concurrency.get_count(CLAUDE), 1);
}

#[test]
fn given_three_waiters_one_removed_when_release_drains_then_both_survivors_grant_in_fifo_and_removed_never_grants()
 {
    let mut concurrency = limited(1);
    concurrency.acquire(CLAUDE, "st_00000001");
    let (log, make) = recorder();
    concurrency.enqueue(CLAUDE, "st_00000002", make("second"));
    concurrency.enqueue(CLAUDE, "st_00000003", make("third"));
    concurrency.enqueue(CLAUDE, "st_00000004", make("fourth"));
    concurrency.remove(CLAUDE, "st_00000003");
    release(&mut concurrency, CLAUDE);
    release(&mut concurrency, CLAUDE);
    assert_eq!(*log.lock().expect("log"), ["second", "fourth"]);
}
