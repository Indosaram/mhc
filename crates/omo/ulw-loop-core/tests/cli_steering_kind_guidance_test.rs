use ulw_loop_core::cli_steering::parse_steering_kind;

fn args(values: &[&str]) -> Vec<String> { values.iter().map(|v| (*v).into()).collect() }

#[test]
fn missing_kind_explains_allowed_kinds_and_example() {
    let error = parse_steering_kind(&[]).unwrap_err();
    assert!(error.message.contains("Allowed --kind values:"));
    assert!(error.message.contains("annotate_ledger"));
    assert_eq!(error.code, "ULW_LOOP_STEERING_KIND_REQUIRED");
}

#[test]
fn invalid_kind_reports_value_and_valid_kinds() {
    let error = parse_steering_kind(&args(&["--kind", "bogus"])).unwrap_err();
    assert!(error.message.contains("Invalid --kind: bogus."));
    assert!(error.message.contains("revise_criterion"));
    assert!(error.message.contains("mark_blocked_superseded"));
}
