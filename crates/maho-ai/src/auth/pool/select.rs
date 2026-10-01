//! Port of senpi packages/ai/src/auth/pool/select.ts.

pub type SlotHasher = fn(&str) -> u64;

pub trait SelectableSlot: Clone {
    fn name(&self) -> &str;
    fn blocked_until(&self) -> Option<f64>;
    fn block_reason(&self) -> Option<&str>;
    fn clear_block(&self) -> Self;
    fn with_auth_block(&self) -> Self;
    fn with_rate_limit_block(&self, blocked_until: f64) -> Self;
}

pub const DEFAULT_POOL_AFFINITY_KEY: &str = "credential-pool-default";

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AllSlotsBlockedError {
    pub message: String,
    pub soonest_unblock_at: Option<f64>,
}

impl AllSlotsBlockedError {
    pub fn new(soonest_unblock_at: Option<f64>) -> Self {
        let message = match soonest_unblock_at {
            None => "All credential slots are blocked until re-login.".to_string(),
            Some(millis) => {
                let datetime = js_date_to_iso(millis);
                format!("All credential slots are blocked until {datetime}.")
            }
        };
        Self { message, soonest_unblock_at }
    }
}

fn js_date_to_iso(millis_since_epoch: f64) -> String {
    let secs = (millis_since_epoch / 1000.0).floor() as i64;
    let nanos = (((millis_since_epoch % 1000.0) + 1000.0) % 1000.0 * 1_000_000.0) as u32;
    chrono::DateTime::from_timestamp(secs, nanos)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default)]
pub struct SlotSelectionOptions {
    pub affinity_key: Option<String>,
    pub session_id: Option<String>,
    pub pinned_slot: Option<String>,
    pub now: Option<f64>,
}

pub fn get_pool_affinity_key(affinity_key: Option<&str>, session_id: Option<&str>) -> String {
    affinity_key.or(session_id).unwrap_or(DEFAULT_POOL_AFFINITY_KEY).to_string()
}

/// HRW ordering hashes `key\0slot.name` exactly like the claude-sdk-oauth affinity oracle, so a
/// sha256 hasher reproduces its winner sequence and no live session remaps when a pool migrates
/// onto this engine.
pub fn rendezvous_order<T: SelectableSlot>(key: &str, slots: &[T], hasher: SlotHasher) -> Vec<T> {
    let mut scored: Vec<(T, u64)> = slots.iter().map(|slot| (slot.clone(), hasher(&format!("{key}\0{}", slot.name())))).collect();
    scored.sort_by(|(_, left), (_, right)| right.cmp(left));
    scored.into_iter().map(|(slot, _)| slot).collect()
}

fn is_blocked<T: SelectableSlot>(slot: &T, now: f64) -> bool {
    slot.block_reason() == Some("auth_error") || slot.blocked_until().is_some_and(|until| until > now)
}

/// Elapsed rate/capacity blocks clear; auth blocks persist until a login rewrites the slot.
pub fn clear_expired_slot_blocks<T: SelectableSlot>(slots: &[T], now: f64) -> Vec<T> {
    slots
        .iter()
        .map(|slot| {
            if slot.block_reason() != Some("auth_error") && slot.blocked_until().is_some_and(|until| until <= now) {
                slot.clear_block()
            } else {
                slot.clone()
            }
        })
        .collect()
}

fn select_unblocked<T: SelectableSlot>(slots: &[T], options: &SlotSelectionOptions, now: f64, hasher: SlotHasher) -> Option<T> {
    if let Some(pinned_name) = &options.pinned_slot
        && let Some(pinned) = slots.iter().find(|slot| slot.name() == pinned_name)
            && !is_blocked(pinned, now) {
                return Some(pinned.clone());
            }
    let key = get_pool_affinity_key(options.affinity_key.as_deref(), options.session_id.as_deref());
    rendezvous_order(&key, slots, hasher).into_iter().find(|slot| !is_blocked(slot, now))
}

fn soonest_unblock_at<T: SelectableSlot>(slots: &[T], now: f64) -> Option<f64> {
    slots.iter().filter_map(|slot| slot.blocked_until()).filter(|until| *until > now).fold(None, |acc, value| {
        Some(acc.map_or(value, |current: f64| current.min(value)))
    })
}

/// Selects a pinned or HRW-ranked slot with no pool-global selection state.
pub fn select_slot<T: SelectableSlot>(
    slots: &[T],
    options: &SlotSelectionOptions,
    hasher: SlotHasher,
) -> Result<T, AllSlotsBlockedError> {
    let now = options.now.unwrap_or_else(|| crate::utils::diagnostics::now_ms() as f64);
    if let Some(selected) = select_unblocked(slots, options, now, hasher) {
        return Ok(selected);
    }

    let cleared = clear_expired_slot_blocks(slots, now);
    let retried_options = SlotSelectionOptions { now: Some(now), ..options.clone() };
    let retried = select_unblocked(&cleared, &retried_options, now, hasher);
    if let Some(retried) = retried
        && let Some(original) = slots.iter().find(|slot| slot.name() == retried.name()) {
            return Ok(original.clone());
        }
    Err(AllSlotsBlockedError::new(soonest_unblock_at(slots, now)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct TestSlot {
        name: String,
        blocked_until: Option<f64>,
        block_reason: Option<String>,
    }

    impl SelectableSlot for TestSlot {
        fn name(&self) -> &str {
            &self.name
        }
        fn blocked_until(&self) -> Option<f64> {
            self.blocked_until
        }
        fn block_reason(&self) -> Option<&str> {
            self.block_reason.as_deref()
        }
        fn clear_block(&self) -> Self {
            Self { blocked_until: None, block_reason: None, ..self.clone() }
        }
        fn with_auth_block(&self) -> Self {
            Self { blocked_until: None, block_reason: Some("auth_error".into()), ..self.clone() }
        }
        fn with_rate_limit_block(&self, blocked_until: f64) -> Self {
            Self { blocked_until: Some(blocked_until), block_reason: Some("rate_limit".into()), ..self.clone() }
        }
    }

    fn slot(name: &str) -> TestSlot {
        TestSlot { name: name.into(), blocked_until: None, block_reason: None }
    }

    fn fixed_hasher(input: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        input.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn pinned_unblocked_slot_wins() {
        let slots = vec![slot("a"), slot("b")];
        let options = SlotSelectionOptions { pinned_slot: Some("b".into()), now: Some(0.0), ..Default::default() };
        let selected = select_slot(&slots, &options, fixed_hasher).unwrap();
        assert_eq!(selected.name, "b");
    }

    #[test]
    fn pinned_blocked_slot_falls_back_to_rendezvous() {
        let slots = vec![slot("a"), TestSlot { name: "b".into(), blocked_until: Some(1000.0), block_reason: Some("rate_limit".into()) }];
        let options = SlotSelectionOptions { pinned_slot: Some("b".into()), now: Some(0.0), ..Default::default() };
        let selected = select_slot(&slots, &options, fixed_hasher).unwrap();
        assert_eq!(selected.name, "a");
    }

    #[test]
    fn auth_error_block_persists_through_clear_expired() {
        let slots = vec![TestSlot { name: "a".into(), blocked_until: Some(1.0), block_reason: Some("auth_error".into()) }];
        let now = 1000.0;
        let cleared = clear_expired_slot_blocks(&slots, now);
        assert_eq!(cleared[0].block_reason, Some("auth_error".into()));
    }

    #[test]
    fn rate_limit_block_clears_after_expiry() {
        let slots = vec![TestSlot { name: "a".into(), blocked_until: Some(1.0), block_reason: Some("rate_limit".into()) }];
        let now = 1000.0;
        let cleared = clear_expired_slot_blocks(&slots, now);
        assert_eq!(cleared[0].block_reason, None);
        assert_eq!(cleared[0].blocked_until, None);
    }

    #[test]
    fn all_blocked_throws_with_soonest_unblock_at() {
        let slots = vec![
            TestSlot { name: "a".into(), blocked_until: Some(500.0), block_reason: Some("auth_error".into()) },
            TestSlot { name: "b".into(), blocked_until: Some(2000.0), block_reason: Some("auth_error".into()) },
        ];
        let options = SlotSelectionOptions { now: Some(0.0), ..Default::default() };
        let err = select_slot(&slots, &options, fixed_hasher).unwrap_err();
        assert_eq!(err.soonest_unblock_at, Some(500.0));
    }

    #[test]
    fn all_blocked_without_unblock_time_uses_relogin_message() {
        let slots = vec![TestSlot { name: "a".into(), blocked_until: None, block_reason: Some("auth_error".into()) }];
        let options = SlotSelectionOptions { now: Some(0.0), ..Default::default() };
        let err = select_slot(&slots, &options, fixed_hasher).unwrap_err();
        assert_eq!(err.message, "All credential slots are blocked until re-login.");
    }

    #[test]
    fn get_pool_affinity_key_prefers_affinity_over_session_over_default() {
        assert_eq!(get_pool_affinity_key(Some("a"), Some("s")), "a");
        assert_eq!(get_pool_affinity_key(None, Some("s")), "s");
        assert_eq!(get_pool_affinity_key(None, None), DEFAULT_POOL_AFFINITY_KEY);
    }

    #[test]
    fn rendezvous_order_is_deterministic_for_same_key() {
        let slots = vec![slot("a"), slot("b"), slot("c")];
        let first = rendezvous_order("key", &slots, fixed_hasher);
        let second = rendezvous_order("key", &slots, fixed_hasher);
        assert_eq!(first.iter().map(|s| s.name.clone()).collect::<Vec<_>>(), second.iter().map(|s| s.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn different_keys_can_rendezvous_with_different_winners() {
        let slots = vec![slot("a"), slot("b"), slot("c")];
        let winners: std::collections::BTreeSet<String> =
            (0..16).map(|index| rendezvous_order(&format!("key-{index}"), &slots, fixed_hasher)[0].name.clone()).collect();
        assert!(winners.len() > 1, "{winners:?}");
    }

    #[test]
    fn removing_a_non_winning_slot_keeps_the_winner_stable() {
        let slots = vec![slot("a"), slot("b"), slot("c")];
        let order = rendezvous_order("key", &slots, fixed_hasher);
        let winner = order[0].name.clone();
        let loser = order[1].name.clone();
        let kept: Vec<TestSlot> = slots.iter().filter(|entry| entry.name != loser).cloned().collect();
        assert_eq!(rendezvous_order("key", &kept, fixed_hasher)[0].name, winner);
    }

    #[test]
    fn a_blocked_rendezvous_winner_is_skipped_for_the_next_unblocked_slot() {
        let slots = vec![slot("a"), slot("b"), slot("c")];
        let order = rendezvous_order("key", &slots, fixed_hasher);
        let winner = order[0].name.clone();
        let runner_up = order[1].name.clone();
        let blocked: Vec<TestSlot> = slots
            .iter()
            .map(|entry| {
                if entry.name == winner {
                    entry.with_rate_limit_block(5000.0)
                } else {
                    entry.clone()
                }
            })
            .collect();
        let options =
            SlotSelectionOptions { affinity_key: Some("key".into()), now: Some(0.0), ..Default::default() };
        assert_eq!(select_slot(&blocked, &options, fixed_hasher).unwrap().name, runner_up);
    }

    #[test]
    fn selection_recovers_from_a_stale_persisted_block_via_the_cleared_view() {
        let slots = vec![
            TestSlot { name: "a".into(), blocked_until: Some(1000.0), block_reason: Some("rate_limit".into()) },
            TestSlot { name: "b".into(), blocked_until: Some(500.0), block_reason: Some("rate_limit".into()) },
        ];
        let winner = rendezvous_order("key", &slots, fixed_hasher)[0].name.clone();
        let options =
            SlotSelectionOptions { affinity_key: Some("key".into()), now: Some(2000.0), ..Default::default() };
        assert_eq!(select_slot(&slots, &options, fixed_hasher).unwrap().name, winner);
    }
}
