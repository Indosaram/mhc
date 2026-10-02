use std::{collections::BTreeMap,fs,path::{Path,PathBuf,Component}};
use serde_json::Value;
#[derive(Debug,thiserror::Error)]
#[error("{reason}: {detail}")]
pub struct HostLaunchSpecError {pub reason:&'static str,pub detail:String}
fn refusal(reason:&'static str,detail:impl Into<String>)->HostLaunchSpecError{HostLaunchSpecError{reason,detail:detail.into()}}
#[derive(Debug,Default,PartialEq)]
pub struct HostLifecyclePolicyInput {pub idle_exit_ms:Option<f64>,pub cold_start:Option<String>}
#[derive(Debug,PartialEq)]
pub struct HostLaunchSpec {pub session_runtime:String,pub extensions:Vec<String>,pub tunables:HostLifecyclePolicyInput,pub env:BTreeMap<String,String>}
#[derive(Debug,Default,PartialEq)]
pub struct ResolvedHostLaunchSpec {pub host_args:Vec<String>,pub policy:HostLifecyclePolicyInput,pub env:BTreeMap<String,String>}
pub fn parse_host_launch_spec(text:&str)->Result<HostLaunchSpec,HostLaunchSpecError>{
    let value:Value=serde_json::from_str(text).map_err(|error|refusal("launch_spec_invalid",error.to_string()))?;
    let object=value.as_object().ok_or_else(||refusal("launch_spec_invalid","the spec is not a JSON object"))?;
    if object.get("spec_version").and_then(Value::as_f64)!=Some(1.){return Err(refusal("launch_spec_invalid","unsupported spec_version"));}
    let core=object.get("core").and_then(Value::as_object).ok_or_else(||refusal("launch_spec_invalid","core must be an object"))?;
    let runtime=core.get("session_runtime").and_then(Value::as_str).filter(|v|matches!(*v,"in-process"|"worker")).ok_or_else(||refusal("launch_spec_invalid","core.session_runtime must be in-process or worker"))?;
    if core.get("multi_session")!=Some(&Value::Bool(true)){return Err(refusal("launch_spec_invalid","core.multi_session must be true"));}
    let extensions=core.get("extensions").and_then(Value::as_array).and_then(|entries|entries.iter().map(|v|v.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>()).ok_or_else(||refusal("launch_spec_invalid","core.extensions must be an array of strings"))?;
    let mut tunables=HostLifecyclePolicyInput::default();
    if let Some(fields)=object.get("tunables").and_then(Value::as_object){tunables.idle_exit_ms=fields.get("idleExitMs").and_then(Value::as_f64).filter(|v|v.is_finite()&&*v>0.);tunables.cold_start=fields.get("coldStart").and_then(Value::as_str).filter(|v|matches!(*v,"transient"|"persistent")).map(str::to_owned);}
    let mut env=BTreeMap::new();
    if let Some(fields)=object.get("env").and_then(Value::as_object){for (name,value) in fields {let value=value.as_str().ok_or_else(||refusal("launch_spec_invalid",format!("env.{name} must be a string")))?;env.insert(name.clone(),value.into());}}
    Ok(HostLaunchSpec{session_runtime:runtime.into(),extensions,tunables,env})
}
fn normalize(path:&Path)->PathBuf{let mut result=PathBuf::new();for part in path.components(){match part{Component::ParentDir=>{result.pop();},Component::CurDir=>{},_=>result.push(part.as_os_str())}}result}
fn escapes(root:&Path,target:&Path)->bool{target.strip_prefix(root).is_err()||target.strip_prefix(root).ok().and_then(|step|step.components().next()).is_some_and(|part|part.as_os_str().to_string_lossy().starts_with(".."))}
pub fn load_host_launch_spec(path:&Path,platform:&str)->Result<ResolvedHostLaunchSpec,HostLaunchSpecError>{
    use std::os::unix::fs::MetadataExt;
    let absolute=normalize(&if path.is_absolute(){path.into()}else{std::env::current_dir().map_err(|error|refusal("launch_spec_unreadable",error.to_string()))?.join(path)});
    if platform!="win32"{
        let stats=fs::metadata(&absolute).map_err(|error|refusal("launch_spec_unreadable",error.to_string()))?;
        let uid=rustix::process::getuid().as_raw();
        if stats.uid()!=uid{return Err(refusal("launch_spec_insecure",format!("{} is owned by uid {}, not {uid}",absolute.display(),stats.uid())));}
        if stats.mode()&0o022!=0{return Err(refusal("launch_spec_insecure",format!("{} is group/world writable (mode {:o})",absolute.display(),stats.mode()&0o777)));}
    }
    let text=fs::read_to_string(&absolute).map_err(|error|refusal("launch_spec_unreadable",error.to_string()))?;let spec=parse_host_launch_spec(&text)?;
    for name in spec.env.keys(){let allowed=["SENPI_","OMO_","PI_"].iter().any(|prefix|name.strip_prefix(prefix).is_some_and(|suffix|!suffix.is_empty()&&suffix.bytes().all(|c|c.is_ascii_uppercase()||c.is_ascii_digit()||c==b'_')));if !allowed{return Err(refusal("launch_spec_env_denied",format!("{name} is outside ^(SENPI|OMO|PI)_[A-Z0-9_]+$")));}}
    let root=absolute.parent().unwrap_or(Path::new("/"));let root_real=fs::canonicalize(root).unwrap_or_else(|_|root.into());let mut host_args=vec!["--session-runtime".into(),spec.session_runtime];
    for entry in spec.extensions{let target=normalize(&root.join(&entry));if escapes(root,&target){return Err(refusal("launch_spec_path_escape",format!("{entry} resolves outside {}",root.display())));}let real=fs::canonicalize(&target).map_err(|_|refusal("launch_spec_missing_extension",format!("{} does not exist",target.display())))?;if escapes(&root_real,&real){return Err(refusal("launch_spec_path_escape",format!("{entry} links outside {}",root.display())));}host_args.push("--extension".into());host_args.push(real.to_string_lossy().into_owned());}
    Ok(ResolvedHostLaunchSpec{host_args,policy:spec.tunables,env:spec.env})
}
#[cfg(test)]
mod tests{
    use super::*;
    fn spec(extensions:Value)->Value{serde_json::json!({"spec_version":1,"core":{"session_runtime":"in-process","multi_session":true,"extensions":extensions}})}
    #[test] fn keeps_core_tunables_and_env(){let mut value=spec(serde_json::json!(["a.js"]));value["tunables"]=serde_json::json!({"idleExitMs":1000,"coldStart":"persistent"});value["env"]=serde_json::json!({"SENPI_X":"1"});let parsed=parse_host_launch_spec(&value.to_string()).unwrap();assert_eq!(parsed.extensions,vec!["a.js"]);assert_eq!(parsed.tunables.idle_exit_ms,Some(1000.));assert_eq!(parsed.env["SENPI_X"],"1");}
    #[test] fn rejects_invalid_documents(){let base=spec(serde_json::json!([]));let mut cases=vec![Value::Null,serde_json::json!({"spec_version":2,"core":base["core"]}),serde_json::json!({"spec_version":1,"core":"in-process"}),spec(serde_json::json!([7]))];for (key,value) in [("session_runtime",serde_json::json!("threads")),("multi_session",serde_json::json!(false))]{let mut document=base.clone();document["core"][key]=value;cases.push(document);}let mut document=base;document["env"]=serde_json::json!({"SENPI_X":7});cases.push(document);for case in cases{assert_eq!(parse_host_launch_spec(&case.to_string()).unwrap_err().reason,"launch_spec_invalid");}assert_eq!(parse_host_launch_spec("not json").unwrap_err().reason,"launch_spec_invalid");}
    fn write(path:&Path,value:&Value){use std::{io::Write,os::unix::fs::OpenOptionsExt};let mut file=fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(path).unwrap();write!(file,"{value}").unwrap();}
    #[test] fn absolute_extension_args_and_symlink_escape(){let temp=tempfile::tempdir().unwrap();let outside=tempfile::tempdir().unwrap();let extension=temp.path().join("probe.js");fs::write(&extension,"").unwrap();let path=temp.path().join("launch.json");write(&path,&spec(serde_json::json!(["probe.js"])));assert_eq!(load_host_launch_spec(&path,"linux").unwrap().host_args,vec!["--session-runtime","in-process","--extension",extension.to_str().unwrap()]);fs::remove_file(&extension).unwrap();fs::write(outside.path().join("evil.js"),"").unwrap();std::os::unix::fs::symlink(outside.path().join("evil.js"),extension).unwrap();assert_eq!(load_host_launch_spec(&path,"linux").unwrap_err().reason,"launch_spec_path_escape");}
    #[test] fn rejects_permissions_missing_extension_and_env_denial(){use std::os::unix::fs::PermissionsExt;let temp=tempfile::tempdir().unwrap();let path=temp.path().join("launch.json");write(&path,&spec(serde_json::json!(["missing.js"])));assert_eq!(load_host_launch_spec(&path,"linux").unwrap_err().reason,"launch_spec_missing_extension");let mut value=spec(serde_json::json!([]));value["env"]=serde_json::json!({"PATH":"/bin"});write(&path,&value);assert_eq!(load_host_launch_spec(&path,"linux").unwrap_err().reason,"launch_spec_env_denied");fs::set_permissions(&path,fs::Permissions::from_mode(0o666)).unwrap();assert_eq!(load_host_launch_spec(&path,"linux").unwrap_err().reason,"launch_spec_insecure");}
}
