use std::{fs, io, path::{Path, PathBuf}, process::Command};
use crate::constants::SOURCE_EXTENSIONS;

const DRIFT_EXCLUDES: &[&str] = &[":(exclude)AGENTS.md", ":(exclude,glob)**/AGENTS.md", ":(exclude).omo/init-deep.json"];

pub fn run(root: &Path, args: &[&str]) -> io::Result<String> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&output.stderr).into_owned()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
pub fn git_is_repo(root: &Path) -> bool { run(root, &["rev-parse", "--is-inside-work-tree"]).is_ok() }
pub fn git_head(root: &Path) -> io::Result<String> { Ok(run(root, &["rev-parse", "HEAD"] )?.trim().into()) }
pub fn git_commits_since(root: &Path, sha: &str) -> io::Result<u64> {
    run(root, &["rev-list", "--count", &format!("{sha}..HEAD")])?.trim().parse().map_err(io::Error::other)
}
pub fn git_tracked_file_count(root: &Path) -> io::Result<usize> { Ok(run(root, &["ls-files", "-z"] )?.bytes().filter(|&b| b == 0).count()) }
fn diff(root: &Path, sha: &str, format: &str) -> io::Result<String> {
    let mut args = vec!["diff", format, "-z", "--find-renames", sha, "HEAD", "--", "."];
    args.extend_from_slice(DRIFT_EXCLUDES);
    run(root, &args)
}
pub fn git_touched_files_since(root: &Path, sha: &str) -> io::Result<Vec<String>> {
    Ok(diff(root, sha, "--name-only")?.split('\0').filter(|s| !s.is_empty()).map(str::to_owned).collect())
}
pub fn git_churn_loc(root: &Path, sha: &str) -> io::Result<u64> {
    let output = diff(root, sha, "--numstat")?;
    let tokens: Vec<_> = output.split('\0').collect();
    let mut index = 0;
    let mut churn = 0;
    while let Some(record) = tokens.get(index) {
        index += 1;
        if record.is_empty() { continue; }
        let mut fields = record.split('\t');
        let added = fields.next().unwrap_or_default();
        let deleted = fields.next().unwrap_or_default();
        if fields.next() == Some("") { index += 2; }
        if added == "-" || deleted == "-" { continue; }
        churn += added.parse::<u64>().map_err(io::Error::other)? + deleted.parse::<u64>().map_err(io::Error::other)?;
    }
    Ok(churn)
}
pub fn git_total_loc(root: &Path) -> io::Result<usize> {
    let files = run(root, &["ls-files", "-z"])?;
    let mut total = 0;
    for file in files.split('\0').filter(|s| !s.is_empty()) {
        if Path::new(file).extension().and_then(|e|e.to_str()).is_some_and(|ext|SOURCE_EXTENSIONS.contains(&ext)) {
            total += fs::read_to_string(root.join(file))?.bytes().filter(|&b|b==b'\n').count();
        }
    }
    Ok(total)
}
pub fn git_object_type(root: &Path, sha: &str) -> Option<String> { run(root, &["cat-file", "-t", sha]).map(|s|s.trim().into()).ok() }
pub fn git_common_dir_realpath(root: &Path) -> io::Result<PathBuf> { fs::canonicalize(root.join(run(root, &["rev-parse", "--git-common-dir"] )?.trim())) }

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub fn repo() -> (tempfile::TempDir, String) {
        let t=tempfile::tempdir().unwrap();
        run(t.path(), &["init", "-q"]).unwrap();
        run(t.path(), &["config", "user.name", "Fixture"]).unwrap();
        run(t.path(), &["config", "user.email", "fixture@example.invalid"]).unwrap();
        fs::create_dir(t.path().join("src")).unwrap();
        for name in ["a.ts", "b.ts"] { fs::write(t.path().join("src").join(name), (0..10).map(|i|format!("line{i}")).collect::<Vec<_>>().join("\n")).unwrap(); }
        let sha=commit(t.path());
        (t,sha)
    }
    pub fn commit(root: &Path) -> String { run(root, &["add", "."]).unwrap(); run(root, &["commit", "-qm", "fixture"]).unwrap(); git_head(root).unwrap() }
    #[test] fn repo_detected() { let (t,_)=repo(); assert!(git_is_repo(t.path())); }
    #[test] fn plain_directory() { let t=tempfile::tempdir().unwrap(); assert!(!git_is_repo(t.path())); }
    #[test] fn head() { let (t,sha)=repo(); assert_eq!(git_head(t.path()).unwrap(),sha); }
    #[test] fn five_commits() { let (t,sha)=repo(); for n in 0..5 { fs::write(t.path().join("src/a.ts"),format!("change {n}\n")).unwrap(); commit(t.path()); } assert_eq!(git_commits_since(t.path(),&sha).unwrap(),5); }
    #[test] fn tracked_count() { let (t,_)=repo(); assert_eq!(git_tracked_file_count(t.path()).unwrap(),2); }
    #[test] fn newline_filename() { let (t,_)=repo(); fs::write(t.path().join("we\nird.ts"),"a\nb").unwrap(); commit(t.path()); assert_eq!(git_tracked_file_count(t.path()).unwrap(),3); }
    #[test] fn touched() { let (t,sha)=repo(); fs::write(t.path().join("src/a.ts"),"changed").unwrap(); commit(t.path()); assert_eq!(git_touched_files_since(t.path(),&sha).unwrap(),vec!["src/a.ts"]); }
    #[test] fn documentation_excluded() { let (t,sha)=repo(); fs::create_dir(t.path().join(".omo")).unwrap(); for name in ["AGENTS.md","src/AGENTS.md",".omo/init-deep.json"] { fs::write(t.path().join(name),"docs").unwrap(); } commit(t.path()); assert!(git_touched_files_since(t.path(),&sha).unwrap().is_empty()); }
    #[test] fn churn() { let (t,sha)=repo(); fs::write(t.path().join("src/a.ts"),(0..12).map(|i|format!("line{i}")).collect::<Vec<_>>().join("\n")).unwrap(); commit(t.path()); assert_eq!(git_churn_loc(t.path(),&sha).unwrap(),4); }
    #[test] fn binary_churn() { let (t,sha)=repo(); fs::write(t.path().join("blob.bin"),b"\0\x01\x02binary\0").unwrap(); commit(t.path()); assert_eq!(git_churn_loc(t.path(),&sha).unwrap(),0); }
    #[test] fn total_loc() { let (t,_)=repo(); assert_eq!(git_total_loc(t.path()).unwrap(),18); }
    #[test] fn commit_type() { let (t,sha)=repo(); assert_eq!(git_object_type(t.path(),&sha).as_deref(),Some("commit")); }
    #[test] fn other_object_types() { let (t,_)=repo(); for (object,kind) in [("HEAD^{tree}","tree"),("HEAD:src/a.ts","blob")] { let sha=run(t.path(), &["rev-parse",object]).unwrap(); assert_eq!(git_object_type(t.path(),sha.trim()).as_deref(),Some(kind)); } run(t.path(), &["tag","-a","v1","-m","tagged"]).unwrap(); let sha=run(t.path(), &["rev-parse","v1"]).unwrap(); assert_eq!(git_object_type(t.path(),sha.trim()).as_deref(),Some("tag")); assert!(git_object_type(t.path(),&"0".repeat(40)).is_none()); }
    #[test] fn common_realpath() { let (t,_)=repo(); assert_eq!(git_common_dir_realpath(t.path()).unwrap(),fs::canonicalize(t.path().join(".git")).unwrap()); }
}
