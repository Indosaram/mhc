use std::{path::Path,time::Instant};
use crate::{definition::AbortSignal,grep::engine::*};
use super::{enumerate::Candidate,args::matcher_flags,process::run,json_rows::{collect_rows,collect_file_rows},prefix_pass::{MAX_FILE_BYTES,read_searchable_prefix}};
pub async fn search_segments(candidates: &[Candidate],request: &GrepEngineRequest,result: &mut GrepEngineResult,deadline: Instant,signal: &AbortSignal) -> Result<(),GrepEngineError> {
    let mode = request.mode.unwrap_or(GrepMode::Content); let mut matcher_ran = false;
    let mut segments: Vec<Vec<&Candidate>> = Vec::new();
    for candidate in candidates {
        if candidate.size <= MAX_FILE_BYTES as u64
            && let Some(previous) = segments.last_mut()
            && previous[0].size <= MAX_FILE_BYTES as u64
            && (request.max_count.is_none() || previous.len() < 200) {
            previous.push(candidate);
        } else { segments.push(vec![candidate]); }
    }
    for (segment_index, segment) in segments.iter().enumerate() {
        let candidate = segment[0];
        let oversized = candidate.size > MAX_FILE_BYTES as u64;
        let prefix = if oversized { read_searchable_prefix(candidate,result).await } else { None };
        if oversized && prefix.is_none() { result.files_searched = candidate.ordinal; continue; }
        let mut args = matcher_flags(request); args.extend(["--".into(),request.pattern.clone()]);
        if !oversized { args.extend(segment.iter().map(|candidate| candidate.absolute.to_string_lossy().into_owned())); }
        let output = run(&args,Path::new(&request.cwd),prefix,deadline,signal).await?; matcher_ran = true;
        if signal.is_aborted() { return Err(GrepEngineError::Aborted); }
        if Instant::now() >= deadline { return Err(GrepEngineError::EngineUnavailable("TIMED_OUT".into())); }
        if oversized { result.prefix_searched += 1; }
        let mut last_committed = None;
        for candidate in segment {
        let (rows,binary) = if oversized { collect_rows(&output,&candidate.display,request)? }
            else { collect_file_rows(&output,&candidate.display,request,Some(&candidate.absolute))? };
        if binary { result.skipped_binary += 1; }
        let matching: Vec<_> = rows.iter().filter(|r| !r.is_context).collect();
        let cap = request.max_count_per_file.unwrap_or(u32::MAX) as usize;
        let per_file = mode != GrepMode::Files && matching.len() > cap; result.per_file_limit_reached |= per_file;
        let used = if mode == GrepMode::Files { result.counts.files } else { result.counts.matches.unwrap_or(0) };
        let remaining = request.max_count.unwrap_or(u32::MAX).saturating_sub(used) as usize;
        let count = matching.len().min(if mode == GrepMode::Files { usize::from(remaining > 0) } else { cap.min(remaining) });
        let available = if mode == GrepMode::Files { usize::from(!matching.is_empty()) } else { matching.len().min(cap) };
        result.limit_reached |= count < available;
        if count > 0 {
            last_committed = Some(candidate.ordinal);
            result.counts.files += 1; if let Some(total) = &mut result.counts.matches { *total += count as u32; }
            if mode == GrepMode::Content {
                let admitted: Vec<_> = matching[..count].iter().map(|r| r.line).collect();
                result.matches.extend(rows.into_iter().filter(|r| admitted.contains(&r.line) || (r.is_context && admitted.iter().any(|line| r.line >= line.saturating_sub(request.context_before.unwrap_or(0)) && r.line <= line.saturating_add(request.context_after.unwrap_or(0))))));
            } else { result.file_counts.push(maho_grep::GrepFileCount { path:candidate.display.clone(),count:if mode == GrepMode::Files { None } else { Some(count as u32) },limit_reached:per_file }); }
        }
        }
        result.files_searched = segment.last().map_or(0, |candidate| candidate.ordinal);
        let admitted = if mode == GrepMode::Files { result.counts.files } else { result.counts.matches.unwrap_or(0) };
        if request.max_count.is_some_and(|max| admitted >= max) {
            if let Some(ordinal) = last_committed { result.files_searched = ordinal; }
            if segment_index + 1 < segments.len() { result.limit_reached = true; }
            break;
        }
    }
    if !matcher_ran { let mut args = matcher_flags(request); args.extend(["--".into(),request.pattern.clone()]); run(&args,Path::new(&request.cwd),Some(Vec::new()),deadline,signal).await?; }
    Ok(())
}
