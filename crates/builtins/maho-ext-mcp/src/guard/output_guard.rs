use std::{collections::BTreeSet,io::Write,path::{Path,PathBuf},sync::{Mutex,OnceLock}};
use serde_json::{json,Value};
use crate::config_schema::OutputGuardSettings;
#[derive(Default)]
pub struct McpOutputArtifacts {files:Mutex<BTreeSet<PathBuf>>}
impl McpOutputArtifacts {
    pub fn track(&self,path:PathBuf) {self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(path);}
    pub fn cleanup(&self)->std::io::Result<()> {
        let files=std::mem::take(&mut *self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        for path in files {match std::fs::remove_file(path) {Ok(())=>(),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>(),Err(e)=>return Err(e)}} Ok(())
    }
}
fn legacy_artifacts()->&'static McpOutputArtifacts {static OWNER:OnceLock<McpOutputArtifacts>=OnceLock::new();OWNER.get_or_init(McpOutputArtifacts::default)}
pub fn cleanup_mcp_output_artifacts(owner:Option<&McpOutputArtifacts>)->std::io::Result<()> {owner.unwrap_or_else(||legacy_artifacts()).cleanup()}
pub struct McpOutputGuardOptions<'a> {pub agent_dir:&'a Path,pub artifacts:Option<&'a McpOutputArtifacts>,pub server:&'a str,pub output_guard:Option<&'a OutputGuardSettings>}
struct Payload {bytes:Vec<u8>,extension:&'static str,lines:usize,preview:String,summary:String}
pub fn apply_mcp_output_guard(content:&[Value],options:McpOutputGuardOptions<'_>)->Vec<Value> {
    let max_bytes=positive(options.output_guard.and_then(|s|s.max_bytes)).unwrap_or(51200.0);
    let max_lines=positive(options.output_guard.and_then(|s|s.max_lines)).unwrap_or(2000.0);
    let payload=payload(content);
    let bytes=f64::from(u32::try_from(payload.bytes.len()).unwrap_or(u32::MAX));
    let lines=f64::from(u32::try_from(payload.lines).unwrap_or(u32::MAX));
    if bytes<=max_bytes && lines<=max_lines {return content.to_vec();}
    let preview_lines=format!("{:.0}",(max_lines/2.0).floor().clamp(2.0,80.0)).parse::<usize>().unwrap_or(80);
    let preview_bytes=format!("{:.0}",(max_bytes/2.0).floor().clamp(1024.0,8192.0)).parse::<usize>().unwrap_or(8192);
    let preview=trim_bytes(&head_tail_lines(&payload.preview,preview_lines),preview_bytes);
    let result=(||->std::io::Result<PathBuf> {
        let dir=options.agent_dir.join("tmp/mcp-out");std::fs::create_dir_all(&dir)?;
        let safe:String=options.server.encode_utf16().map(|c|char::from_u32(u32::from(c)).filter(|c|c.is_ascii_alphanumeric() || *c=='_' || *c=='-').unwrap_or('_')).collect();
        let prefix=format!("{}-",if safe.is_empty(){"server"}else{&safe});
        let suffix=format!(".{}",payload.extension);
        let mut tmp=tempfile::Builder::new().prefix(&prefix).suffix(&suffix).tempfile_in(dir)?;
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;tmp.as_file().set_permissions(std::fs::Permissions::from_mode(0o600))?;}
        tmp.write_all(&payload.bytes)?;
        let (_,path)=tmp.keep().map_err(|e|e.error)?;
        options.artifacts.unwrap_or_else(||legacy_artifacts()).track(path.clone());Ok(path)
    })();
    let text=match result {
        Ok(path)=>format!("MCP tool output exceeded outputGuard; {}.\nFull output saved to: {}\nRead the file in chunks instead of loading the entire file at once.\nPreview:\n{preview}",payload.summary,path.display()),
        Err(error)=>format!("Warning: failed to write MCP output spill file: {error}\nMCP output truncated inline; {}.\nPreview:\n{preview}",payload.summary),
    };
    vec![json!({"type":"text","text":text})]
}
fn positive(value:Option<f64>)->Option<f64> {value.filter(|n|n.is_finite() && *n>0.0).map(f64::floor)}
fn payload(content:&[Value])->Payload {
    if content.len()==1 && let Some(binary)=binary_payload(&content[0]) {return binary;}
    let text=content.iter().map(|b|match b.get("type").and_then(Value::as_str) {
        Some("text")=>b.get("text").and_then(Value::as_str).unwrap_or("").to_owned(),
        Some("image"|"audio")=>format!("[{} binary output, {} bytes]",b.get("mimeType").and_then(Value::as_str).unwrap_or(""),decode(b.get("data").and_then(Value::as_str).unwrap_or("")).len()),
        _=>b.to_string(),
    }).collect::<Vec<_>>().join("\n");
    let lines=count_lines(&text);let bytes=text.as_bytes().to_vec();
    Payload {summary:format!("text output ({} bytes, {lines} lines)",bytes.len()),bytes,lines,extension:"txt",preview:text}
}
fn binary_payload(block:&Value)->Option<Payload> {
    match block.get("type").and_then(Value::as_str) {
        Some("image"|"audio")=>{
            let data=block.get("data")?.as_str()?;if data.is_empty(){return None;} let mime=block.get("mimeType")?.as_str()?;let bytes=decode(data);
            Some(Payload {extension:extension(mime),lines:1,preview:format!("[{mime} binary output, {} bytes]",bytes.len()),summary:format!("{mime} binary output ({} bytes)",bytes.len()),bytes})
        }
        Some("resource")=>{
            let resource=block.get("resource")?.as_object()?;
            let mime=resource.get("mimeType").and_then(Value::as_str).unwrap_or("application/octet-stream");
            if let Some(blob)=resource.get("blob").and_then(Value::as_str) {let bytes=decode(blob);return Some(Payload {extension:extension(mime),lines:1,preview:format!("[{mime} binary resource, {} bytes]",bytes.len()),summary:format!("{mime} binary resource ({} bytes)",bytes.len()),bytes});}
            let text=resource.get("text")?.as_str()?;let lines=count_lines(text);
            Some(Payload {bytes:text.as_bytes().to_vec(),extension:extension(mime),lines,preview:text.into(),summary:format!("{mime} resource ({} bytes, {lines} lines)",text.len())})
        }
        _=>None,
    }
}
fn decode(data:&str)->Vec<u8> {
    let mut decoded=Vec::new();let mut bits=0u32;let mut count=0;
    for byte in data.bytes() {
        let digit=match byte {b'A'..=b'Z'=>byte-b'A',b'a'..=b'z'=>byte-b'a'+26,b'0'..=b'9'=>byte-b'0'+52,b'+'|b'-'=>62,b'/'|b'_'=>63,b'='=>break,_=>continue};
        bits=(bits<<6)|u32::from(digit);count+=6;
        if count>=8 {count-=8;decoded.push((bits>>count).to_le_bytes()[0]);bits&=(1<<count)-1;}
    }
    decoded
}
fn count_lines(text:&str)->usize {if text.is_empty(){0}else{text.split('\n').count()}}
fn extension(mime:&str)->&'static str {match mime {"image/png"=>"png","image/jpeg"=>"jpg","image/webp"=>"webp","image/gif"=>"gif","audio/mpeg"=>"mp3","audio/wav"=>"wav","application/json"=>"json","application/pdf"=>"pdf",m if m.starts_with("text/")=>"txt",_=>"bin"}}
fn head_tail_lines(text:&str,max:usize)->String {
    let lines:Vec<_>=text.split('\n').collect();if lines.len()<=max{return text.into();}
    let head=(max/2).max(1);let tail=(max-head).max(1);
    lines[..head].iter().copied().chain(std::iter::once("[... truncated ...]")).chain(lines[lines.len()-tail..].iter().copied()).collect::<Vec<_>>().join("\n")
}
fn trim_bytes(text:&str,max:usize)->String {
    if text.len()<=max{return text.into();}
    let units:Vec<u16>=text.encode_utf16().collect();
    let mut prefix_bytes=Vec::with_capacity(units.len()+1);prefix_bytes.push(0usize);
    for (index,unit) in units.iter().enumerate() {
        let previous=prefix_bytes[index];
        let bytes=if (0xdc00..=0xdfff).contains(unit) && index>0 && (0xd800..=0xdbff).contains(&units[index-1]) {previous+1}
            else {previous+if *unit<0x80 {1}else if *unit<0x800 {2}else {3}};
        prefix_bytes.push(bytes);
    }
    let mut end=units.len();
    while end>0 && prefix_bytes[end]+20>max {
        end=end.saturating_sub(256);
    }
    let result=String::from_utf16_lossy(&units[..end]);
    format!("{result}\n[... truncated ...]")
}
