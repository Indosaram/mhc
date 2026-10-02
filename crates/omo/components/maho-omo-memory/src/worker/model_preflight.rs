use std::{collections::{BTreeMap,BTreeSet},path::PathBuf,time::Duration};
use super::memory_model_attempts::ReflectionModelCandidate;
pub const PREFLIGHT_TIMEOUT_MS:u64=10_000;
pub const CATALOG_CACHE_TTL_MS:i64=120_000;
pub const MODEL_LIST_ARGS:[&str;5]=["--no-extensions","--no-skills","--no-prompt-templates","--no-context-files","--list-models"];
#[derive(Clone,Debug)]pub struct Launcher{pub command:String,pub prefix_args:Vec<String>}
pub struct ConfigSource{pub path:PathBuf,pub exists:bool}
#[derive(Debug)]pub enum PreflightResult{Filtered{candidates:Vec<ReflectionModelCandidate>,rejected:Vec<String>},NoneVisible{rejected:Vec<String>},Unavailable{candidates:Vec<ReflectionModelCandidate>}}
#[derive(Default)]pub struct ModelPreflight{cache:BTreeMap<String,(i64,BTreeSet<String>)>}
impl ModelPreflight{
    pub async fn preflight(&mut self,candidates:&[ReflectionModelCandidate],launch:&Launcher,env:&BTreeMap<String,String>,sources:&[ConfigSource],now:i64,timeout_ms:u64)->(PreflightResult,Option<String>){
        let mtimes:Vec<_>=sources.iter().filter(|s|s.exists).map(|s|(s.path.clone(),std::fs::metadata(&s.path).and_then(|m|m.modified()).ok())).collect();
        let key=format!("{:?}:{:?}:{mtimes:?}",launch.command,launch.prefix_args);
        let cached=self.cache.get(&key).filter(|(at,_)|now.saturating_sub(*at)<CATALOG_CACHE_TTL_MS).map(|(_,models)|models.clone());
        let reused=cached.is_some();
        let visible=if let Some(models)=cached{models}else{match probe_child_models(launch,env,timeout_ms).await{Ok(models)=>{self.cache.insert(key,(now,models.clone()));models},Err(error)=>return (PreflightResult::Unavailable{candidates:candidates.to_vec()},Some(error))}};
        let filtered:Vec<_>=candidates.iter().filter(|c|visible.contains(&c.model)).cloned().collect();
        let rejected=candidates.iter().filter(|c|!visible.contains(&c.model)).map(|c|c.model.clone()).collect();
        (if filtered.is_empty(){if reused{PreflightResult::Unavailable{candidates:candidates.to_vec()}}else{PreflightResult::NoneVisible{rejected}}}else{PreflightResult::Filtered{candidates:filtered,rejected}},None)
    }
}
pub async fn probe_child_models(launch:&Launcher,env:&BTreeMap<String,String>,timeout_ms:u64)->Result<BTreeSet<String>,String>{
    let mut command=tokio::process::Command::new(&launch.command);
    command.args(&launch.prefix_args).args(MODEL_LIST_ARGS).env_clear().envs(env).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let output=tokio::time::timeout(Duration::from_millis(timeout_ms),command.output()).await.map_err(|_|format!("model catalog probe timed out after {timeout_ms}ms"))?.map_err(|e|e.to_string())?;
    if !output.status.success(){return Err(format!("model catalog probe exited with code {:?}: {}",output.status.code(),String::from_utf8_lossy(&output.stderr).trim()));}
    let models=parse_model_catalog(&String::from_utf8_lossy(&output.stdout));
    if models.is_empty(){return Err("model catalog probe returned no parseable model ids".into());}Ok(models)
}
pub fn parse_model_catalog(output:&str)->BTreeSet<String>{
    let mut clean=String::new();let mut chars=output.chars().peekable();while let Some(ch)=chars.next(){if ch=='\u{1b}'&&chars.peek()==Some(&'['){chars.next();for next in chars.by_ref(){if ('@'..='~').contains(&next){break;}}}else{clean.push(ch);}}
    let lines:Vec<_>=clean.lines().map(str::trim).filter(|s|!s.is_empty()).collect();
    let columns=|line:&str|->Vec<String>{let mut result=Vec::new();let mut part=String::new();let mut spaces=0;for ch in line.chars(){if ch.is_whitespace(){spaces+=1;}else{if spaces>=2{result.push(std::mem::take(&mut part));}else if spaces==1{part.push(' ');}spaces=0;part.push(ch);}}result.push(part);result};
    if let Some(header)=lines.iter().position(|line|{let c=columns(line);c.len()>=2&&c[0].eq_ignore_ascii_case("provider")&&c[1].eq_ignore_ascii_case("model")}){
        lines[header+1..].iter().filter_map(|line|{let c=columns(line);if c.len()<2||c[0].is_empty()||c[0].contains('/')||c[0].chars().any(char::is_whitespace)||c[1].is_empty()||c[1].chars().any(char::is_whitespace){None}else{Some(format!("{}/{}",c[0],c[1]))}}).collect()
    }else{lines.into_iter().filter(|s|s.split_once('/').is_some_and(|(provider,id)|!provider.is_empty()&&!id.is_empty())&&!s.chars().any(char::is_whitespace)).map(str::to_owned).collect()}
}
#[cfg(test)]mod tests{
    use super::*;
    fn candidates()->Vec<ReflectionModelCandidate>{["extension-only/primary","builtin/fallback"].map(|model|ReflectionModelCandidate{model:model.into(),thinking:None}).into()}
    fn launch(text:&str)->Launcher{Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),format!("printf '%s\\n' '{text}'")]}}
    #[test]fn table_preserves_nested_model_ids(){assert_eq!(parse_model_catalog("\u{1b}[32mprovider  model  context\u{1b}[0m\nbuiltin  z-ai/glm  32000\n"),BTreeSet::from(["builtin/z-ai/glm".into()]));}
    #[test]fn malformed_and_duplicates(){assert_eq!(parse_model_catalog("bad\np/model\np/model\n/p\np/\np/model words"),BTreeSet::from(["p/model".into()]));}
    #[test]fn malformed_table_provider_is_rejected(){assert_eq!(parse_model_catalog("provider  model\nbad/provider  some-model\ngood  nested/model"),BTreeSet::from(["good/nested/model".into()]));}
    #[test]fn headerless_nested_ids_remain_visible(){assert_eq!(parse_model_catalog("provider/nested/model\nbuiltin/fallback"),BTreeSet::from(["provider/nested/model".into(),"builtin/fallback".into()]));}
    #[tokio::test]async fn nonzero_catalog_exit_degrades(){let launcher=Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),"printf failure >&2; exit 7".into()]};let (result,warn)=ModelPreflight::default().preflight(&candidates(),&launcher,&BTreeMap::new(),&[],0,10000).await;assert!(matches!(result,PreflightResult::Unavailable{..}));assert!(warn.unwrap().contains("7"));}
    #[tokio::test]async fn cache_hit_avoids_second_child(){let dir=tempfile::tempdir().unwrap();let log=dir.path().join("log");let launcher=Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),format!("printf x >> '{}'; printf 'builtin/fallback\\n'",log.display())]};let mut cache=ModelPreflight::default();for now in [0,1]{cache.preflight(&candidates(),&launcher,&BTreeMap::new(),&[],now,10000).await;}assert_eq!(std::fs::read_to_string(log).unwrap(),"x");}
    #[tokio::test]async fn changed_config_sources_invalidate_cache(){let dir=tempfile::tempdir().unwrap();let log=dir.path().join("log");let config=dir.path().join("config");let launcher=Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),format!("printf x >> '{}'; printf 'builtin/fallback\\n'",log.display())]};let sources=[ConfigSource{path:config.clone(),exists:true}];let mut cache=ModelPreflight::default();cache.preflight(&candidates(),&launcher,&BTreeMap::new(),&sources,0,10000).await;std::fs::write(config,b"{}").unwrap();cache.preflight(&candidates(),&launcher,&BTreeMap::new(),&sources,1,10000).await;assert_eq!(std::fs::read_to_string(log).unwrap(),"xx");}
    #[tokio::test]async fn real_catalog_filters_candidates(){let (result,warn)=ModelPreflight::default().preflight(&candidates(),&launch("builtin/fallback"),&BTreeMap::new(),&[],0,10000).await;assert!(warn.is_none());match result{PreflightResult::Filtered{candidates,rejected}=>{assert_eq!(candidates[0].model,"builtin/fallback");assert_eq!(rejected,["extension-only/primary"]);},_=>panic!("not filtered")}}
    #[tokio::test]async fn cached_negative_retains_reactive_chain(){let mut cache=ModelPreflight::default();let launch=launch("other/model");let (fresh,_)=cache.preflight(&candidates(),&launch,&BTreeMap::new(),&[],1000,10000).await;assert!(matches!(fresh,PreflightResult::NoneVisible{..}));let (cached,_)=cache.preflight(&candidates(),&launch,&BTreeMap::new(),&[],1001,10000).await;assert!(matches!(cached,PreflightResult::Unavailable{..}));let (expired,_)=cache.preflight(&candidates(),&launch,&BTreeMap::new(),&[],121000,10000).await;assert!(matches!(expired,PreflightResult::NoneVisible{..}));}
    #[tokio::test]async fn probe_failure_keeps_candidates(){let (result,warn)=ModelPreflight::default().preflight(&candidates(),&launch("malformed"),&BTreeMap::new(),&[],0,10000).await;assert!(matches!(result,PreflightResult::Unavailable{..}));assert!(warn.is_some());}
}
