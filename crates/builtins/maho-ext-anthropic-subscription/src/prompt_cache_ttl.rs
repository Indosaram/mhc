//! Port of senpi `anthropic-subscription/prompt-cache-ttl.ts` (pin 903b8336).
use crate::auth_lane::TokenInjection;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Prompt-cache lifetimes Claude Code accepts for `CLAUDE_CODE_PROMPT_CACHE_TTL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptCacheTtl {
    FiveMinutes,
    OneHour,
}

impl PromptCacheTtl {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FiveMinutes => "5m",
            Self::OneHour => "1h",
        }
    }
}

pub const PROMPT_CACHE_TTL_ENV: &str = "CLAUDE_CODE_PROMPT_CACHE_TTL";
/// Claude Code reads this before `CLAUDE_CODE_PROMPT_CACHE_TTL`: when set, every lifetime it writes is 5 minutes.
pub const FORCE_PROMPT_CACHING_5M_ENV: &str = "FORCE_PROMPT_CACHING_5M";

fn is_truthy(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        ["1", "true", "yes", "on"].contains(&value.trim().to_lowercase().as_str())
    })
}

fn parse_prompt_cache_ttl(value: Option<&str>) -> Option<PromptCacheTtl> {
    match value {
        Some("5m") => Some(PromptCacheTtl::FiveMinutes),
        Some("1h") => Some(PromptCacheTtl::OneHour),
        _ => None,
    }
}

fn option_env<'a>(options: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    options.get("env")?.get(key)?.as_str()
}

/// Pin Claude Code's prompt-cache lifetime for a one-shot query and return it, so the history
/// breakpoint uses the same lifetime (senpi#2982). `FORCE_PROMPT_CACHING_5M` wins; then an explicit
/// `CLAUDE_CODE_PROMPT_CACHE_TTL` from the query env, then from the process environment. Managed
/// subscription lanes pin 1 hour; the ambient lane pins nothing and the caller adds no breakpoint.
pub fn pin_one_shot_prompt_cache_ttl(
    options: &mut Map<String, Value>,
    auth_lane: TokenInjection,
    environment: &BTreeMap<String, String>,
) -> Option<PromptCacheTtl> {
    let forced_5m = is_truthy(option_env(options, FORCE_PROMPT_CACHING_5M_ENV))
        || is_truthy(
            environment
                .get(FORCE_PROMPT_CACHING_5M_ENV)
                .map(String::as_str),
        );
    let ttl = (if forced_5m {
        Some(PromptCacheTtl::FiveMinutes)
    } else {
        None
    })
    .or_else(|| parse_prompt_cache_ttl(option_env(options, PROMPT_CACHE_TTL_ENV)))
    .or_else(|| parse_prompt_cache_ttl(environment.get(PROMPT_CACHE_TTL_ENV).map(String::as_str)))
    .or(if auth_lane == TokenInjection::Ambient {
        None
    } else {
        Some(PromptCacheTtl::OneHour)
    })?;
    let env = options
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()));
    if !env.is_object() {
        *env = Value::Object(Map::new());
    }
    env.as_object_mut().expect("env object").insert(
        PROMPT_CACHE_TTL_ENV.into(),
        Value::String(ttl.as_str().into()),
    );
    Some(ttl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect()
    }

    #[test]
    fn pins_lane_with_environment_for_both_claude_code_and_the_breakpoint() {
        let cases = [
            (TokenInjection::OAuthSlots, env(&[]), "1h"),
            (TokenInjection::ConfigDir, env(&[]), "1h"),
            (
                TokenInjection::OAuthSlots,
                env(&[(PROMPT_CACHE_TTL_ENV, "5m")]),
                "5m",
            ),
            (
                TokenInjection::Ambient,
                env(&[(PROMPT_CACHE_TTL_ENV, "1h")]),
                "1h",
            ),
            (
                TokenInjection::OAuthSlots,
                env(&[(FORCE_PROMPT_CACHING_5M_ENV, "1")]),
                "5m",
            ),
            (
                TokenInjection::ConfigDir,
                env(&[
                    (FORCE_PROMPT_CACHING_5M_ENV, "true"),
                    (PROMPT_CACHE_TTL_ENV, "1h"),
                ]),
                "5m",
            ),
            (
                TokenInjection::Ambient,
                env(&[(FORCE_PROMPT_CACHING_5M_ENV, "1")]),
                "5m",
            ),
        ];
        for (lane, environment, expected) in cases {
            let mut options = json!({"env":{"CLAUDE_CODE_TOTAL_TOKENS_REMINDER":"off"}})
                .as_object()
                .unwrap()
                .clone();
            let ttl = pin_one_shot_prompt_cache_ttl(&mut options, lane, &environment);
            assert_eq!(ttl.map(PromptCacheTtl::as_str), Some(expected));
            assert_eq!(
                options["env"],
                json!({"CLAUDE_CODE_TOTAL_TOKENS_REMINDER":"off","CLAUDE_CODE_PROMPT_CACHE_TTL":expected})
            );
        }
    }

    #[test]
    fn pins_nothing_on_the_ambient_lane() {
        let mut options = Map::new();
        let ttl = pin_one_shot_prompt_cache_ttl(
            &mut options,
            TokenInjection::Ambient,
            &env(&[(PROMPT_CACHE_TTL_ENV, "10m")]),
        );
        assert_eq!(ttl, None);
        assert!(options.get("env").is_none());
    }

    #[test]
    fn query_env_force_flag_wins_over_explicit_ttl() {
        let mut options =
            json!({"env":{"FORCE_PROMPT_CACHING_5M":" YES ","CLAUDE_CODE_PROMPT_CACHE_TTL":"1h"}})
                .as_object()
                .unwrap()
                .clone();
        let ttl =
            pin_one_shot_prompt_cache_ttl(&mut options, TokenInjection::OAuthSlots, &env(&[]));
        assert_eq!(ttl, Some(PromptCacheTtl::FiveMinutes));
        assert_eq!(options["env"][PROMPT_CACHE_TTL_ENV], "5m");
    }
}
