use std::{collections::VecDeque, path::{Path,PathBuf}};

pub fn expand_home(input:&str,home:&Path)->PathBuf {
    if input == "~" || input == "$HOME" {return home.to_owned();}
    for prefix in ["~/","~\\","$HOME/","$HOME\\"] {if let Some(rest)=input.strip_prefix(prefix){return home.join(rest);}}
    PathBuf::from(input)
}
fn normalize(path:&Path)->PathBuf {
    let mut result=PathBuf::from("/");
    for part in path.to_string_lossy().split('/') {match part {""|"."=>{},".."=>{result.pop();},part=>result.push(part)}}
    result
}
pub fn resolve_without_open(path:&Path)->PathBuf {
    let absolute=if path.is_absolute(){path.to_owned()}else{std::env::current_dir().unwrap_or_else(|_|PathBuf::from("/")).join(path)};
    let mut pending:VecDeque<String>=absolute.to_string_lossy().split('/').filter(|part|!part.is_empty()).map(str::to_owned).collect();
    let mut resolved=PathBuf::from("/"); let mut hops=0;
    while let Some(part)=pending.pop_front(){
        if part == "." {continue;}
        if part == ".." {resolved.pop();continue;}
        let candidate=resolved.join(&part);
        match std::fs::symlink_metadata(&candidate){
            Ok(meta) if meta.file_type().is_symlink()=>{
                hops+=1;
                if hops>40{return normalize(&pending.iter().fold(candidate,|path,part|path.join(part)));}
                match std::fs::read_link(&candidate){
                    Ok(link)=>{if link.is_absolute(){resolved=PathBuf::from("/");} let parts:Vec<String>=link.to_string_lossy().split('/').filter(|part|!part.is_empty()).map(str::to_owned).collect();for part in parts.into_iter().rev(){pending.push_front(part);}}
                    Err(_)=>return normalize(&pending.iter().fold(candidate,|path,part|path.join(part))),
                }
            }
            Ok(_)=>resolved=candidate,
            Err(_)=>return normalize(&pending.iter().fold(candidate,|path,part|path.join(part))),
        }
    }
    resolved
}
pub fn is_external_path(input:&str,cwd:&Path,home:&Path)->bool {
    let cwd=resolve_without_open(cwd);let expanded=expand_home(input,home);
    let target=resolve_without_open(&if expanded.is_absolute(){expanded}else{cwd.join(expanded)});
    !target.starts_with(cwd)
}
pub fn extract_external_paths(command:&str,cwd:&Path,home:&Path)->Vec<String>{
    let mut tokens=Vec::new();let mut current=String::new();let mut quote=None;let mut escaped=false;
    for ch in command.chars(){
        if escaped{current.push(ch);escaped=false;continue;}
        if ch=='\\'{escaped=true;current.push(ch);continue;}
        if let Some(q)=quote{if ch==q{quote=None;}current.push(ch);continue;}
        if ch=='\''||ch=='"'{quote=Some(ch);current.push(ch);continue;}
        if ch==' '||ch=='\t'{if !current.is_empty(){tokens.push(std::mem::take(&mut current));}continue;}
        current.push(ch);
    }
    if !current.is_empty(){tokens.push(current);}
    tokens.into_iter().enumerate().filter_map(|(index,token)|{
        let token=if token.len()>=2&&((token.starts_with('"')&&token.ends_with('"'))||(token.starts_with('\'')&&token.ends_with('\''))){token[1..token.len()-1].to_owned()}else{token};
        let pathlike=token.starts_with('/')||token.starts_with('~')||token.starts_with("$HOME")||token.starts_with("./")||token.starts_with("../")||token.contains('/');
        if index==0&&!pathlike{return None;}
        let assignment=token.split_once('=').is_some_and(|(name,_)|{let mut chars=name.chars();chars.next().is_some_and(|ch|ch.is_ascii_alphabetic()||ch=='_')&&chars.all(|ch|ch.is_ascii_alphanumeric()||ch=='_')});
        if token.starts_with('-')||assignment||["|","||","&&",";","&","$(","${","`"].iter().any(|op|token.contains(op))||!pathlike{return None;}
        is_external_path(&token,cwd,home).then_some(token)
    }).collect()
}
