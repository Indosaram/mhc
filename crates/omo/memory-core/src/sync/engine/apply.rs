//! The locked half of a cycle (sync-design §5): capture local edits into journaled pending ops
//! before anything remote lands, apply verified batches in seq order, then drive the cycle record
//! merged -> rendered -> committed -> checkpointed. `advance` is the same code path for a fresh
//! cycle and for one resumed after a crash, so each stage is reached exactly once.
//!
//! Crash-resume protocol for the render (stage merged), with the invariants it keeps:
//!  1. Render records in renders/<cycle id>/ are append-only until the cycle ends.
//!  2. A record exists only after its staged render file is durable.
//!  3. A staged file is moved into place only after the record that describes it is durable.
//!  4. For the latest record of a key, its staged file is absent iff that render landed (a delete
//!     is the inverse: the working file is moved to the staged path).
//!  5. A key's baseline is set to a recorded render (text, frontier) only when that render landed
//!     and the item doc is still at that frontier; once a capture moved past it, it stays.
//!  6. Replaying advance from any durable state, any number of times, renders each key at most once
//!     per (key, frontier) and leaves the same working tree, docs and pending ops.
//!
//! A person's edit made after a crash is therefore captured against exactly the text it was made
//! on: the landed render (inv. 5), or the old baseline when the render never landed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::atomic::{TEMP_MARK, remove_atomic, write_atomic};
use super::engine_deps::EngineDeps;
use super::error::EngineResult;
use super::journal::{
    CycleRecord, CycleStage, RenderRecord, SyncJournal, WireItem, from_base64, to_base64,
};
use super::legacy_cycle::adopt_legacy_renders;
use crate::fs::resilient;
use crate::journal::sync_journal_directory;
use crate::support::random::random_id;
use crate::support::sha256::sha256_hex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    pub batches: usize,
    /// Distinct memories the cycle rendered.
    pub memories: usize,
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> EngineResult<()> {
    for name in resilient::read_dir_names(dir)? {
        let path = dir.join(&name);
        let meta = resilient::metadata(&path)?;
        if meta.is_dir() {
            if dir == root && name == ".git" {
                continue;
            }
            walk(root, &path, out)?;
        } else if meta.is_file() && !name.contains(TEMP_MARK) {
            let key = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if key != ".git" && !key.starts_with(".git/") && !key.contains(TEMP_MARK) {
                out.push(key);
            }
        }
    }
    Ok(())
}

fn working_keys(root: &Path) -> EngineResult<Vec<String>> {
    let mut keys = Vec::new();
    walk(root, root, &mut keys)?;
    Ok(keys)
}

fn read_working(root: &Path, key: &str) -> EngineResult<Option<String>> {
    match resilient::read_to_string(&root.join(key)) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn text_hash(text: Option<&str>) -> String {
    match text {
        None => sha256_hex("\u{0}absent".as_bytes()),
        Some(t) => sha256_hex(format!("t:{t}").as_bytes()),
    }
}

fn latest_by_key(records: Vec<RenderRecord>) -> BTreeMap<String, RenderRecord> {
    let mut latest = BTreeMap::new();
    for record in records {
        latest.insert(record.key.clone(), record);
    }
    latest
}

/// Invariant 5: a landed render becomes the baseline only while the doc is still at its frontier.
fn adopt_landed_renders(
    deps: &EngineDeps,
    journal: &SyncJournal,
    cycle: &CycleRecord,
) -> EngineResult<()> {
    let latest = latest_by_key(journal.renders(&cycle.id)?);
    for record in latest.values() {
        if !journal.render_landed(&cycle.id, record) {
            continue;
        }
        if deps.store.version(&record.key)? != record.frontier {
            continue;
        }
        deps.store
            .mark_materialized(&record.key, record.text.as_deref())?;
    }
    let recorded: BTreeSet<String> = latest.keys().cloned().collect();
    let root = deps.repo.root().to_path_buf();
    adopt_legacy_renders(deps, cycle, &recorded, &|key| read_working(&root, key))
}

pub fn capture(
    deps: &EngineDeps,
    journal: &SyncJournal,
    resuming: Option<&CycleRecord>,
) -> EngineResult<usize> {
    // Resuming a cycle at merged: every render that landed is the base of any edit made since.
    if let Some(cycle) = resuming {
        adopt_landed_renders(deps, journal, cycle)?;
    }
    let root = deps.repo.root();
    let mut keys: BTreeSet<String> = deps.store.keys().into_iter().collect();
    keys.extend(working_keys(root)?);
    let mut captured: Vec<WireItem> = Vec::new();
    let mut texts: BTreeMap<String, Option<String>> = BTreeMap::new();
    for key in keys {
        let text = read_working(root, &key)?;
        let Some(ops) = deps.store.capture(&key, text.as_deref())? else {
            continue;
        };
        captured.push(WireItem {
            key: key.clone(),
            ops: to_base64(&ops),
        });
        texts.insert(key, text);
    }
    if captured.is_empty() {
        return Ok(0);
    }
    let mut pending = journal.pending()?;
    // PERF-DIVERGENCE(omo sync engine): upstream scans pending linearly per captured item (O(captured x pending)); this port probes a hash set of (key, ops) pairs. The selected items are identical.
    let fresh: Vec<WireItem> = {
        let known: std::collections::HashSet<(&str, &str)> = pending
            .items
            .iter()
            .map(|p| (p.key.as_str(), p.ops.as_str()))
            .collect();
        captured
            .iter()
            .filter(|c| !known.contains(&(c.key.as_str(), c.ops.as_str())))
            .cloned()
            .collect()
    };
    pending.items.extend(fresh);
    journal.set_pending(&pending)?;
    deps.fault_at("captured")?;
    for item in &captured {
        deps.store.apply(&item.key, &from_base64(&item.ops)?)?;
        // The captured text is now what the item's doc holds; without this the next capture sees the
        // same edit again and pushes it again (and a restarted device would not reload the item).
        let text = texts.get(&item.key).and_then(|t| t.as_deref());
        deps.store.mark_materialized(&item.key, text)?;
    }
    deps.store.persist()?;
    deps.fault_at("persisted")?;
    Ok(captured.len())
}

pub fn apply_verified(deps: &EngineDeps, journal: &SyncJournal) -> EngineResult<Applied> {
    if let Some(resumed) = journal.state()?.cycle {
        advance(deps, journal, resumed.clone())?;
        return Ok(Applied {
            batches: 0,
            memories: resumed.keys.len(),
        });
    }
    let records: Vec<_> = journal
        .incoming()?
        .into_iter()
        .filter(|r| r.stage == super::journal::IncomingStage::Verified)
        .collect();
    if records.is_empty() {
        return Ok(Applied {
            batches: 0,
            memories: 0,
        });
    }
    // A cycle that ended in a crash right after clearing its record leaves its renders behind.
    journal.clear_renders(None)?;
    let mut keys = BTreeSet::new();
    for record in &records {
        let items: Vec<super::ports::PayloadItem> = record
            .items
            .iter()
            .map(|i| {
                Ok(super::ports::PayloadItem {
                    key: i.key.clone(),
                    ops: from_base64(&i.ops)?,
                })
            })
            .collect::<EngineResult<_>>()?;
        if record.snapshot {
            // PERF-DIVERGENCE(omo sync engine): upstream deep-copies every payload item for the snapshot call; this port moves the items into the payload and reads the keys back from it. Applied data and keys are identical.
            let payload = super::ports::BatchPayload { items };
            deps.store.apply_snapshot(&payload)?;
            keys.extend(payload.items.into_iter().map(|i| i.key));
        } else {
            for item in &items {
                deps.store.apply(&item.key, &item.ops)?;
            }
            keys.extend(items.into_iter().map(|i| i.key));
        }
    }
    deps.store.persist()?;
    let state = journal.state()?;
    let last = records.last().map_or(state.applied, |r| r.entry.seq);
    let cycle = CycleRecord {
        id: random_id().chars().take(16).collect(),
        stage: CycleStage::Merged,
        first: state.applied + 1,
        last,
        batches: records.iter().map(|r| r.entry.clone()).collect(),
        keys: keys.iter().cloned().collect(),
        tree_id: None,
        progress: None,
        texts: None,
    };
    journal.update_state(|s| s.cycle = Some(cycle.clone()))?;
    deps.fault_at("merged")?;
    advance(deps, journal, cycle)?;
    Ok(Applied {
        batches: records.iter().filter(|r| !r.snapshot).count(),
        memories: keys.len(),
    })
}

/// Move a staged render into place (or the working file aside, for a delete), durably.
fn land(
    journal: &SyncJournal,
    root: &Path,
    cycle_id: &str,
    record: &RenderRecord,
) -> EngineResult<()> {
    let target = root.join(&record.key);
    let staged = journal.staged_path(cycle_id, record.seq);
    let target_dir = target.parent().unwrap_or(root).to_path_buf();
    let staged_dir = staged
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if record.text.is_none() {
        if resilient::exists(&target) {
            resilient::rename(&target, &staged)?;
        } else {
            write_atomic(&staged, b"")?;
        }
        sync_journal_directory(&target_dir)?;
        sync_journal_directory(&staged_dir)?;
        return Ok(());
    }
    resilient::create_dir_all(&target_dir)?;
    resilient::rename(&staged, &target)?;
    sync_journal_directory(&target_dir)?;
    // Invariant 4 reads the staged file's absence, so that side of the rename must be durable too.
    sync_journal_directory(&staged_dir)?;
    Ok(())
}

#[derive(Clone, Copy)]
enum SavePoint {
    Rendered,
    Committed,
    Checkpointed,
}

pub fn advance(deps: &EngineDeps, journal: &SyncJournal, start: CycleRecord) -> EngineResult<()> {
    let mut cycle = start;
    let root = deps.repo.root().to_path_buf();
    let save = |cycle: &CycleRecord, point: SavePoint| -> EngineResult<()> {
        journal.update_state(|s| {
            s.cycle = Some(cycle.clone());
            if matches!(point, SavePoint::Checkpointed) {
                s.applied = cycle.last;
            }
        })?;
        deps.fault_at(match point {
            SavePoint::Rendered => "rendered",
            SavePoint::Committed => "committed",
            SavePoint::Checkpointed => "checkpointed",
        })
    };
    if cycle.stage == CycleStage::Merged {
        let prior_renders = journal.renders(&cycle.id)?;
        // PERF-DIVERGENCE(omo sync engine): upstream re-lists and re-parses every render record per key to find the next seq; this port reads the max seq once and increments it locally. Records are append-only under the sync lock, so the seq values are identical.
        let mut next_seq = prior_renders.iter().map(|r| r.seq).max().unwrap_or(0) + 1;
        let latest = latest_by_key(prior_renders);
        for key in &cycle.keys {
            let working = read_working(&root, key)?;
            let frontier = deps.store.version(key)?;
            // Invariant 6: this (key, frontier) already rendered, landed and untouched.
            if let Some(prior) = latest.get(key)
                && prior.frontier == frontier
                && journal.render_landed(&cycle.id, prior)
                && text_hash(working.as_deref()) == prior.hash
            {
                continue;
            }
            let text = deps.store.render(key, working.as_deref())?;
            let record = RenderRecord {
                seq: next_seq,
                key: key.clone(),
                frontier,
                hash: text_hash(text.as_deref()),
                text,
            };
            // A crash at `staged` leaves a staged file with no record at this seq; it must not
            // survive into a delete record, where a present staged file means "landed" (inv. 4).
            remove_atomic(&journal.staged_path(&cycle.id, record.seq))?;
            if let Some(text) = &record.text {
                write_atomic(&journal.staged_path(&cycle.id, record.seq), text.as_bytes())?;
            }
            deps.fault_at("staged")?;
            journal.append_render(&cycle.id, &record)?;
            deps.fault_at("render-intent")?;
            land(journal, &root, &cycle.id, &record)?;
            next_seq += 1;
            deps.fault_at("render-key")?;
        }
        let tree_id = deps.repo.stage_tree(&cycle.keys)?;
        cycle = CycleRecord {
            stage: CycleStage::Rendered,
            tree_id: Some(tree_id),
            ..cycle
        };
        save(&cycle, SavePoint::Rendered)?;
    }
    if cycle.stage == CycleStage::Rendered {
        let range = format!("{}-{}", cycle.first, cycle.last);
        let changed = deps.repo.stage_tree(&cycle.keys)? != deps.repo.head_tree()?;
        if changed && !deps.repo.has_sync_commit(&range)? {
            let principals: BTreeSet<&str> =
                cycle.batches.iter().map(|b| b.author.as_str()).collect();
            let principals = principals.into_iter().collect::<Vec<_>>().join(", ");
            let message = format!(
                "sync: {} memories updated\n\nSync-Seq: {range}\nSync-Principals: {principals}",
                cycle.keys.len()
            );
            deps.repo.commit(&cycle.keys, &message)?;
            deps.fault_at("git-commit")?;
        }
        cycle = CycleRecord {
            stage: CycleStage::Committed,
            ..cycle
        };
        save(&cycle, SavePoint::Committed)?;
    }
    if cycle.stage == CycleStage::Committed {
        deps.repo.sync_index(&cycle.keys)?;
        adopt_landed_renders(deps, journal, &cycle)?;
        deps.store.persist()?;
        journal.record_applied(&cycle.batches)?;
        cycle = CycleRecord {
            stage: CycleStage::Checkpointed,
            ..cycle
        };
        save(&cycle, SavePoint::Checkpointed)?;
    }
    for batch in &cycle.batches {
        journal.drop_incoming(batch.seq)?;
    }
    journal.update_state(|s| s.cycle = None)?;
    journal.clear_renders(None)?;
    Ok(())
}
