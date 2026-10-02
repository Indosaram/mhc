use super::super::{command::{parse_options, string_option}, command_options::{AuthInput, parse_auth, is_server_id, unsupported_options}};
pub struct ServerCommand {
    pub auth: Option<AuthInput>, pub provider: Option<String>, pub model: Option<String>,
    pub plugin_packages: Vec<String>, pub server_id: Option<String>, pub session_dir: Option<String>,
}
pub fn parse_server(argv: &[String]) -> Result<ServerCommand, Vec<String>> {
    let mut options: Vec<_> = ["--server-id", "--session-dir", "--provider", "--model", "--auth-token", "--auth-token-file"].into_iter().map(|name| string_option(name, false)).collect();
    options.push(string_option("-e", true));
    let input = parse_options(argv, &options);
    let mut errors = input.errors.clone();
    let server_id = input.value("--server-id").map(str::to_owned);
    if let Some(id) = &server_id && !is_server_id(id) {
        errors.push(format!("Invalid --server-id \"{id}\"; expected a lowercase UUIDv4"));
    }
    let auth = match parse_auth(&input) { Ok(value) => value, Err(error) => { errors.push(error); None } };
    let provider = input.value("--provider").map(str::to_owned);
    let model = input.value("--model").map(str::to_owned);
    if provider.is_some() && model.is_none() { errors.push("--provider requires --model".to_owned()); }
    errors.extend(unsupported_options("server", &input));
    if !errors.is_empty() { return Err(errors); }
    Ok(ServerCommand { auth, provider, model, plugin_packages: input.values.get("-e").cloned().unwrap_or_default(), server_id, session_dir: input.value("--session-dir").map(str::to_owned) })
}
