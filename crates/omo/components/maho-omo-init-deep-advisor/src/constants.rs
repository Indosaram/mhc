pub const MISSING_COVERAGE_RATIO_THRESHOLD: f64 = 0.5;
pub const CANDIDATE_MIN_FILES: usize = 8;
pub const CANDIDATE_MIN_LOC: usize = 500;
pub const CANDIDATE_MAX_DEPTH: usize = 3;
pub const COMMIT_DISTANCE_THRESHOLD: u64 = 30;
pub const TOUCHED_RATIO_THRESHOLD: f64 = 0.15;
pub const CHURN_LOC_RATIO_THRESHOLD: f64 = 0.25;
pub const DAYS_SINCE_THRESHOLD: f64 = 90.0;
pub const COOLDOWN_DAYS: u64 = 7;
pub const MS_PER_DAY: u64 = 86_400_000;
pub const SOURCE_EXTENSIONS: &[&str] = &[
    "ts", "tsx", "js", "jsx", "py", "go", "rs", "java", "kt", "swift", "rb", "php", "c", "cpp",
    "cs", "scala", "lua", "ex", "exs", "zig", "dart",
];
pub const EXCLUDED_DIR_NAMES: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "build",
    "vendor",
    ".next",
    "__pycache__",
    ".venv",
    "target",
    "coverage",
    "third_party",
];
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coverage_constants() {
        assert!((MISSING_COVERAGE_RATIO_THRESHOLD - 0.5).abs() < f64::EPSILON);
        assert_eq!(CANDIDATE_MIN_FILES, 8);
        assert_eq!(CANDIDATE_MIN_LOC, 500);
        assert_eq!(CANDIDATE_MAX_DEPTH, 3);
    }
    #[test]
    fn drift_constants() {
        assert_eq!(COMMIT_DISTANCE_THRESHOLD, 30);
        assert!((TOUCHED_RATIO_THRESHOLD - 0.15).abs() < f64::EPSILON);
        assert!((CHURN_LOC_RATIO_THRESHOLD - 0.25).abs() < f64::EPSILON);
        assert!((DAYS_SINCE_THRESHOLD - 90.0).abs() < f64::EPSILON);
        assert_eq!(COOLDOWN_DAYS, 7);
    }
    #[test]
    fn extensions_and_exclusions() {
        assert!(SOURCE_EXTENSIONS.contains(&"ts"));
        assert!(SOURCE_EXTENSIONS.contains(&"dart"));
        assert!(!SOURCE_EXTENSIONS.contains(&"md"));
        assert!(EXCLUDED_DIR_NAMES.contains(&"node_modules"));
        assert!(EXCLUDED_DIR_NAMES.contains(&".git"));
        assert!(!EXCLUDED_DIR_NAMES.contains(&"src"));
    }
}
