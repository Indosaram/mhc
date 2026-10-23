use maho_omo_onboarding::state::*;
use std::io;
fn dir() -> io::Result<tempfile::TempDir> {
    tempfile::tempdir()
}
#[test]
fn fresh_claim() -> io::Result<()> {
    let d = dir()?;
    assert!(claim_onboarding(d.path()));
    assert!(is_onboarding_complete(d.path()));
    Ok(())
}
#[test]
fn claimed_again_false() -> io::Result<()> {
    let d = dir()?;
    assert!(claim_onboarding(d.path()));
    assert!(!claim_onboarding(d.path()));
    Ok(())
}
#[test]
fn corrupted_marker_counts() -> io::Result<()> {
    let d = dir()?;
    std::fs::write(d.path().join("onboarding-completed"), "not valid json {{{")?;
    assert!(is_onboarding_complete(d.path()));
    Ok(())
}
#[test]
fn simultaneous_claim_one_winner() -> io::Result<()> {
    let d = dir()?;
    let gate = std::sync::Arc::new(std::sync::Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let gate = gate.clone();
            let path = d.path().to_owned();
            std::thread::spawn(move || {
                gate.wait();
                claim_onboarding(&path)
            })
        })
        .collect();
    gate.wait();
    let winners = threads
        .into_iter()
        .map(|t| t.join().expect("test worker"))
        .filter(|v| *v)
        .count();
    assert_eq!(winners, 1);
    Ok(())
}
#[test]
fn missing_mtime() -> io::Result<()> {
    let d = dir()?;
    assert!(get_onboarding_marker_mtime(d.path()).is_none());
    Ok(())
}
#[test]
fn existing_mtime() -> io::Result<()> {
    let d = dir()?;
    assert!(claim_onboarding(d.path()));
    assert!(get_onboarding_marker_mtime(d.path()).is_some());
    Ok(())
}
#[test]
fn global_decline() -> io::Result<()> {
    let d = dir()?;
    write_global_decline(d.path())?;
    assert!(is_globally_declined(d.path()));
    Ok(())
}
#[test]
fn project_decline() -> io::Result<()> {
    let d = dir()?;
    write_project_decline(d.path(), "hash")?;
    assert!(is_project_declined(d.path(), "hash"));
    Ok(())
}
#[test]
fn different_project_not_declined() -> io::Result<()> {
    let d = dir()?;
    write_project_decline(d.path(), "a")?;
    assert!(!is_project_declined(d.path(), "b"));
    Ok(())
}
#[test]
fn cooldown_active() -> io::Result<()> {
    let d = dir()?;
    write_cooldown(d.path(), "hash", 1_000_000.0)?;
    assert!(is_cooling_down(d.path(), "hash", 1_000_001.0));
    Ok(())
}
#[test]
fn cooldown_expired() -> io::Result<()> {
    let d = dir()?;
    write_cooldown(d.path(), "hash", 1_000_000.0)?;
    assert!(!is_cooling_down(
        d.path(),
        "hash",
        1_000_000.0 + COOLDOWN_DAYS as f64 * 86_400_000.0 + 1.0
    ));
    Ok(())
}
#[test]
fn missing_cooldown() -> io::Result<()> {
    let d = dir()?;
    assert!(!is_cooling_down(d.path(), "hash", 1.0));
    Ok(())
}
#[test]
fn corrupt_cooldown() -> io::Result<()> {
    let d = dir()?;
    write_cooldown(d.path(), "hash", 1.0)?;
    std::fs::write(d.path().join("onboarding-cooldowns/hash"), "not json{{{")?;
    assert!(!is_cooling_down(d.path(), "hash", 2.0));
    Ok(())
}
#[test]
fn nonnumeric_until() -> io::Result<()> {
    let d = dir()?;
    write_cooldown(d.path(), "hash", 1.0)?;
    std::fs::write(
        d.path().join("onboarding-cooldowns/hash"),
        r#"{"until":"not-a-number"}"#,
    )?;
    assert!(!is_cooling_down(d.path(), "hash", 2.0));
    Ok(())
}
#[test]
fn extra_fields_accepted() -> io::Result<()> {
    let d = dir()?;
    write_cooldown(d.path(), "hash", 1.0)?;
    std::fs::write(
        d.path().join("onboarding-cooldowns/hash"),
        r#"{"until":100000,"extra":"ignored"}"#,
    )?;
    assert!(is_cooling_down(d.path(), "hash", 2.0));
    Ok(())
}
#[cfg(unix)]
#[test]
fn marker_private_permissions() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let d = dir()?;
    assert!(claim_onboarding(d.path()));
    assert_eq!(
        std::fs::metadata(d.path().join("onboarding-completed"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    Ok(())
}
#[test]
fn claim_is_versioned_and_not_completion() -> io::Result<()> {
    let d = dir()?;
    assert!(claim_onboarding(d.path()));
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(d.path().join(ONBOARDING_MARKER))?)?;
    assert_eq!(record["version"], 2);
    assert!(record["claimedAt"].is_string());
    assert!(read_onboarding_completed_at(d.path()).is_none());
    assert!(!is_onboarding_completion_recorded(d.path()));
    Ok(())
}
#[test]
fn completion_blocks_claims_and_survives_release() -> io::Result<()> {
    let d = dir()?;
    assert!(claim_onboarding(d.path()));
    complete_onboarding(d.path())?;
    let first = read_onboarding_completed_at(d.path());
    assert!(first.is_some());
    complete_onboarding(d.path())?;
    assert_eq!(read_onboarding_completed_at(d.path()), first);
    release_onboarding(d.path())?;
    assert!(is_onboarding_complete(d.path()));
    assert!(!claim_onboarding(d.path()));
    Ok(())
}
#[test]
fn legacy_marker_reads_as_completed() -> io::Result<()> {
    let d = dir()?;
    std::fs::write(
        d.path().join(ONBOARDING_MARKER),
        r#"{"completedAt":"2026-01-01T00:00:00.000Z","version":1}"#,
    )?;
    assert_eq!(
        read_onboarding_completed_at(d.path()).as_deref(),
        Some("2026-01-01T00:00:00.000Z")
    );
    Ok(())
}
#[test]
fn invalid_completed_at_falls_back_to_the_file_time() -> io::Result<()> {
    let d = dir()?;
    std::fs::write(
        d.path().join(COMPLETION_MARKER),
        r#"{"completedAt":"nonsense"}"#,
    )?;
    assert!(
        read_onboarding_completed_at(d.path())
            .is_some_and(|at| at.parse::<jiff::Timestamp>().is_ok())
    );
    Ok(())
}
#[test]
fn reset_forgets_claim_and_completion() -> io::Result<()> {
    let d = dir()?;
    assert!(claim_onboarding(d.path()));
    complete_onboarding(d.path())?;
    reset_onboarding(d.path())?;
    assert!(!is_onboarding_complete(d.path()));
    reset_onboarding(d.path())?;
    assert!(claim_onboarding(d.path()));
    Ok(())
}
