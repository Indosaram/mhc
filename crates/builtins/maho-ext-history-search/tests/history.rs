use maho_ext_history_search::{indexer::index_sessions,filter::filter_history,types::HistoryEntry,resolve_search_root};
use serde_json::json;
use std::path::Path;
fn entry(text:&str,time:i64)->HistoryEntry{HistoryEntry{text:text.into(),timestamp:time,session_id:"s".into(),session_file:"s.jsonl".into(),cwd:"/repo".into()}}
fn write(root:&Path,name:&str,texts:&[(&str,i64)]){
    std::fs::create_dir_all(root).expect("mkdir");
    let mut lines=vec![json!({"type":"session","id":name,"cwd":"/repo"}).to_string()];
    for (text,time) in texts {lines.push(json!({"type":"message","timestamp":chrono::DateTime::from_timestamp_millis(*time).expect("timestamp").to_rfc3339(),"message":{"role":"user","content":[{"type":"text","text":text}]}}).to_string());}
    std::fs::write(root.join(name),lines.join("\n")).expect("fixture");
}
#[test]
fn missing_and_empty(){let d=tempfile::tempdir().expect("dir");assert!(index_sessions(&d.path().join("missing")).expect("index").is_empty());assert!(index_sessions(d.path()).expect("index").is_empty());}
#[test]
fn single_prompt(){let d=tempfile::tempdir().expect("dir");write(d.path(),"s.jsonl",&[("ship it",2000)]);let r=index_sessions(d.path()).expect("index");assert_eq!(r[0].text,"ship it");assert_eq!(r[0].timestamp,2000);}
#[test]
fn injected_ignored(){let d=tempfile::tempdir().expect("dir");write(d.path(),"s.jsonl",&[("[SYSTEM DIRECTIVE: hidden]",1),("[system:foo]",2),("[SYSTEM hidden]",3),(" \n\t",4),("visible",5)]);assert_eq!(index_sessions(d.path()).expect("index").iter().map(|entry|entry.text.as_str()).collect::<Vec<_>>(),vec!["visible"]);}
#[test]
fn newest_deduplication(){let d=tempfile::tempdir().expect("dir");write(d.path(),"old.jsonl",&[("duplicate",2)]);write(d.path(),"new.jsonl",&[("duplicate",4),("latest",5)]);let r=index_sessions(d.path()).expect("index");assert_eq!(r.len(),2);assert_eq!(r[0].text,"latest");assert_eq!(r[1].session_id,"new.jsonl");}
#[test]
fn cap_keeps_recent(){let d=tempfile::tempdir().expect("dir");let strings:Vec<String>=(0..10005).map(|n|format!("prompt {n}")).collect();let texts:Vec<(&str,i64)>=strings.iter().enumerate().map(|(i,text)|(text.as_str(),i64::try_from(i).expect("small index"))).collect();write(d.path(),"bulk.jsonl",&texts);let r=index_sessions(d.path()).expect("index");assert_eq!(r.len(),10000);assert_eq!(r[0].text,"prompt 10004");assert!(!r.iter().any(|e|e.text=="prompt 0"));}
#[test]
fn legacy_string(){let d=tempfile::tempdir().expect("dir");let text=[json!({"type":"session","id":"legacy"}).to_string(),json!({"type":"message","timestamp":"2026-05-20T00:00:00Z","message":{"role":"user","content":"legacy string prompt"}}).to_string()].join("\n");std::fs::write(d.path().join("legacy.jsonl"),text).expect("fixture");assert_eq!(index_sessions(d.path()).expect("index")[0].text,"legacy string prompt");}
#[test]
fn top_level(){let d=tempfile::tempdir().expect("dir");write(d.path(),"flat.jsonl",&[("flat",1)]);assert_eq!(index_sessions(d.path()).expect("index")[0].text,"flat");}
#[test]
fn cwd_subdirectories(){let d=tempfile::tempdir().expect("dir");write(&d.path().join("aaa"),"20260101_old.jsonl",&[("old",1)]);write(&d.path().join("zzz"),"20260901_new.jsonl",&[("fresh",2)]);assert_eq!(index_sessions(d.path()).expect("index")[0].text,"fresh");}
#[test]
fn filter_empty_and_case(){let entries=vec![entry("Newest prompt",3),entry("Deploy production",2),entry("older",1)];assert_eq!(filter_history(&entries,""),entries);assert_eq!(filter_history(&entries,"DProd")[0].text,"Deploy production");}
#[test]
fn filter_tighter(){let entries=vec![entry("deploy dev prod",2),entry("deploy production",1)];assert_eq!(filter_history(&entries,"dprod")[0].text,"deploy production");}
#[test]
fn root_empty(){assert_eq!(resolve_search_root(Path::new(""),Path::new("/sessions")),Path::new("/sessions"));}
#[test]
fn root_same(){assert_eq!(resolve_search_root(Path::new("/sessions"),Path::new("/sessions")),Path::new("/sessions"));}
#[test]
fn root_descendant(){assert_eq!(resolve_search_root(Path::new("/sessions/cwd"),Path::new("/sessions")),Path::new("/sessions"));}
#[test]
fn root_custom(){assert_eq!(resolve_search_root(Path::new("/custom"),Path::new("/sessions")),Path::new("/custom"));}
#[test]
fn root_windows(){let root="C:\\Users\\u\\.senpi\\agent\\sessions";assert_eq!(maho_ext_history_search::resolve_search_root_windows(&format!("{root}\\encoded-cwd"),root),root);assert_eq!(maho_ext_history_search::resolve_search_root_windows("D:\\other\\sessions",root),"D:\\other\\sessions");}
