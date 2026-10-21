use super::*;
#[test]
fn experimental_exports_preserve_reserved_authority_and_entry_witnesses() {
    assert_eq!(WATCH_CAPACITY, 256);
    assert!(define_task("pi.forged").is_err());
    assert!(define_entry::<Entry>("pi.forged").is_err());
    assert_eq!(entries().reset.kind(), "pi.reset");
    assert!(is_core_kind("pi.generation"));
    assert!(!is_core_kind("ordinary"));
}
