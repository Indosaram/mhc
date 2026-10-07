use std::path::Path;

use crate::backend::{
    BackendKind, IsolationBackend, IsolationContext, IsolationError, ProbeResult, Result,
    StartDetail,
};
use crate::backend_marker::mark_started;
use crate::backends::btrfs::remove_dir_all_force;
use crate::backends::copy_tree::{apply_metadata, copy_budget, copy_tree};
use crate::git::command::{exists, git, git_result, str_args};
use crate::util::symlink_at;

fn copy_file_new(source: &Path, destination: &Path) -> Result<()> {
    let mut source_file = std::fs::File::open(source)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut destination_file = options.open(destination)?;
    std::io::copy(&mut source_file, &mut destination_file)?;
    Ok(())
}

fn cp_recursive_filtered(
    source: &Path,
    destination: &Path,
    consume: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<()> {
    let info = std::fs::symlink_metadata(source)?;
    if info.file_type().is_symlink() {
        symlink_at(&std::fs::read_link(source)?, destination, source)?;
        return Ok(());
    }
    if info.is_dir() {
        std::fs::create_dir(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            cp_recursive_filtered(&entry.path(), &destination.join(entry.file_name()), consume)?;
        }
        return Ok(());
    }
    if info.is_file() {
        consume(info.len())?;
        copy_file_new(source, destination)?;
        apply_metadata(destination, &info)?;
    }
    Ok(())
}

pub fn seed_dirty_state(
    lower: &Path,
    merged: &Path,
    consume: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<()> {
    let staged = git(
        lower,
        &str_args(&[
            "diff",
            "--binary",
            "--no-color",
            "--no-ext-diff",
            "--cached",
        ]),
        None,
    )?;
    if !staged.is_empty() {
        git(
            merged,
            &str_args(&["apply", "--index", "--binary", "-"]),
            Some(staged),
        )?;
    }
    let unstaged = git(
        lower,
        &str_args(&["diff", "--binary", "--no-color", "--no-ext-diff"]),
        None,
    )?;
    if !unstaged.is_empty() {
        git(
            merged,
            &str_args(&["apply", "--binary", "-"]),
            Some(unstaged),
        )?;
    }
    let untracked = git(
        lower,
        &str_args(&["ls-files", "--others", "--exclude-standard", "-z"]),
        None,
    )?;
    for entry in String::from_utf8_lossy(&untracked)
        .split('\0')
        .filter(|entry| !entry.is_empty())
    {
        // Git reports an embedded repository as a single directory entry and does not
        // descend into it; strip its trailing separator and seed the whole tree.
        let name = entry.strip_suffix('/').unwrap_or(entry);
        let source = lower.join(name);
        let destination = merged.join(name);
        let info = std::fs::symlink_metadata(&source)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if info.file_type().is_symlink() {
            symlink_at(&std::fs::read_link(&source)?, &destination, &source)?;
        } else if info.is_dir() {
            cp_recursive_filtered(&source, &destination, consume)?;
        } else if info.is_file() {
            consume(info.len())?;
            std::fs::copy(&source, &destination)?;
            apply_metadata(&destination, &info)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RcopyBackend;

impl IsolationBackend for RcopyBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Rcopy
    }

    fn clones_tree(&self) -> bool {
        false
    }

    fn probe(&self, _repo_root: &Path, _ctx: Option<&IsolationContext>) -> Result<ProbeResult> {
        Ok(ProbeResult::available())
    }

    fn start(
        &self,
        lower: &Path,
        merged: &Path,
        ctx: &IsolationContext,
    ) -> Result<Option<StartDetail>> {
        if exists(merged)? {
            return Err(IsolationError::other(format!(
                "rcopy destination already exists: {}",
                merged.display()
            )));
        }
        if let Some(parent) = merged.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if exists(&lower.join(".git"))? {
            // Missing git is unavailable, not permission to copy shared git metadata blindly.
            git(
                lower,
                &str_args(&[
                    "worktree",
                    "add",
                    "--detach",
                    &merged.to_string_lossy(),
                    "HEAD",
                ]),
                None,
            )?;
            let mut budget = copy_budget(&ctx.base_dir, ctx.max_copy_bytes, None)?;
            seed_dirty_state(lower, merged, &mut |size| budget.consume(size))?;
            mark_started(&ctx.base_dir, self.kind(), &[])?;
            return Ok(None);
        }
        let mut budget = copy_budget(&ctx.base_dir, ctx.max_copy_bytes, None)?;
        // Copy through the shared walker: it counts during the traversal (no
        // prewalk), skips sockets/FIFOs like the old fs.cp filter, and never
        // descends into its own destination when a pathological layout places it
        // inside the source.
        copy_tree(
            lower,
            merged,
            &mut |source, destination, _size| {
                std::fs::copy(source, destination)?;
                Ok(())
            },
            &mut |size| budget.consume(size),
        )?;
        mark_started(&ctx.base_dir, self.kind(), &[])?;
        Ok(None)
    }

    fn stop(&self, merged: &Path) -> Result<()> {
        let entry = merged.join(".git");
        if exists(&entry)? && std::fs::symlink_metadata(&entry)?.is_file() {
            // Resolve registration through the worktree itself, including after process restart.
            let admin = std::fs::read_to_string(&entry)
                .ok()
                .map(|text| text.trim().trim_start_matches("gitdir: ").to_string());
            match git_result(
                merged,
                &str_args(&[
                    "worktree",
                    "remove",
                    "--force",
                    &merged.to_string_lossy(),
                ]),
                None,
            ) {
                Ok(result) if result.code != 0 => {
                    // Only an actually-gone registration makes removal failure spurious;
                    // a live one must surface instead of leaking a stale registration.
                    if let Some(admin) = &admin {
                        if exists(Path::new(admin))? {
                            return Err(IsolationError::other(format!(
                                "git worktree remove failed ({}): {}",
                                result.code, result.stderr
                            )));
                        }
                    }
                }
                Ok(_) => {}
                // Teardown remains possible if git disappeared after creation.
                Err(error) if error.is_unavailable() => {}
                Err(error) => return Err(error),
            }
        }
        remove_dir_all_force(merged)
    }
}
