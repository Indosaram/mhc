use crate::grep::engine::*;
pub fn walker_flags(request: &GrepEngineRequest) -> Vec<String> {
    let mut args = Vec::new();
    if request.hidden.unwrap_or(true) {
        args.push("--hidden".into());
    }
    args.push(
        if request.gitignore.unwrap_or(true) {
            "--no-require-git"
        } else {
            "--no-ignore"
        }
        .into(),
    );
    if let Some(globs) = &request.glob {
        for negated in [false, true] {
            for glob in globs.iter().filter(|g| g.starts_with('!') == negated) {
                args.extend(["-g".into(), glob.clone()]);
            }
        }
    }
    if let Some(kind) = &request.r#type {
        args.extend(["--type".into(), kind.clone()]);
    }
    if request.hidden.unwrap_or(true) {
        args.extend(["-g".into(), "!.git".into()]);
    }
    args
}
pub fn matcher_flags(request: &GrepEngineRequest) -> Vec<String> {
    let mut args = [
        "--json",
        "--line-number",
        "--color=never",
        "--encoding",
        "none",
    ]
    .map(str::to_owned)
    .to_vec();
    for (enabled, flag) in [
        (request.ignore_case, "-i"),
        (request.literal, "-F"),
        (request.multiline, "-U"),
        (request.pcre2, "--pcre2"),
    ] {
        if enabled == Some(true) {
            args.push(flag.into());
        }
    }
    for (value, flag) in [
        (request.context_before, "-B"),
        (request.context_after, "-A"),
    ] {
        if let Some(value) = value {
            args.extend([flag.into(), value.to_string()]);
        }
    }
    if request.mode == Some(GrepMode::Files) {
        args.extend(["-m".into(), "1".into()]);
    } else if request.mode.unwrap_or(GrepMode::Content) == GrepMode::Content
        && request.line_start.is_none()
        && request.line_end.is_none()
        && let Some(cap) = request.max_count_per_file
    {
        args.extend(["-m".into(), cap.saturating_add(1).to_string()]);
    }
    args
}

/// The per-file argument a segment launch spends on this candidate: the path itself, or an anchored include glob.
pub fn candidate_argument(candidate: &super::enumerate::Candidate) -> String {
    if candidate.root.file {
        return candidate.root.path.to_string_lossy().into_owned();
    }
    let relative = super::paths::relative_path(&candidate.absolute, &candidate.root.cwd);
    let mut escaped = String::with_capacity(relative.len() + 1);
    escaped.push('/');
    for character in relative.chars() {
        if matches!(character, '\\' | '*' | '?' | '[' | ']' | '{' | '}' | '!') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

pub fn argument_cost(candidate: &super::enumerate::Candidate, platform: &str) -> usize {
    let argument = super::argv::argv_cost(&candidate_argument(candidate), platform);
    if candidate.root.file {
        argument
    } else {
        argument + super::argv::argv_cost("-g", platform)
    }
}

pub fn segment_flags(
    segment: &[&super::enumerate::Candidate],
    pattern: &str,
    walk: &[String],
    matcher: &[String],
) -> Vec<String> {
    let first = segment[0];
    let mut args = matcher.to_vec();
    args.extend([
        "--sort".into(),
        "path".into(),
        "--max-filesize".into(),
        super::prefix_pass::MAX_FILE_BYTES.to_string(),
    ]);
    args.extend(walk.iter().cloned());
    // rg searches named files unfiltered, so explicit file roots need no include whitelist.
    if first.root.file {
        args.extend(["--".into(), pattern.into()]);
        args.extend(
            segment
                .iter()
                .map(|candidate| candidate_argument(candidate)),
        );
        return args;
    }
    // Original positive globs would otherwise OR with the include list. Reset file
    // admission, allow traversal, then whitelist precisely the enumerated segment.
    args.extend(["-g".into(), "!**/*".into(), "-g".into(), "**/".into()]);
    for candidate in segment {
        args.extend(["-g".into(), candidate_argument(candidate)]);
    }
    args.extend([
        "--".into(),
        pattern.into(),
        first.root.path.to_string_lossy().into_owned(),
    ]);
    args
}
