use std::{collections::{BTreeMap,BTreeSet},path::PathBuf};
use serde_json::Value;
#[derive(Clone)]
pub struct SkillLike {pub name:String,pub file_path:PathBuf,pub base_dir:PathBuf}
pub struct SkillServerDecl {pub raw:Value,pub source_path:PathBuf,pub include_tools_by_skill:BTreeMap<String,Vec<String>>}
#[derive(Default)]
pub struct SkillMcpDeclarations {pub servers:BTreeMap<String,SkillServerDecl>,pub warnings:Vec<String>}
pub fn parse_skill_mcp_declarations(skills:&[SkillLike])->SkillMcpDeclarations {
    let mut declarations=SkillMcpDeclarations::default();
    for skill in skills {
        let sidecar=skill.base_dir.join("mcp.json");
        let sidecar_exists=sidecar.exists();
        let source=if sidecar_exists{sidecar}else{skill.file_path.clone()};
        let read=(||->Result<Value,String> {
            let text=std::fs::read_to_string(&source).map_err(|e|e.to_string())?;
            if sidecar_exists {serde_json::from_str(&text).map_err(|e|e.to_string())}
            else {Ok(maho_core::frontmatter::parse_frontmatter(&text).map_err(|e|e.message)?.frontmatter.get("mcp").cloned().unwrap_or(Value::Null))}
        })();
        let raw=match read {
            Ok(raw)=>raw,
            Err(error)=>{declarations.warnings.push(format!("Skill '{}': {} skipped ({error}); the skill itself still loads.",skill.name,if sidecar_exists{"invalid mcp.json sidecar"}else{"unreadable frontmatter mcp block"}));continue;}
        };
        let Some(root)=raw.as_object() else{continue;};
        let map=root.get("mcpServers").filter(|value|value.is_object() || value.is_array()).unwrap_or(&raw);
        let entries:Box<dyn Iterator<Item=(String,&Value)>>=match map {
            Value::Object(map)=>Box::new(map.iter().map(|(name,server)|(name.clone(),server))),
            Value::Array(map)=>Box::new(map.iter().enumerate().map(|(index,server)|(index.to_string(),server))),
            _=>continue,
        };
        for (name,server) in entries {
            if !server.is_object(){continue;}
            let globs=normalize_globs(server.get("includeTools"));
            declarations.servers.entry(name).or_insert_with(||SkillServerDecl {raw:server.clone(),source_path:source.clone(),include_tools_by_skill:BTreeMap::new()}).include_tools_by_skill.insert(skill.name.clone(),globs);
        }
    }
    declarations
}
fn normalize_globs(value:Option<&Value>)->Vec<String> {
    let globs:Vec<_>=value.and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).filter(|s|!s.is_empty()).map(str::to_owned).collect();
    if globs.is_empty(){vec!["*".into()]}else{globs}
}
pub fn match_include_tools(globs:&[String],tool:&str)->bool {
    globs.iter().any(|glob| {
        let pattern=format!("^{}$",glob.split('*').map(regex::escape).collect::<Vec<_>>().join(".*"));
        regex::Regex::new(&pattern).is_ok_and(|r|r.is_match(tool))
    })
}
pub struct RegisteredSkillTool {pub name:String,pub tool_name:String,pub server:String}
pub fn skill_activation_targets(declarations:&SkillMcpDeclarations,skill:&str,registered:&[RegisteredSkillTool])->Vec<String> {
    let mut targets=BTreeSet::new();
    for (server,declaration) in &declarations.servers {
        let Some(globs)=declaration.include_tools_by_skill.get(skill) else{continue;};
        for tool in registered {if &tool.server==server && match_include_tools(globs,&tool.tool_name){targets.insert(tool.name.clone());}}
    }
    targets.into_iter().collect()
}
