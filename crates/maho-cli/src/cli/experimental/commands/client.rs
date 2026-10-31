use super::super::{command::{Command, CommandOption, ParsedCommandInput, parse_options, string_option, flag_option}, command_options::{AuthInput, TransportAddress, parse_auth, parse_transport_address, unsupported_options}};
#[derive(Clone)]
pub struct ClientCommand {
    pub auth: Option<AuthInput>, pub connect: Option<TransportAddress>, pub session_id: Option<String>,
    pub continue_session: bool, pub resume: bool, pub provider: Option<String>, pub model: Option<String>,
    pub plugin_packages: Vec<String>, pub prompt: Option<String>,
}
fn client_options() -> Vec<CommandOption> {
    let mut options = vec![string_option("--connect", false), string_option("--session-id", false), string_option("--provider", false), string_option("--model", false), string_option("-e", true), string_option("--auth-token", false), string_option("--auth-token-file", false)];
    options.extend(["--continue", "-c", "--resume", "-r"].map(flag_option));
    options
}
pub fn parse_client(argv: &[String]) -> Result<ClientCommand, Vec<String>> {
    let input = parse_options(argv, &client_options());
    match build_client(&input) {
        Ok(command) if input.errors.is_empty() => Ok(command),
        Ok(_) => Err(input.errors),
        Err(errors) => Err(input.errors.into_iter().chain(errors).collect()),
    }
}
/// senpi `clientCommand.build`: the option-level errors stay on the parsed input.
fn build_client(input: &ParsedCommandInput) -> Result<ClientCommand, Vec<String>> {
    let mut errors = Vec::new();
    let connect = match input.value("--connect").map(parse_transport_address).transpose() { Ok(value) => value, Err(error) => { errors.push(error); None } };
    let auth = match parse_auth(input) { Ok(value) => value, Err(error) => { errors.push(error); None } };
    let session_id = input.value("--session-id").map(str::to_owned);
    let continue_session = input.value("--continue").is_some() || input.value("-c").is_some();
    let resume = input.value("--resume").is_some() || input.value("-r").is_some();
    let provider = input.value("--provider").map(str::to_owned);
    let model = input.value("--model").map(str::to_owned);
    let separated = input.remaining_args.first().is_some_and(|v| v == "--");
    let prompt_args = if separated { &input.remaining_args[1..] } else { &input.remaining_args[..] };
    let prompt = if prompt_args.len() == 1 && (separated || !prompt_args[0].starts_with('-')) && !prompt_args[0].is_empty() { Some(prompt_args[0].clone()) } else { None };
    if provider.is_some() && model.is_none() { errors.push("--provider requires --model".to_owned()); }
    if [session_id.is_some(), continue_session, resume].into_iter().filter(|v| *v).count() > 1 { errors.push("--session-id, --continue, and --resume are mutually exclusive".to_owned()); }
    if prompt.is_none() { errors.extend(unsupported_options("client", input)); }
    if !errors.is_empty() { return Err(errors); }
    Ok(ClientCommand { auth, connect, session_id, continue_session, resume, provider, model, plugin_packages: input.values.get("-e").cloned().unwrap_or_default(), prompt })
}

/// senpi `ClientCommandContext`: the runtime that runs a parsed client command (the chord client
/// runtime behind it is not part of this build, plan D-M5).
pub trait ClientCommandContext: Send + Sync {
    fn run_client(&self, command: ClientCommand) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
}
/// senpi `clientCommand`: options, builder, and an action that hands the command to `runClient`.
pub fn client_command() -> Result<Command<ClientCommand, std::sync::Arc<dyn ClientCommandContext>>, String> {
    let mut command = Command::new("client");
    for option in client_options() { command.option(option)?; }
    command.build(build_client).action(|command, context: std::sync::Arc<dyn ClientCommandContext>| context.run_client(command));
    Ok(command)
}
