use std::path::{Path, PathBuf};

pub fn find_agents_md_up(start_dir: &Path, root_dir: &Path, file_names: &[&str]) -> Vec<PathBuf> {
    let mut collected = Vec::new();
    let mut current = start_dir;
    loop {
        if current == root_dir { break; }
        for name in file_names {
            let candidate = current.join(name);
            if candidate.try_exists().unwrap_or(false) { collected.push(candidate); break; }
        }
        let Some(parent) = current.parent() else { break; };
        if parent == current || !parent.starts_with(root_dir) { break; }
        current = parent;
    }
    collected.reverse();
    collected
}
