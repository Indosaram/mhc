use std::{collections::BTreeMap,io::Read,path::Path};
pub const OBSERVED_MODEL_FLOORS:[(&str,&str);2]=[("claude-fable-5-1","2.1.251"),("claude-opus-5-5","2.1.280")];
#[derive(Debug,PartialEq,Eq)]
pub struct BundledClaudeCodeBinary {pub path:std::path::PathBuf,pub claude_code_version:Option<String>}
pub fn bundled_claude_code_binary(platform:&str,arch:&str,prefer_musl:bool,resolve:impl Fn(&str)->Option<std::path::PathBuf>,manifest:&Path)->Option<BundledClaudeCodeBinary> {
    crate::executable::candidates(platform,arch,prefer_musl).into_iter().filter_map(|candidate|resolve(&candidate)).find(|path|path.is_file()).map(|path|BundledClaudeCodeBinary {path,claude_code_version:crate::executable_version::bundled_version(manifest)})
}
const CHUNK_BYTES:usize=8*1024*1024;
fn id_byte(byte:Option<&u8>)->bool {byte.is_some_and(|b|b.is_ascii_alphanumeric()||[b'.',b'_',b'-'].contains(b))}
fn contains_token(chunk:&[u8],needle:&[u8])->bool {
    if needle.is_empty() {return chunk.iter().enumerate().any(|(at,_)|!id_byte(at.checked_sub(1).and_then(|i|chunk.get(i)))&&!id_byte(chunk.get(at)));}
    chunk.windows(needle.len()).enumerate().any(|(at,value)|value==needle&&!id_byte(at.checked_sub(1).and_then(|i|chunk.get(i)))&&!id_byte(chunk.get(at+needle.len())))
}
pub fn binary_embeds_tokens(path:&Path,tokens:&[String])->std::io::Result<BTreeMap<String,bool>> {
    let mut found:BTreeMap<_,_>=tokens.iter().map(|token|(token.clone(),false)).collect();let overlap=tokens.iter().map(String::len).max().unwrap_or(0)+1;let mut buffer=vec![0;CHUNK_BYTES+overlap];let mut file=std::fs::File::open(path)?;let mut carried=0;
    loop {let read=file.read(&mut buffer[carried..carried+CHUNK_BYTES])?;if read==0 {break;}let end=carried+read;
        for token in tokens {if !found[token]&&contains_token(&buffer[..end],token.as_bytes()) {found.insert(token.clone(),true);}}
        carried=overlap.min(end);buffer.copy_within(end-carried..end,0);
    }Ok(found)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_support_uses_platform_sidecar_not_path_or_override() {
        let directory=tempfile::tempdir().expect("directory");let binary=directory.path().join("bundled");std::fs::write(&binary,"").expect("binary");
        let manifest=directory.path().join("package.json");std::fs::write(&manifest,r#"{"claudeCodeVersion":"2.1.280"}"#).expect("manifest");
        let found=bundled_claude_code_binary("linux","x64",true,|candidate|candidate.contains("-musl/").then(||binary.clone()),&manifest).expect("bundled");
        assert_eq!(found.path,binary);assert_eq!(found.claude_code_version.as_deref(),Some("2.1.280"));
        assert!(bundled_claude_code_binary("win32","x64",false,|_|None,&manifest).is_none());
    }
    #[test]
    fn whole_tokens_cross_chunk_boundary_without_matching_prefixes() {
        let file=tempfile::NamedTempFile::new().expect("file");let mut bytes=vec![b'x';CHUNK_BYTES-5];bytes.extend_from_slice(b" claude-opus-5-5\0claude-haiku-4-5-20251001\0");std::fs::write(file.path(),bytes).expect("binary");
        let result=binary_embeds_tokens(file.path(),&["claude-opus-5-5".into(),"claude-opus-5".into(),"claude-haiku-4-5".into()]).expect("scan");assert!(result["claude-opus-5-5"]);assert!(!result["claude-opus-5"]);assert!(!result["claude-haiku-4-5"]);
    }
}
