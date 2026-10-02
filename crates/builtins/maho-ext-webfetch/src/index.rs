pub fn parse_enable_env(value:Option<&str>)->bool {
    let Some(value)=value.filter(|value|!value.is_empty()) else { return true; };
    !matches!(value.trim_matches(|character:char|character.is_whitespace() || character=='\u{feff}').to_lowercase().as_str(),"0"|"false"|"no"|"off")
}
pub fn is_webfetch_enabled()->bool { parse_enable_env(std::env::var("PI_WEBFETCH").ok().as_deref()) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn disables_only_explicit_false_values() { for value in ["0"," FALSE ","no","\u{feff}off\u{feff}"] { assert!(!parse_enable_env(Some(value))); } for value in [None,Some(""),Some("yes"),Some("unknown"),Some(" ")] { assert!(parse_enable_env(value)); } }
}
