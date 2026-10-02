use std::{collections::{BTreeMap,BTreeSet},path::{Path,PathBuf}};
use super::{constants::*,scanner::{scan_rule_files,ScanOptions,ScannedFile},tool_paths::resolve_path,types::RuleCandidate};
#[derive(Default)]
pub struct RuleDiscoveryCache{pub scanned_rule_files:BTreeMap<PathBuf,Vec<ScannedFile>>,pub single_file_info:BTreeMap<PathBuf,Option<PathBuf>>}
pub struct FinderOptions<'a>{pub project_root:Option<&'a Path>,pub target_file:Option<&'a Path>,pub home_dir:Option<&'a Path>,pub disabled_sources:Option<&'a BTreeSet<String>>,pub skip_user_home:bool,pub cache:Option<&'a mut RuleDiscoveryCache>}
pub fn find_rule_candidates(mut options:FinderOptions<'_>)->Vec<RuleCandidate>{
    let mut result=Vec::new();
    let disabled=|source:&str|options.disabled_sources.is_some_and(|set|set.contains(source));
    if let Some(root)=options.project_root{
        let root=absolute(root);let mut walk=vec![(root.clone(),0)];
        if let Some(target)=options.target_file{
            let target=absolute(target);let mut current=target.parent().unwrap_or(&target).to_owned();
            if current.starts_with(&root){walk.clear();let mut distance=0;loop{walk.push((current.clone(),distance));if current==root{break;}let Some(parent)=current.parent()else{break;};current=parent.to_owned();distance+=1;}}
        }
        for (directory,distance) in &walk{
            for (parent,subdir) in PROJECT_RULE_SUBDIRS{
                let source=format!("{parent}/{subdir}");if disabled(&source){continue;}
                for file in scanned(&directory.join(parent).join(subdir),&mut options.cache){result.push(candidate(&file.path,&root,&source,*distance,false,false));}
            }
        }
        for (directory,distance) in &walk{
            for source in PROJECT_SINGLE_FILES{
                if disabled(source){continue;}
                let path=directory.join(source);if single(&path,&mut options.cache).is_some(){result.push(candidate(&path,&root,source,*distance,false,true));}
            }
        }
    }
    if !options.skip_user_home{
        let home=options.home_dir.map(absolute).or_else(dirs::home_dir);
        if let Some(home)=home{
            for subdir in USER_HOME_RULE_SUBDIRS{
                let source=format!("~/{subdir}");if disabled(&source){continue;}
                for file in scanned(&home.join(subdir),&mut options.cache){result.push(candidate(&file.path,&home,&source,GLOBAL_DISTANCE,true,false));}
            }
            for name in USER_HOME_SINGLE_FILES{
                let source=format!("~/{name}");if disabled(&source){continue;}
                let path=home.join(name);if single(&path,&mut options.cache).is_some(){result.push(candidate(&path,&home,&source,GLOBAL_DISTANCE,true,true));break;}
            }
        }
    }
    result
}
fn absolute(path:&Path)->PathBuf{resolve_path(&std::path::absolute(path).unwrap_or_else(|_|path.to_owned()))}
fn scanned(path:&Path,cache:&mut Option<&mut RuleDiscoveryCache>)->Vec<ScannedFile>{
    if let Some(cache)=cache && let Some(files)=cache.scanned_rule_files.get(path){return files.clone();}
    let files=scan_rule_files(ScanOptions{root_dir:path,excluded_dirs:None,max_depth:None});
    if let Some(cache)=cache{cache.scanned_rule_files.insert(path.into(),files.clone());}files
}
fn single(path:&Path,cache:&mut Option<&mut RuleDiscoveryCache>)->Option<PathBuf>{
    if let Some(cache)=cache && let Some(info)=cache.single_file_info.get(path){return info.clone();}
    let info=path.is_file().then(||path.canonicalize().unwrap_or_else(|_|path.to_owned()));
    if let Some(cache)=cache{cache.single_file_info.insert(path.into(),info.clone());}info
}
fn candidate(path:&Path,root:&Path,source:&str,distance:usize,global:bool,single:bool)->RuleCandidate{
    RuleCandidate{path:path.to_string_lossy().into_owned(),real_path:path.canonicalize().unwrap_or_else(|_|path.to_owned()).to_string_lossy().into_owned(),source:source.into(),distance,is_global:global,is_single_file:single,relative_path:path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\',"/")}
}
