use crate::types::HistoryEntry;
use serde_json::Value;
use std::{fs,io,path::{Path,PathBuf}};

fn directory(path:&Path)->io::Result<Vec<PathBuf>>{
    match fs::read_dir(path){Ok(entries)=>entries.map(|entry|entry.map(|entry|entry.path())).collect(),Err(error) if error.kind()==io::ErrorKind::NotFound=>Ok(Vec::new()),Err(error)=>Err(error)}
}
fn metadata(path:&Path)->io::Result<Option<fs::Metadata>>{match fs::metadata(path){Ok(meta)=>Ok(Some(meta)),Err(error) if error.kind()==io::ErrorKind::NotFound=>Ok(None),Err(error)=>Err(error)}}
fn files(path:&Path)->io::Result<Vec<PathBuf>>{
    let mut files=Vec::new();for path in directory(path)?{if path.extension().is_some_and(|ext|ext=="jsonl")&&metadata(&path)?.is_some_and(|meta|meta.is_file()){files.push(path);}}Ok(files)
}
pub fn index_sessions(root:&Path)->io::Result<Vec<HistoryEntry>>{
    let mut discovered=files(root)?;
    for path in directory(root)?{
        if path.extension().is_some_and(|ext|ext=="jsonl"){continue;}
        if metadata(&path)?.is_some_and(|meta|meta.is_dir()){discovered.extend(files(&path)?);}
    }
    discovered.sort_by(|left,right|right.file_name().cmp(&left.file_name()));
    let mut entries=Vec::new();
    for path in discovered{
        let text=fs::read_to_string(&path)?;let lines:Vec<&str>=text.split('\n').filter(|line|!line.is_empty()).collect();
        let Some(first)=lines.first()else{continue};let header:Value=serde_json::from_str(first).unwrap_or(Value::Null);
        let valid_header=header.get("type").and_then(Value::as_str)==Some("session");
        let id=if valid_header{header.get("id").and_then(Value::as_str)}else{None}.map(str::to_owned).unwrap_or_else(||path.file_stem().unwrap_or_default().to_string_lossy().into_owned());
        let cwd=if valid_header{header.get("cwd").and_then(Value::as_str).unwrap_or("")}else{""};
        for line in lines.iter().skip(1).rev(){
            let Ok(parsed)=serde_json::from_str::<Value>(line)else{continue};
            if parsed.get("type").and_then(Value::as_str)!=Some("message"){continue;}
            let message=&parsed["message"];if message.get("role").and_then(Value::as_str)!=Some("user"){continue;}
            let content=&message["content"];
            let text=match content {Value::String(text)=>text.clone(),Value::Array(parts)=>parts.iter().filter(|part|part.get("type").and_then(Value::as_str)==Some("text")).filter_map(|part|part.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),_=>continue};
            if text.trim().is_empty()||["[SYSTEM DIRECTIVE","[system:","[SYSTEM"].iter().any(|prefix|text.trim_start().starts_with(prefix)){continue;}
            let Some(timestamp)=parsed.get("timestamp").and_then(Value::as_str).and_then(|text|chrono::DateTime::parse_from_rfc3339(text).ok()).map(|time|time.timestamp_millis())else{continue};
            entries.push(HistoryEntry{text,session_id:id.clone(),session_file:path.to_string_lossy().into_owned(),cwd:cwd.into(),timestamp});
            if entries.len()>=10_000{break;}
        }
        if entries.len()>=10_000{break;}
    }
    let mut unique:Vec<HistoryEntry>=Vec::new();for entry in entries{if let Some(existing)=unique.iter_mut().find(|existing|existing.text==entry.text){if entry.timestamp>existing.timestamp{*existing=entry;}}else{unique.push(entry);}}
    unique.sort_by_key(|entry|std::cmp::Reverse(entry.timestamp));Ok(unique)
}
