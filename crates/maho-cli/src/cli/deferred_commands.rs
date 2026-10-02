pub const CONFIG_COMMAND_ARGV: &str = "config";
pub const APP_SERVER_COMMAND_ARGV: &str = "app-server";
pub const HOST_COMMAND_ARGV: &str = "host";
pub fn is_package_command_argv(args: &[String]) -> bool { args.first().is_some_and(|command| matches!(command.as_str(), "install" | "remove" | "update" | "list" | "uninstall")) }
