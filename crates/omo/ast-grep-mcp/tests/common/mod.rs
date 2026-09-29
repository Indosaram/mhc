#![allow(dead_code)]

use std::path::Path;

pub const SG_PATH: &str = "/opt/homebrew/bin/sg";

pub fn sg_available() -> bool {
    Path::new(SG_PATH).exists()
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
