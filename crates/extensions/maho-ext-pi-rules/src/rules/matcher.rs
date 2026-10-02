use std::collections::VecDeque;
use globset::GlobBuilder;
use sha2::{Digest, Sha256};
use super::types::{MatchReason,PatternList,RuleFrontmatter};

pub struct MatcherInput<'a> {pub frontmatter:&'a RuleFrontmatter,pub is_single_file:bool,pub project_relative:&'a str,pub scope_relative:Option<&'a str>,pub basename:&'a str}
#[derive(Debug,PartialEq,Eq)]
pub struct MatchResult {pub matched:bool,pub reason:MatchReason}
#[derive(Debug,PartialEq,Eq)]
pub struct MatcherCacheStats {pub entries:usize,pub compiled_patterns:usize}
struct PatternSet {key:String,positive:Vec<(String,PathMatcher)>,negative:Vec<PathMatcher>}
enum PathMatcher{Glob(globset::GlobMatcher),Expression(fancy_regex::Regex)}
impl PathMatcher{
    fn is_match(&self,path:&str)->Result<bool,MatcherError>{match self{Self::Glob(matcher)=>Ok(matcher.is_match(path)),Self::Expression(matcher)=>matcher.is_match(path).map_err(|error|MatcherError::Expression(Box::new(error)))}}
}
#[derive(Debug)]
pub enum MatcherError{Glob(globset::Error),Expression(Box<fancy_regex::Error>)}
impl From<globset::Error> for MatcherError{fn from(error:globset::Error)->Self{Self::Glob(error)}}
impl std::fmt::Display for MatcherError{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{match self{Self::Glob(error)=>error.fmt(f),Self::Expression(error)=>error.fmt(f)}}}
impl std::error::Error for MatcherError{}
#[derive(Default)]
pub struct Matcher {sets:VecDeque<PatternSet>}
pub fn normalize_globs(frontmatter:&RuleFrontmatter)->Vec<String>{
    let mut result=Vec::new();
    for patterns in [&frontmatter.globs,&frontmatter.paths,&frontmatter.apply_to].into_iter().flatten(){
        let patterns=match patterns{PatternList::Single(value)=>std::slice::from_ref(value),PatternList::Multiple(values)=>values.as_slice()};
        for pattern in patterns{let pattern=pattern.replace('\\',"/");if !result.contains(&pattern){result.push(pattern);}}
    }
    result
}
pub fn hash_content(body:&str)->String{format!("{:x}",Sha256::digest(body.as_bytes()))}
fn normalize_literal_braces(pattern:&str)->String{
    let mut stack:Vec<(usize,bool)>=Vec::new();let mut literal=Vec::new();let mut ranges=Vec::new();let mut in_class=false;
    for (index,value) in pattern.char_indices(){match value{
        '['=>in_class=true,']'=>in_class=false,
        '{' if !in_class=>stack.push((index,false)),
        ',' if !in_class=>{if let Some((_,alternate))=stack.last_mut(){*alternate=true;}},
        '}' if !in_class=>{if let Some((start,alternate))=stack.pop(){let inner=&pattern[start+1..index];if !alternate&&!inner.contains(".."){literal.extend([start,index]);}else if !alternate{let mut bounds=inner.split("..").collect::<Vec<_>>();bounds.sort_unstable();let expression=format!("[{}]",bounds.join("-"));let replacement=if fancy_regex::Regex::new(&expression).is_ok(){expression}else{bounds.join("..")};ranges.push((start,index,replacement));}}else{literal.push(index);}},
        _=>{}
    }}
    literal.extend(stack.into_iter().map(|(index,_)|index));
    let mut result=String::new();let mut skip_until=0;for (index,value) in pattern.char_indices(){if index<skip_until{continue;}
        if let Some((_,end,replacement))=ranges.iter().find(|(start,_,_)|*start==index){result.push_str(replacement);skip_until=end+1;}else if literal.contains(&index){result.push('[');result.push(value);result.push(']');}else{result.push(value);}}result
}
impl Matcher {
    pub fn reset_cache(&mut self){self.sets.clear();}
    pub fn cache_stats(&self)->MatcherCacheStats{MatcherCacheStats{entries:self.sets.len(),compiled_patterns:self.sets.iter().map(|set|set.positive.len()+set.negative.len()).sum()}}
    pub fn match_rule(&mut self,input:MatcherInput<'_>)->Result<MatchResult,MatcherError>{
        if input.is_single_file{return Ok(MatchResult{matched:true,reason:MatchReason::SingleFile});}
        if input.frontmatter.always_apply==Some(true){return Ok(MatchResult{matched:true,reason:MatchReason::AlwaysApply});}
        let patterns=normalize_globs(input.frontmatter);
        if patterns.is_empty(){return Ok(no_match());}
        let key=patterns.join("\0");
        let set=if let Some(index)=self.sets.iter().position(|set|set.key==key){
            self.sets.remove(index).unwrap_or_else(||unreachable!("position belongs to this deque"))
        }else{
            let mut set=PatternSet{key,positive:Vec::new(),negative:Vec::new()};
            for pattern in patterns{
                let negated=pattern.starts_with('!');
                let value=pattern.strip_prefix('!').unwrap_or(&pattern);
                let normalized=normalize_literal_braces(value);
                let compiled=if value.contains('(')||value.contains(')'){PathMatcher::Expression(fancy_regex::Regex::new(&format!("^(?:{})$",compile_expression(&normalized))).map_err(|error|MatcherError::Expression(Box::new(error)))?)}else{PathMatcher::Glob(GlobBuilder::new(&normalized).literal_separator(false).backslash_escape(false).allow_unclosed_class(true).empty_alternates(true).build()?.compile_matcher())};
                if negated{set.negative.push(compiled);}else{set.positive.push((pattern,compiled));}
            }
            if self.sets.len()>=256{self.sets.pop_front();}
            set
        };
        self.sets.push_back(set);
        let set=self.sets.back().unwrap_or_else(||unreachable!("set was just inserted"));
        let bases=[Some(input.project_relative),input.scope_relative.filter(|v|!v.is_empty()),Some(input.basename)];
        for (pattern,matcher) in &set.positive{
            for base in bases.into_iter().flatten(){
                let base=base.replace('\\',"/");
                if matcher.is_match(&base)?{
                    for matcher in &set.negative{if matcher.is_match(&base)?{return Ok(no_match());}}
                    return Ok(MatchResult{matched:true,reason:MatchReason::Glob{pattern:pattern.clone()}});
                }
            }
        }
        Ok(no_match())
    }
}
fn no_match()->MatchResult{MatchResult{matched:false,reason:MatchReason::NoMatch}}
fn compile_expression(pattern:&str)->String{
    let chars=pattern.chars().collect::<Vec<_>>();let mut result=String::new();let mut index=0;let mut parens=0;
    while index<chars.len(){let ch=chars[index];
        if matches!(ch,'@'|'+'|'?'|'*'|'!')&&chars.get(index+1)==Some(&'('){
            let mut depth=1;let mut end=index+2;while end<chars.len(){if chars[end]=='(' {depth+=1;}else if chars[end]==')'{depth-=1;if depth==0{break;}}end+=1;}
            if depth==0{let body=chars[index+2..end].iter().collect::<String>();if matches!(ch,'+'|'*')&&risky_simple_repeat(&body){for literal in &chars[index..=end]{if ".*+?()[]{}|^$\\".contains(*literal){result.push('\\');}result.push(*literal);}index=end+1;continue;}let inner=compile_expression(&body);let quantifier=match ch{'?'=>"?",'+'=>"+",'*'=>"*",_=>""};if index==0&&ch!='@'{result.push_str("(?=.)");}
                if ch=='!'{let rest=chars[end+1..].iter().collect::<String>();let suffix=if body.contains('*')&&rest.starts_with('.')&&!rest[1..].contains(['/', '\\', '.']){compile_expression(&rest)}else if rest.is_empty(){"$".into()}else{String::new()};let star=if rest.is_empty()||body.contains('/') {"(?:(?!(?:^|/)\\.{1,2}(?:/|$)).)*?"}else{"[^/]*?"};result.push_str(&format!("(?:(?!(?:{inner}){suffix}){star})"));}else{result.push_str(&format!("(?:{inner}){quantifier}"));}index=end+1;continue;}
            result.push_str("\\(");index+=2;continue;
        }
        match ch{
            '*'=>{while chars.get(index+1)==Some(&'*'){index+=1;}if chars.get(index+1)==Some(&'/'){result.push_str("(?:.*/)?");index+=1;}else{result.push_str(".*");}},
            '?'=>result.push_str("[^/]"),
            '('=>{let mut depth=1;let mut end=index+1;while end<chars.len(){if chars[end]=='(' {depth+=1;}else if chars[end]==')'{depth-=1;if depth==0{break;}}end+=1;}if depth==0{parens+=1;result.push('(');}else{result.push_str("\\(");}},
            ')'=>if parens>0{parens-=1;result.push(')');}else{result.push_str("\\)");},
            '|'=>result.push('|'),
            '{'=>{let mut depth=1;let mut end=index+1;while end<chars.len(){if chars[end]=='{'{depth+=1;}else if chars[end]=='}'{depth-=1;if depth==0{break;}}end+=1;}if depth==0{let body=chars[index+1..end].iter().collect::<String>();let mut nesting=0;let mut start=0;let mut branches=Vec::new();for (position,ch) in body.char_indices(){match ch{'{'|'('|'['=>nesting+=1,'}'|')'|']'=>nesting-=1,',' if nesting==0=>{branches.push(compile_expression(&body[start..position]));start=position+1;},_=>{}}}branches.push(compile_expression(&body[start..]));result.push_str(&format!("(?:{})",branches.join("|")));index=end;}else{result.push_str("\\{");}},
            '['=>{if let Some(end)=chars[index+1..].iter().position(|ch|*ch==']'){let end=index+1+end;let body=chars[index+1..end].iter().collect::<String>();let class=if body.starts_with('^')&&!body.contains('/'){format!("[{body}/]")}else{format!("[{body}]")};if body.chars().any(|ch|"-*+?.^${}(|)[]".contains(ch)){result.push_str(&class);}else{result.push_str(&format!("(?:\\[{body}\\]|{class})"));}index=end;if end+1==chars.len(){result.push_str("/?");}}else{result.push_str("\\[");}},
            '.'|'+'|'$'|'^'|'}'|'\\'=>{result.push('\\');result.push(ch);},
            _=>result.push(ch)
        }index+=1;
    }result
}
fn risky_simple_repeat(body:&str)->bool{
    let mut branches=Vec::new();let mut depth=0;let mut start=0;
    for (index,ch) in body.char_indices(){match ch{'('| '['=>depth+=1,')'|']'=>depth-=1,'|' if depth==0=>{branches.push(body[start..index].trim());start=index+1;},_=>{}}}
    branches.push(body[start..].trim());
    if branches.len()>1&&branches.iter().any(|branch|branch.is_empty()||branch.chars().all(|ch|matches!(ch,'*'|'?'))){return true;}
    let normalized=branches.iter().map(|branch|branch.strip_prefix("@(").and_then(|value|value.strip_suffix(')')).unwrap_or(branch)).collect::<Vec<_>>();
    if normalized.iter().enumerate().any(|(index,a)|normalized[index+1..].iter().any(|b|a.chars().next().is_some_and(|first|a.chars().all(|ch|ch==first)&&b.chars().all(|ch|ch==first)))){return true;}
    let mut saw_star=false;let mut combinable=true;
    for branch in branches{
        let mut rest=branch;let mut stars=0;
        while let Some(after)=rest.strip_prefix("*("){
            let Some(end)=after.find(')')else{break;};
            if after[..end].chars().count()!=1||after[..end].contains(['*','?','+','@','!','(','[',']','{','}','|']){break;}
            stars+=1;rest=&after[end+1..];
        }
        if stars>0&&rest.is_empty(){saw_star=true;continue;}
        if branch.starts_with("+(")||branch.starts_with("*("){return true;}
        if branch.chars().count()!=1{combinable=false;}
    }
    saw_star&&!combinable
}
