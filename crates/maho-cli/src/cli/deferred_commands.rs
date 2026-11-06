pub const CONFIG_COMMAND_ARGV: &str = "config";
pub const HOST_COMMAND_ARGV: &str = "host";
pub const SCHEDULE_COMMAND_ARGV: &str = "schedule";
pub fn is_package_command_argv(args: &[String]) -> bool {
    args.first().is_some_and(|command| {
        matches!(
            command.as_str(),
            "install" | "remove" | "update" | "list" | "uninstall"
        )
    })
}
pub fn host_subcommand(args: &[String]) -> Option<&str> {
    (args
        .first()
        .is_some_and(|command| command == HOST_COMMAND_ARGV))
    .then(|| args.get(1).map(String::as_str))
    .flatten()
}
pub fn is_schedule_command_argv(args: &[String]) -> bool {
    args.first()
        .is_some_and(|command| command == SCHEDULE_COMMAND_ARGV)
}
pub fn is_host_session_command_argv(args: &[String]) -> bool {
    host_subcommand(args) == Some("session")
}
/// senpi `dispatchHostCommand`: the daemon command answers with an exit code; `None` means this argv is not a host command. `host session ...` goes to `host_session_command`, every other host subcommand to `host_command`.
pub async fn dispatch_host_command(args: &[String]) -> Option<Result<i32, String>> {
    if args.first().map(String::as_str) != Some(HOST_COMMAND_ARGV) {
        return None;
    }
    if is_host_session_command_argv(args) {
        return Some(super::host_session_command::run_host_session_command(&args[2..]).await);
    }
    Some(super::host_command::run_host_command(&args[1..]).await)
}
