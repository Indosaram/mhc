//! senpi `dispatchInternalSupervisor` (main.ts:930, modes/rpc/supervisor-route.ts): the internal
//! supervisor route is answered before the public parser, and argv that merely mentions the
//! sentinel falls through to that parser unchanged.
#[test]
fn the_internal_route_flag_is_the_one_the_launcher_emits() {
    assert_eq!(maho_rpc::supervisor_route::INTERNAL_SUPERVISOR_ROUTE_FLAG, maho_rpc::host_lifecycle::INTERNAL_SUPERVISOR_FLAG);
    let launch = maho_rpc::host_launch::host_launch(std::path::PathBuf::from("mhc"), &["--socket".to_owned(), "s".to_owned()]);
    assert_eq!(launch.args.first().map(String::as_str), Some(maho_rpc::supervisor_route::INTERNAL_SUPERVISOR_ROUTE_FLAG));
}

#[tokio::test]
async fn dispatch_declines_argv_that_merely_mentions_the_sentinel() {
    let argv = ["--model".to_owned(), maho_rpc::supervisor_route::INTERNAL_SUPERVISOR_ROUTE_FLAG.to_owned()];
    assert!(!maho_rpc::supervisor_route::dispatch_internal_supervisor(&argv).await);
}
