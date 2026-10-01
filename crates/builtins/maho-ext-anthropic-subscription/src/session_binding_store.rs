use std::{path::{Path,PathBuf},io::Write,os::unix::fs::PermissionsExt};
#[derive(Clone,Debug,PartialEq,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct StoredBinding {
    pub schema_version:u8,pub session_path:String,pub session_id:String,pub marker_entry_id:String,pub sdk_session_id:String,
    pub sent_count:u64,pub sent_prefix_hash:String,pub assistant_content_hash:String,pub last_assistant_uuid:Option<String>,
    pub account_name:String,pub model_id:String,pub system_prompt_hash:String,pub toolset_hash:String,
}
impl StoredBinding {
    pub fn validate(&self)->anyhow::Result<()> {
        if self.schema_version!=1||self.sent_count>9007199254740991 {anyhow::bail!("invalid binding schema or sent count");}
        for (text,limit) in std::iter::once((self.session_path.as_str(),4096)).chain([self.session_id.as_str(),&self.marker_entry_id,&self.sdk_session_id,&self.account_name,&self.model_id].into_iter().map(|s|(s,256))).chain(self.last_assistant_uuid.as_deref().map(|s|(s,256))) {
            if text.is_empty()||text.encode_utf16().count()>limit {anyhow::bail!("invalid binding string length");}
        }
        for hash in [&self.sent_prefix_hash,&self.assistant_content_hash,&self.system_prompt_hash,&self.toolset_hash] {
            if hash.len()!=64||!hash.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)) {anyhow::bail!("invalid binding hash");}
        }
        Ok(())
    }
}
pub fn canonical_session_path(session:&Path)->std::io::Result<PathBuf> {
    let resolved=std::path::absolute(session)?;
    Ok(std::fs::canonicalize(&resolved).unwrap_or_else(|_|resolved.parent().and_then(|p|std::fs::canonicalize(p).ok()).and_then(|p|resolved.file_name().map(|name|p.join(name))).unwrap_or(resolved)))
}
pub fn sidecar_path(session:&Path)->std::io::Result<PathBuf> {
    let mut path=canonical_session_path(session)?.into_os_string();path.push(".claude-sdk-oauth-binding.json");Ok(path.into())
}
pub fn read_binding(session:&Path)->anyhow::Result<Option<StoredBinding>> {
    let path=sidecar_path(session)?;
    match std::fs::metadata(&path) {Ok(metadata) if metadata.len()>16384=>return Ok(None),Ok(_)=>{},Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(e)=>return Err(e.into())}
    let bytes=std::fs::read(path)?;let Ok(binding)=serde_json::from_slice::<StoredBinding>(&bytes) else {return Ok(None);};
    if binding.validate().is_err()||Path::new(&binding.session_path)!=canonical_session_path(session)? {return Ok(None);}Ok(Some(binding))
}
pub fn write_binding(session:&Path,binding:&StoredBinding)->anyhow::Result<()> {
    binding.validate()?;let canonical=canonical_session_path(session)?;
    if canonical_session_path(Path::new(&binding.session_path))?!=canonical {anyhow::bail!("Stored binding path mismatch: expected {}, received {}",canonical.display(),binding.session_path);}
    let mut binding=binding.clone();binding.session_path=canonical.to_string_lossy().into_owned();let mut bytes=serde_json::to_vec(&binding)?;bytes.push(b'\n');
    if bytes.len()>16384 {anyhow::bail!("Stored binding exceeds 16384 bytes: {}",bytes.len());}
    let path=sidecar_path(session)?;let mut temporary=tempfile::NamedTempFile::new_in(path.parent().ok_or_else(||anyhow::anyhow!("binding has no parent"))?)?;
    temporary.as_file().set_permissions(std::fs::Permissions::from_mode(0o600))?;temporary.write_all(&bytes)?;temporary.persist(path)?;Ok(())
}
pub fn delete_binding(session:&Path)->std::io::Result<()> {match std::fs::remove_file(sidecar_path(session)?) {Err(e) if e.kind()==std::io::ErrorKind::NotFound=>Ok(()),result=>result}}
#[cfg(test)]
mod tests {
    use super::*;
    fn record(path:&Path)->StoredBinding {StoredBinding {schema_version:1,session_path:path.to_string_lossy().into_owned(),session_id:"session".into(),marker_entry_id:"marker".into(),sdk_session_id:"sdk".into(),sent_count:1,sent_prefix_hash:"1".repeat(64),assistant_content_hash:"2".repeat(64),last_assistant_uuid:Some("assistant".into()),account_name:"default".into(),model_id:"model".into(),system_prompt_hash:"3".repeat(64),toolset_hash:"4".repeat(64)}}
    #[test]
    fn roundtrip_replace_delete_private_mode() {
        let directory=tempfile::tempdir().expect("directory");let session=directory.path().join("session.json");std::fs::write(&session,"{}").expect("session");
        let mut binding=record(&session);write_binding(&session,&binding).expect("write");assert_eq!(read_binding(&session).expect("read"),Some(binding.clone()));
        assert_eq!(std::fs::metadata(sidecar_path(&session).expect("path")).expect("metadata").permissions().mode()&0o777,0o600);
        binding.session_id="second".into();write_binding(&session,&binding).expect("replace");assert_eq!(read_binding(&session).expect("read").expect("binding").session_id,"second");delete_binding(&session).expect("delete");assert!(read_binding(&session).expect("missing").is_none());
    }
    #[test]
    fn rejects_unknown_malformed_and_oversized() {
        let directory=tempfile::tempdir().expect("directory");let session=directory.path().join("session.json");let sidecar=sidecar_path(&session).expect("path");
        std::fs::write(&sidecar,"not JSON").expect("malformed");assert!(read_binding(&session).expect("read").is_none());
        let mut value=serde_json::to_value(record(&session)).expect("value");value["unansweredTurnDigest"]=serde_json::json!("5".repeat(64));std::fs::write(&sidecar,serde_json::to_vec(&value).expect("bytes")).expect("unknown");assert!(read_binding(&session).expect("read").is_none());
        let mut binding=record(&session);binding.session_id="x".repeat(1048576);assert!(write_binding(&session,&binding).is_err());
        std::fs::write(&sidecar,"x".repeat(16385)).expect("oversize");assert!(read_binding(&session).expect("read").is_none());
    }
}
