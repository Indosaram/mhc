use super::args::Args;
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AuthCommandKind { Check, ApiKey, BearerToken, Consent }
pub struct AuthCommand { pub kind: AuthCommandKind, pub args: Vec<String>, pub json: bool, pub credentials: bool, pub no_refresh: bool, pub min_expiry_ms: Option<u64>, pub revoke: bool, pub yes: bool, pub rotation: bool }
pub fn get_auth_command_name(kind: AuthCommandKind) -> &'static str { match kind { AuthCommandKind::Check => "auth check", AuthCommandKind::ApiKey => "auth print-api-key", AuthCommandKind::BearerToken => "auth print-bearer-token", AuthCommandKind::Consent => "auth consent" } }
pub fn get_auth_command_usage(kind: AuthCommandKind) -> &'static str {
    match kind {
        AuthCommandKind::Check => "mhc auth check --provider <provider> [--json] [--credentials] [--no-refresh]",
        AuthCommandKind::ApiKey => "mhc auth print-api-key --provider <provider> [--model <model>]",
        AuthCommandKind::BearerToken => "mhc auth print-bearer-token --provider <provider> [--model <model>] [--min-expiry <duration>]",
        AuthCommandKind::Consent => "mhc auth consent <provider> [--rotation] [--revoke] [--yes]",
    }
}
pub fn is_auth_command_help(args: &[String]) -> bool {
    args.first().is_some_and(|arg| arg == "auth") && (args.get(1).is_none_or(|arg| arg == "help") || args.iter().any(|arg| matches!(arg.as_str(), "--help" | "-h")))
}
pub fn auth_command_help() -> &'static str {
    "Usage:\n  mhc auth print-api-key [--provider <provider>] [--model <model>]\n  mhc auth print-bearer-token [--provider <provider>] [--model <model>] [--min-expiry <duration>]\n  mhc auth check [--provider <provider>] [--model <model>] [--json] [--credentials] [--no-refresh]\n  mhc auth consent <provider> [--rotation] [--revoke] [--yes]\n\nconsent records (or with --revoke removes) the risk acknowledgement required by providers that impersonate a first-party client; with --rotation it gates automatic multi-account failover across token pools; it is stored as providerRiskConsent.<provider> or providerRiskConsent.<provider>:rotation in the global settings.json. Without a terminal, --yes is required.\n\nAuth commands require at least one of --provider or --model. Checks refresh expired OAuth credentials by default; --no-refresh prevents this. --credentials emits the credential, or includes it in JSON output.\n"
}
pub fn get_auth_credential(auth: Option<&maho_ai::models::AuthResolution>) -> Option<&str> {
    let auth = &auth?.auth;
    if let Some(key) = auth.api_key.as_deref().filter(|key| !key.is_empty()) { return Some(key); }
    let authorization = auth.headers.as_ref()?.iter().find(|(name, _)| name.eq_ignore_ascii_case("authorization"))?.1.as_deref()?;
    let separator = authorization.find(char::is_whitespace)?;
    if !authorization[..separator].eq_ignore_ascii_case("bearer") { return None; }
    let value = authorization[separator..].trim_start();
    (!value.is_empty() && !value.contains(['\r', '\n'])).then_some(value)
}

pub fn auth_command_error(message: impl Into<String>) -> String { message.into() }
pub fn parse_auth_command(args: &[String]) -> Result<Option<AuthCommand>, String> {
    if args.first().is_none_or(|s| s != "auth") { return Ok(None); }
    let kind = match args.get(1).map(String::as_str) { Some("check") => AuthCommandKind::Check, Some("print-api-key") => AuthCommandKind::ApiKey, Some("print-bearer-token") => AuthCommandKind::BearerToken, Some("consent") => AuthCommandKind::Consent, other => return Err(format!("Unknown auth command \"{}\". Use \"mhc auth print-api-key\", \"mhc auth print-bearer-token\", \"mhc auth check\", or \"mhc auth consent\".", other.unwrap_or(""))) };
    let mut result = AuthCommand { kind, args: Vec::new(), json: false, credentials: false, no_refresh: false, min_expiry_ms: None, revoke: false, yes: false, rotation: false }; let mut index = 2;
    while index < args.len() { let arg = &args[index]; match arg.as_str() {
        "--min-expiry" => { if kind != AuthCommandKind::BearerToken { return Err("--min-expiry is only supported by print-bearer-token".to_owned()); } index += 1; let value = args.get(index).ok_or("--min-expiry must use a duration such as 30m or 1h")?; let split = value.find(|c: char| !c.is_ascii_digit()).unwrap_or(value.len()); let amount = value[..split].parse::<u64>().map_err(|_| "--min-expiry must use a duration such as 30m or 1h")?; let multiplier = match value[split..].to_lowercase().as_str() { "ms" => 1, "s" => 1000, "m" => 60000, "h" => 3600000, _ => return Err("--min-expiry must use a duration such as 30m or 1h".to_owned()) }; result.min_expiry_ms = Some(amount.checked_mul(multiplier).ok_or("--min-expiry duration is too large")?); }
        "--json" | "--credentials" | "--no-refresh" => { if kind != AuthCommandKind::Check { return Err(format!("{arg} is only supported by auth check")); } match arg.as_str() { "--json" => result.json = true, "--credentials" => result.credentials = true, _ => result.no_refresh = true } }
        "--revoke" | "--yes" | "--rotation" => {
            if kind != AuthCommandKind::Consent { return Err(format!("{arg} is only supported by auth consent")); }
            if arg == "--revoke" { result.revoke = true }
            else if arg == "--rotation" { result.rotation = true }
            else { result.yes = true }
        }
        _ => result.args.push(arg.clone()),
    } index += 1; } Ok(Some(result))
}
pub fn validate_auth_command_args(args: &Args, kind: AuthCommandKind) -> Result<(Option<&str>, Option<&str>), String> {
    let provider = args.provider.as_deref().map(str::trim).filter(|v| !v.is_empty()); let model = args.model.as_deref().map(str::trim).filter(|v| !v.is_empty());
    if let Some(option) = args.unknown_flags.keys().next() { return Err(format!("Unknown option --{option} for \"{}\".", get_auth_command_name(kind))); }
    if args.api_key.is_some() || !args.messages.is_empty() || !args.file_args.is_empty() { return Err("Auth commands only accept --provider and --model".to_owned()); }
    if provider.is_none() && model.is_none() { return Err(match kind { AuthCommandKind::Check => "Auth checks require --provider <provider> or --model <model>", AuthCommandKind::ApiKey | AuthCommandKind::BearerToken => "Credential printing requires --provider <provider> or --model <model>" }.to_owned()); } Ok((provider, model))
}

/// `mhc auth consent <provider> [--rotation] [--revoke] [--yes]`: records (or revokes) `providerRiskConsent.<provider>`
/// or `providerRiskConsent.<provider>:rotation` in the GLOBAL settings only. Returns the process exit code.
pub fn run_auth_consent(command: &AuthCommand, agent_dir: &str, is_tty: bool, input: &mut dyn std::io::BufRead, out: &mut dyn std::io::Write, err: &mut dyn std::io::Write) -> i32 {
    use maho_core::provider_consent as consent;
    let known = || consent::IMPERSONATING_PROVIDERS.iter().map(|(id, _)| *id).collect::<Vec<_>>().join(", ");
    let [provider] = command.args.as_slice() else {
        let _ = writeln!(err, "Error: Usage: {}\nKnown providers: {}", get_auth_command_usage(AuthCommandKind::Consent), known());
        return 2;
    };
    let description = consent::impersonation_description(provider);
    if !command.rotation && description.is_none() {
        let _ = writeln!(err, "Error: Unknown provider \"{provider}\" for consent. Known providers: {}", known());
        return 2;
    }
    let provider = consent::canonical_provider(provider).to_owned();
    let mut settings = maho_core::settings_manager::SettingsManager::create(agent_dir, agent_dir, agent_dir, false);
    if command.rotation {
        if command.revoke {
            return match consent::revoke_rotation_consent(&mut settings, &provider) {
                Ok(()) => { let _ = writeln!(out, "Revoked rotation consent for {provider}."); 0 }
                Err(error) => { let _ = writeln!(err, "Error: {error}"); 1 }
            };
        }
        let _ = writeln!(out, "Enabling automatic account rotation for {provider} rotates across configured accounts on rate limits.");
        if !command.yes {
            if !is_tty {
                let _ = writeln!(err, "Error: consent needs a terminal; pass --yes to record it non-interactively.");
                return 1;
            }
            let _ = write!(out, "Type yes to continue: "); let _ = out.flush();
            let mut answer = String::new();
            if input.read_line(&mut answer).unwrap_or(0) == 0 || answer.trim() != "yes" {
                let _ = writeln!(err, "Consent not recorded.");
                return 1;
            }
        }
        return match consent::record_rotation_consent(&mut settings, &provider) {
            Ok(()) => { let _ = writeln!(out, "Recorded providerRiskConsent.{provider}:rotation."); 0 }
            Err(error) => { let _ = writeln!(err, "Error: {error}"); 1 }
        };
    }
    if command.revoke {
        return match consent::revoke_consent(&mut settings, &provider) {
            Ok(()) => { let _ = writeln!(out, "Revoked risk consent for {provider}."); 0 }
            Err(error) => { let _ = writeln!(err, "Error: {error}"); 1 }
        };
    }
    let description = description.unwrap_or("a first-party client");
    let _ = writeln!(out, "{provider} uses {description}. This may violate the provider's terms and can get the account suspended.");
    if !command.yes {
        if !is_tty {
            let _ = writeln!(err, "Error: consent needs a terminal; pass --yes to record it non-interactively.");
            return 1;
        }
        let _ = write!(out, "Type yes to continue: "); let _ = out.flush();
        let mut answer = String::new();
        if input.read_line(&mut answer).unwrap_or(0) == 0 || answer.trim() != "yes" {
            let _ = writeln!(err, "Consent not recorded.");
            return 1;
        }
    }
    match consent::record_consent(&mut settings, &provider) {
        Ok(()) => { let _ = writeln!(out, "Recorded providerRiskConsent.{provider}."); 0 }
        Err(error) => { let _ = writeln!(err, "Error: {error}"); 1 }
    }
}

#[cfg(test)]
mod consent_tests {
    use super::*;
    fn parse(values: &[&str]) -> AuthCommand { parse_auth_command(&values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>()).expect("parse").expect("command") }
    fn run(command: &AuthCommand, dir: &str, tty: bool, input: &str) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_auth_consent(command, dir, tty, &mut input.as_bytes(), &mut out, &mut err);
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }
    #[test]
    fn consent_and_revoke_parse() {
        let command = parse(&["auth", "consent", "github-copilot", "--revoke", "--yes"]);
        assert!(command.kind == AuthCommandKind::Consent && command.revoke && command.yes);
        assert_eq!(command.args, vec!["github-copilot".to_owned()]);
        assert!(parse_auth_command(&["auth".to_owned(), "check".to_owned(), "--revoke".to_owned()]).is_err());
    }
    #[test]
    fn consent_writes_only_on_yes_and_revoke_removes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        let settings = dir.path().join("settings.json");
        let command = parse(&["auth", "consent", "github-copilot"]);
        assert_eq!(run(&command, &path, true, "no\n").0, 1);
        assert_eq!(run(&command, &path, true, "").0, 1);
        assert_eq!(run(&command, &path, false, "yes\n").0, 1);
        assert!(!settings.exists());
        let (code, out, _) = run(&command, &path, true, "yes\n");
        assert_eq!(code, 0);
        assert!(out.contains("VS Code GitHub Copilot Chat"));
        assert!(std::fs::read_to_string(&settings).unwrap().contains("providerRiskConsent"));
        assert_eq!(run(&parse(&["auth", "consent", "github-copilot", "--revoke"]), &path, false, "").0, 0);
        assert!(!std::fs::read_to_string(&settings).unwrap().contains("github-copilot\":"));
    }
    #[test]
    fn consent_unknown_provider_exits_2_listing_known_ids() {
        let dir = tempfile::tempdir().unwrap();
        let (code, _, err) = run(&parse(&["auth", "consent", "openai", "--yes"]), &dir.path().to_string_lossy(), false, "");
        assert_eq!(code, 2);
        assert!(err.contains("github-copilot") && err.contains("kimi-coding"));
    }
    #[test]
    fn rotation_consent_and_revoke_parse_and_write() {
        let command = parse(&["auth", "consent", "anthropic-subscription", "--rotation", "--yes"]);
        assert!(command.kind == AuthCommandKind::Consent && command.rotation && command.yes);
        assert_eq!(command.args, vec!["anthropic-subscription".to_owned()]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        let settings = dir.path().join("settings.json");
        let (code, out, _) = run(&command, &path, false, "");
        assert_eq!(code, 0);
        assert!(out.contains("Recorded providerRiskConsent.anthropic-subscription:rotation."));
        assert!(std::fs::read_to_string(&settings).unwrap().contains("anthropic-subscription:rotation"));

        let revoke_command = parse(&["auth", "consent", "anthropic-subscription", "--rotation", "--revoke"]);
        let (code_rev, out_rev, _) = run(&revoke_command, &path, false, "");
        assert_eq!(code_rev, 0);
        assert!(out_rev.contains("Revoked rotation consent for anthropic-subscription."));
        assert!(!std::fs::read_to_string(&settings).unwrap().contains("anthropic-subscription:rotation"));
    }
}
