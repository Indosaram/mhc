use maho_ext_prompt_preset::file_operations::{FileMutationMode, resolve_file_mutation_routing, build_file_operations_tuning};
#[test]
fn apply_patch_takes_precedence_over_edit_and_write() {
    let routing = resolve_file_mutation_routing(&["write".into(), "apply_patch".into(), "edit".into()]);
    assert_eq!(routing.mode, FileMutationMode::ApplyPatch);
    assert_eq!(routing.tools, ["apply_patch"]);
}
#[test]
fn edit_and_write_use_stable_order() {
    let routing = resolve_file_mutation_routing(&["write".into(), "edit".into(), "edit".into()]);
    assert_eq!(routing.mode, FileMutationMode::EditWrite);
    assert_eq!(routing.tools, ["edit", "write"]);
}
#[test]
fn no_mutation_tools_has_no_routing() {
    let routing = resolve_file_mutation_routing(&["read".into()]);
    assert_eq!(routing.mode, FileMutationMode::None);
    assert!(routing.tools.is_empty());
}
#[test]
fn unselected_file_tools_produce_no_tuning() {
    assert!(build_file_operations_tuning(&["eval".into()]).is_empty());
    assert!(!build_file_operations_tuning(&["read".into()]).is_empty());
}
