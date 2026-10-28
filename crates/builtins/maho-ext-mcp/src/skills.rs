use std::{collections::{BTreeMap,BTreeSet},path::PathBuf};
use serde_json::Value;
#[derive(Clone,Debug,serde::Serialize)]
pub struct SkillProvenance {pub kind:String,pub selected_root:PathBuf,pub project_root:Option<PathBuf>}
#[derive(Clone,Debug,Default)]
pub struct SkillAllowedSet {pub roots:Vec<PathBuf>,pub files:Vec<PathBuf>,pub digest:String}
#[derive(Clone,Default)]
pub struct SkillLike {pub name:String,pub file_path:PathBuf,pub base_dir:PathBuf,pub scope:Option<String>,pub provenance:Option<SkillProvenance>,pub allowed_set:Option<SkillAllowedSet>}
#[derive(Clone)]
struct SkillServers {raw:Value,source:PathBuf,warning:Option<String>}
fn blocked(path:&std::path::Path,target:impl std::fmt::Display)->String {
    let display=|path:&std::path::Path| {let value=path.to_string_lossy();let home=std::env::var("HOME").unwrap_or_default();if !home.is_empty() && path.starts_with(&home){format!("~{}",&value[home.len()..])}else{value.into_owned()}};
    format!("Blocked skill link outside the project: {} -> {target}. Add the target folder to your user skills settings to allow it.",display(path))
}
pub fn authorize_skill_path(path:&std::path::Path,provenance:Option<&SkillProvenance>,allowed:Option<&SkillAllowedSet>)->Result<PathBuf,String> {
    let Some(provenance)=provenance.filter(|p|p.kind!="user") else{return Ok(path.into());};
    let resolve=||->std::io::Result<PathBuf>{
        let canonical=std::fs::canonicalize(path)?;
        let mut roots=Vec::new();
        if let Some(project)=&provenance.project_root {roots.push(std::fs::canonicalize(project)?);}
        if provenance.kind=="project-selected" {roots.push(std::fs::canonicalize(&provenance.selected_root)?);}
        if let Some(allowed)=allowed {roots.extend(allowed.roots.iter().cloned());}
        if roots.iter().any(|root|canonical.starts_with(root)) || allowed.is_some_and(|set|set.files.contains(&canonical)){return Ok(canonical);}
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,blocked(path,canonical.display())))
    };
    resolve().map_err(|error|if error.kind()==std::io::ErrorKind::PermissionDenied{error.to_string()}else{blocked(path,"unresolvable target")})
}
fn read_skill_file_checked(path:&std::path::Path,skill:&SkillLike)->Result<String,String> {
    use std::io::Read;
    let canonical=authorize_skill_path(path,skill.provenance.as_ref(),skill.allowed_set.as_ref())?;
    if skill.provenance.as_ref().is_none_or(|p|p.kind=="user"){return std::fs::read_to_string(canonical).map_err(|e|e.to_string());}
    let fail=||blocked(path,canonical.display());
    let expected=std::fs::symlink_metadata(&canonical).map_err(|_|fail())?;
    if !expected.is_file(){return Err(fail());}
    let mut file=std::fs::File::open(&canonical).map_err(|_|fail())?;
    let opened=file.metadata().map_err(|_|fail())?;
    if !opened.is_file() || std::fs::symlink_metadata(&canonical).map_err(|_|fail())?.file_type().is_symlink(){return Err(fail());}
    #[cfg(unix)] {use std::os::unix::fs::MetadataExt;if expected.dev()!=opened.dev() || expected.ino()!=opened.ino(){return Err(fail());}}
    let mut text=String::new();file.read_to_string(&mut text).map_err(|e|e.to_string())?;Ok(text)
}
fn file_stamp(path:&std::path::Path)->String {
    match std::fs::metadata(path){Ok(stat)=>{
        #[cfg(unix)] {use std::os::unix::fs::MetadataExt;format!("{}:{}:{}:{}:{}:{}",stat.mtime(),stat.mtime_nsec(),stat.ctime(),stat.ctime_nsec(),stat.len(),stat.ino())}
        #[cfg(not(unix))] {format!("{:?}:{:?}:{}",stat.modified(),stat.created(),stat.len())}
    },Err(_)=>"missing".into()}
}
fn read_skill_servers(skill:&SkillLike)->SkillServers {
    type Cache=BTreeMap<PathBuf,(String,SkillServers)>;
    static CACHE:std::sync::LazyLock<std::sync::Mutex<Cache>>=std::sync::LazyLock::new(Default::default);
    let key=format!("{}|{}|{}|{}",file_stamp(&skill.base_dir.join("mcp.json")),file_stamp(&skill.file_path),serde_json::to_string(&skill.provenance).unwrap_or_default(),skill.allowed_set.as_ref().map_or("",|set|set.digest.as_str()));
    let cached=CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&skill.file_path).cloned();
    if let Some((previous,servers))=cached && previous==key && authorize_skill_path(&servers.source,skill.provenance.as_ref(),skill.allowed_set.as_ref()).is_ok(){return servers;}
    let sidecar=skill.base_dir.join("mcp.json");let sidecar_exists=sidecar.exists();
    let source=if sidecar_exists{sidecar}else{skill.file_path.clone()};
    let read=(||->Result<Value,String>{let text=read_skill_file_checked(&source,skill)?;if sidecar_exists{serde_json::from_str(&text).map_err(|e|e.to_string())}else{Ok(maho_core::frontmatter::parse_frontmatter(&text).map_err(|e|e.message)?.frontmatter.get("mcp").cloned().unwrap_or(Value::Null))}})();
    let project=skill.provenance.as_ref().map_or(skill.scope.as_deref()==Some("project"),|p|p.kind!="user");
    let servers=match read {Ok(raw)=>SkillServers{raw,source,warning:None},Err(error)=>{
        let warning=if error.starts_with("Blocked skill link outside the project:"){error}else{format!("Skill '{}': {} skipped{}; the skill itself still loads.",skill.name,if sidecar_exists{"invalid mcp.json sidecar"}else{"unreadable frontmatter mcp block"},if project{String::new()}else{format!(" ({error})")})};
        SkillServers{raw:Value::Null,source,warning:Some(warning)}
    }};
    CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(skill.file_path.clone(),(key,servers.clone()));servers
}
pub struct SkillServerDecl {pub skill_name:String,pub scope:Option<String>,pub provenance:Option<SkillProvenance>,pub allowed_set:Option<SkillAllowedSet>,pub skill_path:PathBuf,pub raw:Value,pub source_path:PathBuf,pub include_tools_by_skill:BTreeMap<String,Vec<String>>}
#[derive(Default)]
pub struct SkillMcpDeclarations {pub servers:BTreeMap<String,SkillServerDecl>,pub warnings:Vec<String>}
pub fn parse_skill_mcp_declarations(skills:&[SkillLike])->SkillMcpDeclarations {
    let mut declarations=SkillMcpDeclarations::default();
    for skill in skills {
        let servers=read_skill_servers(skill);
        let source=servers.source;
        if let Some(warning)=servers.warning{declarations.warnings.push(warning);}
        let raw=servers.raw;
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
            declarations.servers.entry(name).or_insert_with(||SkillServerDecl {skill_name:skill.name.clone(),scope:skill.scope.clone(),provenance:skill.provenance.clone(),allowed_set:skill.allowed_set.clone(),skill_path:skill.file_path.clone(),raw:server.clone(),source_path:source.clone(),include_tools_by_skill:BTreeMap::new()}).include_tools_by_skill.insert(skill.name.clone(),globs);
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
        let pattern=format!("^{}$",glob.split('*').map(regex::escape).collect::<Vec<_>>().join(r"[^\n\r\x{2028}\x{2029}]*"));
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
    let mut targets=targets.into_iter().collect::<Vec<_>>();
    targets.sort_by_cached_key(|name|name.encode_utf16().collect::<Vec<_>>());targets
}
