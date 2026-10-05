use std::{collections::BTreeSet, path::{Path, PathBuf}};
use super::constants::{RULE_FILE_EXTENSIONS, SCANNER_EXCLUDED_DIRS};

#[derive(Clone,Debug,PartialEq,Eq)]
pub struct ScannedFile { pub path: PathBuf, pub real_path: PathBuf }
pub struct ScanOptions<'a> { pub root_dir: &'a Path, pub excluded_dirs: Option<&'a [&'a str]>, pub max_depth: Option<usize> }
pub fn scan_rule_files(options: ScanOptions<'_>) -> Vec<ScannedFile> {
    let Ok(root) = std::path::absolute(options.root_dir) else { return Vec::new(); };
    if !root.is_dir() { return Vec::new(); }
    let mut results=Vec::new(); let mut visited=BTreeSet::new();
    scan_directory(&root,0,options.max_depth.unwrap_or(10),options.excluded_dirs.unwrap_or(SCANNER_EXCLUDED_DIRS),&mut visited,&mut results);
    results
}
fn scan_directory(path:&Path, depth:usize, max_depth:usize, excluded:&[&str], visited:&mut BTreeSet<PathBuf>, results:&mut Vec<ScannedFile>) {
    let Ok(real)=path.canonicalize() else {return;};
    if !visited.insert(real) {return;}
    let Ok(entries)=std::fs::read_dir(path) else {return;};
    let Ok(mut entries)=entries.collect::<Result<Vec<_>,_>>() else {return;};
    static PREFERENCES:std::sync::OnceLock<icu_collator::CollatorPreferences>=std::sync::OnceLock::new();
    let preferences=PREFERENCES.get_or_init(||{
        let configured=["LC_ALL","LC_MESSAGES","LANG"].into_iter().find_map(|key|std::env::var(key).ok()).unwrap_or_else(||"en-US".into());
        let language=configured.split(['.','@']).next().unwrap_or("");
        let language=if language=="C"||language=="POSIX"{"en-US".into()}else{language.replace('_',"-")};
        language.parse::<icu_locale_core::Locale>().map(Into::into).unwrap_or_default()
    });
    let collator=icu_collator::Collator::try_new(*preferences,Default::default()).unwrap_or_else(|error|unreachable!("compiled collation data: {error}"));
    entries.sort_by(|left,right|collator.compare(&left.file_name().to_string_lossy(),&right.file_name().to_string_lossy()));
    for entry in entries {
        let path=entry.path(); let name=entry.file_name(); let name=name.to_string_lossy();
        let Ok(kind)=entry.file_type() else {continue;};
        if kind.is_dir() || kind.is_symlink() && path.is_dir() {
            if !excluded.contains(&name.as_ref()) && depth < max_depth { scan_directory(&path,depth+1,max_depth,excluded,visited,results); }
        } else if (kind.is_file() || kind.is_symlink() && path.is_file()) && RULE_FILE_EXTENSIONS.iter().any(|extension|name.ends_with(extension)) {
            let real_path=if kind.is_symlink(){path.canonicalize().unwrap_or_else(|_|path.clone())}else{path.clone()};
            results.push(ScannedFile{path,real_path});
        }
    }
}
