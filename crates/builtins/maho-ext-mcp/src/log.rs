use std::{collections::{BTreeSet,VecDeque},io::Write,path::{Path,PathBuf}};
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use fancy_regex::Regex;
pub fn fingerprint_secret(secret:&str)->String {format!("{:x}",Sha256::digest(secret))[..8].into()}
fn redaction(secret:&str,secrets:&mut BTreeSet<String>)->String {
    if !secret.is_empty() && !secret.starts_with("<redacted:"){secrets.insert(secret.into());}
    format!("<redacted:{}>",fingerprint_secret(secret))
}
pub fn redact_mcp_log_text(text:&str)->Result<String,Box<fancy_regex::Error>> {redact_text(text,&mut BTreeSet::new())}
static REDACTION_PATTERNS:std::sync::LazyLock<[Regex;4]>=std::sync::LazyLock::new(||[
    r#"(?i)((?:"|')?\bAuthorization(?:"|')?\s*[:=]\s*(?:"|')?(?:[A-Za-z][A-Za-z0-9+.-]*\s+)?)(?!<redacted:)([^"',\s}\]&]+)"#,
    r"(?i)(\bBearer\s+)(?!<redacted:)([A-Za-z0-9._~+/=-]+)",
    r#"(?i)([?&][^=\s&"'<>]*(?:key|token|secret|password|client_secret|auth)[^=\s&"'<>]*=)([^&\s"'<>]+)"#,
    r#"(?i)((?:"|')?[^"'\s:=,{}]*(?:api[_-]?key|key|token|secret|password|client_secret|auth)[^"'\s:=,{}]*(?:"|')?\s*[:=]\s*(?:"|')?)([^"',\s}\]&]+)"#,
].map(|pattern|Regex::new(pattern).expect("redaction patterns are literals")));
static AUTHORIZATION_PREFIX:std::sync::LazyLock<Regex>=std::sync::LazyLock::new(||Regex::new(r"(?i)\bAuthorization\b").expect("authorization pattern is a literal"));
static SENSITIVE_KEY:std::sync::LazyLock<Regex>=std::sync::LazyLock::new(||Regex::new(r"(?i)(?:^|[_-])(?:api[_-]?key|key|token|secret|password|client_secret|auth|authorization)(?:$|[_-])").expect("sensitive-key pattern is a literal"));
fn redact_text(text:&str,secrets:&mut BTreeSet<String>)->Result<String,Box<fancy_regex::Error>> {
    let mut text=text.to_owned();
    for (index,regex) in REDACTION_PATTERNS.iter().enumerate() {
        let mut output=String::new();let mut end=0;
        for capture in regex.captures_iter(&text) {
            let capture=capture?;let Some(whole)=capture.get(0) else{continue;};
            let prefix=capture.get(1).map_or("",|m|m.as_str());let secret=capture.get(2).map_or("",|m|m.as_str());
            output.push_str(&text[end..whole.start()]);
            if index==3 && (secret.starts_with("<redacted:") || AUTHORIZATION_PREFIX.is_match(prefix)?) {output.push_str(whole.as_str());}
            else {output.push_str(prefix);output.push_str(&redaction(secret,secrets));}
            end=whole.end();
        }
        output.push_str(&text[end..]);text=output;
    }
    Ok(text)
}
fn redact_data(value:&Value,secrets:&mut BTreeSet<String>)->Result<Value,Box<fancy_regex::Error>> {
    match value {
        Value::String(text)=>Ok(Value::String(redact_text(text,secrets)?)),
        Value::Array(items)=>items.iter().map(|v|redact_data(v,secrets)).collect::<Result<Vec<_>,_>>().map(Value::Array),
        Value::Object(map)=>{
            let sensitive=&*SENSITIVE_KEY;
            let mut output=serde_json::Map::new();
            for (key,item) in map {
                let redacted=if sensitive.is_match(key)? {Value::String(redaction(&item.as_str().map_or_else(||item.to_string(),str::to_owned),secrets))}else{redact_data(item,secrets)?};
                output.insert(key.clone(),redacted);
            }
            Ok(Value::Object(output))
        }
        _=>Ok(value.clone()),
    }
}
const REDACTED_VALUE:&str="***REDACTED***";
/// Rewrites one URL: query parameters whose key contains `key`, `token` or `secret` (case-insensitive)
/// get the value `***REDACTED***`; everything else is left byte-for-byte unchanged.
pub fn redact_url(url:&str)->String {
    let Some((head,rest))=url.split_once('?') else{return url.into();};
    let (query,fragment)=match rest.split_once('#') {Some((query,fragment))=>(query,Some(fragment)),None=>(rest,None)};
    let query=query.split('&').map(|pair|{
        let Some((name,_))=pair.split_once('=') else{return pair.to_owned();};
        let decoded=url::form_urlencoded::parse(name.as_bytes()).next().map_or_else(||name.to_owned(),|(key,_)|key.into_owned()).to_lowercase();
        if ["key","token","secret"].iter().any(|word|decoded.contains(word)) {format!("{name}={REDACTED_VALUE}")}else{pair.to_owned()}
    }).collect::<Vec<_>>().join("&");
    match fragment {Some(fragment)=>format!("{head}?{query}#{fragment}"),None=>format!("{head}?{query}")}
}
static URL_TOKEN:std::sync::LazyLock<regex::Regex>=std::sync::LazyLock::new(||regex::Regex::new(r#"(?i)https?://[^\s"'<>)}\]]+"#).expect("url pattern is a literal"));
/// Applies [`redact_url`] to every `http://` / `https://` token in `text`.
pub fn redact_urls_in_text(text:&str)->String {URL_TOKEN.replace_all(text,|captures:&regex::Captures|redact_url(&captures[0])).into_owned()}
pub fn map_mcp_log_level(level:&str)->(&'static str,u8) {
    match level {"emergency"=>("emergency",0),"alert"=>("alert",1),"critical"|"crit"=>("critical",2),"error"|"err"=>("error",3),"warning"|"warn"=>("warning",4),"notice"=>("notice",5),"debug"=>("debug",7),_=>("info",6)}
}
pub struct McpLogger {pub file_path:PathBuf,server:String,max_file_bytes:usize,ring:VecDeque<String>,file_retry_at:Option<std::time::Instant>,file_mode_set:bool,clock:LogClock}
/// How long the file sink stays skipped after a write failure.
pub const FILE_SINK_RETRY_DELAY:std::time::Duration=std::time::Duration::from_secs(5);
/// Monotonic time source; tests inject their own to drive the retry window without sleeping.
pub type LogClock=std::sync::Arc<dyn Fn()->std::time::Instant+Send+Sync>;
impl McpLogger {
    pub fn new(server:&str,log_dir:&Path,max_file_bytes:Option<usize>)->Result<Self,regex::Error> {
        let safe=regex::Regex::new(r"[^A-Za-z0-9._-]+")?.replace_all(server,"_");let safe=if safe.is_empty(){"server"}else{safe.as_ref()};
        Ok(Self {file_path:log_dir.join(format!("{safe}.log")),server:server.into(),max_file_bytes:max_file_bytes.unwrap_or(1024*1024),ring:VecDeque::new(),file_retry_at:None,file_mode_set:false,clock:std::sync::Arc::new(std::time::Instant::now)})
    }
    pub fn set_clock(&mut self,clock:LogClock) {self.clock=clock;}
    pub fn get_ring_buffer(&self)->Vec<String> {self.ring.iter().cloned().collect()}
    fn push_ring(&mut self,line:String) {self.ring.push_back(line);while self.ring.len()>200{self.ring.pop_front();}}
    fn format_line(&self,level:&str,message:&str,data:Option<&Value>,channel:&str)->Result<String,Box<fancy_regex::Error>> {
        let mut secrets=BTreeSet::new();let data=data.map(|data|redact_data(data,&mut secrets)).transpose()?;let (level,severity)=map_mcp_log_level(level);
        let message=redact_text(message,&mut secrets)?;
        let mut entry=json!({"timestamp":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"server":self.server,"level":level,"severity":severity,"channel":channel,"message":message});
        if let Some(data)=data {entry["data"]=data;}
        let mut text=entry.to_string();let mut secrets=secrets.into_iter().collect::<Vec<_>>();secrets.sort_by_key(|s|std::cmp::Reverse(s.len()));
        for secret in secrets {text=text.replace(&secret,&format!("<redacted:{}>",fingerprint_secret(&secret)));}Ok(text)
    }
    pub fn log(&mut self,level:&str,message:&str,data:Option<&Value>,channel:Option<&str>)->Result<(),Box<fancy_regex::Error>> {
        let line=self.format_line(level,message,data,channel.unwrap_or("server"))?;self.push_ring(line.clone());
        let now=(self.clock)();
        if self.file_retry_at.is_none_or(|at|now>=at) && self.write_file(&line).is_err() {self.file_retry_at=Some(now+FILE_SINK_RETRY_DELAY);let warning=self.format_line("warning","file sink disabled after write failure",None,"file")?;self.push_ring(warning);}
        Ok(())
    }
    fn write_file(&mut self,line:&str)->std::io::Result<()> {
        let parent=self.file_path.parent().ok_or_else(||std::io::Error::other("log parent missing"))?;
        let mut builder=std::fs::DirBuilder::new();builder.recursive(true);
        #[cfg(unix)] {use std::os::unix::fs::DirBuilderExt;builder.mode(0o700);}
        builder.create(parent)?;
        let current_len=match std::fs::metadata(&self.file_path) {Ok(metadata)=>Some(metadata.len()),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>None,Err(e)=>return Err(e)};
        if self.max_file_bytes>0 && current_len.is_some_and(|len|len+u64::try_from(line.len()+1).unwrap_or(u64::MAX)>u64::try_from(self.max_file_bytes).unwrap_or(u64::MAX)) {
            let rotated=PathBuf::from(format!("{}.1",self.file_path.display()));
            match std::fs::remove_file(&rotated){Ok(())=>(),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>(),Err(e)=>return Err(e)}
            std::fs::rename(&self.file_path,&rotated)?;set_file_permissions(&rotated)?;self.file_mode_set=false;
        }
        // Chmod once per file instance (new, rotated-away or pre-existing) instead of on every line.
        if current_len.is_none() {self.file_mode_set=false;}
        let mut options=std::fs::OpenOptions::new();options.create(true).append(true);
        #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
        let mut file=options.open(&self.file_path)?;writeln!(file,"{line}")?;
        if !self.file_mode_set {set_file_permissions(&self.file_path)?;self.file_mode_set=true;}
        Ok(())
    }
}
fn set_file_permissions(path:&Path)->std::io::Result<()> {
    #[cfg(not(unix))] let _=path;
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(path,std::fs::Permissions::from_mode(0o600))?;}
    Ok(())
}
