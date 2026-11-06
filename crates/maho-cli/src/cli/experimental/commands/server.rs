use super::super::{
    command::{Command, CommandOption, ParsedCommandInput, parse_options, string_option},
    command_options::{AuthInput, is_server_id, parse_auth, unsupported_options},
};
#[derive(Clone)]
pub struct ServerCommand {
    pub auth: Option<AuthInput>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub plugin_packages: Vec<String>,
    pub server_id: Option<String>,
    pub session_dir: Option<String>,
}
fn server_options() -> Vec<CommandOption> {
    let mut options: Vec<_> = [
        "--server-id",
        "--session-dir",
        "--provider",
        "--model",
        "--auth-token",
        "--auth-token-file",
    ]
    .into_iter()
    .map(|name| string_option(name, false))
    .collect();
    options.push(string_option("-e", true));
    options
}
pub fn parse_server(argv: &[String]) -> Result<ServerCommand, Vec<String>> {
    let input = parse_options(argv, &server_options());
    match build_server(&input) {
        Ok(command) if input.errors.is_empty() => Ok(command),
        Ok(_) => Err(input.errors),
        Err(errors) => Err(input.errors.into_iter().chain(errors).collect()),
    }
}
/// senpi `serverCommand.build`: the option-level errors stay on the parsed input.
fn build_server(input: &ParsedCommandInput) -> Result<ServerCommand, Vec<String>> {
    let mut errors = Vec::new();
    let server_id = input.value("--server-id").map(str::to_owned);
    if let Some(id) = &server_id
        && !is_server_id(id)
    {
        errors.push(format!(
            "Invalid --server-id \"{id}\"; expected a lowercase UUIDv4"
        ));
    }
    let auth = match parse_auth(input) {
        Ok(value) => value,
        Err(error) => {
            errors.push(error);
            None
        }
    };
    let provider = input.value("--provider").map(str::to_owned);
    let model = input.value("--model").map(str::to_owned);
    if provider.is_some() && model.is_none() {
        errors.push("--provider requires --model".to_owned());
    }
    errors.extend(unsupported_options("server", input));
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ServerCommand {
        auth,
        provider,
        model,
        plugin_packages: input.values.get("-e").cloned().unwrap_or_default(),
        server_id,
        session_dir: input.value("--session-dir").map(str::to_owned),
    })
}

/// senpi `ServerCommandContext`: the runtime that serves a parsed server command (the chord server
/// runtime behind it is not part of this build, plan D-M5).
pub trait ServerCommandContext: Send + Sync {
    fn run_server(
        &self,
        command: ServerCommand,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
}
/// senpi `serverCommand`: options, builder, and an action that hands the command to `runServer`.
pub fn server_command()
-> Result<Command<ServerCommand, std::sync::Arc<dyn ServerCommandContext>>, String> {
    let mut command = Command::new("server");
    for option in server_options() {
        command.option(option)?;
    }
    command.build(build_server).action(
        |command, context: std::sync::Arc<dyn ServerCommandContext>| context.run_server(command),
    );
    Ok(command)
}
