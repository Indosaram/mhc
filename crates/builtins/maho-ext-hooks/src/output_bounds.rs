use std::io::Write;
use std::path::PathBuf;
use maho_core::sensitive_output::{redact_sensitive_output,redact_sensitive_token_values};

pub const DEFAULT_STDOUT_LIMIT_BYTES:usize=64*1024;
pub const DEFAULT_STDERR_LIMIT_BYTES:usize=64*1024;

#[derive(Default)]
pub struct HookOutputPolicy {
    pub max_stdout_bytes:Option<usize>,
    pub max_stderr_bytes:Option<usize>,
    pub spill_dir:Option<PathBuf>,
}
pub struct HookStreamSafetyMetadata {
    pub original_bytes:usize,
    pub returned_bytes:usize,
    pub redacted:bool,
    pub spilled:bool,
    pub truncated:bool,
    pub spill_path:Option<PathBuf>,
}
pub struct HookOutputSafetyMetadata {pub stdout:HookStreamSafetyMetadata,pub stderr:HookStreamSafetyMetadata}
pub struct HookSafeOutput {pub text:String,pub safety:HookStreamSafetyMetadata}
#[derive(Clone,Copy)]
pub enum HookStream {Stdout,Stderr}
pub struct CaptureMetadata {pub original_bytes:usize,pub truncated:bool}

pub fn apply_hook_output_safety(stream:HookStream,text:&str,policy:Option<&HookOutputPolicy>,capture:Option<CaptureMetadata>)->std::io::Result<HookSafeOutput> {
    let original_bytes=capture.as_ref().map_or(text.len(),|c|c.original_bytes);
    let redacted_text=redact_sensitive_output(text);let redacted=redacted_text!=text;
    let max_bytes=match stream {
        HookStream::Stdout=>policy.and_then(|p|p.max_stdout_bytes).unwrap_or(DEFAULT_STDOUT_LIMIT_BYTES),
        HookStream::Stderr=>policy.and_then(|p|p.max_stderr_bytes).unwrap_or(DEFAULT_STDERR_LIMIT_BYTES),
    };
    let truncated=capture.is_some_and(|c|c.truncated) || redacted_text.len()>max_bytes;
    let spill_path=if truncated {
        let dir=policy.and_then(|p|p.spill_dir.clone()).unwrap_or_else(||std::env::temp_dir().join("senpi-hook-output"));
        std::fs::create_dir_all(&dir)?;
        let name=match stream {HookStream::Stdout=>"stdout",HookStream::Stderr=>"stderr"};
        let mut file=tempfile::Builder::new().prefix(&format!("hook-{name}-{}-",std::process::id())).suffix(".txt").tempfile_in(dir)?;
        file.write_all(redacted_text.as_bytes())?;
        let (_,path)=file.keep().map_err(|e|e.error)?;Some(path)
    } else {None};
    let returned_text=if truncated {String::from_utf8_lossy(&redacted_text.as_bytes()[..max_bytes.min(redacted_text.len())]).into_owned()} else {redacted_text};
    Ok(HookSafeOutput {safety:HookStreamSafetyMetadata {original_bytes,returned_bytes:returned_text.len(),redacted,spilled:spill_path.is_some(),truncated,spill_path},text:returned_text})
}

pub fn redact_hook_token_values(text:&str,replacement:&str)->String {redact_sensitive_token_values(text,replacement)}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redact_before_spill_and_truncate()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let policy=HookOutputPolicy {max_stdout_bytes:Some(64),max_stderr_bytes:Some(64),spill_dir:Some(dir.path().to_owned())};
        for stream in [HookStream::Stdout,HookStream::Stderr] {
            let text=format!("SECRET_TOKEN=example\n{}","x".repeat(200));
            let result=apply_hook_output_safety(stream,&text,Some(&policy),None)?;
            assert!(result.safety.redacted && result.safety.truncated && result.safety.spilled);
            assert!(result.text.contains("[REDACTED]"));assert!(!result.text.contains("example"));assert_eq!(result.text.len(),64);
            let spilled=std::fs::read_to_string(result.safety.spill_path.expect("spill"))?;
            assert!(spilled.contains("[REDACTED]"));assert!(!spilled.contains("example"));
        }Ok(())
    }
    #[test]
    fn byte_cut_and_capture_metadata_are_preserved()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let policy=HookOutputPolicy {max_stdout_bytes:Some(2),spill_dir:Some(dir.path().to_owned()),..Default::default()};
        let result=apply_hook_output_safety(HookStream::Stdout,"\u{20ac}",Some(&policy),Some(CaptureMetadata {original_bytes:100,truncated:true}))?;
        assert_eq!(result.text,"\u{fffd}");assert_eq!(result.safety.original_bytes,100);assert_eq!(result.safety.returned_bytes,3);Ok(())
    }
}
