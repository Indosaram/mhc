use super::{
    argv::{argv_budget, argv_cost, pack_argv, platform, room_after},
    paths::{path_order, relative_path},
    prefix_pass::MAX_FILE_BYTES,
    process::{check, run},
};
use crate::{definition::AbortSignal, grep::engine::*};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Instant,
};

#[derive(Debug)]
pub struct Root {
    pub path: PathBuf,
    pub alias: PathBuf,
    pub cwd: PathBuf,
    /// rg searches an explicitly named file as-is: no walk, ignore, glob, type or size filter applies.
    pub file: bool,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub absolute: PathBuf,
    pub canonical: PathBuf,
    pub display: String,
    pub root: Arc<Root>,
    pub size: u64,
    /// One-based position in the complete filtered candidate order, including binary files.
    pub ordinal: u32,
    pub binary: bool,
}

fn io_error(error: std::io::Error) -> GrepEngineError {
    GrepEngineError::EngineUnavailable(error.to_string())
}

/// Lexical `path.resolve` over an already absolute base.
pub(crate) fn resolve(base: &Path, relative: &str) -> PathBuf {
    let mut result = PathBuf::new();
    for component in base.join(relative).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            component => result.push(component.as_os_str()),
        }
    }
    result
}

pub async fn resolve_roots(
    request: &GrepEngineRequest,
    result: &mut GrepEngineResult,
) -> Result<Vec<Arc<Root>>, GrepEngineError> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for path in &request.paths {
        if !seen.insert(path) {
            continue;
        }
        let (info, canonical) =
            tokio::join!(tokio::fs::metadata(path), tokio::fs::canonicalize(path));
        match (info, canonical) {
            (Ok(info), Ok(canonical)) => {
                // Anchored rg globs use the OS cwd (canonical even for /var aliases on macOS).
                let cwd = if info.is_dir() {
                    canonical.clone()
                } else {
                    canonical
                        .parent()
                        .map_or_else(|| canonical.clone(), Path::to_path_buf)
                };
                roots.push(Arc::new(Root {
                    path: canonical,
                    alias: PathBuf::from(path),
                    cwd,
                    file: info.is_file(),
                }));
            }
            (Err(error), _) | (_, Err(error)) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(io_error(error));
                }
                result.missing_paths.push(path.clone());
            }
        }
    }
    if roots.is_empty() {
        return Err(GrepEngineError::PathNotFound(format!(
            "No search paths exist: {}",
            request.paths.join(", ")
        )));
    }
    Ok(roots)
}

fn split_nul(output: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(output)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub async fn enumerate_candidates(
    roots: &[Arc<Root>],
    cwd: &Path,
    walk: &[String],
    result: &mut GrepEngineResult,
    budget: Option<usize>,
    deadline: Instant,
    signal: &AbortSignal,
) -> Result<Vec<Candidate>, GrepEngineError> {
    let platform = platform();
    let budget = budget.unwrap_or_else(|| argv_budget(platform));
    let mut candidates: BTreeMap<PathBuf, Candidate> = BTreeMap::new();
    for root in roots {
        let listed = if root.file {
            vec![root.path.to_string_lossy().into_owned()]
        } else {
            let mut args: Vec<String> = ["--files", "--null", "--sort", "path"]
                .map(str::to_owned)
                .to_vec();
            args.extend(walk.iter().cloned());
            args.extend(["--".into(), root.path.to_string_lossy().into_owned()]);
            split_nul(&run(&args, &root.cwd, None, deadline, signal).await?)
        };
        for path in listed {
            check(deadline, signal)?;
            let absolute = resolve(&root.cwd, &path);
            // PERF-DIVERGENCE(senpi grep): upstream uses sync fs per file; this port runs canonicalize and metadata concurrently instead of two serial blocking-pool hops. Error precedence and abort checks keep the same order, so results are identical.
            let (canonical, metadata) = tokio::join!(
                tokio::fs::canonicalize(&absolute),
                tokio::fs::metadata(&absolute)
            );
            let (canonical, metadata) = match (canonical, metadata) {
                (Ok(canonical), Ok(metadata)) => (canonical, metadata),
                (Err(error), _) | (_, Err(error)) => {
                    // A named file that vanished after root resolution fails like rg's own listing did.
                    if root.file && error.kind() == std::io::ErrorKind::NotFound {
                        return Err(GrepEngineError::EngineUnavailable(format!(
                            "{}: No such file or directory",
                            root.path.display()
                        )));
                    }
                    return Err(io_error(error));
                }
            };
            let inner = relative_path(&absolute, &root.path);
            let alias = if inner.is_empty() {
                root.alias.clone()
            } else {
                resolve(&root.alias, &inner)
            };
            let mut display = relative_path(&alias, cwd);
            if display.is_empty() {
                display = alias
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
            if candidates
                .get(&canonical)
                .is_none_or(|old| path_order(&display, &old.display).is_lt())
            {
                candidates.insert(
                    canonical.clone(),
                    Candidate {
                        absolute,
                        canonical,
                        display,
                        root: Arc::clone(root),
                        size: metadata.len(),
                        ordinal: 0,
                        binary: false,
                    },
                );
            }
        }
    }
    let mut binary_paths = BTreeSet::new();
    // rg ignores --max-filesize for explicit file roots (the facade's second phase).
    // Oversized roots must be classified solely by the bounded prefix pass.
    let scanned: Vec<&Arc<Root>> = roots
        .iter()
        .filter(|root| candidates.get(&root.path).map_or(0, |c| c.size) <= MAX_FILE_BYTES as u64)
        .collect();
    // Explicit files share one scan per batch; a directory root keeps its own anchored walk.
    let mut nul_scan: Vec<String> = [
        "-a",
        "-l",
        "--null",
        "--sort",
        "path",
        "--encoding",
        "none",
        "--max-filesize",
    ]
    .map(str::to_owned)
    .to_vec();
    nul_scan.push(MAX_FILE_BYTES.to_string());
    nul_scan.extend(walk.iter().cloned());
    nul_scan.extend(["-e".into(), "\\x00".into(), "--".into()]);
    let room = room_after(&nul_scan, budget, platform);
    let files: Vec<&Arc<Root>> = scanned.iter().copied().filter(|root| root.file).collect();
    let mut scans: Vec<(PathBuf, Vec<String>)> = pack_argv(
        files,
        |root| argv_cost(&root.path.to_string_lossy(), platform),
        room,
    )
    .into_iter()
    .map(|batch| {
        (
            batch[0].cwd.clone(),
            batch
                .iter()
                .map(|root| root.path.to_string_lossy().into_owned())
                .collect(),
        )
    })
    .collect();
    scans.extend(scanned.iter().filter(|root| !root.file).map(|root| {
        (
            root.cwd.clone(),
            vec![root.path.to_string_lossy().into_owned()],
        )
    }));
    // Scan the entire normal-size file, including NULs after rg's first search buffer.
    for (scan_cwd, paths) in scans {
        let mut args = nul_scan.clone();
        args.extend(paths);
        let output = run(&args, &scan_cwd, None, deadline, signal).await?;
        for path in split_nul(&output) {
            binary_paths.insert(
                tokio::fs::canonicalize(resolve(&scan_cwd, &path))
                    .await
                    .map_err(io_error)?,
            );
        }
    }
    result.skipped_binary = u32::try_from(
        binary_paths
            .iter()
            .filter(|path| candidates.contains_key(*path))
            .count(),
    )
    .unwrap_or(u32::MAX);
    let mut ordered: Vec<_> = candidates.into_values().collect();
    ordered.sort_by(|a, b| path_order(&a.display, &b.display));
    for (index, candidate) in ordered.iter_mut().enumerate() {
        candidate.ordinal = u32::try_from(index + 1).unwrap_or(u32::MAX);
        candidate.binary = binary_paths.contains(&candidate.canonical);
    }
    Ok(ordered)
}
