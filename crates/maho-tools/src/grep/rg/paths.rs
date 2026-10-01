pub fn slash_path(path: &str) -> String { path.replace(std::path::MAIN_SEPARATOR,"/") }
pub fn path_order(a: &str,b: &str) -> std::cmp::Ordering { a.as_bytes().cmp(b.as_bytes()) }
pub fn relative_path(path: &std::path::Path, base: &std::path::Path) -> String {
    let path: Vec<_> = path.components().collect(); let base: Vec<_> = base.components().collect();
    let shared = path.iter().zip(&base).take_while(|(a,b)| a == b).count();
    let mut result = std::path::PathBuf::new();
    for _ in shared..base.len() { result.push(".."); }
    for part in &path[shared..] { result.push(part.as_os_str()); }
    slash_path(&result.to_string_lossy())
}
