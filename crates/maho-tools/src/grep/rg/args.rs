use crate::grep::engine::*;
pub fn walker_flags(request: &GrepEngineRequest) -> Vec<String> {
    let mut args = Vec::new();
    if request.hidden.unwrap_or(true) { args.push("--hidden".into()); }
    args.push(if request.gitignore.unwrap_or(true) { "--no-require-git" } else { "--no-ignore" }.into());
    if let Some(globs) = &request.glob {
        for negated in [false,true] { for glob in globs.iter().filter(|g| g.starts_with('!') == negated) { args.extend(["-g".into(),glob.clone()]); } }
    }
    if let Some(kind) = &request.r#type { args.extend(["--type".into(),kind.clone()]); }
    if request.hidden.unwrap_or(true) { args.extend(["-g".into(),"!.git".into()]); } args
}
pub fn matcher_flags(request: &GrepEngineRequest) -> Vec<String> {
    let mut args = ["--json","--line-number","--color=never","--encoding","none"].map(str::to_owned).to_vec();
    for (enabled,flag) in [(request.ignore_case,"-i"),(request.literal,"-F"),(request.multiline,"-U"),(request.pcre2,"--pcre2")] { if enabled == Some(true) { args.push(flag.into()); } }
    for (value,flag) in [(request.context_before,"-B"),(request.context_after,"-A")] { if let Some(value) = value { args.extend([flag.into(),value.to_string()]); } }
    if request.mode == Some(GrepMode::Files) { args.extend(["-m".into(),"1".into()]); }
    else if request.mode.unwrap_or(GrepMode::Content) == GrepMode::Content && request.line_start.is_none() && request.line_end.is_none() {
        if let Some(cap) = request.max_count_per_file { args.extend(["-m".into(),cap.saturating_add(1).to_string()]); }
    } args
}
