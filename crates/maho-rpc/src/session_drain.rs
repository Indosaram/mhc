//! One handoff-drain pass's decisions (senpi `rpc/session-drain.ts`).
//!
//! Every session is judged on its own: a session whose activity cannot be read cannot be proven busy, so
//! it parks like any other (reported through `unreadable`) and never strands the rest on a superseded
//! generation.
use crate::handoff_activity::is_handoff_busy;
use crate::session_path_key::session_directory_removed;
use maho_core::session_activity::SessionActivitySnapshot;
use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DrainVerdicts {
    /// Settled sessions a handoff parks: the successor reopens each by its `sessionPath`.
    pub park: Vec<String>,
    /// Settled sessions whose transcript directory is gone; the drain ends them as
    /// `session_dir_removed` instead of handing out a dead path (senpi#2285).
    pub gone: Vec<String>,
}

/// What a drain reads from one open registry entry.
pub struct DrainCandidate<'a> {
    pub session_id: &'a str,
    pub session_path: Option<&'a Path>,
    pub worker_handoff_busy: bool,
    /// The runtime's activity snapshot: `None` when it publishes none, `Some(Err)` when reading failed.
    pub activity: Option<Result<SessionActivitySnapshot, String>>,
}

pub fn select_drain_verdicts<'a>(
    candidates: impl IntoIterator<Item = DrainCandidate<'a>>,
    in_flight: impl Fn(&str) -> bool,
    mut unreadable: impl FnMut(&str, &str),
) -> DrainVerdicts {
    let mut verdicts = DrainVerdicts::default();
    for candidate in candidates {
        if in_flight(candidate.session_id) {
            continue;
        }
        let busy = match &candidate.activity {
            _ if candidate.worker_handoff_busy => true,
            Some(Ok(snapshot)) => is_handoff_busy(*snapshot),
            Some(Err(cause)) => {
                unreadable(candidate.session_id, cause);
                false
            }
            None => false,
        };
        if busy {
            continue;
        }
        let id = candidate.session_id.to_owned();
        if candidate
            .session_path
            .is_some_and(session_directory_removed)
        {
            verdicts.gone.push(id);
        } else {
            verdicts.park.push(id);
        }
    }
    verdicts
}

/// One drain pass for the host: logs what it skips.
pub fn read_drain_verdicts<'a>(
    candidates: impl IntoIterator<Item = DrainCandidate<'a>>,
    in_flight: impl Fn(&str) -> bool,
) -> DrainVerdicts {
    select_drain_verdicts(candidates, in_flight, |session_id, cause| {
        eprintln!("senpi rpc handoff activity of {session_id} unreadable, parking: {cause}");
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate<'a>(
        id: &'a str,
        activity: Option<Result<SessionActivitySnapshot, String>>,
    ) -> DrainCandidate<'a> {
        DrainCandidate {
            session_id: id,
            session_path: None,
            worker_handoff_busy: false,
            activity,
        }
    }

    #[test]
    fn unreadable_activity_parks_and_busy_sessions_stay() {
        let mut reported = Vec::new();
        let busy = SessionActivitySnapshot {
            is_streaming: true,
            ..Default::default()
        };
        let verdicts = select_drain_verdicts(
            [
                candidate("busy", Some(Ok(busy))),
                candidate("broken", Some(Err("boom".into()))),
                candidate("quiet", None),
                candidate("inflight", None),
            ],
            |id| id == "inflight",
            |id, _| reported.push(id.to_owned()),
        );
        assert_eq!(verdicts.park, vec!["broken".to_owned(), "quiet".to_owned()]);
        assert_eq!(reported, vec!["broken".to_owned()]);
    }

    #[test]
    fn a_removed_directory_is_gone_not_parked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gone").join("s.jsonl");
        let mut gone = candidate("g", None);
        gone.session_path = Some(path.as_path());
        let verdicts = select_drain_verdicts([gone], |_| false, |_, _| {});
        assert_eq!(verdicts.gone, vec!["g".to_owned()]);
    }
}
