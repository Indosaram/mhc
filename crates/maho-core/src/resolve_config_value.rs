//! Port of senpi packages/coding-agent/src/core/resolve-config-value.ts and resolve-config-command.ts.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use regex::Regex;

pub const COMMAND_EXECUTION_MAX_ATTEMPTS: usize = 3;
pub const COMMAND_EXECUTION_BACKOFF_MS: [u64; 2] = [250, 1000];
pub const COMMAND_TIMEOUT_MS: u64 = 10_000;
pub const COMMAND_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplatePart {
    Literal(String),
    Env(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValueReference {
    Command(String),
    Template(Vec<TemplatePart>),
}

fn env_var_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("env var name regex"))
}

fn env_var_name_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*").expect("env var name prefix regex"))
}

fn append_literal(parts: &mut Vec<TemplatePart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(TemplatePart::Literal(previous)) = parts.last_mut() {
        previous.push_str(value);
        return;
    }
    parts.push(TemplatePart::Literal(value.to_owned()));
}

pub fn parse_config_value_template(config: &str) -> Vec<TemplatePart> {
    let mut parts: Vec<TemplatePart> = Vec::new();
    let bytes: Vec<char> = config.chars().collect();
    let mut index = 0usize;

    while index < bytes.len() {
        let Some(offset) = bytes[index..].iter().position(|c| *c == '$') else {
            append_literal(&mut parts, &bytes[index..].iter().collect::<String>());
            break;
        };
        let dollar_index = index + offset;
        append_literal(&mut parts, &bytes[index..dollar_index].iter().collect::<String>());
        let next_char = bytes.get(dollar_index + 1).copied();

        if next_char == Some('$') || next_char == Some('!') {
            append_literal(&mut parts, &next_char.unwrap_or_default().to_string());
            index = dollar_index + 2;
            continue;
        }

        if next_char == Some('{') {
            let end_index = bytes[dollar_index + 2..].iter().position(|c| *c == '}').map(|offset| dollar_index + 2 + offset);
            let Some(end_index) = end_index else {
                append_literal(&mut parts, "$");
                index = dollar_index + 1;
                continue;
            };
            let name: String = bytes[dollar_index + 2..end_index].iter().collect();
            if env_var_name_re().is_match(&name) {
                parts.push(TemplatePart::Env(name));
            } else {
                append_literal(&mut parts, &bytes[dollar_index..=end_index].iter().collect::<String>());
            }
            index = end_index + 1;
            continue;
        }

        let remainder: String = bytes[dollar_index + 1..].iter().collect();
        if let Some(matched) = env_var_name_prefix_re().find(&remainder) {
            let name = matched.as_str().to_owned();
            index = dollar_index + 1 + name.chars().count();
            parts.push(TemplatePart::Env(name));
            continue;
        }

        append_literal(&mut parts, "$");
        index = dollar_index + 1;
    }

    parts
}

pub fn parse_config_value_reference(config: &str) -> ConfigValueReference {
    if let Some(command) = config.strip_prefix('!') {
        return ConfigValueReference::Command(format!("!{command}"));
    }
    ConfigValueReference::Template(parse_config_value_template(config))
}

fn resolve_env_config_value(name: &str, env: Option<&HashMap<String, String>>) -> Option<String> {
    if let Some(value) = env.and_then(|env| env.get(name)).filter(|value| !value.is_empty()) {
        return Some(value.clone());
    }
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn template_env_var_names(parts: &[TemplatePart]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for part in parts {
        if let TemplatePart::Env(name) = part {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
    }
    names
}

fn resolve_template(parts: &[TemplatePart], env: Option<&HashMap<String, String>>) -> Option<String> {
    let mut resolved = String::new();
    for part in parts {
        match part {
            TemplatePart::Literal(value) => resolved.push_str(value),
            TemplatePart::Env(name) => {
                let value = resolve_env_config_value(name, env)?;
                resolved.push_str(&value);
            }
        }
    }
    Some(resolved)
}

pub fn get_config_value_env_var_name(config: &str) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Template(parts) => match parts.as_slice() {
            [TemplatePart::Env(name)] => Some(name.clone()),
            _ => None,
        },
        ConfigValueReference::Command(_) => None,
    }
}

pub fn get_config_value_env_var_names(config: &str) -> Vec<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Template(parts) => template_env_var_names(&parts),
        ConfigValueReference::Command(_) => Vec::new(),
    }
}

pub fn get_missing_config_value_env_var_names(config: &str, env: Option<&HashMap<String, String>>) -> Vec<String> {
    get_config_value_env_var_names(config)
        .into_iter()
        .filter(|name| resolve_env_config_value(name, env).is_none())
        .collect()
}

pub fn is_command_config_value(config: &str) -> bool {
    matches!(parse_config_value_reference(config), ConfigValueReference::Command(_))
}

pub fn is_config_value_configured(config: &str, env: Option<&HashMap<String, String>>) -> bool {
    get_missing_config_value_env_var_names(config, env).is_empty()
}

fn command_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve a config value: `!command`, `$ENV`/`${ENV}` templates, or a literal.
pub async fn resolve_config_value(config: &str, env: Option<&HashMap<String, String>>) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(command) => match env {
            None => execute_command(&command).await,
            Some(env) => run_config_command(&command, Some(env)).await,
        },
        ConfigValueReference::Template(parts) => resolve_template(&parts, env),
    }
}

pub async fn resolve_config_value_uncached(config: &str, env: Option<&HashMap<String, String>>) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(command) => run_config_command(&command, env).await,
        ConfigValueReference::Template(parts) => resolve_template(&parts, env),
    }
}

async fn execute_command(command_config: &str) -> Option<String> {
    if let Some(cached) = command_cache().lock().expect("config cache lock").get(command_config).cloned() {
        return cached;
    }
    let result = run_config_command(command_config, None).await;
    command_cache().lock().expect("config cache lock").insert(command_config.to_owned(), result.clone());
    result
}

pub async fn resolve_config_value_or_throw(
    config: &str,
    description: &str,
    env: Option<&HashMap<String, String>>,
) -> Result<String, String> {
    if let Some(resolved) = resolve_config_value_uncached(config, env).await {
        return Ok(resolved);
    }
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(command) => {
            Err(format!("Failed to resolve {description} from shell command: {}", command.trim_start_matches('!')))
        }
        ConfigValueReference::Template(_) => {
            let missing = get_missing_config_value_env_var_names(config, env);
            match missing.len() {
                1 => Err(format!("Failed to resolve {description} from environment variable: {}", missing[0])),
                count if count > 1 => Err(format!("Failed to resolve {description} from environment variables: {}", missing.join(", "))),
                _ => Err(format!("Failed to resolve {description}")),
            }
        }
    }
}

pub async fn resolve_headers_or_throw(
    headers: Option<&HashMap<String, String>>,
    description: &str,
    env: Option<&HashMap<String, String>>,
) -> Result<Option<HashMap<String, String>>, String> {
    let Some(headers) = headers else { return Ok(None) };
    let mut resolved = HashMap::new();
    for (key, value) in headers {
        let resolved_value = resolve_config_value_or_throw(value, &format!("{description} header \"{key}\""), env).await?;
        resolved.insert(key.clone(), resolved_value);
    }
    if resolved.is_empty() { Ok(None) } else { Ok(Some(resolved)) }
}

pub fn clear_config_value_cache() {
    command_cache().lock().expect("config cache lock").clear();
}

/// Execute a `!command` config value, retrying a transient failure with an awaited backoff.
pub async fn run_config_command(command_config: &str, env: Option<&HashMap<String, String>>) -> Option<String> {
    let command = command_config.trim_start_matches('!');
    for attempt in 0..COMMAND_EXECUTION_MAX_ATTEMPTS {
        if let Some(value) = execute_command_once(command, env).await {
            return Some(value);
        }
        if let Some(backoff_ms) = COMMAND_EXECUTION_BACKOFF_MS.get(attempt) {
            tokio::time::sleep(Duration::from_millis(*backoff_ms)).await;
        }
    }
    None
}

async fn execute_command_once(command: &str, env: Option<&HashMap<String, String>>) -> Option<String> {
    let mut process = tokio::process::Command::new("sh");
    process.arg("-c").arg(command);
    process.stdin(std::process::Stdio::null());
    process.stdout(std::process::Stdio::piped());
    process.stderr(std::process::Stdio::null());
    if let Some(env) = env {
        for (key, value) in env {
            process.env(key, value);
        }
    }
    process.kill_on_drop(true);
    let child = process.spawn().ok()?;
    match tokio::time::timeout(Duration::from_millis(COMMAND_TIMEOUT_MS), child.wait_with_output()).await {
        Ok(Ok(output)) => {
            if output.stdout.len() > COMMAND_MAX_OUTPUT_BYTES {
                return None;
            }
            let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if output.status.success() && !text.is_empty() { Some(text) } else { None }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
    }

    #[tokio::test]
    async fn resolves_literals_environment_templates_and_escapes() {
        let env = env_map(&[("TEST_CONFIG_LEFT", "left"), ("TEST_CONFIG_RIGHT", "right")]);
        assert_eq!(resolve_config_value("literal-key", Some(&env)).await.as_deref(), Some("literal-key"));
        assert_eq!(resolve_config_value("$TEST_CONFIG_LEFT", Some(&env)).await.as_deref(), Some("left"));
        assert_eq!(resolve_config_value("${TEST_CONFIG_LEFT}_$TEST_CONFIG_RIGHT", Some(&env)).await.as_deref(), Some("left_right"));
        assert_eq!(resolve_config_value("$$TEST_CONFIG_LEFT", Some(&env)).await.as_deref(), Some("$TEST_CONFIG_LEFT"));
        assert_eq!(resolve_config_value("$!literal-$TEST_CONFIG_RIGHT", Some(&env)).await.as_deref(), Some("!literal-right"));
        assert_eq!(resolve_config_value("${NOT-A-NAME}", Some(&env)).await.as_deref(), Some("${NOT-A-NAME}"));
        assert_eq!(resolve_config_value("${UNSET_NAME}", Some(&env)).await, None);
        assert_eq!(resolve_config_value("trailing$", Some(&env)).await.as_deref(), Some("trailing$"));
        assert_eq!(resolve_config_value("${UNCLOSED", Some(&env)).await.as_deref(), Some("${UNCLOSED"));
    }

    #[tokio::test]
    async fn uses_credential_scoped_environment_before_the_process_environment() {
        let scoped = env_map(&[("MAHO_CORE_TEST_SCOPED", "credential")]);
        assert_eq!(resolve_config_value("$MAHO_CORE_TEST_SCOPED", Some(&scoped)).await.as_deref(), Some("credential"));
        assert_eq!(resolve_config_value("$MAHO_CORE_TEST_SCOPED", None).await, None);
        let empty = env_map(&[("MAHO_CORE_TEST_SCOPED", "")]);
        assert_eq!(resolve_config_value("$MAHO_CORE_TEST_SCOPED", Some(&empty)).await, None);
        let process_visible = env_map(&[("HOME", "scoped-home")]);
        assert_eq!(resolve_config_value("$HOME", Some(&process_visible)).await.as_deref(), Some("scoped-home"));
        assert!(resolve_config_value("$HOME", None).await.is_some());
    }

    #[tokio::test]
    async fn executes_shell_commands_and_trims_their_output() {
        clear_config_value_cache();
        assert_eq!(resolve_config_value("!echo '  spaced-key  '", None).await.as_deref(), Some("spaced-key"));
        assert_eq!(resolve_config_value("!printf 'line1\nline2'", None).await.as_deref(), Some("line1\nline2"));
        assert_eq!(resolve_config_value("!echo 'hello world' | tr ' ' '-'", None).await.as_deref(), Some("hello-world"));
        clear_config_value_cache();
    }

    #[tokio::test]
    async fn a_failing_command_resolves_to_none_after_retries() {
        assert_eq!(resolve_config_value_uncached("!exit 3", None).await, None);
        assert_eq!(resolve_config_value_uncached("!true", None).await, None);
    }

    #[tokio::test]
    async fn caches_command_results_but_not_scoped_or_uncached_ones() {
        clear_config_value_cache();
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("counter");
        let script = format!("!n=$(cat {c} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {c}; echo $n", c = counter.display());
        assert_eq!(resolve_config_value(&script, None).await.as_deref(), Some("1"));
        assert_eq!(resolve_config_value(&script, None).await.as_deref(), Some("1"));
        assert_eq!(resolve_config_value_uncached(&script, None).await.as_deref(), Some("2"));
        let scoped = env_map(&[("UNUSED", "x")]);
        assert_eq!(resolve_config_value(&script, Some(&scoped)).await.as_deref(), Some("3"));
        clear_config_value_cache();
        assert_eq!(resolve_config_value(&script, None).await.as_deref(), Some("4"));
        clear_config_value_cache();
    }

    #[test]
    fn classifies_references_and_reports_missing_variables() {
        let env = env_map(&[("PRESENT", "1")]);
        assert!(is_command_config_value("!echo hi"));
        assert!(!is_command_config_value("$PRESENT"));
        assert_eq!(get_config_value_env_var_name("$PRESENT").as_deref(), Some("PRESENT"));
        assert_eq!(get_config_value_env_var_name("${PRESENT}").as_deref(), Some("PRESENT"));
        assert_eq!(get_config_value_env_var_name("$PRESENT-suffix"), None);
        assert_eq!(get_config_value_env_var_names("$A-$B-$A"), vec!["A", "B"]);
        assert_eq!(get_config_value_env_var_names("!echo $A"), Vec::<String>::new());
        assert_eq!(get_missing_config_value_env_var_names("$PRESENT-$MISSING", Some(&env)), vec!["MISSING"]);
        assert!(is_config_value_configured("$PRESENT", Some(&env)));
        assert!(!is_config_value_configured("$MISSING", Some(&env)));
    }

    #[tokio::test]
    async fn throws_the_exact_unresolved_messages() {
        let env = env_map(&[]);
        let error = resolve_config_value_or_throw("!exit 1", "Anthropic API key", None).await.expect_err("error");
        assert_eq!(error, "Failed to resolve Anthropic API key from shell command: exit 1");
        let error = resolve_config_value_or_throw("$MISSING_ONE", "Anthropic API key", Some(&env)).await.expect_err("error");
        assert_eq!(error, "Failed to resolve Anthropic API key from environment variable: MISSING_ONE");
        let error = resolve_config_value_or_throw("$MISSING_ONE-$MISSING_TWO", "Anthropic API key", Some(&env)).await.expect_err("error");
        assert_eq!(error, "Failed to resolve Anthropic API key from environment variables: MISSING_ONE, MISSING_TWO");
        assert_eq!(resolve_config_value_or_throw("$PRESENT", "key", Some(&env_map(&[("PRESENT", "v")]))).await.as_deref(), Ok("v"));
    }

    #[tokio::test]
    async fn resolves_headers_and_reports_the_header_name() {
        let env = env_map(&[("HEADER_VALUE", "v")]);
        assert!(resolve_headers_or_throw(None, "provider", None).await.expect("none").is_none());
        let headers = env_map(&[("x-key", "$HEADER_VALUE")]);
        let resolved = resolve_headers_or_throw(Some(&headers), "provider", Some(&env)).await.expect("resolved");
        assert_eq!(resolved.and_then(|headers| headers.get("x-key").cloned()).as_deref(), Some("v"));
        let headers = env_map(&[("x-key", "$MISSING")]);
        let error = resolve_headers_or_throw(Some(&headers), "provider", Some(&env)).await.expect_err("error");
        assert_eq!(error, "Failed to resolve provider header \"x-key\" from environment variable: MISSING");
    }

    #[test]
    fn template_parts_merge_adjacent_literals() {
        assert_eq!(parse_config_value_template("abc"), vec![TemplatePart::Literal("abc".to_owned())]);
        assert_eq!(
            parse_config_value_template("a$$b"),
            vec![TemplatePart::Literal("a$b".to_owned())]
        );
        assert_eq!(
            parse_config_value_template("a$X b"),
            vec![TemplatePart::Literal("a".to_owned()), TemplatePart::Env("X".to_owned()), TemplatePart::Literal(" b".to_owned())]
        );
        assert_eq!(parse_config_value_template(""), Vec::<TemplatePart>::new());
    }
}
