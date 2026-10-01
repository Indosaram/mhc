use std::time::{Duration,Instant};
use crate::{definition::AbortSignal,grep::engine::*};
pub async fn search(request: GrepEngineRequest,signal: &AbortSignal) -> Result<GrepEngineResult,GrepEngineError> {
    let started = Instant::now(); let timeout = request.timeout_ms.unwrap_or(30000); let deadline = started+Duration::from_millis(u64::from(timeout));
    let mut result = GrepEngineResult { matches:Vec::new(),file_counts:Vec::new(),counts:maho_grep::GrepCounts { matches:if request.mode == Some(GrepMode::Files) { None } else { Some(0) },files:0,exact:true },files_searched:0,limit_reached:false,per_file_limit_reached:false,skipped_oversized:0,prefix_searched:0,skipped_binary:0,missing_paths:Vec::new(),warnings:Vec::new(),timed_out:false,elapsed_ms:0.0,effective_pattern:request.pattern.clone(),pattern_kind:if request.literal == Some(true) { "literal" } else { "regex" }.into(),regex_engine:if request.pcre2 == Some(true) { "pcre2" } else { "rust" }.into() };
    let work = async {
        let candidates = super::rg::enumerate::enumerate_candidates(&request,&mut result,deadline,signal).await?;
        super::rg::segments::search_segments(&candidates,&request,&mut result,deadline,signal).await
    }.await;
    match work {
        Err(GrepEngineError::EngineUnavailable(message)) if message == "TIMED_OUT" => {
            result.timed_out = true; result.warnings.push(maho_grep::GrepWarning { path:None,code:"TIMED_OUT".into(),message:format!("Timed out after {timeout} ms; showing the completed ordered prefix.") });
        }
        Err(error) => return Err(error), Ok(()) => {},
    }
    result.counts.exact = !result.limit_reached && !result.per_file_limit_reached && !result.timed_out; result.elapsed_ms = started.elapsed().as_secs_f64()*1000.0; Ok(result)
}
