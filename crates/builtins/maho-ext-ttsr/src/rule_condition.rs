pub struct CompiledCondition { pub regex:Option<regress::Regex>,pub flags:String,pub warning:Option<String> }
pub fn compile_rule_condition(condition:&str)->CompiledCondition {
    let mut flags=String::new(); let mut pattern=condition.to_owned();
    if let Some(rest)=condition.strip_prefix("(?") && let Some(end)=rest.find(')') {
        let prefix=&rest[..end];
        if !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_lowercase()) {
            if !prefix.bytes().all(|b| matches!(b,b'i'|b'm'|b's')) { return CompiledCondition { regex:None,flags,warning:Some(format!("invalid condition \"{condition}\": unsupported inline flags")) }; }
            for flag in prefix.chars() { if !flags.contains(flag) { flags.push(flag); } }
            pattern=rest[end+1..].to_owned();
        }
    }
    match regress::Regex::from_unicode(pattern.encode_utf16().map(u32::from),flags.as_str()) {
        Ok(regex)=>CompiledCondition { regex:Some(regex),flags,warning:None },
        Err(error)=>CompiledCondition { regex:None,flags,warning:Some(format!("invalid condition \"{condition}\": {error}")) },
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn leading_case_flag_is_translated() { let result=compile_rule_condition("(?i)pre.existing"); assert_eq!(result.flags,"i"); assert!(result.regex.unwrap().find("These are Pre-existing failures").is_some()); }
    #[test] fn unflagged_pattern_remains_case_sensitive() { let result=compile_rule_condition("pre.existing"); let regex=result.regex.unwrap(); assert!(regex.find("pre-existing").is_some()); assert!(regex.find("PRE-EXISTING").is_none()); }
    #[test] fn noncapturing_group_is_preserved() { let result=compile_rule_condition("foo(?:bar)"); assert!(result.regex.unwrap().find("foobar").is_some()); }
    #[test] fn combined_flags_enable_multiline_case_matching() { let result=compile_rule_condition("(?im)^fail"); assert_eq!(result.flags,"im"); assert!(result.regex.unwrap().find("ok\nFAIL here").is_some()); }
    #[test] fn unsupported_inline_flags_are_rejected() { let result=compile_rule_condition("(?x)broken"); assert!(result.regex.is_none()); assert!(result.warning.is_some()); }
    #[test] fn malformed_expression_is_rejected() { let result=compile_rule_condition("(unclosed"); assert!(result.regex.is_none()); assert!(result.warning.is_some()); }
    #[test] fn ecmascript_condition_matrix_matches_nonunicode_javascript() {
        for (pattern,text,expected) in [(r"^\w+$","é",false),(r"^\d+$","١",false),(r"^\s$","\u{feff}",true),(r"^\s$","\u{0085}",false),("^.$","😀",false),("^..$","😀",true),(r"^\q$","q",true),(r"^(a|(b))\2$","a",true),(r"(?<=a)b","ab",true)] {
            let regex=compile_rule_condition(pattern).regex.unwrap(); let units=text.encode_utf16().collect::<Vec<_>>(); assert_eq!(regex.find_from_ucs2(&units,0).next().is_some(),expected,"{pattern} against {text}");
        }
    }
}
