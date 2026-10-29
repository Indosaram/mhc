use std::{collections::{BTreeMap,BTreeSet},path::PathBuf,time::Duration};
use super::memory_model_attempts::ReflectionModelCandidate;
pub const PREFLIGHT_TIMEOUT_MS:u64=10_000;
pub const CATALOG_CACHE_TTL_MS:i64=120_000;
pub const MODEL_LIST_ARGS:[&str;5]=["--no-extensions","--no-skills","--no-prompt-templates","--no-context-files","--list-models"];
pub const EXTENSION_MODEL_LIST_ARGS:[&str;4]=["--no-skills","--no-prompt-templates","--no-context-files","--list-models"];
#[derive(Clone,Debug)]pub struct Launcher{pub command:String,pub prefix_args:Vec<String>}
pub struct ConfigSource{pub path:PathBuf,pub exists:bool}
#[derive(Debug)]pub enum PreflightResult{Filtered{candidates:Vec<ReflectionModelCandidate>,rejected:Vec<String>},NoneVisible{rejected:Vec<String>},Unavailable{candidates:Vec<ReflectionModelCandidate>}}
#[derive(Default)]pub struct ModelPreflight{cache:BTreeMap<String,(i64,BTreeSet<String>)>}
impl ModelPreflight{
    async fn visible_models(&mut self,launch:&Launcher,env:&BTreeMap<String,String>,sources:&[ConfigSource],now:i64,timeout_ms:u64,extensions:bool)->Result<BTreeSet<String>,String>{
        let mtimes:Vec<_>=sources.iter().filter(|s|s.exists).map(|s|(s.path.clone(),std::fs::metadata(&s.path).and_then(|m|m.modified()).ok())).collect();
        let key=format!("{extensions}:{:?}:{:?}:{mtimes:?}",launch.command,launch.prefix_args);
        if let Some((_,models))=self.cache.get(&key).filter(|(at,_)|now.saturating_sub(*at)<CATALOG_CACHE_TTL_MS){return Ok(models.clone());}
        let visible=probe_child_models_mode(launch,env,timeout_ms,extensions).await?;
        self.cache.insert(key,(now,visible.clone()));
        Ok(visible)
    }
    pub async fn preflight(&mut self,candidates:&[ReflectionModelCandidate],launch:&Launcher,env:&BTreeMap<String,String>,sources:&[ConfigSource],now:i64,timeout_ms:u64)->(PreflightResult,Option<String>){
        let core=match self.visible_models(launch,env,sources,now,timeout_ms,false).await{Ok(models)=>models,Err(error)=>return(PreflightResult::Unavailable{candidates:candidates.to_vec()},Some(format!("memory child model preflight failed; falling back to reactive model retries: {error}")))};
        if candidates.iter().all(|candidate|core.contains(&candidate.model)){return(PreflightResult::Filtered{candidates:candidates.to_vec(),rejected:vec![]},None);}
        let (extended,warning)=match self.visible_models(launch,env,sources,now,timeout_ms,true).await{Ok(models)=>(models,None),Err(error)=>(BTreeSet::new(),Some(format!("memory child model preflight with extensions failed; extension-registered models stay unrouted: {error}")))};
        let filtered:Vec<_>=candidates.iter().filter_map(|candidate|{if core.contains(&candidate.model){Some(candidate.clone())}else if extended.contains(&candidate.model){let mut candidate=candidate.clone();candidate.load_extensions=Some(true);Some(candidate)}else{None}}).collect();
        let rejected=candidates.iter().filter(|candidate|!core.contains(&candidate.model)&&!extended.contains(&candidate.model)).map(|candidate|candidate.model.clone()).collect();
        if filtered.is_empty(){let candidates=candidates.iter().cloned().map(|mut candidate|{candidate.load_extensions=Some(true);candidate}).collect();return(PreflightResult::Unavailable{candidates},Some([warning,Some("memory child model catalog omits every candidate; falling back to reactive model retries".into())].into_iter().flatten().collect::<Vec<_>>().join("\n")));}
        (PreflightResult::Filtered{candidates:filtered,rejected},warning)
    }
}
pub async fn probe_child_models(launch:&Launcher,env:&BTreeMap<String,String>,timeout_ms:u64)->Result<BTreeSet<String>,String>{
    probe_child_models_mode(launch,env,timeout_ms,false).await
}
async fn probe_child_models_mode(launch:&Launcher,env:&BTreeMap<String,String>,timeout_ms:u64,extensions:bool)->Result<BTreeSet<String>,String>{
    let mut command=tokio::process::Command::new(&launch.command);
    command.args(&launch.prefix_args).args(if extensions{&EXTENSION_MODEL_LIST_ARGS[..]}else{&MODEL_LIST_ARGS[..]}).env_clear().envs(env).stdin(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    for key in ["SystemRoot", "PATH", "PATHEXT"] {
        if let Some(val) = std::env::var_os(key).filter(|_| !env.contains_key(key)) {
            command.env(key, val);
        }
    }
    if extensions{command.env("OMO_SENPI_DISABLED","1");}
    let output=tokio::time::timeout(Duration::from_millis(timeout_ms),command.output()).await.map_err(|_|format!("model catalog probe timed out after {timeout_ms}ms"))?.map_err(|e|e.to_string())?;
    if !output.status.success(){let stderr=String::from_utf8_lossy(&output.stderr);let stdout=String::from_utf8_lossy(&output.stdout);let detail=if stderr.trim().is_empty(){stdout.trim()}else{stderr.trim()};let code=output.status.code().map_or_else(||"null".into(),|code|code.to_string());return Err(format!("model catalog probe exited with code {code}: {detail}"));}
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
    #[tokio::test]
    async fn extension_catalog_routes_in_order_and_disables_omo_only_in_probe() {
        let launch = if cfg!(windows) {
            Launcher{command:"node".into(),prefix_args:vec!["-e".into(),"const args=process.argv; if(args.includes('--no-extensions')){if(process.env.OMO_SENPI_DISABLED)process.exit(8);console.log('builtin/fallback');}else{if(process.env.OMO_SENPI_DISABLED!=='1')process.exit(9);console.log('extension-only/primary\\nbuiltin/fallback');}".into(),"--".into()]}
        } else {
            Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),"if [ \"$1\" = --no-extensions ]; then [ -z \"$OMO_SENPI_DISABLED\" ] || exit 8; printf 'builtin/fallback\\n'; else [ \"$OMO_SENPI_DISABLED\" = 1 ] || exit 9; printf 'extension-only/primary\\nbuiltin/fallback\\n'; fi".into(),"--".into()]}
        };
        let env=BTreeMap::new();
        let (result,warning)=ModelPreflight::default().preflight(&candidates(),&launch,&env,&[],0,10000).await;
        assert!(warning.is_none());
        let PreflightResult::Filtered{candidates,rejected}=result else{panic!("expected filtered chain")};
        assert!(rejected.is_empty());
        assert_eq!(candidates[0].model,"extension-only/primary");
        assert_eq!(candidates[0].load_extensions,Some(true));
        assert_eq!(candidates[1].model,"builtin/fallback");
        assert_eq!(candidates[1].load_extensions,None);
        assert!(env.is_empty());
    }
    #[tokio::test]
    async fn core_visible_chain_never_probes_extensions() {
        let launch = if cfg!(windows) {
            Launcher{command:"node".into(),prefix_args:vec!["-e".into(),"if(!process.argv.includes('--no-extensions'))process.exit(9);console.log('extension-only/primary\\nbuiltin/fallback');".into(),"--".into()]}
        } else {
            Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),"[ \"$1\" = --no-extensions ] || exit 9; printf 'extension-only/primary\\nbuiltin/fallback\\n'".into(),"--".into()]}
        };
        let (result,warning)=ModelPreflight::default().preflight(&candidates(),&launch,&BTreeMap::new(),&[],0,10000).await;
        assert!(warning.is_none());
        let PreflightResult::Filtered{candidates,rejected}=result else{panic!("expected filtered chain")};
        assert!(rejected.is_empty());
        assert!(candidates.iter().all(|candidate|candidate.load_extensions.is_none()));
    }
    #[tokio::test]
    async fn failed_extension_catalog_keeps_core_candidate_and_reports_cause() {
        let launch = if cfg!(windows) {
            Launcher{command:"node".into(),prefix_args:vec!["-e".into(),"if(process.argv.includes('--no-extensions')){console.log('builtin/fallback');}else{process.stderr.write('extension boom');process.exit(3);}".into(),"--".into()]}
        } else {
            Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),"if [ \"$1\" = --no-extensions ]; then printf 'builtin/fallback\\n'; else printf 'extension boom' >&2; exit 3; fi".into(),"--".into()]}
        };
        let (result,warning)=ModelPreflight::default().preflight(&candidates(),&launch,&BTreeMap::new(),&[],0,10000).await;
        assert!(warning.unwrap().contains("extension boom"));
        let PreflightResult::Filtered{candidates,rejected}=result else{panic!("expected core candidate")};
        assert_eq!(candidates.len(),1);
        assert_eq!(candidates[0].model,"builtin/fallback");
        assert_eq!(candidates[0].load_extensions,None);
        assert_eq!(rejected,["extension-only/primary"]);
    }
    fn candidates()->Vec<ReflectionModelCandidate>{["extension-only/primary","builtin/fallback"].map(|model|ReflectionModelCandidate{model:model.into(),thinking:None, load_extensions: None }).into()}
    fn launch(text:&str)->Launcher{
        if cfg!(windows) {
            Launcher{command:"node".into(),prefix_args:vec!["-e".into(),format!("console.log('{text}')"),"--".into()]}
        } else {
            Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),format!("printf '%s\\n' '{text}'")]}
        }
    }
    #[test]fn table_preserves_nested_model_ids(){assert_eq!(parse_model_catalog("\u{1b}[32mprovider  model  context\u{1b}[0m\nbuiltin  z-ai/glm  32000\n"),BTreeSet::from(["builtin/z-ai/glm".into()]));}
    #[test]fn malformed_and_duplicates(){assert_eq!(parse_model_catalog("bad\np/model\np/model\n/p\np/\np/model words"),BTreeSet::from(["p/model".into()]));}
    #[test]fn malformed_table_provider_is_rejected(){assert_eq!(parse_model_catalog("provider  model\nbad/provider  some-model\ngood  nested/model"),BTreeSet::from(["good/nested/model".into()]));}
    #[test]fn headerless_nested_ids_remain_visible(){assert_eq!(parse_model_catalog("provider/nested/model\nbuiltin/fallback"),BTreeSet::from(["provider/nested/model".into(),"builtin/fallback".into()]));}
    #[tokio::test]async fn nonzero_catalog_exit_degrades(){
        let launcher = if cfg!(windows) {
            Launcher{command:"node".into(),prefix_args:vec!["-e".into(),"process.stderr.write('failure'); process.exit(7);".into(),"--".into()]}
        } else {
            Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),"printf failure >&2; exit 7".into()]}
        };
        let (result,warn)=ModelPreflight::default().preflight(&candidates(),&launcher,&BTreeMap::new(),&[],0,10000).await;assert!(matches!(result,PreflightResult::Unavailable{..}));assert!(warn.unwrap().contains("7"));
    }
    #[tokio::test]async fn failed_probe_retains_stdout_when_stderr_is_empty(){
        let launcher = if cfg!(windows) {
            Launcher{command:"node".into(),prefix_args:vec!["-e".into(),"process.stdout.write('catalog-failure'); process.exit(7);".into(),"--".into()]}
        } else {
            Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),"printf catalog-failure; exit 7".into()]}
        };
        let error=probe_child_models(&launcher,&BTreeMap::new(),10000).await.unwrap_err();assert!(error.contains("catalog-failure"));
    }
    #[tokio::test]async fn cache_hit_avoids_second_child(){
        let dir=tempfile::tempdir().unwrap();let log=dir.path().join("log");
        let launcher = if cfg!(windows) {
            let log_escaped = log.to_string_lossy().replace('\\', "\\\\");
            Launcher{command:"node".into(),prefix_args:vec!["-e".into(),format!("require('fs').appendFileSync('{log_escaped}', 'x'); console.log('builtin/fallback');"),"--".into()]}
        } else {
            Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),format!("printf x >> '{}'; printf 'builtin/fallback\\n'",log.display())]}
        };
        let mut cache=ModelPreflight::default();for now in [0,1]{cache.preflight(&candidates(),&launcher,&BTreeMap::new(),&[],now,10000).await;}assert_eq!(std::fs::read_to_string(log).unwrap(),"xx");
    }
    #[tokio::test]async fn changed_config_sources_invalidate_cache(){
        let dir=tempfile::tempdir().unwrap();let log=dir.path().join("log");let config=dir.path().join("config");
        let launcher = if cfg!(windows) {
            let log_escaped = log.to_string_lossy().replace('\\', "\\\\");
            Launcher{command:"node".into(),prefix_args:vec!["-e".into(),format!("require('fs').appendFileSync('{log_escaped}', 'x'); console.log('builtin/fallback');"),"--".into()]}
        } else {
            Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),format!("printf x >> '{}'; printf 'builtin/fallback\\n'",log.display())]}
        };
        let sources=[ConfigSource{path:config.clone(),exists:true}];let mut cache=ModelPreflight::default();cache.preflight(&candidates(),&launcher,&BTreeMap::new(),&sources,0,10000).await;std::fs::write(config,b"{}").unwrap();cache.preflight(&candidates(),&launcher,&BTreeMap::new(),&sources,1,10000).await;assert_eq!(std::fs::read_to_string(log).unwrap(),"xxxx");}
    #[tokio::test]async fn real_catalog_filters_candidates(){let (result,warn)=ModelPreflight::default().preflight(&candidates(),&launch("builtin/fallback"),&BTreeMap::new(),&[],0,10000).await;assert!(warn.is_none());match result{PreflightResult::Filtered{candidates,rejected}=>{assert_eq!(candidates[0].model,"builtin/fallback");assert_eq!(rejected,["extension-only/primary"]);},_=>panic!("not filtered")}}
    #[tokio::test]async fn cached_negative_retains_reactive_chain(){let mut cache=ModelPreflight::default();let launch=launch("other/model");let (fresh,_)=cache.preflight(&candidates(),&launch,&BTreeMap::new(),&[],1000,10000).await;assert!(matches!(fresh,PreflightResult::Unavailable{..}));let (cached,_)=cache.preflight(&candidates(),&launch,&BTreeMap::new(),&[],1001,10000).await;assert!(matches!(cached,PreflightResult::Unavailable{..}));let (expired,_)=cache.preflight(&candidates(),&launch,&BTreeMap::new(),&[],121000,10000).await;assert!(matches!(expired,PreflightResult::Unavailable{..}));}
    #[tokio::test]async fn probe_failure_keeps_candidates(){let (result,warn)=ModelPreflight::default().preflight(&candidates(),&launch("malformed"),&BTreeMap::new(),&[],0,10000).await;assert!(matches!(result,PreflightResult::Unavailable{..}));assert!(warn.is_some());}
}
