use super::*;
use memory_core::recall::RecallCandidate;

fn candidate(path: &str) -> RecallCandidate {
    RecallCandidate { path: path.into(), description: String::new(), excerpt: String::new(), score: 1.0 }
}

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|path| (*path).to_string()).collect()
}

#[test]
fn given_max_items_zero_when_deciding_then_silent() {
    assert_eq!(decide_wake(&[candidate("a.md")], &set(&[]), &set(&[]), 0, false), WakeDecision::Silent(WakeSilenceReason::MaxItemsZero));
}

#[test]
fn given_no_candidates_when_deciding_then_silent() {
    assert_eq!(decide_wake(&[], &set(&[]), &set(&[]), 2, false), WakeDecision::Silent(WakeSilenceReason::NoCandidates));
}

#[test]
fn given_only_seen_or_system_paths_when_deciding_then_no_new_candidate() {
    let candidates = [candidate("a.md"), candidate("system/persona.md")];
    assert_eq!(decide_wake(&candidates, &set(&["a.md"]), &set(&[]), 2, false), WakeDecision::Silent(WakeSilenceReason::NoNewCandidate));
}

#[test]
fn given_a_fresh_candidate_under_cooldown_when_deciding_then_cooldown_wins() {
    assert_eq!(decide_wake(&[candidate("a.md")], &set(&[]), &set(&[]), 2, true), WakeDecision::Silent(WakeSilenceReason::Cooldown));
}

#[test]
fn given_fresh_candidates_when_deciding_then_they_return_in_batch_order_deduped() {
    let candidates = [candidate("a.md"), candidate("a.md"), candidate("b.md")];
    assert_eq!(decide_wake(&candidates, &set(&[]), &set(&["c.md"]), 2, false), WakeDecision::Wake(vec![candidate("a.md"), candidate("b.md")]));
}

#[test]
fn given_a_cooldown_when_charged_to_the_limit_then_it_is_exhausted_until_the_window_slides() {
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let read = clock.clone();
    let now: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(move || read.load(std::sync::atomic::Ordering::SeqCst));
    let mut cooldown = AcceptedNudgeCooldown::with_options(now, ACCEPTED_NUDGE_COOLDOWN_LIMIT, ACCEPTED_NUDGE_COOLDOWN_WINDOW_MS);
    assert!(!cooldown.exhausted());
    cooldown.charge();
    assert!(!cooldown.exhausted());
    cooldown.charge();
    assert!(cooldown.exhausted());
    clock.store(ACCEPTED_NUDGE_COOLDOWN_WINDOW_MS + 1, std::sync::atomic::Ordering::SeqCst);
    assert!(!cooldown.exhausted());
}

#[test]
fn given_the_default_constructor_when_charged_twice_then_the_default_band_applies() {
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let read = clock.clone();
    let now: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(move || read.load(std::sync::atomic::Ordering::SeqCst));
    let mut cooldown = AcceptedNudgeCooldown::new(now);
    cooldown.charge();
    cooldown.charge();
    assert!(cooldown.exhausted());
}

#[test]
fn given_system_paths_when_classified_then_only_the_prefix_matches() {
    assert!(is_system_memory_path("system/"));
    assert!(is_system_memory_path("system/persona.md"));
    assert!(!is_system_memory_path("notes/a.md"));
    assert!(!is_system_memory_path("systematic.md"));
}
