pub const REGION_START:&str="<!--senpi:project-rules:1:start-->";
pub const REGION_END:&str="<!--senpi:project-rules:1:end-->";
// PERF-DIVERGENCE(omo anthropic-subscription prompt-append.ts): upstream builds these config-dependent regexes per call; we memoize the three compiled regexes per config directory. Matches are identical.
fn config_regexes(config:&str)->Option<std::sync::Arc<[regex::Regex;3]>> {
    static CACHE:std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String,std::sync::Arc<[regex::Regex;3]>>>>=std::sync::LazyLock::new(Default::default);
    let mut cache=CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found)=cache.get(config) {return Some(found.clone());}
    let built=std::sync::Arc::new([regex::Regex::new(&format!(r"(?i)~/{config}\b")).ok()?,regex::Regex::new(&format!(r#"(^|[\s'"`])\.{config}/"#)).ok()?,regex::Regex::new(&format!(r"(?i)\b{config}\b")).ok()?]);
    cache.insert(config.to_owned(),built.clone());Some(built)
}
pub fn agents(cwd:&std::path::Path,agent_dir:&std::path::Path,config_directory:&str)->Option<String> {
    let mut current=std::path::absolute(cwd).ok()?;
    let path=loop {let candidate=current.join("AGENTS.md");if candidate.exists() {break candidate;}
        if !current.pop() {break agent_dir.join("AGENTS.md");}};
    let content=std::fs::read_to_string(path).ok()?;let config=regex::escape(config_directory.trim_start_matches('.'));let regexes=config_regexes(&config)?;let [home,dot,bare]=&*regexes;let content=home.replace_all(content.trim(),"~/.claude");let content=dot.replace_all(&content,"${1}.claude/");let content=bare.replace_all(&content,"environment").into_owned();if content.is_empty() {None}else {Some(format!("# CLAUDE.md\n\n{content}"))}
}
pub fn project_rules(prompt:Option<&str>)->Option<&str> {
    let prompt=prompt?;let mut search=0;
    while search<=prompt.len() {
        let start=search+prompt[search..].find(REGION_START)?+REGION_START.len();let end=start+prompt[start..].find(REGION_END)?;let region=prompt[start..end].trim();
        if region.starts_with("<project_rules>\n## Project Instructions\n")&&region.ends_with("\n</project_rules>") {return Some(region);}search=start;
    }None
}
pub fn skills(prompt:Option<&str>,global_root:&std::path::Path,project_root:&std::path::Path)->Option<String> {
    let prompt=prompt?;let start=prompt.find("The following skills provide specialized instructions for specific tasks.")?;let end=start+prompt[start..].find("</available_skills>")?+"</available_skills>".len();let block=prompt[start..end].trim();
    static LOCATION:std::sync::LazyLock<regex::Regex>=std::sync::LazyLock::new(||regex::Regex::new(r"<location>([^<]+)</location>").expect("location regex"));let regex=&*LOCATION;Some(regex.replace_all(block,|capture:&regex::Captures<'_>| {
        let location=&capture[1];for (root,prefix) in [(global_root,"~/.claude/skills/"),(project_root,".claude/skills/")] {
            let root=root.to_string_lossy();if location.starts_with(root.as_ref()) {let suffix=location[root.len()..].trim_start_matches('/').trim_start_matches('.');return format!("<location>{prefix}{suffix}</location>");}
        }capture[0].to_owned()
    }).into_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_agents_wins_and_empty_file_does_not_fall_back() {
        let directory=tempfile::tempdir().expect("dir");let global=directory.path().join("global");let project=directory.path().join("project");let child=project.join("child");std::fs::create_dir_all(&global).expect("global");std::fs::create_dir_all(&child).expect("child");std::fs::write(global.join("AGENTS.md"),"global").expect("global file");std::fs::write(project.join("AGENTS.md")," ~/omo/skills ~/.omo/skills .omo/file omo ").expect("project file");assert_eq!(agents(&child,&global,".omo").expect("agents"),"# CLAUDE.md\n\n~/.claude/skills ~/.environment/skills .claude/file environment");std::fs::write(child.join("AGENTS.md"),"  ").expect("empty");assert!(agents(&child,&global,".omo").is_none());
    }
    #[test]
    fn reserved_region_rejects_false_candidate_and_unterminated_region() {
        let valid="<project_rules>\n## Project Instructions\nrule\n</project_rules>";let prompt=format!("{REGION_START}invalid{REGION_END} unrelated {REGION_START}{valid}{REGION_END} tail");assert_eq!(project_rules(Some(&prompt)),Some(valid));assert_eq!(project_rules(Some(&format!("{REGION_START}{valid}"))),None);
    }
    #[test]
    fn skill_paths_rewrite_only_known_roots_and_require_end_marker() {
        let prompt="The following skills provide specialized instructions for specific tasks.\n<available_skills><location>/global/skills/test</location><location>/project/.senpi/skills/local</location><location>/other/test</location></available_skills>tail";let result=skills(Some(prompt),std::path::Path::new("/global/skills"),std::path::Path::new("/project/.senpi/skills")).expect("skills");assert!(result.contains("<location>~/.claude/skills/test</location>"));assert!(result.contains("<location>.claude/skills/local</location>"));assert!(result.contains("<location>/other/test</location>"));assert!(!result.ends_with("tail"));assert!(skills(Some("The following skills provide specialized instructions for specific tasks."),std::path::Path::new("/global"),std::path::Path::new("/project")).is_none());
    }
}
