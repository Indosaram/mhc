//! One-shot migrations of older config shapes, with a lock, a journal and backups (spec §12).

pub mod lock;
pub mod plans;
pub mod transforms;

use crate::schema::{self, Mode, dotted};
use crate::writer::{Edit, WriteOptions, WriteTarget, backup_timestamp, first_free, update_config};
use maho_support::{parse_jsonc_lenient, write_atomically};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Lock file name inside `<home>/.omo` (interface fact).
pub const LOCK_FILE: &str = ".migration.lock";
/// Journal file name inside `<home>/.omo` (interface fact).
pub const JOURNAL_FILE: &str = ".migration-journal.json";
/// Journal format version.
pub const JOURNAL_VERSION: i64 = 1;

/// How a plan produces the new document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanMode {
    /// Add the transform output to the target without clobbering existing values.
    Merge,
    /// The transform output replaces the target content.
    ReplaceTarget,
}

/// A source file of a merge plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanSource {
    pub path: PathBuf,
    /// Explicit backup destination (must not exist).
    pub backup: Option<PathBuf>,
}

/// What a transform sees.
pub struct TransformInput<'a> {
    pub target: &'a Value,
    /// Parsed existing sources (merge mode).
    pub sources: &'a [(PathBuf, Value)],
}

/// What a transform returns.
#[derive(Clone, Debug, Default)]
pub struct TransformOutput {
    pub document: Value,
    pub diagnostics: Vec<String>,
}

pub type Transform =
    Arc<dyn Fn(&TransformInput<'_>) -> Result<TransformOutput, String> + Send + Sync>;
pub type Predicate = Arc<dyn Fn(&Value) -> bool + Send + Sync>;

/// One migration plan.
#[derive(Clone)]
pub struct Plan {
    pub id: String,
    pub mode: PlanMode,
    pub predicate: Option<Predicate>,
    pub target: PathBuf,
    pub sources: Vec<PlanSource>,
    pub transform: Transform,
}

/// A durable boundary reported to the test hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    JournalWritten,
    TargetWritten,
    TargetRecorded,
    SourceMoved,
    SourceRecorded,
}

pub type BoundaryHook = Arc<dyn Fn(Boundary) -> Result<(), String> + Send + Sync>;

/// Injectable process facts.
#[derive(Clone)]
pub struct MigrationEnv {
    pub now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub pid: i64,
    pub is_alive: Arc<dyn Fn(i64) -> bool + Send + Sync>,
    pub lease_ms: i64,
    pub hook: Option<BoundaryHook>,
}

impl Default for MigrationEnv {
    fn default() -> Self {
        Self {
            now_ms: Arc::new(|| chrono::Utc::now().timestamp_millis()),
            pid: i64::from(std::process::id()),
            is_alive: Arc::new(process_alive),
            lease_ms: lock::LEASE_MS,
            hook: None,
        }
    }
}

impl MigrationEnv {
    fn boundary(&self, boundary: Boundary) -> Result<(), String> {
        match &self.hook {
            Some(hook) => hook(boundary),
            None => Ok(()),
        }
    }

    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp_millis((self.now_ms)()).unwrap_or_else(chrono::Utc::now)
    }

    /// The ISO-8601 timestamp (`:` and `.` replaced by `-`) used in backup names.
    pub fn stamp(&self) -> String {
        backup_timestamp(self.now())
    }
}

/// Whether a process id is alive (Unix: signal 0; elsewhere assumed alive).
pub fn process_alive(pid: i64) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
            Ok(()) => true,
            Err(errno) => errno == nix::errno::Errno::EPERM,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Batch options.
#[derive(Clone)]
pub struct BatchOptions {
    /// `<home>/.omo` holds the lock and the journal.
    pub omo_dir: PathBuf,
    pub dry_run: bool,
    pub env: MigrationEnv,
}

/// Per-plan status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStatus {
    Skipped,
    Planned,
    Migrated,
}

/// A source → backup move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// What a dry run would do.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub target: PathBuf,
    pub document: Value,
    pub moves: Vec<Move>,
}

/// One plan's outcome.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanResult {
    pub id: String,
    pub target: PathBuf,
    pub status: PlanStatus,
    pub diagnostics: Vec<String>,
    pub preview: Option<Preview>,
    pub target_backup: Option<PathBuf>,
}

/// Batch status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchStatus {
    Completed,
    Locked,
}

/// The batch outcome. `error` is set when a plan aborted the batch (later plans did not run).
#[derive(Clone, Debug, PartialEq)]
pub struct BatchResult {
    pub status: BatchStatus,
    pub journal_resumed: bool,
    pub resumed_target_backup: Option<PathBuf>,
    pub plans: Vec<PlanResult>,
    pub error: Option<String>,
}

/// Runs a batch of plans under the migration lock.
pub fn run_migrations(plans: &[Plan], options: &BatchOptions) -> BatchResult {
    let lock_path = options.omo_dir.join(LOCK_FILE);
    let journal_path = options.omo_dir.join(JOURNAL_FILE);
    let mut result = BatchResult {
        status: BatchStatus::Completed,
        journal_resumed: false,
        resumed_target_backup: None,
        plans: Vec::new(),
        error: None,
    };
    let mut held = match lock::acquire(&lock_path, &options.env) {
        Ok(Some(held)) => held,
        Ok(None) => {
            result.status = BatchStatus::Locked;
            return result;
        }
        Err(error) => {
            result.error = Some(format!(
                "cannot take the migration lock {}: {error}",
                lock_path.display()
            ));
            return result;
        }
    };
    let mut ctx = Ctx {
        options,
        lock_path: &lock_path,
        journal_path: &journal_path,
        held: &mut held,
    };
    if journal_path.exists() {
        match ctx.resume() {
            Ok(backup) => {
                result.journal_resumed = true;
                result.resumed_target_backup = backup;
            }
            Err(error) => {
                result.error = Some(error);
                return result;
            }
        }
    }
    for plan in plans {
        match ctx.run_plan(plan) {
            Ok(plan_result) => result.plans.push(plan_result),
            Err(error) => {
                result.error = Some(format!(
                    "migration {} of {}: {error}",
                    plan.id,
                    plan.target.display()
                ));
                break;
            }
        }
    }
    held.release();
    result
}

struct Ctx<'a> {
    options: &'a BatchOptions,
    lock_path: &'a Path,
    journal_path: &'a Path,
    held: &'a mut lock::HeldLock,
}

fn read_document(path: &Path) -> Result<Option<Value>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let parsed = parse_jsonc_lenient(&text);
    if !parsed.errors.is_empty() {
        let errors: Vec<String> = parsed.errors.iter().map(ToString::to_string).collect();
        return Err(format!(
            "{} has JSONC errors: {}",
            path.display(),
            errors.join("; ")
        ));
    }
    match parsed.value {
        Some(value @ Value::Object(_)) => Ok(Some(value)),
        None => Ok(Some(Value::Object(Map::new()))),
        Some(_) => Err(format!("{}: the root must be an object", path.display())),
    }
}

/// Adds `incoming` into `target` without clobbering; conflicting scalars keep the target value.
pub fn merge_without_clobber(
    target: &mut Value,
    incoming: &Value,
    path: &str,
    diagnostics: &mut Vec<String>,
) {
    let (Value::Object(target), Value::Object(incoming)) = (target, incoming) else {
        return;
    };
    for (key, value) in incoming {
        if schema::is_unsafe_key(key) {
            continue;
        }
        let child = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        match target.get_mut(key) {
            None => {
                target.insert(key.clone(), value.clone());
            }
            Some(existing) if existing.is_object() && value.is_object() => {
                merge_without_clobber(existing, value, &child, diagnostics);
            }
            Some(existing) => {
                if existing != value {
                    diagnostics.push(format!("skipped: {child} legacy={value} kept={existing}"));
                }
            }
        }
    }
}

/// Removes the retired `codegraph` key at the top level, inside harness blocks, profiles and the
/// profiles' harness blocks.
pub fn remove_retired_keys(document: &mut Value, diagnostics: &mut Vec<String>) {
    fn strip(container: &mut Value, path: &str, diagnostics: &mut Vec<String>) {
        let Value::Object(map) = container else {
            return;
        };
        if map.shift_remove("codegraph").is_some() {
            let shown = if path.is_empty() {
                "codegraph".to_owned()
            } else {
                format!("{path}.codegraph")
            };
            diagnostics.push(format!("removed {shown} (retired configuration)"));
        }
        for block in crate::model_string::HARNESS_BLOCKS {
            if let Some(inner) = map.get_mut(block) {
                let child = if path.is_empty() {
                    block.to_owned()
                } else {
                    format!("{path}.{block}")
                };
                strip(inner, &child, diagnostics);
            }
        }
    }
    strip(document, "", diagnostics);
    if let Some(Value::Object(profiles)) = document.get_mut("profiles") {
        for (name, body) in profiles.iter_mut() {
            strip(body, &format!("profiles.{name}"), diagnostics);
        }
    }
}

/// Structural diff turned into writer edits (objects recurse so unchanged keys keep comments).
pub fn diff_edits(old: &Value, new: &Value) -> Vec<Edit> {
    let mut edits = Vec::new();
    diff_into(old, new, &mut Vec::new(), &mut edits);
    edits
}

fn diff_into(old: &Value, new: &Value, path: &mut Vec<String>, edits: &mut Vec<Edit>) {
    let (Value::Object(old_map), Value::Object(new_map)) = (old, new) else {
        if old != new {
            edits.push(Edit::Set {
                path: path.clone(),
                value: new.clone(),
            });
        }
        return;
    };
    for key in old_map.keys() {
        if !new_map.contains_key(key) {
            let mut removed = path.clone();
            removed.push(key.clone());
            edits.push(Edit::Remove { path: removed });
        }
    }
    for (key, value) in new_map {
        path.push(key.clone());
        match old_map.get(key) {
            Some(existing) if existing.is_object() && value.is_object() => {
                diff_into(existing, value, path, edits)
            }
            Some(existing) if existing == value => {}
            _ => edits.push(Edit::Set {
                path: path.clone(),
                value: value.clone(),
            }),
        }
        path.pop();
    }
}

fn path_eq(a: &Path, b: &Path) -> bool {
    let a = maho_support::canonicalize_lenient(a);
    let b = maho_support::canonicalize_lenient(b);
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

impl Ctx<'_> {
    fn renew(&mut self) -> Result<(), String> {
        self.held
            .renew(&self.options.env)
            .map_err(|e| e.to_string())
    }

    fn write_journal(&mut self, journal: &Value) -> Result<(), String> {
        self.renew()?;
        let text = serde_json::to_string_pretty(journal).map_err(|e| e.to_string())?;
        write_atomically(self.journal_path, text.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", self.journal_path.display()))
    }

    fn write_target(
        &mut self,
        target: &Path,
        document: &Value,
        mode: PlanMode,
        backup: Option<&Path>,
    ) -> Result<Option<PathBuf>, String> {
        self.renew()?;
        let current = read_document(target)?.unwrap_or_else(|| Value::Object(Map::new()));
        let desired = match mode {
            PlanMode::ReplaceTarget => document.clone(),
            PlanMode::Merge => {
                let mut merged = current.clone();
                merge_without_clobber(&mut merged, document, "", &mut Vec::new());
                merged
            }
        };
        let edits = diff_edits(&current, &desired);
        if edits.is_empty() && target.exists() {
            return Ok(None);
        }
        let mut options = WriteOptions::new(
            WriteTarget::Path(target.to_path_buf()),
            crate::env::Env::new(),
        );
        options.planned_backup = backup.map(Path::to_path_buf);
        options.now = Some(self.options.env.now());
        let outcome = update_config(&edits, &options).map_err(|e| e.to_string())?;
        Ok(outcome.backup)
    }

    fn move_source(&mut self, from: &Path, to: &Path) -> Result<bool, String> {
        self.renew()?;
        match (from.exists(), to.exists()) {
            (false, true) => Ok(false),
            (true, true) => Err(format!(
                "cannot move {} to {}: the destination exists",
                from.display(),
                to.display()
            )),
            (false, false) => Err(format!(
                "cannot move {}: the source is missing",
                from.display()
            )),
            (true, false) => {
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::rename(from, to).map_err(|e| {
                    format!("cannot move {} to {}: {e}", from.display(), to.display())
                })?;
                Ok(true)
            }
        }
    }

    fn resume(&mut self) -> Result<Option<PathBuf>, String> {
        let text = std::fs::read_to_string(self.journal_path).map_err(|e| e.to_string())?;
        let mut journal: Value = serde_json::from_str(&text).map_err(|e| {
            format!(
                "the migration journal {} is unreadable: {e}",
                self.journal_path.display()
            )
        })?;
        let target = PathBuf::from(
            journal["targetPath"]
                .as_str()
                .ok_or("the migration journal has no targetPath")?,
        );
        let planned_backup = journal["targetBackupPath"].as_str().map(PathBuf::from);
        let mut backup = None;
        if journal["targetWritten"] != json!(true) {
            let additions = journal["targetWrite"]["additions"].clone();
            let mode = if journal["targetWrite"]["mode"] == json!("replace-target") {
                PlanMode::ReplaceTarget
            } else {
                PlanMode::Merge
            };
            backup = self.write_target(&target, &additions, mode, planned_backup.as_deref())?;
            journal["targetWritten"] = json!(true);
            self.write_journal(&journal)?;
        }
        let moves: Vec<(PathBuf, PathBuf)> = journal["backupMoves"]
            .as_array()
            .map(|moves| {
                moves
                    .iter()
                    .filter_map(|m| {
                        Some((
                            PathBuf::from(m["from"].as_str()?),
                            PathBuf::from(m["to"].as_str()?),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (from, to) in moves {
            let done = journal["completedMoves"].as_array().is_some_and(|c| {
                c.iter()
                    .any(|f| f.as_str() == Some(&*from.to_string_lossy()))
            });
            if done {
                continue;
            }
            self.move_source(&from, &to)?;
            push_completed(&mut journal, &from);
            self.write_journal(&journal)?;
        }
        std::fs::remove_file(self.journal_path).map_err(|e| e.to_string())?;
        Ok(backup.or(planned_backup.filter(|p| p.exists())))
    }

    fn run_plan(&mut self, plan: &Plan) -> Result<PlanResult, String> {
        let mut seen: Vec<&Path> = Vec::new();
        for source in &plan.sources {
            if seen.iter().any(|s| path_eq(s, &source.path)) {
                return Err(format!("duplicate source {}", source.path.display()));
            }
            for reserved in [plan.target.as_path(), self.lock_path, self.journal_path] {
                if path_eq(&source.path, reserved) {
                    return Err(format!("the source {} is reserved", source.path.display()));
                }
            }
            seen.push(&source.path);
        }
        let skipped = |diagnostics: Vec<String>| PlanResult {
            id: plan.id.clone(),
            target: plan.target.clone(),
            status: PlanStatus::Skipped,
            diagnostics,
            preview: None,
            target_backup: None,
        };
        let target_exists = plan.target.exists();
        let target = read_document(&plan.target)?.unwrap_or_else(|| Value::Object(Map::new()));
        if let Some(predicate) = &plan.predicate
            && !predicate(&target)
        {
            return Ok(skipped(Vec::new()));
        }
        let existing_sources: Vec<&PlanSource> =
            plan.sources.iter().filter(|s| s.path.exists()).collect();
        let something = match plan.mode {
            PlanMode::ReplaceTarget => target_exists,
            PlanMode::Merge => !existing_sources.is_empty(),
        };
        let marked = target
            .get("_migrations")
            .and_then(Value::as_array)
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(plan.id.as_str())));
        if !something || marked {
            return Ok(skipped(Vec::new()));
        }
        let mut loaded = Vec::new();
        if plan.mode == PlanMode::Merge {
            for source in &existing_sources {
                let value =
                    read_document(&source.path)?.unwrap_or_else(|| Value::Object(Map::new()));
                loaded.push((source.path.clone(), value));
            }
        }
        let output = (plan.transform)(&TransformInput {
            target: &target,
            sources: &loaded,
        })?;
        if !output.document.is_object() {
            return Err("the transform did not return an object".into());
        }
        let mut diagnostics = output.diagnostics;
        let mut document = match plan.mode {
            PlanMode::Merge => {
                let mut merged = target.clone();
                merge_without_clobber(&mut merged, &output.document, "", &mut diagnostics);
                merged
            }
            PlanMode::ReplaceTarget => output.document,
        };
        remove_retired_keys(&mut document, &mut diagnostics);
        append_marker(&mut document, &plan.id)?;
        let mut check = document.clone();
        let issues = schema::validate(&mut check, Mode::Full);
        if !issues.is_empty() {
            let listed: Vec<String> = issues
                .iter()
                .map(|issue| format!("{} ({})", dotted(&issue.path), issue.message))
                .collect();
            return Err(format!(
                "the migrated document is invalid: {}",
                listed.join(", ")
            ));
        }

        let mut moves = Vec::new();
        let reserved: Vec<PathBuf> = plan
            .sources
            .iter()
            .map(|s| s.path.clone())
            .chain([
                plan.target.clone(),
                self.lock_path.to_path_buf(),
                self.journal_path.to_path_buf(),
            ])
            .collect();
        let encoded_id = utf8_percent_encode(&plan.id, NON_ALPHANUMERIC).to_string();
        for source in &existing_sources {
            let to = match &source.backup {
                Some(explicit) => {
                    if explicit.exists() {
                        return Err(format!(
                            "the backup path {} already exists",
                            explicit.display()
                        ));
                    }
                    explicit.clone()
                }
                None => {
                    let base = PathBuf::from(format!("{}.bak.{encoded_id}", source.path.display()));
                    first_free(&base, |candidate| {
                        !candidate.exists()
                            && !moves.iter().any(|m: &Move| m.to == candidate)
                            && !reserved.iter().any(|r| path_eq(r, candidate))
                    })
                }
            };
            if reserved.iter().any(|r| path_eq(r, &to)) || moves.iter().any(|m: &Move| m.to == to) {
                return Err(format!(
                    "the backup path {} collides with another file of the plan",
                    to.display()
                ));
            }
            moves.push(Move {
                from: source.path.clone(),
                to,
            });
        }
        if self.options.dry_run {
            return Ok(PlanResult {
                id: plan.id.clone(),
                target: plan.target.clone(),
                status: PlanStatus::Planned,
                diagnostics,
                preview: Some(Preview {
                    target: plan.target.clone(),
                    document,
                    moves,
                }),
                target_backup: None,
            });
        }
        let target_backup = target_exists.then(|| {
            let base = PathBuf::from(format!(
                "{}.bak.{}",
                plan.target.display(),
                self.options.env.stamp()
            ));
            first_free(&base, |candidate| {
                !candidate.exists() && !moves.iter().any(|m| m.to == candidate)
            })
        });
        let mut write = json!({"additions": document});
        if plan.mode == PlanMode::ReplaceTarget {
            write["mode"] = json!("replace-target");
        }
        let mut journal = json!({
            "version": JOURNAL_VERSION,
            "migrationId": plan.id,
            "targetPath": plan.target.to_string_lossy(),
            "targetBackupPath": target_backup.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "targetWrite": write,
            "targetWritten": false,
            "backupMoves": moves.iter().map(|m| json!({"from": m.from.to_string_lossy(), "to": m.to.to_string_lossy()})).collect::<Vec<_>>(),
            "completedMoves": [],
            "diagnostics": diagnostics,
        });
        self.write_journal(&journal)?;
        self.options.env.boundary(Boundary::JournalWritten)?;
        let written_backup = self.write_target(
            &plan.target,
            &document,
            PlanMode::ReplaceTarget,
            target_backup.as_deref(),
        )?;
        self.options.env.boundary(Boundary::TargetWritten)?;
        journal["targetWritten"] = json!(true);
        self.write_journal(&journal)?;
        self.options.env.boundary(Boundary::TargetRecorded)?;
        for m in &moves {
            self.move_source(&m.from, &m.to)?;
            self.options.env.boundary(Boundary::SourceMoved)?;
            push_completed(&mut journal, &m.from);
            self.write_journal(&journal)?;
            self.options.env.boundary(Boundary::SourceRecorded)?;
        }
        std::fs::remove_file(self.journal_path).map_err(|e| e.to_string())?;
        Ok(PlanResult {
            id: plan.id.clone(),
            target: plan.target.clone(),
            status: PlanStatus::Migrated,
            diagnostics,
            preview: Some(Preview {
                target: plan.target.clone(),
                document,
                moves,
            }),
            target_backup: written_backup.or(target_backup.filter(|p| p.exists())),
        })
    }
}

fn push_completed(journal: &mut Value, from: &Path) {
    if let Some(completed) = journal["completedMoves"].as_array_mut() {
        completed.push(json!(from.to_string_lossy()));
    }
}

fn append_marker(document: &mut Value, id: &str) -> Result<(), String> {
    let Value::Object(map) = document else {
        return Err("the document is not an object".into());
    };
    match map.get_mut("_migrations") {
        None => {
            map.insert("_migrations".into(), json!([id]));
            Ok(())
        }
        Some(Value::Array(ids)) if ids.iter().all(Value::is_string) => {
            if !ids.iter().any(|existing| existing.as_str() == Some(id)) {
                ids.push(json!(id));
            }
            Ok(())
        }
        Some(_) => Err("_migrations must be an array of strings".into()),
    }
}
