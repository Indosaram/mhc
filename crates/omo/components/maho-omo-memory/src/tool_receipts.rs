use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use memory_core::support::sha256::sha256_hex;
use crate::worker::run_artifacts::{ArtifactError, unlink_run_artifact};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryToolReceipt { pub version: u32, pub tool_call_id: String, pub sha: String, pub subject: String, pub affected_paths: Vec<String> }
pub fn tool_receipt_path(receipts_dir: &Path, tool_call_id: &str) -> PathBuf {
    let hash = sha256_hex(tool_call_id.as_bytes());
    receipts_dir.join(format!("{}.json", &hash[..32]))
}
pub fn write_tool_receipt(receipts_dir: &Path, receipt: &MemoryToolReceipt) -> Result<(), ArtifactError> {
    let mut builder = std::fs::DirBuilder::new(); builder.recursive(true);
    #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
    builder.create(receipts_dir)?;
    let target=tool_receipt_path(receipts_dir,&receipt.tool_call_id);
    let temporary=target.with_file_name(format!("{}.tmp-{}",target.file_name().unwrap_or_default().to_string_lossy(),std::process::id()));
    let mut options=std::fs::OpenOptions::new();options.write(true).create(true).truncate(true);
    #[cfg(unix)]{use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    {use std::io::Write;let mut file=options.open(&temporary)?;file.write_all(serde_json::to_string(receipt)?.as_bytes())?;file.write_all(b"\n")?;}
    std::fs::rename(temporary,target)?;Ok(())
}
pub fn consume_tool_receipt(receipts_dir: &Path, tool_call_id: &str) -> Result<Option<MemoryToolReceipt>, ArtifactError> {
    let path = tool_receipt_path(receipts_dir, tool_call_id);
    let raw = match std::fs::read(&path) { Ok(raw) => raw, Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None), Err(error) => return Err(error.into()) };
    unlink_run_artifact(&path)?;
    let Ok(receipt) = serde_json::from_slice::<MemoryToolReceipt>(&raw) else { return Ok(None); };
    Ok((receipt.version == 1 && receipt.tool_call_id == tool_call_id && !receipt.sha.is_empty()).then_some(receipt))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_consumed_once() {
        let root = tempfile::tempdir().unwrap(); let receipt = MemoryToolReceipt { version: 1, tool_call_id: "call-1".into(), sha: "abc".into(), subject: "save".into(), affected_paths: vec!["system/human.md".into()] };
        write_tool_receipt(root.path(), &receipt).unwrap();
        assert_eq!(std::fs::read(tool_receipt_path(root.path(),"call-1")).unwrap(),format!("{}\n",serde_json::to_string(&receipt).unwrap()).as_bytes());assert_eq!(std::fs::read_dir(root.path()).unwrap().count(),1);
        assert_eq!(consume_tool_receipt(root.path(), "call-1").unwrap(), Some(receipt)); assert!(consume_tool_receipt(root.path(), "call-1").unwrap().is_none());
    }
    #[test]
    fn corrupt_and_mismatched_consumed_silently() {
        let root = tempfile::tempdir().unwrap(); let path = tool_receipt_path(root.path(), "call-1");
        for bytes in ["{", "{\"version\":1,\"toolCallId\":\"other\",\"sha\":\"abc\",\"subject\":\"save\",\"affectedPaths\":[]}"] { std::fs::write(&path, bytes).unwrap(); assert!(consume_tool_receipt(root.path(), "call-1").unwrap().is_none()); assert!(!path.exists()); }
    }
}
