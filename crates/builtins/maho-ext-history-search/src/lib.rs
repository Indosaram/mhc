pub mod types;
pub mod filter;
pub mod indexer;
pub mod overlay;

use std::path::{Path,PathBuf};
pub fn resolve_search_root(current:&Path,default:&Path)->PathBuf {
    let normalize=|path:&Path|{
        let absolute=if path.is_absolute(){path.to_owned()}else{std::env::current_dir().expect("working directory available").join(path)};
        let mut result=PathBuf::new();for component in absolute.components(){match component{std::path::Component::CurDir=>{},std::path::Component::ParentDir=>{result.pop();},component=>result.push(component.as_os_str())}}result
    };
    let default=normalize(default);
    if current.as_os_str().is_empty(){return default;}
    let current=normalize(current);
    if current.starts_with(&default){default}else{current}
}
pub fn resolve_search_root_windows(current:&str,default:&str)->String{
    let normalize=|input:&str|{let input=input.replace('/',"\\");let mut parts=Vec::new();for part in input.split('\\'){match part{""|"."=>{},".."=>{if parts.len()>1{parts.pop();}},part=>parts.push(part)}}parts.join("\\")};
    let default=normalize(default);if current.is_empty(){return default;}let current=normalize(current);
    if current.eq_ignore_ascii_case(&default)||current.to_lowercase().starts_with(&format!("{}\\",default.to_lowercase())){default}else{current}
}

