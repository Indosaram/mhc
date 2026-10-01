use std::{collections::BTreeMap,io::Read,path::Path};
pub const OBSERVED_MODEL_FLOORS:[(&str,&str);2]=[("claude-fable-5-1","2.1.251"),("claude-opus-5-5","2.1.280")];
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
    fn whole_tokens_cross_chunk_boundary_without_matching_prefixes() {
        let file=tempfile::NamedTempFile::new().expect("file");let mut bytes=vec![b'x';CHUNK_BYTES-5];bytes.extend_from_slice(b" claude-opus-5-5\0claude-haiku-4-5-20251001\0");std::fs::write(file.path(),bytes).expect("binary");
        let result=binary_embeds_tokens(file.path(),&["claude-opus-5-5".into(),"claude-opus-5".into(),"claude-haiku-4-5".into()]).expect("scan");assert!(result["claude-opus-5-5"]);assert!(!result["claude-opus-5"]);assert!(!result["claude-haiku-4-5"]);
    }
}
