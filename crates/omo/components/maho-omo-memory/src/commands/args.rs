//! Minimal slash-command argument parser shared by the memory command handlers.
//! Port of `components/memory/commands/args.ts` at pin 77f3067f1.

use std::collections::BTreeMap;

/// A parsed flag value: `--flag` yields `True`, `--flag=value`/`--flag value` yield `Value`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlagValue {
    True,
    Value(String),
}

impl FlagValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::True => None,
            Self::Value(value) => Some(value),
        }
    }

    pub fn is_true(&self) -> bool {
        matches!(self, Self::True)
    }
}

/// Parsed positionals plus the flag map, mirroring senpi's `ParsedCommandArgs`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedCommandArgs {
    pub positionals: Vec<String>,
    pub flags: BTreeMap<String, FlagValue>,
}

impl ParsedCommandArgs {
    pub fn flag(&self, name: &str) -> Option<&FlagValue> {
        self.flags.get(name)
    }

    pub fn has_flag(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    /// The string value of a flag, ignoring booleans.
    pub fn flag_value(&self, name: &str) -> Option<&str> {
        self.flags.get(name).and_then(FlagValue::as_str)
    }

    /// True only for a boolean-style flag (`--force` without a value).
    pub fn flag_is_true(&self, name: &str) -> bool {
        self.flags.get(name).is_some_and(FlagValue::is_true)
    }
}

/// Flag names that never consume the following token.
#[derive(Clone, Copy, Debug, Default)]
pub struct ParseCommandArgsOptions<'a> {
    pub booleans: &'a [&'a str],
}

/// Parse `--flag value`, `--flag=value`, and declared boolean flags.
pub fn parse_command_args_full(args: &str, options: ParseCommandArgsOptions<'_>) -> ParsedCommandArgs {
    let booleans = options.booleans;
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut positionals = Vec::new();
    let mut flags: BTreeMap<String, FlagValue> = BTreeMap::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        let Some(body) = token.strip_prefix("--") else {
            positionals.push(token.to_owned());
            index += 1;
            continue;
        };
        if let Some((key, value)) = body.split_once('=') {
            flags.insert(key.to_owned(), FlagValue::Value(value.to_owned()));
            index += 1;
            continue;
        }
        let next = tokens.get(index + 1);
        if !booleans.contains(&body) && next.is_some_and(|next| !next.starts_with("--")) {
            flags.insert(
                body.to_owned(),
                FlagValue::Value(next.copied().unwrap_or_default().to_owned()),
            );
            index += 2;
            continue;
        }
        flags.insert(body.to_owned(), FlagValue::True);
        index += 1;
    }
    ParsedCommandArgs { positionals, flags }
}

/// Retained native helper used by the pre-existing modules; delegates to the full parser.
pub fn parse_command_args(
    args: &str,
    booleans: &[&str],
) -> (Vec<String>, BTreeMap<String, Option<String>>) {
    let parsed = parse_command_args_full(args, ParseCommandArgsOptions { booleans });
    let flags = parsed
        .flags
        .into_iter()
        .map(|(key, value)| {
            let value = match value {
                FlagValue::True => None,
                FlagValue::Value(value) => Some(value),
            };
            (key, value)
        })
        .collect();
    (parsed.positionals, flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equals_values_and_boolean_flags_do_not_consume_focus() {
        let (words, flags) = parse_command_args("--force focus --recent=3 --conversation a,b", &["force"]);
        assert_eq!(words, ["focus"]);
        assert_eq!(flags["force"], None);
        assert_eq!(flags["recent"].as_deref(), Some("3"));
        assert_eq!(flags["conversation"].as_deref(), Some("a,b"));
    }

    #[test]
    fn given_empty_input_when_parsed_then_positionals_and_flags_are_empty() {
        let parsed = parse_command_args_full("   ", ParseCommandArgsOptions::default());
        assert!(parsed.positionals.is_empty());
        assert!(parsed.flags.is_empty());
    }

    #[test]
    fn given_positionals_and_a_valued_flag_when_parsed_then_flag_captures_the_next_token() {
        let parsed = parse_command_args_full("status --recent 3 extra", ParseCommandArgsOptions::default());
        assert_eq!(parsed.positionals, ["status", "extra"]);
        assert_eq!(parsed.flag_value("recent"), Some("3"));
    }

    #[test]
    fn given_an_equals_style_flag_when_parsed_then_the_inline_value_is_used() {
        let parsed = parse_command_args_full("--conversation=abc123 focus", ParseCommandArgsOptions::default());
        assert_eq!(parsed.flag_value("conversation"), Some("abc123"));
        assert_eq!(parsed.positionals, ["focus"]);
    }

    #[test]
    fn given_a_declared_boolean_before_a_positional_when_parsed_then_it_is_not_swallowed() {
        let parsed = parse_command_args_full(
            "--include-hidden cats",
            ParseCommandArgsOptions { booleans: &["include-hidden"] },
        );
        assert!(parsed.flag_is_true("include-hidden"));
        assert_eq!(parsed.positionals, ["cats"]);
    }

    #[test]
    fn given_a_flag_at_the_end_of_input_when_parsed_then_it_degrades_to_a_boolean() {
        let parsed = parse_command_args_full("reset --force", ParseCommandArgsOptions::default());
        assert!(parsed.flag_is_true("force"));
        assert_eq!(parsed.positionals, ["reset"]);
    }
}
