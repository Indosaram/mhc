use tokio::io::AsyncReadExt;
use crate::grep::engine::GrepEngineResult;
use super::enumerate::Candidate;
pub const MAX_FILE_BYTES: usize = 4_194_304;
pub async fn read_searchable_prefix(candidate: &Candidate,result: &mut GrepEngineResult) -> Option<Vec<u8>> {
    let read = async {
        let file = tokio::fs::File::open(&candidate.absolute).await?; let mut data = Vec::new();
        file.take(MAX_FILE_BYTES as u64).read_to_end(&mut data).await?; Ok::<_,std::io::Error>(data)
    }.await;
    let mut prefix = match read { Ok(data) => data, Err(error) => {
        result.skipped_oversized += 1; result.warnings.push(maho_grep::GrepWarning { path:Some(candidate.display.clone()),code:"SKIPPED_OVERSIZED".into(),message:error.to_string() }); return None;
    } };
    if prefix.contains(&0) { result.skipped_binary += 1; return None; }
    let Some(newline) = prefix.iter().rposition(|b| *b == b'\n') else { result.skipped_oversized += 1; return None; };
    prefix.truncate(newline+1); Some(prefix)
}
