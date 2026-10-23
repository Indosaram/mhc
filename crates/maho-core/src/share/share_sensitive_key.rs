//! Port of senpi packages/coding-agent/src/core/share/share-sensitive-key.ts.
//!
//! Which keys hold credentials. The decision is made from the key's name alone: a value under a
//! sensitive key is replaced whole, whatever it contains. Names are split into words across
//! camelCase, snake_case, kebab-case and dots; ANY sensitive word makes the name sensitive, except
//! for a short list of exact names. There are no word-level exemptions.

const SENSITIVE_WORDS: [&str; 23] = [
    "key",
    "keys",
    "apikey",
    "token",
    "tokens",
    "secret",
    "secrets",
    "pass",
    "passwd",
    "password",
    "passwords",
    "passphrase",
    "pwd",
    "auth",
    "authorization",
    "credential",
    "credentials",
    "cookie",
    "cookies",
    "session",
    "sessionid",
    "private",
    "signature",
];

/// Exact names (compared as lower snake_case) that describe a credential without holding one.
const ORDINARY_NAMES: [&str; 7] = [
    "public_key",
    "public_key_path",
    "key_id",
    "auth_mode",
    "auth_type",
    "max_tokens",
    "max_output_tokens",
];

/// Splits across `aB` and `ABc` boundaries, lowercases, then splits on anything but `[a-z0-9]`.
fn words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut spaced = String::with_capacity(name.len() + 4);
    for (index, &current) in chars.iter().enumerate() {
        if index > 0 {
            let previous = chars[index - 1];
            let lower_then_upper = (previous.is_ascii_lowercase() || previous.is_ascii_digit())
                && current.is_ascii_uppercase();
            let acronym_end = previous.is_ascii_uppercase()
                && current.is_ascii_uppercase()
                && chars.get(index + 1).is_some_and(char::is_ascii_lowercase);
            if lower_then_upper || acronym_end {
                spaced.push('_');
            }
        }
        spaced.push(current);
    }
    spaced
        .to_lowercase()
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn is_sensitive_key_name(name: &str) -> bool {
    let parts = words(name);
    if !parts
        .iter()
        .any(|word| SENSITIVE_WORDS.contains(&word.as_str()))
    {
        return false;
    }
    !ORDINARY_NAMES.contains(&parts.join("_").as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_sensitive_word_makes_a_name_sensitive() {
        for name in [
            "apiKey",
            "api_key",
            "X-Auth-Token",
            "AWS_SECRET_ACCESS_KEY",
            "sessionId",
            "db.password",
            "APIKey",
            "authorization",
        ] {
            assert!(is_sensitive_key_name(name), "{name}");
        }
    }

    #[test]
    fn ordinary_names_and_plain_words_are_not_sensitive() {
        for name in [
            "public_key",
            "publicKey",
            "key_id",
            "keyId",
            "auth_mode",
            "max_tokens",
            "maxOutputTokens",
            "name",
            "monkey",
            "tokenizer",
        ] {
            assert!(!is_sensitive_key_name(name), "{name}");
        }
    }

    #[test]
    fn no_word_level_exemption_hides_a_secret() {
        assert!(is_sensitive_key_name("public_key_secret"));
        assert!(is_sensitive_key_name("max_tokens_key"));
    }
}
