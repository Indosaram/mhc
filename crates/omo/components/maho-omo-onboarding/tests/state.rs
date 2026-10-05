use maho_omo_onboarding::state::*;
use std::io;
fn dir() -> io::Result<tempfile::TempDir> { tempfile::tempdir() }
#[test] fn fresh_claim() -> io::Result<()> { let d=dir()?; assert!(claim_onboarding(d.path())); assert!(is_onboarding_complete(d.path())); Ok(()) }
#[test] fn claimed_again_false() -> io::Result<()> { let d=dir()?; assert!(claim_onboarding(d.path())); assert!(!claim_onboarding(d.path())); Ok(()) }
#[test] fn corrupted_marker_counts() -> io::Result<()> { let d=dir()?; std::fs::write(d.path().join("onboarding-completed"),"not valid json {{{")?; assert!(is_onboarding_complete(d.path())); Ok(()) }
#[test] fn simultaneous_claim_one_winner() -> io::Result<()> {
    let d=dir()?; let gate=std::sync::Arc::new(std::sync::Barrier::new(3));
    let threads:Vec<_>=(0..2).map(|_| { let gate=gate.clone(); let path=d.path().to_owned(); std::thread::spawn(move || { gate.wait(); claim_onboarding(&path) }) }).collect();
    gate.wait(); let winners=threads.into_iter().map(|t| t.join().expect("test worker")).filter(|v| *v).count(); assert_eq!(winners,1); Ok(())
}
#[test] fn missing_mtime() -> io::Result<()> { let d=dir()?; assert!(get_onboarding_marker_mtime(d.path()).is_none()); Ok(()) }
#[test] fn existing_mtime() -> io::Result<()> { let d=dir()?; assert!(claim_onboarding(d.path())); assert!(get_onboarding_marker_mtime(d.path()).is_some()); Ok(()) }
#[test] fn global_decline() -> io::Result<()> { let d=dir()?; write_global_decline(d.path())?; assert!(is_globally_declined(d.path())); Ok(()) }
#[test] fn project_decline() -> io::Result<()> { let d=dir()?; write_project_decline(d.path(),"hash")?; assert!(is_project_declined(d.path(),"hash")); Ok(()) }
#[test] fn different_project_not_declined() -> io::Result<()> { let d=dir()?; write_project_decline(d.path(),"a")?; assert!(!is_project_declined(d.path(),"b")); Ok(()) }
#[test] fn cooldown_active() -> io::Result<()> { let d=dir()?; write_cooldown(d.path(),"hash",1_000_000.0)?; assert!(is_cooling_down(d.path(),"hash",1_000_001.0)); Ok(()) }
#[test] fn cooldown_expired() -> io::Result<()> { let d=dir()?; write_cooldown(d.path(),"hash",1_000_000.0)?; assert!(!is_cooling_down(d.path(),"hash",1_000_000.0+COOLDOWN_DAYS as f64*86_400_000.0+1.0)); Ok(()) }
#[test] fn missing_cooldown() -> io::Result<()> { let d=dir()?; assert!(!is_cooling_down(d.path(),"hash",1.0)); Ok(()) }
#[test] fn corrupt_cooldown() -> io::Result<()> { let d=dir()?; write_cooldown(d.path(),"hash",1.0)?; std::fs::write(d.path().join("onboarding-cooldowns/hash"),"not json{{{")?; assert!(!is_cooling_down(d.path(),"hash",2.0)); Ok(()) }
#[test] fn nonnumeric_until() -> io::Result<()> { let d=dir()?; write_cooldown(d.path(),"hash",1.0)?; std::fs::write(d.path().join("onboarding-cooldowns/hash"),r#"{"until":"not-a-number"}"#)?; assert!(!is_cooling_down(d.path(),"hash",2.0)); Ok(()) }
#[test] fn extra_fields_accepted() -> io::Result<()> { let d=dir()?; write_cooldown(d.path(),"hash",1.0)?; std::fs::write(d.path().join("onboarding-cooldowns/hash"),r#"{"until":100000,"extra":"ignored"}"#)?; assert!(is_cooling_down(d.path(),"hash",2.0)); Ok(()) }
#[test] fn marker_private_permissions() -> io::Result<()> { use std::os::unix::fs::PermissionsExt; let d=dir()?; assert!(claim_onboarding(d.path())); assert_eq!(std::fs::metadata(d.path().join("onboarding-completed"))?.permissions().mode()&0o777,0o600); Ok(()) }
