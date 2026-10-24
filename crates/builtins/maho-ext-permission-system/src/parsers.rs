use crate::{arity, external_dir::{extract_command_paths,extract_external_paths,is_credential_path,is_external_path}};
pub use maho_ext_api::{ToolDefinition, ToolPermissionRequest as PermissionRequest};
use serde_json::Value;
use std::{collections::BTreeMap,path::Path,sync::Arc};

pub type ToolPermissionParser=Arc<dyn Fn(&Value,&Path,&Path)->Vec<PermissionRequest>+Send+Sync>;
pub type ToolDefinitionResolver = Arc<dyn Fn(&str) -> Option<ToolDefinition> + Send + Sync>;

#[derive(Default, Clone)]
pub struct ParserRegistry {
    parsers: BTreeMap<String, ToolPermissionParser>,
    definition_resolver: Option<ToolDefinitionResolver>,
}
fn request(permission:&str,patterns:Vec<String>,always:Vec<String>)->PermissionRequest {PermissionRequest{permission:permission.into(),patterns,always}}
fn fallback(permission:&str)->Vec<PermissionRequest>{vec![request(permission,vec!["*".into()],vec!["*".into()])]}
fn string<'a>(input:&'a Value,keys:&[&str])->Option<&'a str>{keys.iter().find_map(|key|input.get(key).and_then(Value::as_str))}
fn external(mut requests:Vec<PermissionRequest>,paths:&[String],location:(&Path,&Path),directory:bool,agent_dir:&Path)->Vec<PermissionRequest>{
    let all=paths;
    let paths:Vec<String>=paths.iter().filter(|path|is_external_path(path,location.0,location.1)).cloned().collect();
    if !paths.is_empty(){requests.push(request("external_directory",paths.clone(),paths.iter().map(|path|crate::external_dir::to_parent_directory_pattern(path,directory)).collect()));}
    credential(requests,all,location,agent_dir)
}
/// Emits `credential_file` for any path that is one of mhc's credential files, whether or not it is external.
fn credential(mut requests:Vec<PermissionRequest>,paths:&[String],location:(&Path,&Path),agent_dir:&Path)->Vec<PermissionRequest>{
    let paths:Vec<String>=paths.iter().filter(|path|is_credential_path(path,location.0,location.1,agent_dir)).cloned().collect();
    if !paths.is_empty(){requests.push(request("credential_file",paths.clone(),paths));}
    requests
}
fn fallback_for_tool(name:&str,input:&Value)->Vec<PermissionRequest>{
    let Some((_key,arg))=["path","file_path","command","url","input","pattern","filePath"].iter().find_map(|key|input.get(key).and_then(Value::as_str).map(|arg|(*key,arg)))else{return fallback(name)};
    vec![request(name,vec![arg.into()],vec![arg.into()])]
}
impl ParserRegistry {
    pub fn register(&mut self,name:&str,parser:ToolPermissionParser){self.parsers.insert(name.into(),parser);}
    pub fn set_definition_resolver(&mut self, resolver: ToolDefinitionResolver) { self.definition_resolver = Some(resolver); }
    pub fn parse(&self,name:&str,input:&Value,location:(&Path,&Path))->Vec<PermissionRequest>{
        if let Some(parser) = self.parsers.get(name) {
            return parser(input, location.0, location.1);
        }
        if let Some(resolver) = &self.definition_resolver
            && let Some(definition) = resolver(name)
            && let Some(custom) = &definition.permission_parser {
                    return custom(input, location.0);
        }
        fallback_for_tool(name,input)
    }
}
/// Registry whose credential check uses the process default agent dir; sessions use [`create_builtin_parser_registry_in`].
pub fn create_builtin_parser_registry()->ParserRegistry{create_builtin_parser_registry_in(Path::new(&maho_core::config::get_agent_dir()))}
/// Registry whose `credential_file` check protects `agent_dir` (the session's agent dir).
pub fn create_builtin_parser_registry_in(agent_dir:&Path)->ParserRegistry{
    let mut registry=ParserRegistry::default();
    for (name,key) in [("bash","command"),("bash_input","input")]{
        let agent=agent_dir.to_path_buf();
        registry.register(name,Arc::new(move|input,cwd,home|{
            let Some(command)=string(input,&[key]).filter(|command|!command.is_empty())else{return fallback("bash")};
            let tokens:Vec<&str>=command.split_whitespace().collect();let prefix=arity::prefix(&tokens).join(" ");
            let always=if prefix.is_empty(){vec!["*".into()]}else{vec![prefix.clone(),format!("{prefix} *")]};
            let mut requests=vec![request("bash",vec![if prefix.is_empty(){command.into()}else{prefix}],always)];
            let paths=extract_external_paths(command,cwd,home);
            if !paths.is_empty(){requests.push(request("external_directory",paths.clone(),paths.iter().map(|path|crate::external_dir::to_parent_directory_pattern(path,false)).collect()));}
            let all:Vec<String>=extract_command_paths(command);
            credential(requests,&all,(cwd,home),&agent)
        }));
    }
    let bash=Arc::clone(registry.parsers.get("bash").expect("bash parser registered"));
    let agent=agent_dir.to_path_buf();
    registry.register("monitor",Arc::new(move|input,cwd,home|{
        if let Some(path)=string(input,&["path"]).filter(|path|!path.is_empty()){
            let paths=vec![path.into()];return external(vec![request("read",paths.clone(),paths.clone())],&paths,(cwd,home),false,&agent);
        }
        bash(input,cwd,home)
    }));
    for (name,permission) in [("edit","edit"),("write","edit"),("apply_patch","edit"),("multiedit","edit"),("read","read"),("read_permission","read")]{
        let agent=agent_dir.to_path_buf();
        registry.register(name,Arc::new(move|input,cwd,home|{
            let paths=if let Some(path)=string(input,&["path","file_path"]).filter(|path|!path.is_empty()){
                vec![path.into()]
            }else if permission=="edit"{
                maho_ext_gpt_apply_patch::text::extract_patched_paths(string(input,&["input","patchText"]).unwrap_or_default())
            }else{Vec::new()};
            if paths.is_empty(){return fallback(permission);}
            let requests=paths.iter().map(|path|request(permission,vec![path.clone()],vec![if permission=="read" {crate::external_dir::to_parent_directory_pattern(path,false)} else {path.clone()}])).collect();
            external(requests,&paths,(cwd,home),false,&agent)
        }));
    }
    let agent=agent_dir.to_path_buf();
    registry.register("read",Arc::new(move|input,cwd,home|{
        let Some(path)=string(input,&["path","file_path"]).filter(|path|!path.is_empty())else{return fallback("read")};
        let targets=vec![path.to_owned()];
        external(vec![request("read",targets.clone(),targets.clone())],&targets,(cwd,home),false,&agent)
    }));
    let agent=agent_dir.to_path_buf();
    registry.register("grep",Arc::new(move|input,cwd,home|{
        let path=string(input,&["path"]);let Some(pattern)=path.or_else(||string(input,&["pattern"])).filter(|pattern|!pattern.is_empty())else{return fallback("grep")};
        let requests=vec![request("grep",vec![pattern.into()],vec!["*".into()])];
        path.filter(|path|!path.is_empty()).map_or(requests.clone(),|path|external(requests,&[path.into()],(cwd,home),true,&agent))
    }));
    for name in ["find","glob","ls"]{let agent=agent_dir.to_path_buf();registry.register(name,Arc::new(move|input,cwd,home|{
        let path=string(input,&["path"]).unwrap_or(".");let paths=vec![path.into()];external(vec![request("list",paths.clone(),paths.clone())],&paths,(cwd,home),true,&agent)
    }));}
    registry
}
