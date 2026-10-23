use super::{args::walker_flags, paths::relative_path, process::run};
use crate::{definition::AbortSignal, grep::engine::*};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Instant,
};
#[derive(Clone)]
pub struct Candidate {
    pub absolute: PathBuf,
    pub display: String,
    pub size: u64,
    pub ordinal: u32,
}
pub async fn enumerate_candidates(
    request: &GrepEngineRequest,
    result: &mut GrepEngineResult,
    deadline: Instant,
    signal: &AbortSignal,
) -> Result<Vec<Candidate>, GrepEngineError> {
    let mut candidates: BTreeMap<PathBuf, Candidate> = BTreeMap::new();
    for root in &request.paths {
        super::process::check(deadline, signal)?;
        let metadata = tokio::fs::metadata(root).await;
        super::process::check(deadline, signal)?;
        let meta = match metadata {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                result.missing_paths.push(root.clone());
                continue;
            }
            Err(error) => return Err(GrepEngineError::EngineUnavailable(error.to_string())),
        };
        let listed = if meta.is_file() {
            vec![root.clone()]
        } else {
            let mut args = vec![
                "--files".into(),
                "--null".into(),
                "--sort".into(),
                "path".into(),
            ];
            args.extend(walker_flags(request));
            args.extend(["--".into(), root.clone()]);
            let output = run(&args, Path::new(&request.cwd), None, deadline, signal).await?;
            String::from_utf8_lossy(&output)
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        for name in listed {
            super::process::check(deadline, signal)?;
            let absolute = Path::new(&request.cwd).join(name);
            // PERF-DIVERGENCE(senpi grep): upstream uses sync fs per file; this port runs canonicalize and metadata concurrently instead of two serial blocking-pool hops. Error precedence and abort checks keep the same order, so results are identical.
            let (canonical, metadata) = tokio::join!(
                tokio::fs::canonicalize(&absolute),
                tokio::fs::metadata(&absolute)
            );
            super::process::check(deadline, signal)?;
            let canonical =
                canonical.map_err(|e| GrepEngineError::EngineUnavailable(e.to_string()))?;
            super::process::check(deadline, signal)?;
            let size = metadata
                .map_err(|e| GrepEngineError::EngineUnavailable(e.to_string()))?
                .len();
            let display = relative_path(&absolute, Path::new(&request.cwd));
            if candidates
                .get(&canonical)
                .is_none_or(|old| display < old.display)
            {
                candidates.insert(
                    canonical,
                    Candidate {
                        absolute,
                        display,
                        size,
                        ordinal: 0,
                    },
                );
            }
        }
    }
    if result.missing_paths.len() == request.paths.len() {
        return Err(GrepEngineError::PathNotFound(format!(
            "No search paths exist: {}",
            request.paths.join(", ")
        )));
    }
    let mut candidates: Vec<_> = candidates.into_values().collect();
    candidates.sort_by(|a, b| super::paths::path_order(&a.display, &b.display));
    for (i, candidate) in candidates.iter_mut().enumerate() {
        candidate.ordinal = u32::try_from(i + 1).unwrap_or(u32::MAX);
    }
    Ok(candidates)
}
