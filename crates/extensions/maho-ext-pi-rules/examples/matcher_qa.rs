use maho_ext_pi_rules::rules::{matcher::{Matcher,MatcherInput},types::{PatternList,RuleFrontmatter}};

fn main()->Result<(),Box<dyn std::error::Error>>{
    let cases:serde_json::Value=serde_json::from_str(include_str!("../tests/fixtures/pinned-range-punct.json"))?;
    let mut matcher=Matcher::default();
    for case in cases.as_array().ok_or("expected cases")?{
        let pattern=case["pattern"].as_str().ok_or("pattern")?;
        let path=case["path"].as_str().ok_or("path")?;
        let frontmatter=RuleFrontmatter{globs:Some(PatternList::Single(pattern.into())),..Default::default()};
        let actual=matcher.match_rule(MatcherInput{frontmatter:&frontmatter,is_single_file:false,project_relative:path,scope_relative:None,basename:path})?;
        if actual.matched!=case["matched"].as_bool().ok_or("matched")?{return Err(format!("Mismatch {pattern}: {path}").into());}
    }
    println!("PASS pinned matcher corpus: {} cases; no tasks, sockets or temporary stores",cases.as_array().ok_or("expected cases")?.len());
    Ok(())
}
