use crate::{arity, external_dir::{extract_external_paths,is_external_path}};
use serde_json::Value;
use std::{collections::BTreeMap,path::Path,sync::Arc};

#[derive(Clone,Debug,PartialEq,Eq)]
pub struct PermissionRequest {pub permission:String,pub patterns:Vec<String>,pub always:Vec<String>}
pub type ToolPermissionParser=Arc<dyn Fn(&Value,&Path,&Path)->Vec<PermissionRequest>+Send+Sync>;
#[derive(Default)]
pub struct ParserRegistry {parsers:BTreeMap<String,ToolPermissionParser>}
fn request(permission:&str,patterns:Vec<String>,always:Vec<String>)->PermissionRequest {PermissionRequest{permission:permission.into(),patterns,always}}
fn fallback(permission:&str)->Vec<PermissionRequest>{vec![request(permission,vec!["*".into()],vec!["*".into()])]}
fn string<'a>(input:&'a Value,keys:&[&str])->Option<&'a str>{keys.iter().find_map(|key|input.get(key).and_then(Value::as_str))}
fn parent_pattern(path:&str,directory:bool)->String{
    if path=="~"||path=="$HOME"{return format!("{path}/*");}
    if path.ends_with('/')||path.ends_with('\\'){return format!("{path}*");}
    if directory{return format!("{path}/*");}
    match path.rfind(['/', '\\']){Some(index) if index>0=>format!("{}/*",&path[..index]),_=>path.into()}
}
fn external(mut requests:Vec<PermissionRequest>,paths:&[String],location:(&Path,&Path),directory:bool)->Vec<PermissionRequest>{
    let paths:Vec<String>=paths.iter().filter(|path|is_external_path(path,location.0,location.1)).cloned().collect();
    if !paths.is_empty(){requests.push(request("external_directory",paths.clone(),paths.iter().map(|path|parent_pattern(path,directory)).collect()));}
    requests
}
impl ParserRegistry {
    pub fn register(&mut self,name:&str,parser:ToolPermissionParser){self.parsers.insert(name.into(),parser);}
    pub fn parse(&self,name:&str,input:&Value,location:(&Path,&Path))->Vec<PermissionRequest>{
        self.parsers.get(name).map_or_else(||fallback(name),|parser|parser(input,location.0,location.1))
    }
}
pub fn create_builtin_parser_registry()->ParserRegistry{
    let mut registry=ParserRegistry::default();
    for (name,key) in [("bash","command"),("bash_input","input")]{
        registry.register(name,Arc::new(move|input,cwd,home|{
            let Some(command)=string(input,&[key]).filter(|command|!command.is_empty())else{return fallback("bash")};
            let tokens:Vec<&str>=command.split_whitespace().collect();let prefix=arity::prefix(&tokens).join(" ");
            let always=if prefix.is_empty(){vec!["*".into()]}else{vec![prefix.clone(),format!("{prefix} *")]};
            let mut requests=vec![request("bash",vec![if prefix.is_empty(){command.into()}else{prefix}],always)];
            let paths=extract_external_paths(command,cwd,home);
            if !paths.is_empty(){requests.push(request("external_directory",paths.clone(),paths.iter().map(|path|parent_pattern(path,false)).collect()));}
            requests
        }));
    }
    for (name,permission) in [("edit","edit"),("write","edit"),("multiedit","edit"),("read","read")]{
        registry.register(name,Arc::new(move|input,cwd,home|{
            let Some(path)=string(input,&["path","file_path"]).filter(|path|!path.is_empty())else{return fallback(permission)};
            let paths=vec![path.into()];external(vec![request(permission,paths.clone(),paths.clone())],&paths,(cwd,home),false)
        }));
    }
    registry.register("grep",Arc::new(|input,cwd,home|{
        let path=string(input,&["path"]);let Some(pattern)=path.or_else(||string(input,&["pattern"])).filter(|pattern|!pattern.is_empty())else{return fallback("grep")};
        let requests=vec![request("grep",vec![pattern.into()],vec!["*".into()])];
        path.map_or(requests.clone(),|path|external(requests,&[path.into()],(cwd,home),true))
    }));
    for name in ["find","ls"]{registry.register(name,Arc::new(|input,cwd,home|{
        let path=string(input,&["path"]).unwrap_or(".");let paths=vec![path.into()];external(vec![request("list",paths.clone(),paths.clone())],&paths,(cwd,home),true)
    }));}
    registry
}
