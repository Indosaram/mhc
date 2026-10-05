use std::{collections::BTreeMap,path::PathBuf};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub enum SandboxPolicy{Required,Auto,Off}
#[derive(Clone,Debug,PartialEq,Eq)]pub struct SandboxSpawnArgs{pub command:String,pub args:Vec<String>,pub cwd:PathBuf,pub env:BTreeMap<String,String>}
#[derive(Debug)]pub enum SandboxError{Unavailable{platform:String,reason:String},Io(std::io::Error)}
impl std::fmt::Display for SandboxError{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{match self{Self::Unavailable{platform,reason}=>write!(f,"required reflection sandbox unavailable on {platform}: {reason}"),Self::Io(error)=>error.fmt(f)}}}
impl std::error::Error for SandboxError{}
pub struct SandboxTransform{pub was_sandboxed:bool,pub warning:Option<String>,pub(crate) executable:Option<String>,pub(crate) inner_command:String,pub(crate) profile:Option<String>,pub(crate) temp_dir:Option<PathBuf>,pub(crate) writable_dirs:Vec<PathBuf>}
impl SandboxTransform{
    pub fn apply(&self,mut args:SandboxSpawnArgs)->SandboxSpawnArgs{
        let Some(executable)=&self.executable else{return args;};let mut prefix=if let Some(profile)=&self.profile{vec!["-p".into(),profile.clone(),"--".into()]}else{let mut prefix=vec!["--ro-bind".into(),"/".into(),"/".into(),"--dev-bind".into(),"/dev".into(),"/dev".into(),"--tmpfs".into(),"/tmp".into()];for dir in &self.writable_dirs{prefix.extend(["--bind".into(),dir.to_string_lossy().into_owned(),dir.to_string_lossy().into_owned()]);}prefix.extend(["--chdir".into(),args.cwd.to_string_lossy().into_owned(),"--".into()]);prefix};prefix.push(self.inner_command.clone());prefix.extend(args.args);args.command=executable.clone();args.args=prefix;if let Some(dir)=&self.temp_dir{args.env.insert("TMPDIR".into(),dir.to_string_lossy().into_owned());}args
    }
}
