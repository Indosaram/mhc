//! Incremental last-touch times, persisted in the common git directory.
use super::{errors::GitError, exec::GitExecResult};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};
type Times = BTreeMap<String, i64>;
type Runner<'a> = dyn Fn(&[String], u64) -> Result<GitExecResult, GitError> + 'a;
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Stored {
    version: u32,
    revision: String,
    times: Times,
}
#[derive(Default)]
pub struct PathCommitTimes {
    computed: Mutex<Vec<(String, Times)>>,
}
impl PathCommitTimes {
    pub fn at(
        &self,
        dir: &Path,
        revision: &str,
        present: &[String],
        run: &Runner<'_>,
    ) -> Result<Times, GitError> {
        let oid = if full_object_id(revision) {
            revision.to_string()
        } else {
            run(
                &[
                    "rev-parse".into(),
                    "--verify".into(),
                    format!("{revision}^{{commit}}"),
                ],
                30_000,
            )?
            .stdout
            .trim()
            .to_string()
        };
        let mut cache = self.computed.lock().expect("commit times cache");
        if let Some(index) = cache.iter().position(|(key, _)| key == &oid) {
            let entry = cache.remove(index);
            let live = live_times(present, &entry.1);
            cache.push(entry);
            return Ok(live);
        }
        let store_path = || -> Result<PathBuf, GitError> {
            let common = run(&["rev-parse".into(), "--git-common-dir".into()], 30_000)?.stdout;
            let common = PathBuf::from(common.trim());
            Ok((if common.is_absolute() {
                common
            } else {
                dir.join(common)
            })
            .join("omo-path-commit-times.json"))
        };
        let path = store_path().ok();
        let stored = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Stored>(&bytes).ok())
            .filter(|s| {
                s.version == 2
                    && full_object_id(&s.revision)
                    && s.times
                        .values()
                        .all(|v| v.unsigned_abs() <= 9_007_199_254_740_991)
            });
        let ancestor = |base: &str| -> Result<bool, GitError> {
            Ok(run(
                &[
                    "merge-base".into(),
                    "--is-ancestor".into(),
                    base.into(),
                    oid.clone(),
                ],
                30_000,
            )?
            .code
                == 0)
        };
        let mut base = None;
        for (key, times) in cache.iter().rev() {
            if key == &oid || ancestor(key)? {
                base = Some((key.clone(), times.clone(), false));
                break;
            }
        }
        if base.is_none()
            && let Some(stored) = &stored
                && (stored.revision == oid || ancestor(&stored.revision)?) {
                    base = Some((stored.revision.clone(), stored.times.clone(), true));
                }
        let (all, write) = if let Some((key, mut times, persisted)) = base {
            if key != oid {
                for (path, time) in
                    parse_log_times(&log(run, &format!("{key}..{oid}"), 30_000)?.stdout)
                {
                    times
                        .entry(path)
                        .and_modify(|old| *old = (*old).max(time))
                        .or_insert(time);
                }
            }
            (times, persisted)
        } else {
            (
                parse_log_times(&log(run, &oid, 300_000)?.stdout),
                match &stored {
                    None => true,
                    Some(s) => ancestor(&s.revision)?,
                },
            )
        };
        if write
            && let Some(path) = path {
                let temporary = path.with_extension(format!(
                    "{}.{}.tmp",
                    std::process::id(),
                    crate::support::random::random_uuid()
                ));
                let save = || -> std::io::Result<()> {
                    let bytes = serde_json::to_vec(&Stored {
                        version: 2,
                        revision: oid.clone(),
                        times: all.clone(),
                    })?;
                    std::fs::write(&temporary, bytes)?;
                    std::fs::rename(&temporary, &path)
                };
                if save().is_err() {
                    let _ = std::fs::remove_file(temporary);
                }
            }
        let live = live_times(present, &all);
        cache.push((oid, all));
        if cache.len() > 4 {
            cache.remove(0);
        }
        Ok(live)
    }
}
fn full_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn log(run: &Runner<'_>, range: &str, timeout: u64) -> Result<GitExecResult, GitError> {
    let mut args = super::stable_log_output::stable_log_output();
    args.extend(
        [
            "log",
            "--format=%x01%ct",
            "--name-only",
            "-z",
            "--no-renames",
            "--diff-merges=first-parent",
            "--diff-filter=d",
            range,
            "--",
        ]
        .map(str::to_string),
    );
    run(&args, timeout)
}
pub fn parse_log_times(output: &str) -> Times {
    let mut times = Times::new();
    let mut committed = None;
    for token in output.split('\0') {
        if let Some(mark) = token.strip_prefix('\x01') {
            committed = mark.trim().parse::<i64>().ok();
            continue;
        }
        let path = token.strip_prefix('\n').unwrap_or(token);
        if let Some(time) = committed.filter(|_| !path.is_empty()) {
            times
                .entry(path.into())
                .and_modify(|old| *old = (*old).max(time))
                .or_insert(time);
        }
    }
    times
}
fn live_times(present: &[String], all: &Times) -> Times {
    present
        .iter()
        .filter_map(|p| all.get(p).map(|time| (p.clone(), *time)))
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn takes_max_even_when_older_parent_is_listed_later() {
        assert_eq!(
            parse_log_times("\x01100\0\na.md\0\x0190\0\na.md\0b.md\0"),
            BTreeMap::from([("a.md".into(), 100), ("b.md".into(), 90)])
        );
    }
}
