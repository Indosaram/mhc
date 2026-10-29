use super::*;
use std::cell::{Cell, RefCell};
use std::io::Error;
use std::path::Path;

fn busy() -> Error {
    Error::from_raw_os_error(16)
}

#[test]
fn non_windows_platform_never_retries() {
    let calls = Cell::new(0u32);
    let mut rename = |_: &Path, _: &Path| -> std::io::Result<()> {
        calls.set(calls.get() + 1);
        Err(busy())
    };
    let mut sleep = |_: u64| panic!("must not sleep off Windows");
    let result = rename_with_contention_retry(
        &mut rename,
        Path::new("a"),
        Path::new("b"),
        RenamePlatform::Other,
        &CONTENTION_DELAYS_MS,
        &mut sleep,
    );
    assert!(result.is_err());
    assert_eq!(calls.get(), 1);
}

#[test]
fn windows_ladder_retries_contention_then_succeeds() {
    let calls = Cell::new(0u32);
    let sleeps = RefCell::new(Vec::new());
    let mut rename = |_: &Path, _: &Path| -> std::io::Result<()> {
        calls.set(calls.get() + 1);
        if calls.get() < 3 { Err(busy()) } else { Ok(()) }
    };
    let mut sleep = |ms: u64| sleeps.borrow_mut().push(ms);
    rename_with_contention_retry(
        &mut rename,
        Path::new("a"),
        Path::new("b"),
        RenamePlatform::Windows,
        &CONTENTION_DELAYS_MS,
        &mut sleep,
    )
    .expect("eventual success");
    assert_eq!(calls.get(), 3);
    assert_eq!(*sleeps.borrow(), vec![10, 25]);
}

#[test]
fn windows_ladder_exhaustion_surfaces_the_last_error() {
    let calls = Cell::new(0u32);
    let mut rename = |_: &Path, _: &Path| -> std::io::Result<()> {
        calls.set(calls.get() + 1);
        Err(busy())
    };
    let mut sleep = |_: u64| {};
    let result = rename_with_contention_retry(
        &mut rename,
        Path::new("a"),
        Path::new("b"),
        RenamePlatform::Windows,
        &CONTENTION_DELAYS_MS,
        &mut sleep,
    );
    assert!(result.is_err());
    assert_eq!(calls.get(), CONTENTION_DELAYS_MS.len() as u32 + 1);
}

#[test]
fn non_contention_error_surfaces_at_once_even_on_windows() {
    let calls = Cell::new(0u32);
    let mut rename = |_: &Path, _: &Path| -> std::io::Result<()> {
        calls.set(calls.get() + 1);
        Err(Error::from_raw_os_error(2))
    };
    let mut sleep = |_: u64| panic!("must not sleep on a real error");
    let result = rename_with_contention_retry(
        &mut rename,
        Path::new("a"),
        Path::new("b"),
        RenamePlatform::Windows,
        &CONTENTION_DELAYS_MS,
        &mut sleep,
    );
    assert!(result.is_err());
    assert_eq!(calls.get(), 1);
}
