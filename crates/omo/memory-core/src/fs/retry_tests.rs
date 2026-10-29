use super::*;
use std::io::{Error, ErrorKind};

#[test]
fn eintr_is_retried_up_to_the_cap_and_then_surfaces() {
    let mut attempts = 0usize;
    let result: std::io::Result<()> = retry_on_eintr(|| {
        attempts += 1;
        Err(Error::new(ErrorKind::Interrupted, "interrupted"))
    });
    assert!(result.is_err());
    assert_eq!(attempts, EINTR_RETRY_CAP + 1);
}

#[test]
fn non_eintr_surfaces_immediately() {
    let mut attempts = 0usize;
    let result: std::io::Result<()> = retry_on_eintr(|| {
        attempts += 1;
        Err(Error::new(ErrorKind::PermissionDenied, "denied"))
    });
    assert!(result.is_err());
    assert_eq!(attempts, 1);
}

#[test]
fn interrupted_then_success_returns_the_value() {
    let mut attempts = 0usize;
    let value = retry_on_eintr(|| {
        attempts += 1;
        if attempts < 3 {
            Err(Error::new(ErrorKind::Interrupted, "again"))
        } else {
            Ok("done")
        }
    })
    .expect("eventual success");
    assert_eq!(value, "done");
    assert_eq!(attempts, 3);
}

#[test]
fn is_eintr_classifies_only_interrupted() {
    assert!(is_eintr(&Error::new(ErrorKind::Interrupted, "x")));
    assert!(!is_eintr(&Error::new(ErrorKind::NotFound, "x")));
}
