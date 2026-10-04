use serde_json::Value;
use std::path::{Path,PathBuf};

#[derive(Debug,PartialEq,Eq)]
pub enum SessionReference { Gist { id:String }, File { path:PathBuf }, Issue {owner:String,repo:String,issue:String} }
pub fn parse_ref(reference:&str,cwd:&Path)->Result<SessionReference,String> {
    if reference.ends_with(".html") || reference.ends_with(".jsonl") {
        let path=Path::new(reference);
        let path=if path.is_absolute(){path.to_owned()}else{cwd.join(path)};
        let mut normalized=PathBuf::new();
        for component in path.components() {match component {std::path::Component::CurDir=>{},std::path::Component::ParentDir=>{normalized.pop();},component=>normalized.push(component.as_os_str())}}
        return Ok(SessionReference::File {path:normalized});
    }
    for pattern in [r"^https://pi\.dev/session/#([0-9a-fA-F]{20,})(?:[/#?].*)?$",r"^https://gist\.github\.com/(?:[^/]+/)?([0-9a-fA-F]{20,})(?:[/#?].*)?$",r"^([0-9a-fA-F]{20,})$"] {
        if let Some(captures)=regex::Regex::new(pattern).expect("pinned reference pattern").captures(reference) {return Ok(SessionReference::Gist {id:captures[1].into()});}
    }
    if let Some(captures)=regex::Regex::new(r"^https://github\.com/([^/]+)/([^/]+)/issues/(\d+)(?:[/#?].*)?$").expect("pinned issue pattern").captures(reference) {
        return Ok(SessionReference::Issue {owner:captures[1].into(),repo:captures[2].into(),issue:captures[3].into()});
    }
    Err(format!("expected a gist ID, gist URL, pi.dev share URL, issue URL, .html file, or .jsonl file: {reference}"))
}

pub fn parse_session_jsonl(raw: &str) -> Result<Value, String> {
    let first_line = raw.split('\n').next().unwrap_or("");
    let header: Value = serde_json::from_str(first_line).map_err(|_| "first line of session file is not valid JSON".to_owned())?;
    if header.get("type").and_then(Value::as_str) != Some("session")
        || header.get("id").and_then(Value::as_str).is_none()
        || header.get("cwd").and_then(Value::as_str).is_none_or(str::is_empty) {
        return Err("session file has no valid session header with a cwd".into());
    }
    Ok(header)
}

pub fn decode_exported_html(html:&str)->Result<(Value,String),String> {
    use base64::{Engine,engine::general_purpose::STANDARD};
    let pattern=regex::Regex::new(r#"<script id="session-data" type="application/json">([^<]+)</script>"#).expect("session script");
    let captures=pattern.captures(html).ok_or("HTML does not contain embedded pi session data")?;
    let bytes=STANDARD.decode(&captures[1]).map_err(|_|"embedded pi session data is not valid JSON")?;
    let data:Value=serde_json::from_slice(&bytes).map_err(|_|"embedded pi session data is not valid JSON")?;
    let header=&data["header"];
    if header["type"]!="session"||!header["id"].is_string()||!header["cwd"].is_string() {return Err("embedded pi session data has no valid session header".into());}
    let entries=data["entries"].as_array().ok_or("embedded pi session data has no entries array")?;
    let mut lines=vec![header.to_string()];lines.extend(entries.iter().map(Value::to_string));
    Ok((header.clone(),format!("{}\n",lines.join("\n"))))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPlatform { Windows, Unix, Unknown }

pub fn detect_session_platform(cwd: &str) -> SessionPlatform {
    let bytes = cwd.as_bytes();
    let drive = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\');
    let msys = bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b'/';
    if drive || msys { SessionPlatform::Windows }
    else if cwd.starts_with('/') { SessionPlatform::Unix }
    else { SessionPlatform::Unknown }
}

pub fn rewrite_session_cwd(raw:&str,source:&str,target:&str)->String {
    let escape=|value:&str| {let encoded=serde_json::to_string(value).expect("string encoding");encoded[1..encoded.len()-1].to_owned()};
    let trimmed=source.trim_end_matches(['/', '\\']);
    let mut variants=vec![trimmed.to_owned()];
    let drive=regex::Regex::new(r"^([A-Za-z]):[\\/](.*)$").expect("drive");
    let msys=regex::Regex::new(r"^/([A-Za-z])/(.*)$").expect("msys");
    if let Some(parts)=drive.captures(trimmed).or_else(||msys.captures(trimmed)) {
        let drive=parts[1].to_ascii_uppercase();
        let rest=regex::Regex::new(r"[\\/]+").expect("separators").replace_all(&parts[2],"/").trim_matches('/').to_owned();
        variants.extend([format!("{drive}:\\{}",rest.replace('/',"\\")),format!("{drive}:/{rest}"),format!("/{}/{rest}",drive.to_ascii_lowercase()),format!("/{drive}/{rest}")]);
    }
    variants.sort_by_key(|value|std::cmp::Reverse(value.len()));variants.dedup();
    let target_encoded=escape(target);
    let mut rewritten=raw.to_owned();
    for variant in variants {if !variant.is_empty()&&variant!=target {rewritten=rewritten.replace(&escape(&variant),&target_encoded);}}
    let name=trimmed.rsplit(['/', '\\']).next().unwrap_or("");
    if regex::Regex::new(r"(?i)^pi-ci-[0-9a-f]{32}$").expect("CI directory").is_match(name) {
        for pattern in [format!(r#"[A-Za-z]:(?:[^"\r\n])*?{}"#,regex::escape(name)),format!(r#"/[A-Za-z]/(?:[^"\r\n])*?{}"#,regex::escape(name))] {
            rewritten=regex::Regex::new(&pattern).expect("CI rewrite").replace_all(&rewritten,regex::NoExpand(&target_encoded)).into_owned();
        }
    }
    rewritten
}

use maho_ext_api::*;
use std::sync::Arc;
pub type FetchText=Arc<dyn Fn(String)->ExtensionFuture<'static,String>+Send+Sync>;
pub struct ImportRepro{
    pub session_dir:Arc<dyn Fn(&ExtensionContext)->Result<PathBuf,ExtensionFailure>+Send+Sync>,
    pub fetch:FetchText,
}
pub fn github_fetch()->FetchText{
    let client=reqwest::Client::new();
    Arc::new(move|url|{let client=client.clone();Box::pin(async move{
        let response=client.get(&url).header("Accept","application/vnd.github+json").header("X-GitHub-Api-Version","2022-11-28").send().await.map_err(|error|ExtensionFailure::new(error.to_string()))?;
        if !response.status().is_success(){return Err(ExtensionFailure::new(format!("failed to fetch {url}: HTTP {}",response.status().as_u16())));}
        response.text().await.map_err(|error|ExtensionFailure::new(error.to_string()))
    })})
}
async fn read_gist_file(file:&Value,fetch:&FetchText)->Result<String,ExtensionFailure>{
    if file["truncated"]!=true&&let Some(content)=file["content"].as_str().filter(|content|!content.is_empty()){return Ok(content.into());}
    let url=file["raw_url"].as_str().ok_or_else(||ExtensionFailure::new(format!("gist file {} has no raw URL",file["filename"].as_str().unwrap_or("<unknown>"))))?;
    fetch(url.into()).await
}
async fn gist_session(id:&str,fetch:&FetchText)->Result<(Value,String),ExtensionFailure>{
    let gist:Value=serde_json::from_str(&fetch(format!("https://api.github.com/gists/{id}")).await?).map_err(|error|ExtensionFailure::new(error.to_string()))?;
    let files=gist["files"].as_object();
    for extension in [".jsonl",".html"]{
        if let Some(file)=files.and_then(|files|files.values().find(|file|file["filename"].as_str().is_some_and(|name|name.ends_with(extension)))){
            let raw=read_gist_file(file,fetch).await?;
            return if extension==".html"{decode_exported_html(&raw).map_err(ExtensionFailure::new)}else{Ok((parse_session_jsonl(&raw).map_err(ExtensionFailure::new)?,raw))};
        }
    }
    Err(ExtensionFailure::new(format!("gist {id} has no .jsonl or .html session file")))
}
async fn issue_gist(owner:&str,repo:&str,issue:&str,fetch:&FetchText)->Result<String,ExtensionFailure>{
    let encode=|value:&str|percent_encoding::utf8_percent_encode(value,percent_encoding::NON_ALPHANUMERIC).to_string();
    let pattern=regex::Regex::new(r"https://gist\.github\.com/(?:[^/\s]+/)?([0-9a-fA-F]{20,})\b").expect("gist link");
    let mut last=None;let mut page=1;
    loop{
        let comments:Vec<Value>=serde_json::from_str(&fetch(format!("https://api.github.com/repos/{}/{}/issues/{}/comments?per_page=100&page={page}",encode(owner),encode(repo),encode(issue))).await?).map_err(|error|ExtensionFailure::new(error.to_string()))?;
        for comment in &comments{if comment["user"]["login"]=="github-actions[bot]"{for matched in pattern.captures_iter(comment["body"].as_str().unwrap_or("")){last=Some(matched[1].to_owned());}}}
        if comments.len()<100{break;}page+=1;
    }
    last.ok_or_else(||ExtensionFailure::new(format!("no github-actions gist link found in comments on {owner}/{repo}#{issue}")))
}
impl Extension for ImportRepro{
    fn register(&self,api:&mut ExtensionApi){
        let session_dir=self.session_dir.clone();let fetch=self.fetch.clone();
        api.register_command_with_context("ir",Some("Import a CI issue-analysis session from a gist ID, share URL, or issue URL and switch to it".into()),None,Arc::new(move|args,ctx|{let session_dir=session_dir.clone();let fetch=fetch.clone();Box::pin(async move{
            if !ctx.is_idle()||ctx.is_compacting(){ctx.ui.notify("/ir is unavailable while the agent is working",NotificationType::Warning);return Ok(());}
            let reference=args.trim();if reference.is_empty(){ctx.ui.notify("Usage: /ir <gist-id | gist-url | pi.dev/session URL | issue URL>",NotificationType::Error);return Ok(());}
            let outcome=async{
                let directory=session_dir(ctx)?;let parsed=parse_ref(reference,&ctx.cwd).map_err(ExtensionFailure::new)?;
                ctx.ui.notify(&format!("Importing repro session from {reference}..."),NotificationType::Info);
                let (header,raw,filename)=match parsed{
                    SessionReference::Gist{id}=>{let (header,raw)=gist_session(&id,&fetch).await?;(header,raw,format!("{id}.jsonl"))}
                    SessionReference::Issue{owner,repo,issue}=>{let id=issue_gist(&owner,&repo,&issue,&fetch).await?;let (header,raw)=gist_session(&id,&fetch).await?;(header,raw,format!("{id}.jsonl"))}
                    SessionReference::File{path}=>{
                        let raw=std::fs::read_to_string(&path).map_err(|error|ExtensionFailure::new(error.to_string()))?;
                        let (header,raw)=if path.extension().is_some_and(|extension|extension=="html"){decode_exported_html(&raw).map_err(ExtensionFailure::new)?}else{(parse_session_jsonl(&raw).map_err(ExtensionFailure::new)?,raw)};
                        let filename=path.file_name().expect("session filename").to_string_lossy().into_owned();let filename=filename.strip_suffix(".html").map_or_else(||filename.clone(),|name|format!("{name}.jsonl"));
                        (header,raw,filename)
                    }
                };
                let source=header["cwd"].as_str().expect("validated header").to_owned();let target=ctx.cwd.to_string_lossy().into_owned();
                let destination=directory.join(filename);
                if destination.exists()&&!ctx.ui.confirm("Session already imported",&format!("Overwrite {}? Local changes to that session will be lost.",destination.display()),Default::default()).await{ctx.ui.notify("Import cancelled",NotificationType::Warning);return Ok(());}
                std::fs::write(&destination,rewrite_session_cwd(&raw,&source,&target)).map_err(|error|ExtensionFailure::new(error.to_string()))?;
                ctx.ui.notify(&format!("Imported session {} (cwd {source} -> {target})",header["id"].as_str().expect("validated id")),NotificationType::Info);
                let platform=detect_session_platform(&source);let local=if cfg!(windows){SessionPlatform::Windows}else{SessionPlatform::Unix};
                let with_session:WithSession=Arc::new(move|next|{let source=source.clone();let target=target.clone();Box::pin(async move{
                    if platform!=SessionPlatform::Unknown&&platform!=local{
                        let text=if local==SessionPlatform::Windows{"This session was continued on a Windows machine; paths are now Windows style."}else{"This session was continued on a non-Windows machine; paths are now Unix style."};
                        next.send_message(CustomMessage{custom_type:"import-repro".into(),content:vec![ToolContent::text(text)],display:true,details:Some(serde_json::json!({"sourceCwd":source,"targetCwd":target}))},SendMessageOptions::default()).await?;
                    }Ok(())
                })});
                ctx.switch_session(&destination.to_string_lossy(),SwitchSessionOptions{with_session:Some(with_session)}).await?;
                Ok::<(),ExtensionFailure>(())
            }.await;
            if let Err(error)=outcome{ctx.ui.notify(&format!("ir: {}",error.message),NotificationType::Error);}Ok(())
        })}));
    }
}

#[cfg(test)]
mod fetch_tests{
    use super::*;
    use std::sync::Mutex;
    #[tokio::test]
    async fn gist_prefers_jsonl_and_fetches_truncated_file(){
        let calls=Arc::new(Mutex::new(Vec::new()));let observed=calls.clone();
        let raw="{\"type\":\"session\",\"id\":\"fixture\",\"cwd\":\"/source\"}\n";
        let fetch:FetchText=Arc::new(move|url|{observed.lock().expect("calls").push(url.clone());Box::pin(async move{
            if url=="https://api.github.com/gists/fixture"{Ok(serde_json::json!({"files":{"html":{"filename":"session.html","content":"not selected"},"jsonl":{"filename":"session.jsonl","content":"ignored truncated","truncated":true,"raw_url":"https://fixture.invalid/raw"}}}).to_string())}
            else if url=="https://fixture.invalid/raw"{Ok(raw.into())}else{panic!("unexpected URL {url}")}
        })});
        let (header,decoded)=gist_session("fixture",&fetch).await.expect("gist");
        assert_eq!(header["id"],"fixture");assert_eq!(decoded,raw);
        assert_eq!(*calls.lock().expect("calls"),["https://api.github.com/gists/fixture","https://fixture.invalid/raw"]);
    }
    #[tokio::test]
    async fn issue_paginates_and_uses_last_actions_link_only(){
        let calls=Arc::new(Mutex::new(Vec::new()));let observed=calls.clone();
        let fetch:FetchText=Arc::new(move|url|{observed.lock().expect("calls").push(url.clone());Box::pin(async move{
            if url.ends_with("page=1"){
                let mut comments=vec![serde_json::json!({"user":{"login":"other"},"body":"https://gist.github.com/ffffffffffffffffffff"});100];
                comments[0]=serde_json::json!({"user":{"login":"github-actions[bot]"},"body":"https://gist.github.com/bot/aaaaaaaaaaaaaaaaaaaa"});Ok(serde_json::to_string(&comments).expect("comments"))
            }else if url.ends_with("page=2"){Ok(serde_json::json!([{"user":{"login":"github-actions[bot]"},"body":"https://gist.github.com/bot/bbbbbbbbbbbbbbbbbbbb https://gist.github.com/cccccccccccccccccccc"}]).to_string())}else{panic!("unexpected URL {url}")}
        })});
        assert_eq!(issue_gist("owner","repo","42",&fetch).await.expect("gist"),"cccccccccccccccccccc");
        assert_eq!(calls.lock().expect("calls").len(),2);
    }
    #[tokio::test]
    async fn remote_fetch_error_is_not_replaced_by_empty_session(){
        let fetch:FetchText=Arc::new(|_|Box::pin(async{Err(ExtensionFailure::new("transport failed"))}));
        assert_eq!(gist_session("fixture",&fetch).await.expect_err("fetch error").message,"transport failed");
    }
}
