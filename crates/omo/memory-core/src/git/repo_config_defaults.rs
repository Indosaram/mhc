//! Machine-owned repository defaults and byte-preserving checkout repair.
use super::{errors::GitError, exec::GitExecResult};
use std::path::Path;

type Runner<'a> = dyn Fn(&[String], Option<&[u8]>) -> Result<GitExecResult, GitError> + 'a;

pub fn ensure_repo_config_defaults(
    dir: &Path,
    agent_id: &str,
    author_name: &str,
    get: impl Fn(&str) -> Result<Option<String>, GitError>,
    set: impl Fn(&str, &str) -> Result<(), GitError>,
    run: &Runner<'_>,
) -> Result<(), GitError> {
    if get("omo.agentId")?.as_deref() != Some(agent_id) {
        set("omo.agentId", agent_id)?;
    }
    if get("user.email")?.is_none() {
        set("user.email", &format!("{agent_id}@omo.local"))?;
    }
    if get("user.name")?.is_none() {
        set("user.name", author_name)?;
    }
    for (key, value) in [("commit.gpgsign", "false"), ("gc.auto", "0")] {
        if get(key)?.is_none() {
            set(key, value)?;
        }
    }
    if get("core.autocrlf")?.as_deref() != Some("false")
        && restore_converted_checkouts(dir, run).unwrap_or(false)
    {
        set("core.autocrlf", "false")?;
    }
    Ok(())
}

pub fn restore_converted_checkouts(dir: &Path, run: &Runner<'_>) -> Result<bool, GitError> {
    let listing = run(&["ls-files".into(), "--eol".into(), "-z".into()], None)?;
    if listing.code != 0 {
        return Ok(false);
    }
    for record in listing.stdout.split('\0') {
        let Some((eol, path)) = record.split_once('\t') else {
            continue;
        };
        let mut eol = eol.split_whitespace();
        if eol.next() != Some("i/lf") || !matches!(eol.next(), Some("w/crlf" | "w/mixed")) {
            continue;
        }
        let target = dir.join(path);
        let Ok(copy) = std::fs::read(&target) else {
            continue;
        };
        let bytes = drop_carriage_returns_before_line_feeds(&copy);
        if !matches_index_blob(run, path, &bytes)? {
            continue;
        }
        let repair = || -> std::io::Result<()> {
            let permissions = std::fs::metadata(&target)?.permissions();
            let temporary = target.with_file_name(format!(
                "{}.omo-eol.tmp",
                target.file_name().unwrap_or_default().to_string_lossy()
            ));
            std::fs::write(&temporary, &bytes)?;
            std::fs::set_permissions(&temporary, permissions)?;
            if let Err(error) = std::fs::rename(&temporary, &target) {
                let _ = std::fs::remove_file(&temporary);
                return Err(error);
            }
            Ok(())
        };
        if repair().is_err() {
            return Ok(false);
        }
    }
    reindex_unchanged_copies(dir, run)
}

fn reindex_unchanged_copies(dir: &Path, run: &Runner<'_>) -> Result<bool, GitError> {
    let changed = run(
        &["diff-files".into(), "--name-only".into(), "-z".into()],
        None,
    )?;
    if changed.code != 0 {
        return Ok(false);
    }
    let mut input = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in changed.stdout.split('\0').filter(|p| !p.is_empty()) {
        if !seen.insert(path) {
            continue;
        }
        if let Ok(copy) = std::fs::read(dir.join(path))
            && matches_index_blob(run, path, &copy)? {
                input.extend_from_slice(path.as_bytes());
                input.push(0);
            }
    }
    if input.is_empty() {
        return Ok(true);
    }
    Ok(run(
        &[
            "-c".into(),
            "core.autocrlf=false".into(),
            "update-index".into(),
            "-q".into(),
            "-z".into(),
            "--stdin".into(),
        ],
        Some(&input),
    )?
    .code
        == 0)
}

fn matches_index_blob(run: &Runner<'_>, path: &str, bytes: &[u8]) -> Result<bool, GitError> {
    let blob = run(
        &["cat-file".into(), "blob".into(), format!(":{path}")],
        None,
    )?;
    Ok(blob.code == 0 && blob.stdout.as_bytes() == bytes)
}

fn drop_carriage_returns_before_line_feeds(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| {
            if *byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                None
            } else {
                Some(*byte)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_cr_before_lf_is_removed() {
        assert_eq!(
            drop_carriage_returns_before_line_feeds(b"a\r\nb\rc\n"),
            b"a\nb\rc\n"
        );
    }
}
