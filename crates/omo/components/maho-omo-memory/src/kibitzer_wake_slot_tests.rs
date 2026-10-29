use super::*;

fn slot(dir: &std::path::Path, max_concurrent: usize, wait_timeout_ms: u64) -> KibitzerWakeSlot {
    KibitzerWakeSlot::new(KibitzerWakeSlotOptions {
        locks_directory: dir.to_path_buf(),
        max_concurrent,
        wait_timeout_ms: Some(wait_timeout_ms),
        poll_ms: Some(1),
        now: Arc::new(|| 0),
    })
    .unwrap()
}

#[test]
fn given_zero_concurrency_when_constructed_then_it_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let result = KibitzerWakeSlot::new(KibitzerWakeSlotOptions {
        locks_directory: dir.path().to_path_buf(),
        max_concurrent: 0,
        wait_timeout_ms: None,
        poll_ms: None,
        now: Arc::new(|| 0),
    });
    match result {
        Err(_) => {}
        Ok(_) => panic!("zero concurrency must be rejected"),
    }
}

#[test]
fn given_a_free_domain_when_acquired_then_a_slot_is_taken_and_released_once() {
    let dir = tempfile::tempdir().unwrap();
    let slot = slot(dir.path(), 2, 0);
    let lease = match slot.acquire(None).unwrap() {
        KibitzerWakeAdmission::Acquired { lease, .. } => lease,
        _ => panic!("expected an acquired lease"),
    };
    assert!(lease.slot >= 1);
    assert!(lease.release());
    assert!(!lease.release());
}

#[test]
fn given_every_slot_held_when_acquired_with_no_wait_then_busy() {
    let dir = tempfile::tempdir().unwrap();
    let slot = slot(dir.path(), 1, 0);
    let _lease = match slot.acquire(None).unwrap() {
        KibitzerWakeAdmission::Acquired { lease, .. } => lease,
        _ => panic!("expected the first lease"),
    };
    assert!(matches!(slot.acquire(None).unwrap(), KibitzerWakeAdmission::Busy { .. }));
}

#[test]
fn given_a_released_slot_when_acquired_again_then_it_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let slot = slot(dir.path(), 1, 0);
    if let KibitzerWakeAdmission::Acquired { lease, .. } = slot.acquire(None).unwrap() {
        assert!(lease.release());
    } else {
        panic!("expected the first lease");
    }
    assert!(matches!(slot.acquire(None).unwrap(), KibitzerWakeAdmission::Acquired { .. }));
}
