//! senpi rpc-host-idle-handover-refusals.test.ts "host handoff --when idle arguments" through the
//! real `parse_host_args`.
use maho_cli::cli::host_command::parse_host_args;

fn terms() -> Vec<String> {
    let build = format!("sha256:{}", "b".repeat(64));
    [
        "--when",
        "idle",
        "--operation",
        "op",
        "--if-instance",
        "i",
        "--target-build",
        build.as_str(),
    ]
    .map(str::to_owned)
    .to_vec()
}

fn handoff(rest: &[String]) -> Vec<String> {
    std::iter::once("handoff".to_owned())
        .chain(rest.iter().cloned())
        .collect()
}

#[test]
fn reads_the_terms_of_a_conditional_handover() {
    let mut args = terms();
    args.extend(["--if-generation".to_owned(), "3".to_owned()]);
    let parsed = parse_host_args(&handoff(&args)).expect("handoff terms");
    let handover = parsed.handover.expect("conditional handover");
    assert_eq!(handover.operation_id, "op");
    assert_eq!(handover.if_instance_id, "i");
    assert_eq!(handover.if_generation, 3);
}

#[test]
fn refuses_a_generation_it_cannot_represent_exactly_and_incomplete_terms() {
    let error = |args: Vec<String>| match parse_host_args(&handoff(&args)) {
        Ok(_) => panic!("{args:?} must be refused"),
        Err(error) => error,
    };
    let mut unrepresentable = terms();
    unrepresentable.extend(["--if-generation".to_owned(), "9007199254740993".to_owned()]);
    assert!(error(unrepresentable).contains("--if-generation"));
    let incomplete = ["--when", "idle", "--operation", "op"]
        .map(str::to_owned)
        .to_vec();
    assert!(error(incomplete).contains("needs --operation"));
    let mut later = terms()[2..].to_vec();
    later.extend(["--if-generation", "1", "--when", "later"].map(str::to_owned));
    assert!(error(later).contains("only --when idle"));
}
