use std::{collections::BTreeSet,sync::OnceLock};
use crate::types::{TtsrScope,TtsrStreamSource,TtsrToolScope};
pub const ANY_TOOL_NAME:&str="*";
pub fn parse_scope(tokens:&[String])->TtsrScope {
    if tokens.is_empty() { return TtsrScope { allow_text:true,allow_thinking:false,tool_scopes:vec![TtsrToolScope { tool_name:ANY_TOOL_NAME.into(),path_glob:None }] }; }
    static TOKEN:OnceLock<regex::Regex>=OnceLock::new();
    let pattern=TOKEN.get_or_init(||match regex::Regex::new(r"(?i)\A(?:(?P<prefix>tool)(?::(?P<tool>[a-z0-9_-]+))?|(?P<bare>[a-z0-9_-]+))(?:\((?P<path>[^)]+)\))?\z") { Ok(regex)=>regex,Err(error)=>panic!("invalid static scope expression: {error}") });
    let mut scope=TtsrScope { allow_text:false,allow_thinking:false,tool_scopes:vec![] }; let mut seen=BTreeSet::new();
    for raw in tokens {
        let token=maho_ai::utils::js::trim(raw); if token.is_empty() { continue; }
        let normalized=token.to_lowercase();
        match normalized.as_str() { "text"=>{scope.allow_text=true;continue;},"thinking"=>{scope.allow_thinking=true;continue;},_=>{} }
        let tool=if matches!(normalized.as_str(),"tool"|"toolcall") { Some(TtsrToolScope { tool_name:ANY_TOOL_NAME.into(),path_glob:None }) } else { pattern.captures(token).map(|groups|TtsrToolScope { tool_name:groups.name("tool").or_else(||groups.name("bare")).map_or_else(||ANY_TOOL_NAME.into(),|name|name.as_str().to_lowercase()),path_glob:groups.name("path").map(|path|maho_ai::utils::js::trim(path.as_str())).filter(|path|!path.is_empty()).map(str::to_owned) }) };
        if let Some(tool)=tool && seen.insert(format!("{}({})",tool.tool_name,tool.path_glob.as_deref().unwrap_or(""))) { scope.tool_scopes.push(tool); }
    }
    scope
}
pub fn has_reachable_scope(scope:&TtsrScope)->bool { scope.allow_text || scope.allow_thinking || !scope.tool_scopes.is_empty() }
pub(crate) fn glob_compilation_error(pattern:&str)->Option<String> {
    if pattern.is_empty() { return Some("Expected pattern to be a non-empty string".into()); }
    let length=pattern.encode_utf16().count();
    (length>65536).then(||format!("Input length: {length}, exceeds maximum allowed length: 65536"))
}
fn matches_any_path(pattern:&str,paths:Option<&[String]>)->bool {
    if glob_compilation_error(pattern).is_some() { return false; }
    paths.is_some_and(|paths|paths.iter().any(|path| {
        let normalized=path.replace('\\',"/");
        matches_path(pattern,&normalized) || normalized.rsplit_once('/').is_some_and(|(_,basename)|matches_path(pattern,basename))
    }))
}
fn matches_path(pattern:&str,path:&str)->bool {
    if path.is_empty() { return false; }
    if path==pattern { return true; }
    if pattern.starts_with('!') && !pattern.starts_with("!(") {
        let bangs=pattern.chars().take_while(|c|*c=='!').count(); let remaining=&pattern[bangs..];
        let matched=matches_path(remaining,path);
        return if bangs%2==0 { matched } else { !matched };
    }
    if pattern.contains('(') || pattern.contains("..") || pattern.contains('[') || pattern.contains('{') {
        let Some(fragment)=glob_fragment(pattern) else { return false; };
        let pattern=format!("^{fragment}$");
        let Ok(matcher)=regress::Regex::new(&pattern) else { return false; };
        return matcher.find(path).is_some();
    }
    let Ok(glob)=globset::GlobBuilder::new(pattern).literal_separator(true).empty_alternates(true).allow_unclosed_class(true).build() else { return false; }; let matcher=glob.compile_matcher();
    if path=="."||path==".." { return false; }
    matcher.is_match(path) || pattern.contains('*')&&path.strip_suffix('/').is_some_and(|path|matcher.is_match(path))
}
fn glob_fragment(pattern:&str)->Option<String> {
    if let Some(start)=pattern.find('[') && !pattern[start..].starts_with("[[:") && let Some(offset)=pattern[start+1..].find(']') {
        let end=start+1+offset; let body=&pattern[start+1..end];
        let class=format!("[{body}]");
        if regress::Regex::new(&class).is_ok() {
            let class=if !body.starts_with('^')&&!body.contains('-') { format!("(?:{}|{class})",regex::escape(&pattern[start..=end])) } else { class };
            return Some(format!("{}{class}{}",glob_fragment(&pattern[..start])?,glob_fragment(&pattern[end+1..])?));
        }
    }
    if let Some(start)=pattern.find('{') && let Some(offset)=pattern[start+1..].find('}') {
        let end=start+1+offset; let body=&pattern[start+1..end];
        if !body.contains("..")&&!body.contains(',') {
            return Some(format!("{}{}{}",glob_fragment(&pattern[..start])?,regex::escape(&pattern[start..=end]),glob_fragment(&pattern[end+1..])?));
        }
        if body.contains("..")&&!body.contains(',') {
            let mut parts=body.split("..").collect::<Vec<_>>(); parts.sort_unstable();
            let range=format!("[{}]",parts.join("-"));
            let expanded=if regress::Regex::new(&range).is_ok() { range } else { parts.into_iter().map(regex::escape).collect::<Vec<_>>().join("..") };
            return Some(format!("{}{expanded}{}",glob_fragment(&pattern[..start])?,glob_fragment(&pattern[end+1..])?));
        }
    }
    for (name,source) in [("alnum","a-zA-Z0-9"),("alpha","a-zA-Z"),("ascii",r"\x00-\x7F"),("blank",r" \t"),("cntrl",r"\x00-\x1F\x7F"),("digit","0-9"),("graph",r"\x21-\x7E"),("lower","a-z"),("print",r"\x20-\x7E "),("space",r" \t\r\n\v\f"),("upper","A-Z"),("word","A-Za-z0-9_"),("xdigit","A-Fa-f0-9")] {
        let token=format!("[[:{name}:]]");
        if let Some(start)=pattern.find(&token) {
            return Some(format!("{}[{source}]{}",glob_fragment(&pattern[..start])?,glob_fragment(&pattern[start+token.len()..])?));
        }
    }
    if let Some((start,operator,end))=extglob(pattern) {
        let body=&pattern[start+2..end]; let suffix=&pattern[end+1..];
        let mut alternatives=Vec::new(); let mut depth=0; let mut part=0; let mut escaped=false;
        for (index,value) in body.char_indices() {
            if escaped { escaped=false; continue; }
            match value {
                '\\'=>escaped=true,'('=>depth+=1,')'=>depth-=1,
                '|' if depth==0=>{ alternatives.push(glob_fragment(&body[part..index])?); part=index+1; },_=>(),
            }
        }
        alternatives.push(glob_fragment(&body[part..])?);
        let alternatives=alternatives.join("|");
        let group=match operator {
            '@'=>format!("(?:{alternatives})"),'+'=>format!("(?:{alternatives})+"),
            '?'=>format!("(?:{alternatives})?"),'*'=>format!("(?:{alternatives})*"),
            '!'=>format!("(?:(?!(?:{alternatives}){})[^/]*?)",if suffix.is_empty() { "$" } else { "" }),
            _=>unreachable!(),
        };
        return Some(format!("{}{group}{}",glob_fragment(&pattern[..start])?,glob_fragment(suffix)?));
    }
    if let Some(start)=pattern.find('(') {
        let mut depth=1; let mut end=None;
        for (offset,value) in pattern[start+1..].char_indices() {
            match value { '('=>depth+=1,')'=>{ depth-=1; if depth==0 { end=Some(start+1+offset); break; } },_=>() }
        }
        if let Some(end)=end {
            let body=&pattern[start+1..end]; let mut depth=0; let mut part=0; let mut alternatives=Vec::new();
            for (index,value) in body.char_indices() {
                match value { '('=>depth+=1,')'=>depth-=1,'|' if depth==0=>{ alternatives.push(glob_fragment(&body[part..index])?); part=index+1; },_=>() }
            }
            alternatives.push(glob_fragment(&body[part..])?);
            let suffix=&pattern[end+1..]; let (quantifier,suffix)=if suffix.starts_with('+')||suffix.starts_with('?') { (&suffix[..1],&suffix[1..]) } else { ("",suffix) };
            return Some(format!("{}(?:{}){quantifier}{}",glob_fragment(&pattern[..start])?,alternatives.join("|"),glob_fragment(suffix)?));
        }
    }
    let glob=globset::GlobBuilder::new(pattern).literal_separator(true).empty_alternates(true).allow_unclosed_class(true).build().ok()?;
    Some(glob.regex().strip_prefix("(?-u)^")?.strip_suffix('$')?.into())
}
fn extglob(pattern:&str)->Option<(usize,char,usize)> {
    let bytes=pattern.as_bytes(); let mut escaped=false; let mut in_class=false;
    for (start,operator) in pattern.char_indices() {
        if escaped { escaped=false; continue; }
        if operator=='\\' { escaped=true; continue; }
        if operator=='[' { in_class=true; } else if operator==']' { in_class=false; }
        if in_class { continue; }
        if matches!(operator,'@'|'+'|'?'|'*'|'!')&&bytes.get(start+1)==Some(&b'(') {
            let mut depth=1; let mut escaped=false;
            for (offset,value) in pattern[start+2..].char_indices() {
                if escaped { escaped=false; continue; }
                match value { '\\'=>escaped=true,'('=>depth+=1,')'=>{ depth-=1; if depth==0 { return Some((start,operator,start+2+offset)); } },_=>() }
            }
        }
    }
    None
}
pub fn matches_scope(scope:&TtsrScope,source:TtsrStreamSource,tool_name:Option<&str>,paths:Option<&[String]>)->bool {
    match source { TtsrStreamSource::Text=>scope.allow_text,TtsrStreamSource::Thinking=>scope.allow_thinking,TtsrStreamSource::Tool=>{ let name=tool_name.map(|name|maho_ai::utils::js::trim(name).to_lowercase()); scope.tool_scopes.iter().any(|tool| (tool.tool_name==ANY_TOOL_NAME || Some(tool.tool_name.to_lowercase())==name) && tool.path_glob.as_ref().is_none_or(|glob|matches_any_path(glob,paths))) } }
}
pub fn matches_path_globs(globs:&[String],paths:Option<&[String]>)->bool { globs.is_empty() || globs.iter().any(|glob|matches_any_path(glob,paths)) }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn picomatch_plain_group_matrix() {
        for (pattern,path,expected) in [("(a|b).rs","a.rs",true),("(a|b).rs","b.rs",true),("(a|b).rs","c.rs",false),("(a).rs","a.rs",true),("a(b|c).rs","ac.rs",true),("(a|b)*.rs","banana.rs",true)] {
            assert_eq!(matches_path_globs(&[pattern.into()],Some(&[path.into()])),expected,"{pattern}: {path}");
        }
    }
    #[test] fn picomatch_bracket_literal_and_negation_matrix() {
        for (pattern,path,expected) in [("[abc].rs","[abc].rs",true),("[abc].rs","a.rs",true),("[!a].rs","b.rs",false),("[!a].rs","!.rs",true),("[^a].rs","b.rs",true),("{abc}.rs","abc.rs",false),("{abc}.rs","{abc}.rs",true)] {
            assert_eq!(matches_path_globs(&[pattern.into()],Some(&[path.into()])),expected,"{pattern}: {path}");
        }
    }
    #[test] fn picomatch_range_posix_and_separator_matrix() {
        for (pattern,path,expected) in [("{1..3}.rs","2.rs",true),("{a..c}.rs","b.rs",true),("[[:digit:]].rs","2.rs",true),("*.rs","a.rs/",true),("*",".",false),("*","..",false),("**/*.rs","a/../b.rs",true),("**/*.rs","src/.hidden.rs",true)] {
            assert_eq!(matches_path_globs(&[pattern.into()],Some(&[path.into()])),expected,"{pattern}: {path}");
        }
    }
    #[test] fn picomatch_literal_extglob_and_negated_path_matrix() {
        for (pattern,path,expected) in [("@(a|b).rs","a.rs",true),("@(a|b).rs","c.rs",false),("+(a|b).rs","aba.rs",true),("?(a|b).rs",".rs",true),("*(a|b).rs","aba.rs",true),("!(a|b).rs","aa.rs",false),("!(a|b).rs","c.rs",true),("!*.rs","a.rs",false),("!*.rs","src/a.rs",true),("!!*.rs","src/a.rs",true),("@(a*|b?).rs","abc.rs",true),("a@(b|c)*.rs","abzz.rs",true),("[abc","[abc",true)] {
            assert_eq!(matches_path_globs(&[pattern.into()],Some(&[path.into()])),expected,"{pattern}: {path}");
        }
    }
    #[test] fn picomatch_nested_multiple_and_terminal_negative_extglobs() {
        for (pattern,path,expected) in [("@(a|@(b|c)).rs","c.rs",true),("@(a|b)@(c|d).rs","bd.rs",true),("!(a|b)","aa",true),("!(a|b)","a",false),("!(a|b@(c|d)).rs","bc.rs",false),("!(a|b@(c|d)).rs","e.rs",true)] {
            assert_eq!(matches_path_globs(&[pattern.into()],Some(&[path.into()])),expected,"{pattern}: {path}");
        }
    }
    #[test] fn upstream_keywords_are_case_insensitive_and_toolcall_is_wildcard() {
        let scope=parse_scope(&["TEXT".into()," Thinking ".into()]); assert!(scope.allow_text); assert!(scope.allow_thinking); assert!(scope.tool_scopes.is_empty());
        assert_eq!(parse_scope(&["toolcall".into()]).tool_scopes,[TtsrToolScope { tool_name:"*".into(),path_glob:None }]);
    }
    #[test] fn upstream_optional_tool_glob_tokens_preserve_order() {
        assert_eq!(parse_scope(&["tool:Edit(*.ts)".into(),"write".into(),"tool(*.md)".into()]).tool_scopes,[TtsrToolScope { tool_name:"edit".into(),path_glob:Some("*.ts".into()) },TtsrToolScope { tool_name:"write".into(),path_glob:None },TtsrToolScope { tool_name:"*".into(),path_glob:Some("*.md".into()) }]);
    }
    #[test] fn default_allows_text_and_any_tool() { let scope=parse_scope(&[]); let result=(matches_scope(&scope,TtsrStreamSource::Text,None,None),matches_scope(&scope,TtsrStreamSource::Tool,Some("edit"),None),matches_scope(&scope,TtsrStreamSource::Thinking,None,None)); assert_eq!(result,(true,true,false)); }
    #[test] fn scope_deduplicates_tools() { let scope=parse_scope(&["tool:edit(*.rs)".into(),"EDIT(*.rs)".into(),"thinking".into()]); assert_eq!(scope.tool_scopes.len(),1); assert!(scope.allow_thinking); }
    #[test] fn basename_matches_and_backslashes_normalize() { let scope=parse_scope(&["tool:edit(*.rs)".into()]); let result=matches_scope(&scope,TtsrStreamSource::Tool,Some("EDIT"),Some(&["C:\\src\\lib.rs".into()])); assert!(result); }
    #[test] fn scoped_glob_requires_paths() { let scope=parse_scope(&["tool:edit(*.rs)".into()]); let result=matches_scope(&scope,TtsrStreamSource::Tool,Some("edit"),None); assert!(!result); }
    #[test] fn invalid_tokens_leave_unreachable_scope() { let scope=parse_scope(&["???".into()]); let result=has_reachable_scope(&scope); assert!(!result); }
    #[test] fn empty_path_globs_match_without_paths() { let result=matches_path_globs(&[],None); assert!(result); }
    #[test] fn dotfiles_are_matched() { let result=matches_path_globs(&["*.rs".into()],Some(&[".hidden.rs".into()])); assert!(result); }
    #[test] fn scope_and_tool_names_trim_ecmascript_bom_not_next_line() {
        let scope=parse_scope(&["\u{feff}text\u{feff}".into(),"\u{feff}tool:edit(\u{feff}*.rs\u{feff})\u{feff}".into()]);
        assert!(scope.allow_text); assert_eq!(scope.tool_scopes[0].path_glob.as_deref(),Some("*.rs"));
        assert!(matches_scope(&scope,TtsrStreamSource::Tool,Some("\u{feff}EDIT\u{feff}"),Some(&["src/lib.rs".into()])));
        assert!(!has_reachable_scope(&parse_scope(&["\u{0085}text\u{0085}".into()])));
    }
}
