use regex::Regex;
use serde_json::{Map,Value};
use std::{fs::{self,OpenOptions},io::Write,path::{Path,PathBuf}};
use std::os::unix::fs::{OpenOptionsExt,PermissionsExt};

fn safe_text(value:&str)->Result<String,regex::Error> {
    let pattern=Regex::new(r#"(?i)((?:authorization\s*[:=]\s*(?:bearer|basic)\s+)|(?:bearer\s+)|(?:[?&](?:api[_-]?key|token|secret|password|auth(?:orization)?)=))[^\s&,"'}\]]+"#)?;
    let redacted=pattern.replace_all(value,"${1}[redacted]");
    if redacted.encode_utf16().count()<=200 {Ok(redacted.into_owned())} else {Ok(format!("{}...",redacted.chars().take(197).collect::<String>()))}
}
fn blocked(key:&str)->bool {matches!(key.to_ascii_lowercase().as_str(),"__proto__"|"constructor"|"prototype"|"header"|"headers"|"env"|"environment"|"authorization"|"credential"|"credentials"|"password"|"secret"|"token"|"apikey"|"api_key"|"clientsecret"|"client_secret")}
fn serialize_value(value:&Value)->Result<Value,regex::Error> {
    match value {
        Value::String(value)=>safe_text(value).map(Value::String),
        Value::Array(values)=>values.iter().map(serialize_value).collect::<Result<Vec<_>,_>>().map(Value::Array),
        Value::Object(values)=>values.iter().filter(|(key,_)|!blocked(key)).map(|(key,value)|serialize_value(value).map(|value|(key.clone(),value))).collect::<Result<Map<_,_>,_>>().map(Value::Object),
        Value::Null|Value::Bool(_)|Value::Number(_)=>Ok(value.clone()),
    }
}
pub fn format_line(timestamp:&str,level:&str,event:&str,data:Option<&Map<String,Value>>)->Result<String,Box<dyn std::error::Error+Send+Sync>> {
    let mut entry=Map::new();entry.insert("ts".into(),timestamp.into());entry.insert("level".into(),level.into());entry.insert("event".into(),safe_text(event)?.into());
    for (key,value) in data.into_iter().flatten() {
        if ["selector","candidate","currentSelector","originalSelector","from","to","model","chainKey","reason","skipReason","classification","thinkingLevel","durationMs","retryAfterMs","error","errorMessage","warning","trigger"].contains(&key.as_str())&&!blocked(key){entry.insert(key.clone(),serialize_value(value)?);}
    }
    Ok(serde_json::to_string(&entry)?)
}
pub struct FallbackLogger {path:PathBuf,max_bytes:u64,reported_write_failure:bool}
impl FallbackLogger {
    pub fn new(agent_dir:&Path,max_bytes:Option<u64>)->Self {Self{path:agent_dir.join("logs/fallback.log"),max_bytes:max_bytes.filter(|v|*v>0).unwrap_or(5*1024*1024),reported_write_failure:false}}
    pub fn log(&mut self,timestamp:&str,level:&str,event:&str,data:Option<&Map<String,Value>>) {
        if let Err(error)=self.write(timestamp,level,event,data)&&!self.reported_write_failure{self.reported_write_failure=true;eprintln!("Unable to write retry fallback debug log {error}");}
    }
    fn write(&self,timestamp:&str,level:&str,event:&str,data:Option<&Map<String,Value>>)->Result<(),Box<dyn std::error::Error+Send+Sync>> {
        let text=format!("{}\n",format_line(timestamp,level,event,data)?);
        if let Some(parent)=self.path.parent(){fs::create_dir_all(parent)?;fs::set_permissions(parent,fs::Permissions::from_mode(0o700))?;}
        match fs::metadata(&self.path) {
            Ok(meta) if meta.len()+text.len() as u64>self.max_bytes => {
                let rotated=self.path.with_extension("log.1");
                match fs::remove_file(&rotated){Ok(())=>{},Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},Err(error)=>return Err(error.into())}
                fs::rename(&self.path,&rotated)?;fs::set_permissions(rotated,fs::Permissions::from_mode(0o600))?;
            },
            Ok(_)=>{},Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},Err(error)=>return Err(error.into()),
        }
        let mut file=OpenOptions::new().create(true).append(true).mode(0o600).open(&self.path)?;file.write_all(text.as_bytes())?;file.set_permissions(fs::Permissions::from_mode(0o600))?;Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logs_scrub_nested_secrets_and_bearer_text() {
        let data=serde_json::json!({"headers":{"key":"secret"},"error":{"token":"hidden","detail":"Bearer test-secret"},"selector":"p/m"});
        let line=format_line("fixed","info","event",data.as_object()).expect("format");
        let parsed:Value=serde_json::from_str(&line).expect("parse");
        assert!(parsed.get("headers").is_none());assert!(parsed["error"].get("token").is_none());assert_eq!(parsed["selector"],"p/m");assert!(!line.contains("test-secret"));
    }
}
