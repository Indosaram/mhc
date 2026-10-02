use super::args::Args;
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AuthCommandKind { Check, ApiKey, BearerToken }
pub struct AuthCommand { pub kind: AuthCommandKind, pub args: Vec<String>, pub json: bool, pub credentials: bool, pub no_refresh: bool, pub min_expiry_ms: Option<u64> }
pub fn get_auth_command_name(kind: AuthCommandKind) -> &'static str { match kind { AuthCommandKind::Check => "auth check", AuthCommandKind::ApiKey => "auth print-api-key", AuthCommandKind::BearerToken => "auth print-bearer-token" } }
pub fn parse_auth_command(args: &[String]) -> Result<Option<AuthCommand>, String> {
    if args.first().is_none_or(|s| s != "auth") { return Ok(None); }
    let kind = match args.get(1).map(String::as_str) { Some("check") => AuthCommandKind::Check, Some("print-api-key") => AuthCommandKind::ApiKey, Some("print-bearer-token") => AuthCommandKind::BearerToken, other => return Err(format!("Unknown auth command \"{}\". Use \"mhc auth print-api-key\", \"mhc auth print-bearer-token\", or \"mhc auth check\".", other.unwrap_or(""))) };
    let mut result = AuthCommand { kind, args: Vec::new(), json: false, credentials: false, no_refresh: false, min_expiry_ms: None }; let mut index = 2;
    while index < args.len() { let arg = &args[index]; match arg.as_str() {
        "--min-expiry" => { if kind != AuthCommandKind::BearerToken { return Err("--min-expiry is only supported by print-bearer-token".to_owned()); } index += 1; let value = args.get(index).ok_or("--min-expiry must use a duration such as 30m or 1h")?; let split = value.find(|c: char| !c.is_ascii_digit()).unwrap_or(value.len()); let amount = value[..split].parse::<u64>().map_err(|_| "--min-expiry must use a duration such as 30m or 1h")?; let multiplier = match value[split..].to_lowercase().as_str() { "ms" => 1, "s" => 1000, "m" => 60000, "h" => 3600000, _ => return Err("--min-expiry must use a duration such as 30m or 1h".to_owned()) }; result.min_expiry_ms = Some(amount.checked_mul(multiplier).ok_or("--min-expiry duration is too large")?); }
        "--json" | "--credentials" | "--no-refresh" => { if kind != AuthCommandKind::Check { return Err(format!("{arg} is only supported by auth check")); } match arg.as_str() { "--json" => result.json = true, "--credentials" => result.credentials = true, _ => result.no_refresh = true } }
        _ => result.args.push(arg.clone()),
    } index += 1; } Ok(Some(result))
}
pub fn validate_auth_command_args(args: &Args, kind: AuthCommandKind) -> Result<(Option<&str>, Option<&str>), String> {
    let provider = args.provider.as_deref().map(str::trim).filter(|v| !v.is_empty()); let model = args.model.as_deref().map(str::trim).filter(|v| !v.is_empty());
    if let Some(option) = args.unknown_flags.keys().next() { return Err(format!("Unknown option --{option} for \"{}\".", get_auth_command_name(kind))); }
    if args.api_key.is_some() || !args.messages.is_empty() || !args.file_args.is_empty() { return Err("Auth commands only accept --provider and --model".to_owned()); }
    if provider.is_none() && model.is_none() { return Err(match kind { AuthCommandKind::Check => "Auth checks require --provider <provider> or --model <model>", AuthCommandKind::ApiKey | AuthCommandKind::BearerToken => "Credential printing requires --provider <provider> or --model <model>" }.to_owned()); } Ok((provider, model))
}
