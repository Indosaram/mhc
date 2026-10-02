use std::{io,path::{Path,PathBuf},time::{Duration,SystemTime}};
use serde_json::{Value,json};
use tokio::io::AsyncWriteExt;

fn valid_uuid_v4(value:&str)->bool {
    let bytes=value.as_bytes();bytes.len()==36 && [8,13,18,23].into_iter().all(|index|bytes[index]==b'-')
        && bytes.iter().enumerate().all(|(index,byte)|[8,13,18,23].contains(&index) || byte.is_ascii_hexdigit())
        && bytes[14]==b'4' && matches!(bytes[19].to_ascii_lowercase(),b'8'|b'9'|b'a'|b'b')
}
async fn read_id(path:&Path)->io::Result<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(text)=>{let id=maho_ai::utils::js::trim(&text);Ok(valid_uuid_v4(id).then(||id.to_owned()))},
        Err(error) if error.kind()==io::ErrorKind::NotFound=>Ok(None),Err(error)=>Err(error),
    }
}
async fn write_private(path:&Path,text:&str,exclusive:bool)->io::Result<()> {
    let mut options=tokio::fs::OpenOptions::new();options.write(true).mode(0o600);
    if exclusive {options.create_new(true);} else {options.create(true).truncate(true);}
    options.open(path).await?.write_all(text.as_bytes()).await
}
async fn unlink(path:&Path)->io::Result<()> {
    match tokio::fs::remove_file(path).await {Ok(())=>Ok(()),Err(error) if matches!(error.kind(),io::ErrorKind::NotFound|io::ErrorKind::NotADirectory)=>Ok(()),Err(error)=>Err(error)}
}
async fn remove_empty(path:&Path)->io::Result<()> {
    match tokio::fs::remove_dir(path).await {Ok(())=>Ok(()),Err(error) if matches!(error.kind(),io::ErrorKind::NotFound|io::ErrorKind::NotADirectory|io::ErrorKind::DirectoryNotEmpty)=>Ok(()),Err(error)=>Err(error)}
}
enum LockFile {Missing,Invalid,Metadata(u64)}
async fn lock_file(path:&Path)->io::Result<LockFile> {
    let text=match tokio::fs::read_to_string(path).await {Ok(text)=>text,Err(error) if error.kind()==io::ErrorKind::NotFound=>return Ok(LockFile::Missing),Err(error) if error.kind()==io::ErrorKind::IsADirectory=>return Ok(LockFile::Invalid),Err(error)=>return Err(error)};
    let Ok(value)=serde_json::from_str::<Value>(maho_ai::utils::js::trim(&text)) else {return Ok(LockFile::Invalid);};
    let Some(pid)=value["pid"].as_f64().filter(|pid|*pid>0.0 && *pid<=9_007_199_254_740_991.0 && pid.fract()==0.0) else {return Ok(LockFile::Invalid);};
    if value["ownerToken"].as_str().is_none_or(str::is_empty) || !value["createdAtMs"].as_f64().is_some_and(|time|time.is_finite() && time>=0.0) {return Ok(LockFile::Invalid);}
    Ok(LockFile::Metadata(pid as u64))
}
async fn reclaim(path:&Path)->io::Result<()> {
    let metadata=match tokio::fs::metadata(path).await {Ok(metadata)=>metadata,Err(error) if error.kind()==io::ErrorKind::NotFound=>return Ok(()),Err(error)=>return Err(error)};
    let stale=SystemTime::now().duration_since(metadata.modified()?).is_ok_and(|age|age>=Duration::from_millis(500));
    if !metadata.is_dir() {
        match lock_file(path).await? {
            LockFile::Metadata(pid) if !super::daemon_process::process_is_live(pid).await?=>unlink(path).await?,
            LockFile::Invalid if stale=>unlink(path).await?,_=>{},
        }
        return Ok(());
    }
    let mut entries=match tokio::fs::read_dir(path).await {Ok(entries)=>entries,Err(error) if error.kind()==io::ErrorKind::NotFound=>return Ok(()),Err(error)=>return Err(error)};
    let mut markers=0;let mut invalid:Option<PathBuf>=None;let mut abandoned:Option<PathBuf>=None;
    while let Some(entry)=entries.next_entry().await? {
        let name=entry.file_name();let name=name.to_string_lossy();if !name.starts_with("owner-") || !name.ends_with(".json") {continue;}
        markers+=1;let marker=entry.path();match lock_file(&marker).await? {
            LockFile::Metadata(pid)=>{
                if super::daemon_process::process_is_live(pid).await? {return Ok(());}
                if abandoned.is_none() {abandoned=Some(marker);}
            },
            LockFile::Invalid=>if invalid.is_none() {invalid=Some(marker);},LockFile::Missing=>{},
        }
    }
    if let Some(marker)=abandoned.or_else(||stale.then_some(invalid).flatten()) {unlink(&marker).await?;remove_empty(path).await?;}
    else if markers==0 && stale {remove_empty(path).await?;}
    Ok(())
}
pub async fn ensure_installation_id(agent_dir:&Path)->io::Result<String> {
    let path=agent_dir.join("app-server").join("installation-id");let lock=path.with_file_name("installation-id.lock");
    for _ in 0..100 {
        if let Some(id)=read_id(&path).await? {return Ok(id);}
        tokio::fs::create_dir_all(agent_dir.join("app-server")).await?;
        let mut directory=tokio::fs::DirBuilder::new();directory.mode(0o700);
        match directory.create(&lock).await {
            Ok(())=>{
                let owner=uuid::Uuid::new_v4().to_string();let marker=lock.join(format!("owner-{owner}.json"));
                write_private(&marker,&format!("{}\n",json!({"ownerToken":owner,"pid":std::process::id(),"createdAtMs":chrono::Utc::now().timestamp_millis()})),true).await?;
                let result=async {
                    if let Some(id)=read_id(&path).await? {return Ok(id);}
                    let id=uuid::Uuid::new_v4().to_string();write_private(&path,&format!("{id}\n"),false).await?;Ok::<_,io::Error>(id)
                }.await;
                unlink(&marker).await?;remove_empty(&lock).await?;return result;
            },
            Err(error) if error.kind()==io::ErrorKind::AlreadyExists=>reclaim(&lock).await?,Err(error)=>return Err(error),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err(io::Error::other(format!("timed out waiting for installation id lock: {}",path.display())))
}
