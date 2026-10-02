use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct IgnoreScope {
    pub directory: PathBuf,
    pub matcher: Gitignore,
}

pub async fn add_gitignore_scope(mut scopes: Vec<IgnoreScope>, directory: &Path) -> Vec<IgnoreScope> {
    if let Ok(contents) = tokio::fs::read_to_string(directory.join(".gitignore")).await {
        let mut builder = GitignoreBuilder::new(directory);
        for line in contents.lines() {
            // Invalid patterns are ignored by upstream's non-strict ignore matcher.
            if builder.add_line(None, line).is_err() { continue; }
        }
        if let Ok(matcher) = builder.build() {
            scopes.push(IgnoreScope { directory: directory.to_owned(), matcher });
        }
    }
    scopes
}

pub fn is_ignored(scopes: &[IgnoreScope], path: &Path, is_directory: bool) -> bool {
    let mut ignored = false;
    for scope in scopes {
        let result = scope.matcher.matched_path_or_any_parents(path, is_directory);
        if result.is_ignore() { ignored = true; }
        if result.is_whitelist() { ignored = false; }
    }
    ignored
}
