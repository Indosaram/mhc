#![allow(dead_code)]

use std::path::Path;

pub fn resolve_sg_for_tests() -> String {
    if let Ok(val) = std::env::var("OMO_AST_GREP_SG_PATH") {
        let trimmed = val.trim().to_string();
        if !trimmed.is_empty() {
            let path = Path::new(&trimmed);
            assert!(
                path.is_file(),
                "OMO_AST_GREP_SG_PATH is set to '{}', but file does not exist or is not a file",
                trimmed
            );
            return trimmed;
        }
    }

    match utils::resolve_sg_binary_sync(&utils::SgResolverOptions::default()) {
        utils::SgResolution::Found { path, .. } => path,
        utils::SgResolution::NotFound { error } => {
            panic!(
                "Required ast-grep executable not found: {}. Hints: {:?}",
                error.message, error.hints
            );
        }
    }
}

pub fn fixture_repo() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("omo-search-")
        .tempdir()
        .expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        dir.path().join("src/a.ts"),
        "console.log(\"hello\");\nconsole.log(\"world\");\nconst x = 1;\nconsole.log(x);\n",
    )
    .expect("write a.ts");
    std::fs::write(dir.path().join("src/b.ts"), "console.log(\"from-b\");\n").expect("write b.ts");
    dir
}

pub fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(unix)]
pub fn write_script(dir: &Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path_str(&path)
}
