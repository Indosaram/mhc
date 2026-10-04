use std::{collections::{BTreeMap,BTreeSet},path::{Path,PathBuf}};
use sha2::{Digest,Sha256};
use maho_omo_config_resolution::{SenpiConfigDiagnostic,load_senpi_omo_config};
pub const MERGED_OMO_CONFIG_DIAGNOSTIC_PATH:&str="(merged omo config)";
fn parts(d:&SenpiConfigDiagnostic)->(&str,&str,&str) { match d { SenpiConfigDiagnostic::Config(d)=>(d.kind,&d.path,&d.message),SenpiConfigDiagnostic::Model(d)=>(d.kind,&d.path,&d.message) } }
fn fingerprint(d:&SenpiConfigDiagnostic)->String { let (kind,path,message)=parts(d);format!("{:x}",Sha256::digest(format!("{kind}\0{path}\0{message}"))) }
fn config_directory(path:&Path,user:&Path)->Option<PathBuf> { if path.starts_with(user) { return Some(user.to_owned()); } path.ancestors().find(|p|p.file_name().is_some_and(|s|s==".omo")).map(Path::to_owned) }
fn attributable(d:&SenpiConfigDiagnostic,changed:&[PathBuf],user:&Path)->bool {
    let (kind,path,_)=parts(d);if path==MERGED_OMO_CONFIG_DIAGNOSTIC_PATH||kind=="model_catalog_cycle" { return true; }
    let path=PathBuf::from(omo_config_core::internal::posix_path::posix_resolve(&[path]));let directory=config_directory(&path,user);
    changed.iter().any(|changed| { let changed=PathBuf::from(omo_config_core::internal::posix_path::posix_resolve(&[&changed.to_string_lossy()]));path.starts_with(&changed)||(directory.is_some()&&config_directory(&changed,user)==directory) })
}
pub enum ConfigWatchValidation { Ok,Rejected{errors:Vec<String>} }
pub type ConfigDiagnosticLoader=Box<dyn Fn(&str,&BTreeMap<String,String>)->Vec<SenpiConfigDiagnostic>>;
pub struct OmoConfigValidator { cwd:String,env:BTreeMap<String,String>,user:PathBuf,baseline:BTreeSet<String>,unresolved:Vec<String>,load_config:ConfigDiagnosticLoader }
impl OmoConfigValidator {
    pub fn new(cwd:String,env:BTreeMap<String,String>)->Self {
        Self::with_loader(cwd,env,Box::new(|cwd,env|load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions{cwd:Some(cwd.into()),env:Some(env.clone()),..Default::default()}).diagnostics))
    }
    pub fn with_loader(cwd:String,env:BTreeMap<String,String>,load_config:ConfigDiagnosticLoader)->Self {
        let user=PathBuf::from(omo_config_core::resolve_user_omo_config_directory(&env));
        let diagnostics=load_config(&cwd,&env);
        Self{cwd,env,user,baseline:diagnostics.iter().map(fingerprint).collect(),unresolved:Vec::new(),load_config}
    }
    pub fn validate(&mut self,changed:&[PathBuf])->ConfigWatchValidation {
        let diagnostics=(self.load_config)(&self.cwd,&self.env);
        let by_fingerprint:BTreeMap<_,_>=diagnostics.iter().map(|d|(fingerprint(d),d)).collect();
        self.unresolved.retain(|f|by_fingerprint.contains_key(f));
        for d in &diagnostics { let f=fingerprint(d);if !self.baseline.contains(&f)&&attributable(d,changed,&self.user)&&!self.unresolved.contains(&f) { self.unresolved.push(f); } }
        if !self.unresolved.is_empty() { return ConfigWatchValidation::Rejected{errors:self.unresolved.iter().filter_map(|f|by_fingerprint.get(f)).map(|d|parts(d).2.to_owned()).collect()}; }
        self.baseline=by_fingerprint.into_keys().collect();ConfigWatchValidation::Ok
    }
}
