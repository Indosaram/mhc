use std::path::Path;

use crate::backend::{IsolationError, Result};
use crate::test_support::{fixture, Fixture};

pub fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(IsolationError::other(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn repo() -> Fixture {
    let f = fixture();
    git(&f.repo_root, &["init"]).expect("git init");
    // Never let a fixture command outlive itself: commit would otherwise spawn
    // `git maintenance run --auto --detach`, a background process that races the
    // tests' direct .git mutations (observed as EEXIST on macOS CI when it
    // recreated .git/objects between rm() and symlink()).
    git(&f.repo_root, &["config", "maintenance.auto", "false"]).expect("maintenance.auto");
    git(&f.repo_root, &["config", "user.name", "Fixture"]).expect("user.name");
    git(&f.repo_root, &["config", "user.email", "fixture@example.invalid"]).expect("user.email");
    git(&f.repo_root, &["config", "core.autocrlf", "false"]).expect("core.autocrlf");
    git(&f.repo_root, &["config", "core.symlinks", "false"]).expect("core.symlinks");
    std::fs::write(f.repo_root.join("tracked"), "base\n").expect("tracked file");
    std::fs::write(f.repo_root.join(".gitignore"), "ignored\nnode_modules/\n")
        .expect("gitignore file");
    git(&f.repo_root, &["add", "."]).expect("git add");
    git(&f.repo_root, &["commit", "-m", "fixture"]).expect("git commit");
    f
}
