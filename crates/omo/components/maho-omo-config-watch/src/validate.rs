use std::{collections::{BTreeMap,BTreeSet},path::{Path,PathBuf}};
use sha2::{Digest,Sha256};
use maho_omo_config_resolution::{SenpiConfigDiagnostic,load_senpi_omo_config};
pub const MERGED_OMO_CONFIG_DIAGNOSTIC_PATH:&str="(merged omo config)";
fn parts(d:&SenpiConfigDiagnostic)->(&str,&str,&str) { match d { SenpiConfigDiagnostic::Config(d)=>(d.kind,&d.path,&d.message),SenpiConfigDiagnostic::Model(d)=>(d.kind,&d.path,&d.message) } }
fn fingerprint(d:&SenpiConfigDiagnostic)->String { let (kind,path,message)=parts(d);format!("{:x}",Sha256::digest(format!("{kind}\0{path}\0{message}"))) }
fn config_directory(path:&Path,user:&Path)->Option<PathBuf> { if path.starts_with(user) { return Some(user.to_owned()); } path.ancestors().find(|p|p.file_name().is_some_and(|s|s==".omo")).map(Path::to_owned) }
fn attributable(d:&SenpiConfigDiagnostic,changed:&[PathBuf],user:&Path)->bool {
    let (kind,path,_)=parts(d);if path==MERGED_OMO_CONFIG_DIAGNOSTIC_PATH||kind=="model_catalog_cycle" { return true; }
    let path=Path::new(path);let directory=config_directory(path,user);
    changed.iter().any(|changed|path.starts_with(changed)||(directory.is_some()&&config_directory(changed,user)==directory))
}
pub enum ConfigWatchValidation { Ok,Rejected{errors:Vec<String>} }
pub struct OmoConfigValidator { cwd:String,env:BTreeMap<String,String>,user:PathBuf,baseline:BTreeSet<String>,unresolved:BTreeSet<String> }
impl OmoConfigValidator {
    pub fn new(cwd:String,env:BTreeMap<String,String>)->Self {
        let user=PathBuf::from(omo_config_core::resolve_user_omo_config_directory(&env));
        let diagnostics=load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions{cwd:Some(cwd.clone()),env:Some(env.clone()),..Default::default()}).diagnostics;
        Self{cwd,env,user,baseline:diagnostics.iter().map(fingerprint).collect(),unresolved:BTreeSet::new()}
    }
    pub fn validate(&mut self,changed:&[PathBuf])->ConfigWatchValidation {
        let diagnostics=load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions{cwd:Some(self.cwd.clone()),env:Some(self.env.clone()),..Default::default()}).diagnostics;
        let by_fingerprint:BTreeMap<_,_>=diagnostics.iter().map(|d|(fingerprint(d),d)).collect();
        self.unresolved.retain(|f|by_fingerprint.contains_key(f));
        for d in &diagnostics { let f=fingerprint(d);if !self.baseline.contains(&f)&&attributable(d,changed,&self.user) { self.unresolved.insert(f); } }
        if !self.unresolved.is_empty() { return ConfigWatchValidation::Rejected{errors:self.unresolved.iter().filter_map(|f|by_fingerprint.get(f)).map(|d|parts(d).2.to_owned()).collect()}; }
        self.baseline=by_fingerprint.into_keys().collect();ConfigWatchValidation::Ok
    }
}
