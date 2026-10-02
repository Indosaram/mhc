use std::collections::VecDeque;
use globset::{GlobBuilder, GlobMatcher};
use sha2::{Digest, Sha256};
use super::types::{MatchReason,PatternList,RuleFrontmatter};

pub struct MatcherInput<'a> {pub frontmatter:&'a RuleFrontmatter,pub is_single_file:bool,pub project_relative:&'a str,pub scope_relative:Option<&'a str>,pub basename:&'a str}
#[derive(Debug,PartialEq,Eq)]
pub struct MatchResult {pub matched:bool,pub reason:MatchReason}
#[derive(Debug,PartialEq,Eq)]
pub struct MatcherCacheStats {pub entries:usize,pub compiled_patterns:usize}
struct PatternSet {key:String,positive:Vec<(String,GlobMatcher)>,negative:Vec<GlobMatcher>}
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
impl Matcher {
    pub fn reset_cache(&mut self){self.sets.clear();}
    pub fn cache_stats(&self)->MatcherCacheStats{MatcherCacheStats{entries:self.sets.len(),compiled_patterns:self.sets.iter().map(|set|set.positive.len()+set.negative.len()).sum()}}
    pub fn match_rule(&mut self,input:MatcherInput<'_>)->Result<MatchResult,globset::Error>{
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
                let compiled=GlobBuilder::new(value).literal_separator(false).backslash_escape(false).allow_unclosed_class(true).empty_alternates(true).build()?.compile_matcher();
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
                if matcher.is_match(&base){
                    if set.negative.iter().any(|matcher|matcher.is_match(&base)){return Ok(no_match());}
                    return Ok(MatchResult{matched:true,reason:MatchReason::Glob{pattern:pattern.clone()}});
                }
            }
        }
        Ok(no_match())
    }
}
fn no_match()->MatchResult{MatchResult{matched:false,reason:MatchReason::NoMatch}}
