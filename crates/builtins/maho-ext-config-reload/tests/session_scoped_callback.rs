use maho_ai::node::provider_scope::{active_provider_scope, run_with_provider_scope, ProviderScope};
use maho_ext_config_reload::session_scoped_callback::bind_session_scoped_callback;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

#[test]
fn unscoped_callback_preserves_arguments() {
    let count = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&count);
    let callback = bind_session_scoped_callback(move |value| { observed.fetch_add(value, Ordering::SeqCst); });
    callback(3).unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 3);
}
#[test]
fn callback_reenters_scope_and_becomes_inert_after_close() {
    let scope = ProviderScope::new();
    let expected = scope.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&count);
    let callback = run_with_provider_scope(&scope, || bind_session_scoped_callback(move |value| {
        assert!(active_provider_scope().unwrap().ptr_eq(&expected));
        observed.fetch_add(value, Ordering::SeqCst);
    })).unwrap();
    callback(2).unwrap();
    scope.close();
    callback(4).unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
}
