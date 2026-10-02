use crate::constants::*;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
#[derive(Debug)]
pub struct DirInfo {
    pub path: PathBuf,
    pub files: usize,
    pub loc: usize,
    pub covered: bool,
}
#[derive(Debug)]
pub struct CoverageRatio {
    pub coverage: f64,
    pub missing_ratio: f64,
    pub candidate_dirs: usize,
    pub covered_dirs: usize,
}
pub fn is_covered(root: &Path, dir: &Path) -> bool {
    let mut current = dir;
    loop {
        if current.join("AGENTS.md").exists() {
            return true;
        }
        if current == root {
            return false;
        }
        let Some(parent) = current.parent() else {
            return false;
        };
        current = parent;
    }
}
pub fn compute_candidate_dirs(root: &Path) -> io::Result<Vec<DirInfo>> {
    let mut candidates = Vec::new();
    walk(root, root, 0, &mut candidates)?;
    Ok(candidates)
}
pub fn compute_coverage_ratio(root: &Path) -> io::Result<Option<CoverageRatio>> {
    let candidates = compute_candidate_dirs(root)?;
    if candidates.is_empty() {
        return Ok(None);
    }
    let covered_dirs = candidates.iter().filter(|d| d.covered).count();
    let count = candidates.iter().fold(0.0, |n, _| n + 1.0);
    let covered = candidates
        .iter()
        .filter(|d| d.covered)
        .fold(0.0, |n, _| n + 1.0);
    let coverage = covered / count;
    Ok(Some(CoverageRatio {
        coverage,
        missing_ratio: 1.0 - coverage,
        candidate_dirs: candidates.len(),
        covered_dirs,
    }))
}
pub fn should_propose_init(missing_ratio: Option<f64>, _root_has_agents_md: bool) -> bool {
    missing_ratio.is_some_and(|r| r >= MISSING_COVERAGE_RATIO_THRESHOLD)
}
fn walk(root: &Path, dir: &Path, depth: usize, candidates: &mut Vec<DirInfo>) -> io::Result<()> {
    if depth > CANDIDATE_MAX_DEPTH {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    let entries = entries.collect::<io::Result<Vec<_>>>()?;
    if depth >= 1 {
        let mut files = 0;
        let mut loc = 0;
        for entry in &entries {
            let ty = entry.file_type()?;
            if ty.is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| SOURCE_EXTENSIONS.contains(&ext))
            {
                files += 1;
                loc += fs::read_to_string(entry.path())?.split('\n').count();
            }
        }
        if files >= CANDIDATE_MIN_FILES || loc >= CANDIDATE_MIN_LOC {
            candidates.push(DirInfo {
                path: dir.into(),
                files,
                loc,
                covered: is_covered(root, dir),
            });
        }
    }
    for entry in entries {
        if entry.file_type()?.is_dir()
            && !EXCLUDED_DIR_NAMES.contains(&entry.file_name().to_string_lossy().as_ref())
        {
            walk(root, &entry.path(), depth + 1, candidates)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn files(root: &Path, dir: &str, count: usize) {
        fs::create_dir_all(root.join(dir)).unwrap();
        for n in 0..count {
            fs::write(root.join(dir).join(format!("{n}.ts")), "a\nb").unwrap();
        }
    }
    fn three() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        for (name, count) in [("alpha", 8), ("beta", 9), ("gamma", 10)] {
            files(t.path(), name, count);
        }
        t
    }
    #[test]
    fn candidate_files() {
        let t = three();
        let mut paths = compute_candidate_dirs(t.path())
            .unwrap()
            .into_iter()
            .map(|d| d.path)
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(paths, ["alpha", "beta", "gamma"].map(|s| t.path().join(s)));
    }
    #[test]
    fn candidate_loc() {
        let t = tempfile::tempdir().unwrap();
        files(t.path(), "big", 1);
        fs::write(t.path().join("big/0.ts"), "a\n".repeat(600)).unwrap();
        let c = compute_candidate_dirs(t.path()).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].files, 1);
        assert!(c[0].loc >= 500);
    }
    #[test]
    fn tiny() {
        let t = tempfile::tempdir().unwrap();
        files(t.path(), "small", 3);
        assert!(compute_candidate_dirs(t.path()).unwrap().is_empty());
    }
    #[test]
    fn per_directory() {
        let t = tempfile::tempdir().unwrap();
        files(t.path(), "src", 8);
        files(t.path(), "src/domain", 8);
        let c = compute_candidate_dirs(t.path()).unwrap();
        assert_eq!(c.len(), 2);
        assert!(c.iter().all(|d| d.files == 8));
    }
    #[test]
    fn depth_limit() {
        let t = tempfile::tempdir().unwrap();
        files(t.path(), "a/b/c/d", 12);
        assert!(compute_candidate_dirs(t.path()).unwrap().is_empty());
    }
    #[test]
    fn excluded() {
        let t = tempfile::tempdir().unwrap();
        files(t.path(), "node_modules", 20);
        files(t.path(), "dist", 20);
        assert!(compute_candidate_dirs(t.path()).unwrap().is_empty());
    }
    #[test]
    fn symlink_skipped() {
        let t = tempfile::tempdir().unwrap();
        files(t.path(), "real", 8);
        fs::create_dir(t.path().join("holder")).unwrap();
        std::os::unix::fs::symlink(t.path().join("real"), t.path().join("holder/linked")).unwrap();
        assert_eq!(compute_candidate_dirs(t.path()).unwrap().len(), 1);
    }
    #[test]
    fn own_coverage() {
        let t = three();
        fs::write(t.path().join("alpha/AGENTS.md"), "").unwrap();
        assert!(is_covered(t.path(), &t.path().join("alpha")));
    }
    #[test]
    fn ancestor_coverage() {
        let t = tempfile::tempdir().unwrap();
        files(t.path(), "src/domain", 8);
        fs::write(t.path().join("src/AGENTS.md"), "").unwrap();
        assert!(is_covered(t.path(), &t.path().join("src/domain")));
    }
    #[test]
    fn root_coverage() {
        let t = tempfile::tempdir().unwrap();
        files(t.path(), "src/domain", 8);
        fs::write(t.path().join("AGENTS.md"), "").unwrap();
        assert!(is_covered(t.path(), &t.path().join("src/domain")));
    }
    #[test]
    fn no_coverage() {
        let t = tempfile::tempdir().unwrap();
        files(t.path(), "src/domain", 8);
        assert!(!is_covered(t.path(), &t.path().join("src/domain")));
    }
    #[test]
    fn missing_all() {
        let t = three();
        let c = compute_coverage_ratio(t.path()).unwrap().unwrap();
        assert_eq!(c.candidate_dirs, 3);
        assert_eq!(c.covered_dirs, 0);
        assert!((c.coverage - 0.0).abs() < f64::EPSILON);
        assert!(should_propose_init(Some(c.missing_ratio), false));
    }
    #[test]
    fn covered_all() {
        let t = three();
        for s in ["alpha", "beta", "gamma"] {
            fs::write(t.path().join(s).join("AGENTS.md"), "").unwrap();
        }
        let c = compute_coverage_ratio(t.path()).unwrap().unwrap();
        assert_eq!(c.covered_dirs, 3);
        assert!((c.coverage - 1.0).abs() < f64::EPSILON);
        assert!(!should_propose_init(Some(c.missing_ratio), false));
    }
    #[test]
    fn zero_candidates() {
        let t = tempfile::tempdir().unwrap();
        files(t.path(), "small", 2);
        assert!(compute_coverage_ratio(t.path()).unwrap().is_none());
        assert!(!should_propose_init(None, false));
    }
    #[test]
    fn nested_full_coverage() {
        let t = three();
        for s in ["alpha", "beta", "gamma"] {
            fs::write(t.path().join(s).join("AGENTS.md"), "").unwrap();
        }
        assert!(!should_propose_init(
            compute_coverage_ratio(t.path())
                .unwrap()
                .map(|c| c.missing_ratio),
            false
        ));
    }
    #[test]
    fn half_threshold() {
        let t = tempfile::tempdir().unwrap();
        files(t.path(), "alpha", 8);
        files(t.path(), "beta", 8);
        fs::write(t.path().join("alpha/AGENTS.md"), "").unwrap();
        let c = compute_coverage_ratio(t.path()).unwrap().unwrap();
        assert!((c.missing_ratio - 0.5).abs() < f64::EPSILON);
        assert!(should_propose_init(Some(c.missing_ratio), false));
    }
    #[test]
    fn null_ratio() {
        assert!(!should_propose_init(None, false));
    }
}
