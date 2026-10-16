pub const CONFIG_COMMAND_ARGV: &str = "config";
pub const HOST_COMMAND_ARGV: &str = "host";
pub const SCHEDULE_COMMAND_ARGV: &str = "schedule";
pub fn is_package_command_argv(args: &[String]) -> bool { args.first().is_some_and(|command| matches!(command.as_str(), "install" | "remove" | "update" | "list" | "uninstall")) }
pub fn host_subcommand(args: &[String]) -> Option<&str> { (args.first().is_some_and(|command| command == HOST_COMMAND_ARGV)).then(|| args.get(1).map(String::as_str)).flatten() }
pub fn is_schedule_command_argv(args: &[String]) -> bool { args.first().is_some_and(|command| command == SCHEDULE_COMMAND_ARGV) }
pub fn is_host_session_command_argv(args: &[String]) -> bool { host_subcommand(args) == Some("session") }
