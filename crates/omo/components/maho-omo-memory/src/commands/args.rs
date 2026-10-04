use std::collections::BTreeMap;

pub fn parse_command_args(args: &str, booleans: &[&str]) -> (Vec<String>, BTreeMap<String, Option<String>>) {
    let mut positionals = Vec::new(); let mut flags = BTreeMap::new();
    let mut tokens = args.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        let Some(body) = token.strip_prefix("--") else { positionals.push(token.into()); continue; };
        if let Some((key, value)) = body.split_once('=') { flags.insert(key.into(), Some(value.into())); }
        else if !booleans.contains(&body) && tokens.peek().is_some_and(|token| !token.starts_with("--")) {
            flags.insert(body.into(), tokens.next().map(str::to_owned));
        } else { flags.insert(body.into(), None); }
    }
    (positionals, flags)
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn equals_values_and_boolean_flags_do_not_consume_focus() {
        let (words, flags) = parse_command_args("--force focus --recent=3 --conversation a,b", &["force"]);
        assert_eq!(words, ["focus"]); assert_eq!(flags["force"], None);
        assert_eq!(flags["recent"].as_deref(), Some("3")); assert_eq!(flags["conversation"].as_deref(), Some("a,b"));
    }
}
