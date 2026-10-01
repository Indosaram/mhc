#[derive(Default)]
pub struct RunSgOptions {
    pub pattern: String, pub lang: String, pub paths: Vec<String>, pub globs: Vec<String>,
    pub rewrite: Option<String>, pub context: Option<f64>, pub update_all: bool,
}

pub fn build_sg_args(options: &RunSgOptions, include_update_all: bool) -> Vec<String> {
    let write_pass = options.update_all && !include_update_all;
    let mut args = vec!["run".into(), "-p".into(), options.pattern.clone(), "--lang".into(), options.lang.clone()];
    if !write_pass { args.push("--json=compact".into()); }
    if let Some(rewrite) = options.rewrite.as_ref().filter(|value| !value.is_empty()) {
        args.extend(["-r".into(), rewrite.clone()]);
        if include_update_all { args.push("--update-all".into()); }
    }
    if let Some(context) = options.context.filter(|value| *value > 0.0) { args.extend(["-C".into(), context.to_string()]); }
    for glob in &options.globs { args.extend(["--globs".into(), glob.clone()]); }
    if options.paths.is_empty() { args.push(".".into()); } else { args.extend(options.paths.iter().cloned()); }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options() -> RunSgOptions { RunSgOptions { pattern: "console.log($MSG)".into(), lang: "typescript".into(), paths: vec!["src".into()], ..Default::default() } }
    #[test] fn compact_when_search() { assert_eq!(build_sg_args(&options(), false), ["run", "-p", "console.log($MSG)", "--lang", "typescript", "--json=compact", "src"]); }
    #[test] fn context_when_positive() { let value = RunSgOptions { context: Some(3.0), ..options() }; assert_eq!(&build_sg_args(&value, false)[6..], ["-C", "3", "src"]); }
    #[test] fn rewrite_when_dry_pass() { let value = RunSgOptions { rewrite: Some("logger.info($MSG)".into()), ..options() }; assert!(!build_sg_args(&value, false).iter().any(|arg| arg == "--update-all")); }
    #[test] fn update_when_included() { let value = RunSgOptions { rewrite: Some("logger.info($MSG)".into()), ..options() }; assert!(build_sg_args(&value, true).iter().any(|arg| arg == "--update-all")); }
    #[test] fn repeated_when_globs() { let value = RunSgOptions { globs: vec!["**/*.ts".into(), "!**/*.test.ts".into()], ..options() }; assert_eq!(build_sg_args(&value, false).iter().filter(|arg| *arg == "--globs").count(), 2); }
    #[test] fn current_when_default_paths() { let value = RunSgOptions { paths: Vec::new(), ..options() }; assert_eq!(build_sg_args(&value, false).last().map(String::as_str), Some(".")); }
    #[test] fn current_when_empty_options() { assert_eq!(build_sg_args(&RunSgOptions::default(), false).last().map(String::as_str), Some(".")); }
    #[test] fn non_json_when_write_pass() { let value = RunSgOptions { rewrite: Some("x".into()), update_all: true, ..options() }; assert!(!build_sg_args(&value, false).iter().any(|arg| arg == "--json=compact")); }
}
