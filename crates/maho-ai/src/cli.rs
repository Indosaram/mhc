//! Port of senpi packages/ai/src/cli.ts: the `pi-ai` OAuth login CLI (`login`, `list`, `help`).
//!
//! senpi derives `PROVIDERS` from `builtinProviders()` filtered to providers with `auth.oauth`; the
//! native providers do not carry an auth field, so the same set is `builtin_providers()` filtered to
//! the ids `load_builtin_oauth` can load (`BUILTIN_OAUTH_PROVIDERS`), keeping registry order and the
//! provider display names.

use crate::auth::oauth::load::{BUILTIN_OAUTH_PROVIDERS, load_builtin_oauth};
use crate::auth::types::{AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, Credential, ProviderAuthInteraction};
use crate::utils::abort::{AbortController, AbortSignal};
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::io::Write;
use std::sync::Arc;

const AUTH_FILE: &str = "auth.json";

struct CliProvider {
    id: String,
    name: String,
}

fn providers() -> Vec<CliProvider> {
    crate::providers::all::builtin_providers()
        .into_iter()
        .filter(|provider| BUILTIN_OAUTH_PROVIDERS.iter().any(|(id, _)| *id == provider.id()))
        .map(|provider| CliProvider { id: provider.id().to_owned(), name: provider.name().to_owned() })
        .collect()
}

async fn prompt(question: String) -> anyhow::Result<String> {
    print!("{question}");
    std::io::stdout().flush()?;
    let line = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map(|_| line)
    })
    .await??;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// JS `Number.parseInt(value, 10)`: leading whitespace, optional sign, then leading digits.
fn parse_int(value: &str) -> Option<i64> {
    let trimmed = value.trim_start();
    let (sign, rest) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<i64>().ok().map(|n| sign * n)
}

fn selection_index(answer: &str, len: usize) -> Option<usize> {
    let index = parse_int(answer)? - 1;
    usize::try_from(index).ok().filter(|index| *index < len)
}

fn load_auth() -> Map<String, Value> {
    let Ok(raw) = std::fs::read_to_string(AUTH_FILE) else { return Map::new() };
    serde_json::from_str::<Map<String, Value>>(&raw).unwrap_or_default()
}

fn save_auth(auth: &Map<String, Value>) -> anyhow::Result<()> {
    std::fs::write(AUTH_FILE, serde_json::to_string_pretty(auth)?)?;
    Ok(())
}

struct CliInteraction;

#[async_trait]
impl AuthInteraction for CliInteraction {
    fn signal(&self) -> Option<AbortSignal> {
        None
    }

    async fn prompt(&self, auth_prompt: AuthPrompt) -> anyhow::Result<String> {
        match auth_prompt.kind {
            AuthPromptKind::Select { message, options } => {
                println!("\n{message}");
                for (index, option) in options.iter().enumerate() {
                    println!("  {}. {}", index + 1, option.label);
                }
                let answer = prompt(format!("Enter number (1-{}): ", options.len())).await?;
                let index = selection_index(&answer, options.len()).ok_or_else(|| anyhow::anyhow!("Invalid selection"))?;
                Ok(options[index].id.clone())
            }
            AuthPromptKind::Text { message, placeholder }
            | AuthPromptKind::Secret { message, placeholder }
            | AuthPromptKind::ManualCode { message, placeholder } => {
                let suffix = placeholder.map(|placeholder| format!(" ({placeholder})")).unwrap_or_default();
                prompt(format!("{message}{suffix}: ")).await
            }
        }
    }

    fn notify(&self, event: AuthEvent) {
        match event {
            AuthEvent::AuthUrl { url, instructions } => {
                println!("\nOpen this URL in your browser:\n{url}");
                if let Some(instructions) = instructions.filter(|i| !i.is_empty()) {
                    println!("{instructions}");
                }
            }
            AuthEvent::DeviceCode { user_code, verification_uri, .. } => {
                println!("\nOpen this URL in your browser:\n{verification_uri}");
                println!("Enter code: {user_code}");
            }
            AuthEvent::Info { message, .. } | AuthEvent::Progress { message } => println!("{message}"),
        }
    }
}

async fn login(provider_id: &str) -> anyhow::Result<()> {
    let oauth = load_builtin_oauth(provider_id).await?.ok_or_else(|| anyhow::anyhow!("Unknown provider: {provider_id}"))?;
    let interaction = ProviderAuthInteraction::new(AbortController::new().signal(), Arc::new(CliInteraction));
    let credential = oauth.login(&interaction).await?;
    let mut auth = load_auth();
    auth.insert(provider_id.to_owned(), serde_json::to_value(Credential::OAuth(credential))?);
    save_auth(&auth)?;
    println!("\nCredentials saved to {AUTH_FILE}");
    Ok(())
}

fn usage(providers: &[CliProvider]) -> String {
    let provider_list =
        providers.iter().map(|provider| format!("  {:<20} {}", provider.id, provider.name)).collect::<Vec<_>>().join("\n");
    format!(
        "Usage: npx @earendil-works/pi-ai <command> [provider]\n\nCommands:\n  login [provider]  Login to an OAuth provider\n  list              List available providers\n\nProviders:\n{provider_list}"
    )
}

pub async fn main(args: &[String]) -> anyhow::Result<()> {
    let providers = providers();
    let command = args.first().map(String::as_str);
    match command {
        None | Some("help" | "--help" | "-h") => {
            println!("{}", usage(&providers));
            Ok(())
        }
        Some("list") => {
            for provider in &providers {
                println!("{:<20} {}", provider.id, provider.name);
            }
            Ok(())
        }
        Some("login") => {
            let mut provider_id = args.get(1).cloned();
            if provider_id.is_none() {
                for (index, provider) in providers.iter().enumerate() {
                    println!("  {}. {}", index + 1, provider.name);
                }
                let answer = prompt(format!("Enter number (1-{}): ", providers.len())).await?;
                provider_id = selection_index(&answer, providers.len()).map(|index| providers[index].id.clone());
            }
            let provider_id = provider_id.filter(|id| providers.iter().any(|provider| &provider.id == id));
            let Some(provider_id) = provider_id else {
                return Err(anyhow::anyhow!("Unknown provider: {}", args.get(1).map(String::as_str).unwrap_or("")));
            };
            login(&provider_id).await
        }
        Some(command) => Err(anyhow::anyhow!("Unknown command: {command}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_follows_js_parse_int_and_rejects_out_of_range() {
        assert_eq!(selection_index("2", 3), Some(1));
        assert_eq!(selection_index(" 3abc", 3), Some(2));
        assert_eq!(selection_index("0", 3), None);
        assert_eq!(selection_index("4", 3), None);
        assert_eq!(selection_index("x", 3), None);
    }

    #[test]
    fn lists_only_oauth_capable_builtin_providers() {
        let ids: Vec<String> = providers().into_iter().map(|provider| provider.id).collect();
        assert!(ids.iter().all(|id| BUILTIN_OAUTH_PROVIDERS.iter().any(|(oauth_id, _)| oauth_id == id)));
        assert!(ids.contains(&"anthropic".to_owned()));
    }
}
