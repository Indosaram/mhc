pub struct CompiledCondition { pub regex:Option<fancy_regex::Regex>,pub flags:String,pub warning:Option<String> }
pub fn compile_rule_condition(condition:&str)->CompiledCondition {
    let mut flags=String::new(); let mut pattern=condition.to_owned();
    if let Some(rest)=condition.strip_prefix("(?") && let Some(end)=rest.find(')') {
        let prefix=&rest[..end];
        if !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_lowercase()) {
            if !prefix.bytes().all(|b| matches!(b,b'i'|b'm'|b's')) { return CompiledCondition { regex:None,flags,warning:Some(format!("invalid condition \"{condition}\": unsupported inline flags")) }; }
            for flag in prefix.chars() { if !flags.contains(flag) { flags.push(flag); } }
            pattern=format!("(?{flags}){}",&rest[end+1..]);
        }
    }
    match fancy_regex::Regex::new(&pattern) {
        Ok(regex)=>CompiledCondition { regex:Some(regex),flags,warning:None },
        Err(error)=>CompiledCondition { regex:None,flags,warning:Some(format!("invalid condition \"{condition}\": {error}")) },
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn leading_case_flag_is_translated() { let result=compile_rule_condition("(?i)pre.existing"); assert_eq!(result.flags,"i"); assert!(result.regex.unwrap().is_match("These are Pre-existing failures").unwrap()); }
    #[test] fn unflagged_pattern_remains_case_sensitive() { let result=compile_rule_condition("pre.existing"); let regex=result.regex.unwrap(); assert!(regex.is_match("pre-existing").unwrap()); assert!(!regex.is_match("PRE-EXISTING").unwrap()); }
    #[test] fn noncapturing_group_is_preserved() { let result=compile_rule_condition("foo(?:bar)"); assert!(result.regex.unwrap().is_match("foobar").unwrap()); }
    #[test] fn combined_flags_enable_multiline_case_matching() { let result=compile_rule_condition("(?im)^fail"); assert_eq!(result.flags,"im"); assert!(result.regex.unwrap().is_match("ok\nFAIL here").unwrap()); }
    #[test] fn unsupported_inline_flags_are_rejected() { let result=compile_rule_condition("(?x)broken"); assert!(result.regex.is_none()); assert!(result.warning.is_some()); }
    #[test] fn malformed_expression_is_rejected() { let result=compile_rule_condition("(unclosed"); assert!(result.regex.is_none()); assert!(result.warning.is_some()); }
}
