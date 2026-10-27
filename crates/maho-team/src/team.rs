//! Team runs (spec §1–§3): a named set of member tasks started through `maho-task`, a per-team
//! parallel limit, messages delivered as follow-up runs (queued while a run is active), wait and
//! collect, stop, and team updates to the lead. Team model after the MIT pi-agent-teams leader
//! (members, direct messages, completion notices with progress counters, an "all done" notice);
//! the mapping onto non-interactive task children is mhc's own (spec §1).
use crate::panes::PaneObserver;
use crate::settings::TeamLimits;
use maho_task::{CancelToken, Notifier, SpawnRequest, TaskManager, TaskRecord, TaskStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// `custom_type` of team updates sent to the lead.
pub const UPDATE_MESSAGE_TYPE: &str = "team-update";
/// Longest team or member name.
pub const MAX_NAME_LEN: usize = 32;
const POLL: Duration = Duration::from_millis(250);
const RESULT_PREVIEW_CHARS: usize = 500;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `^[a-z0-9-]+$`, at most [`MAX_NAME_LEN`] characters.
///
/// # Errors
/// The name does not match.
pub fn validate_name(kind: &str, name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "Invalid {kind} name \"{name}\": use lowercase letters, digits and '-' (at most {MAX_NAME_LEN})"
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamStatus {
    Running,
    Settled,
    Stopped,
    TimedOut,
}

impl TeamStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Settled => "settled",
            Self::Stopped => "stopped",
            Self::TimedOut => "timed out",
        }
    }
    fn is_final(self) -> bool {
        matches!(self, Self::Stopped | Self::TimedOut)
    }
}

/// One member as persisted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberEntry {
    pub name: String,
    /// First line of the assignment.
    pub summary: String,
    /// Backing task (`None` while pending).
    pub task_id: Option<String>,
    /// Messages waiting for the current run to settle.
    #[serde(default)]
    pub queue: Vec<String>,
    #[serde(default)]
    pub stopped: bool,
}

/// `teams/<name>.json` in the task state directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamFile {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub context: Option<String>,
    pub created_at_ms: u64,
    pub status: TeamStatus,
    pub members: Vec<MemberEntry>,
}

/// A member to start: its name and a background spawn request built with the `task` rules.
#[derive(Clone, Debug)]
pub struct MemberSpec {
    pub name: String,
    pub request: SpawnRequest,
}

/// Derived member state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberState {
    Pending,
    Active(TaskStatus),
    Idle(TaskStatus),
    Stopped,
}

impl MemberState {
    pub fn label(self) -> String {
        match self {
            Self::Pending => "pending".into(),
            Self::Active(status) => status.as_str().into(),
            Self::Idle(status) => format!("idle ({})", status.as_str()),
            Self::Stopped => "stopped".into(),
        }
    }
}

struct Inner {
    file: TeamFile,
    pending: VecDeque<(String, SpawnRequest)>,
    /// Highest run of each task whose settlement was handled.
    seen_runs: HashMap<String, u32>,
    settled_notified: bool,
    collected: bool,
    /// Members covered by an in-progress `team_wait`.
    waiting: HashMap<String, usize>,
    created: Instant,
}

/// A live team (driven in this process).
pub struct Team {
    root: PathBuf,
    manager: Arc<TaskManager>,
    limits: TeamLimits,
    notifier: Option<Notifier>,
    inner: Mutex<Inner>,
    wake: tokio::sync::Notify,
}

impl std::fmt::Debug for Team {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = lock(&self.inner);
        f.debug_struct("Team")
            .field("name", &inner.file.name)
            .field("status", &inner.file.status)
            .finish_non_exhaustive()
    }
}

type Key = (PathBuf, String);
static TEAMS: Mutex<Option<HashMap<Key, Arc<Team>>>> = Mutex::new(None);

fn teams_dir(root: &Path) -> PathBuf {
    root.join("teams")
}

fn file_path(root: &Path, name: &str) -> PathBuf {
    teams_dir(root).join(format!("{name}.json"))
}

/// Reads `teams/<name>.json` of a state directory.
pub fn load_file(root: &Path, name: &str) -> Option<TeamFile> {
    serde_json::from_slice(&std::fs::read(file_path(root, name)).ok()?).ok()
}

/// Names of the teams of a state directory (live and persisted), sorted.
pub fn team_names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(teams_dir(root))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .strip_suffix(".json")
                .map(str::to_owned)
        })
        .collect();
    if let Some(map) = lock(&TEAMS).as_ref() {
        names.extend(
            map.keys()
                .filter(|(r, _)| r == root)
                .map(|(_, n)| n.clone()),
        );
    }
    names.sort();
    names.dedup();
    names
}

fn tail_lines(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.trim_end().lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

fn preview(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

impl Team {
    /// Creates and starts a team (spec §2.1).
    ///
    /// # Errors
    /// Invalid names, too many or no members, a live team of the same name, or a member that
    /// cannot be started.
    pub fn create(
        manager: Arc<TaskManager>,
        name: &str,
        context: Option<String>,
        members: Vec<MemberSpec>,
        limits: TeamLimits,
        notifier: Option<Notifier>,
    ) -> Result<Arc<Self>, String> {
        validate_name("team", name)?;
        if members.is_empty() {
            return Err("A team needs at least one member".into());
        }
        if members.len() > limits.max_members {
            return Err(format!(
                "A team has at most {} members (task.team.max_members); got {}",
                limits.max_members,
                members.len()
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for member in &members {
            validate_name("member", &member.name)?;
            if !seen.insert(member.name.clone()) {
                return Err(format!("Duplicate member name \"{}\"", member.name));
            }
        }
        let root = manager.state().root.clone();
        let key = (root.clone(), name.to_owned());
        if let Some(existing) = lock(&TEAMS).as_ref().and_then(|map| map.get(&key)).cloned() {
            let status = lock(&existing.inner).file.status;
            if status == TeamStatus::Running {
                return Err(format!(
                    "Team \"{name}\" is still running; stop it or pick another name"
                ));
            }
        }
        let file = TeamFile {
            version: 1,
            name: name.to_owned(),
            context: context.clone(),
            created_at_ms: maho_task::record::now_ms(),
            status: TeamStatus::Running,
            members: members
                .iter()
                .map(|member| MemberEntry {
                    name: member.name.clone(),
                    summary: member
                        .request
                        .task
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(120)
                        .collect(),
                    task_id: None,
                    queue: Vec::new(),
                    stopped: false,
                })
                .collect(),
        };
        let pending = members
            .into_iter()
            .map(|member| {
                let mut request = member.request;
                request.name = Some(format!("{name}-{}", member.name));
                request.group = Some(name.to_owned());
                request.quiet = true;
                request.background = true;
                if request.context.is_none() {
                    request.context.clone_from(&context);
                }
                (member.name, request)
            })
            .collect();
        let team = Arc::new(Self {
            root,
            manager,
            limits,
            notifier,
            inner: Mutex::new(Inner {
                file,
                pending,
                seen_runs: HashMap::new(),
                settled_notified: false,
                collected: false,
                waiting: HashMap::new(),
                created: Instant::now(),
            }),
            wake: tokio::sync::Notify::new(),
        });
        {
            let mut inner = lock(&team.inner);
            team.start_pending(&mut inner)?;
            team.persist(&inner);
        }
        lock(&TEAMS)
            .get_or_insert_with(HashMap::new)
            .insert(key, Arc::clone(&team));
        team.spawn_driver();
        Ok(team)
    }

    /// The live team `name` of `manager`'s state directory, reviving a persisted one (its pending
    /// members, if any, are dropped: they belonged to a lead process that ended).
    pub fn find(
        manager: &Arc<TaskManager>,
        name: &str,
        limits: &TeamLimits,
        notifier: Option<Notifier>,
    ) -> Option<Arc<Self>> {
        let root = manager.state().root.clone();
        let key = (root.clone(), name.to_owned());
        if let Some(team) = lock(&TEAMS).as_ref().and_then(|map| map.get(&key)).cloned() {
            return Some(team);
        }
        let mut file = load_file(&root, name)?;
        for member in &mut file.members {
            if member.task_id.is_none() {
                member.stopped = true;
            }
        }
        let team = Arc::new(Self {
            root,
            manager: Arc::clone(manager),
            limits: limits.clone(),
            notifier,
            inner: Mutex::new(Inner {
                // Settlements up to now were handled by the earlier lead.
                seen_runs: file
                    .members
                    .iter()
                    .filter_map(|m| m.task_id.as_ref())
                    .filter_map(|id| manager.get(id).map(|r| (id.clone(), r.runs)))
                    .collect(),
                file,
                pending: VecDeque::new(),
                settled_notified: true,
                collected: true,
                waiting: HashMap::new(),
                created: Instant::now(),
            }),
            wake: tokio::sync::Notify::new(),
        });
        let final_status = lock(&team.inner).file.status.is_final();
        lock(&TEAMS)
            .get_or_insert_with(HashMap::new)
            .insert(key, Arc::clone(&team));
        if !final_status {
            team.spawn_driver();
        }
        Some(team)
    }

    pub fn name(&self) -> String {
        lock(&self.inner).file.name.clone()
    }

    pub fn file(&self) -> TeamFile {
        lock(&self.inner).file.clone()
    }

    fn persist(&self, inner: &Inner) {
        let path = file_path(&self.root, &inner.file.name);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&inner.file) {
            let temp = path.with_extension("json.tmp");
            if std::fs::write(&temp, bytes).is_ok() {
                let _ = std::fs::rename(temp, path);
            }
        }
    }

    fn record(&self, member: &MemberEntry) -> Option<TaskRecord> {
        member.task_id.as_ref().and_then(|id| self.manager.get(id))
    }

    fn state_of(&self, inner: &Inner, member: &MemberEntry) -> MemberState {
        if member.stopped {
            return MemberState::Stopped;
        }
        if inner.pending.iter().any(|(name, _)| *name == member.name) {
            return MemberState::Pending;
        }
        match self.record(member) {
            Some(record) if record.status.is_active() => MemberState::Active(record.status),
            Some(record) => MemberState::Idle(record.status),
            None => MemberState::Stopped,
        }
    }

    fn start_pending(&self, inner: &mut Inner) -> Result<(), String> {
        let mut active = inner
            .file
            .members
            .iter()
            .filter(|m| matches!(self.state_of(inner, m), MemberState::Active(_)))
            .count();
        while active < self.limits.max_parallel {
            let Some((name, request)) = inner.pending.pop_front() else {
                break;
            };
            let record = self.manager.start(request)?;
            if let Some(member) = inner.file.members.iter_mut().find(|m| m.name == name) {
                member.task_id = Some(record.id.clone());
            }
            inner.seen_runs.insert(record.id, 0);
            active += 1;
        }
        Ok(())
    }

    fn mark_changed(inner: &mut Inner) {
        inner.settled_notified = false;
        inner.collected = false;
        if !inner.file.status.is_final() {
            inner.file.status = TeamStatus::Running;
        }
    }

    fn progress(&self, inner: &Inner) -> (usize, usize, usize, usize) {
        let (mut done, mut running, mut pending) = (0, 0, 0);
        for member in &inner.file.members {
            match self.state_of(inner, member) {
                MemberState::Idle(_) | MemberState::Stopped if member.queue.is_empty() => done += 1,
                MemberState::Pending => pending += 1,
                _ => running += 1,
            }
        }
        (done, inner.file.members.len(), running, pending)
    }

    /// One driver pass (spec §3); returns the updates to send and whether the team is final.
    fn step(&self) -> (Vec<(String, Value)>, bool, bool) {
        let mut updates = Vec::new();
        let mut inner = lock(&self.inner);
        if inner.file.status.is_final() {
            return (updates, true, true);
        }
        if inner.created.elapsed() >= self.limits.max_wall_clock {
            drop(inner);
            let lines = self.stop_members(None, TeamStatus::TimedOut);
            let name = self.name();
            updates.push((
                format!(
                    "[team {name}] stopped after the wall-clock limit (task.team.max_wall_clock_minutes).\n{}",
                    lines.join("\n")
                ),
                json!({"team": name, "event": "timed_out"}),
            ));
            return (updates, true, true);
        }
        let team = inner.file.name.clone();
        let mut changed = false;
        let mut settled_now = Vec::new();
        for index in 0..inner.file.members.len() {
            let member = inner.file.members[index].clone();
            if member.stopped {
                continue;
            }
            let Some(id) = member.task_id.clone() else {
                continue;
            };
            let Some(record) = self.manager.get(&id) else {
                continue;
            };
            if record.status.is_active() {
                continue;
            }
            let fresh = inner.seen_runs.get(&id).copied().unwrap_or(0) < record.runs;
            if fresh {
                inner.seen_runs.insert(id.clone(), record.runs);
                changed = true;
            }
            if member.queue.is_empty() {
                if fresh {
                    settled_now.push((member.name.clone(), record));
                }
                continue;
            }
            changed = true;
            let joined = member.queue.join("\n\n");
            inner.file.members[index].queue.clear();
            if let Err(error) = self.manager.resume(&id, Some(&joined), true) {
                updates.push((
                    format!(
                        "[team {team}] could not deliver queued messages to {}: {error}",
                        member.name
                    ),
                    json!({"team": team, "member": member.name, "event": "delivery_failed"}),
                ));
            }
        }
        if let Err(error) = self.start_pending(&mut inner) {
            updates.push((
                format!("[team {team}] could not start a pending member: {error}"),
                json!({"team": team, "event": "start_failed"}),
            ));
            inner.pending.clear();
        }
        let (done, total, running, pending) = self.progress(&inner);
        for (member, record) in settled_now {
            if inner.waiting.get(&member).copied().unwrap_or(0) > 0 {
                continue;
            }
            let output = self.manager.state().read_output(&record.id);
            let mut text = format!(
                "[team {team}] {member} {} (task {}, run {})",
                record.status.as_str(),
                record.id,
                record.runs
            );
            let shown = if output.trim().is_empty() {
                record.error.clone().unwrap_or_default()
            } else {
                output
            };
            if !shown.trim().is_empty() {
                text.push('\n');
                text.push_str(&preview(&shown, RESULT_PREVIEW_CHARS));
            }
            text.push_str(&format!(
                "\nProgress: {done}/{total} done ({running} running, {pending} pending)"
            ));
            updates.push((text, json!({"team": team, "member": member, "id": record.id, "status": record.status.as_str()})));
        }
        let settled = done == total;
        if settled {
            if inner.file.status != TeamStatus::Settled {
                inner.file.status = TeamStatus::Settled;
                changed = true;
            }
            let covered = inner
                .file
                .members
                .iter()
                .all(|m| inner.waiting.get(&m.name).copied().unwrap_or(0) > 0);
            if !inner.settled_notified && !inner.collected && !covered {
                let failed = inner
                    .file
                    .members
                    .iter()
                    .filter(|m| matches!(self.state_of(&inner, m), MemberState::Idle(s) if s != TaskStatus::Completed))
                    .count();
                updates.push((
                    format!(
                        "[team {team}] all {total} members finished ({} succeeded, {failed} not). Use team_wait to collect the results.",
                        total - failed
                    ),
                    json!({"team": team, "event": "settled"}),
                ));
            }
            inner.settled_notified = true;
        }
        if changed {
            self.persist(&inner);
        }
        (updates, settled, false)
    }

    fn spawn_driver(self: &Arc<Self>) {
        let team = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let (updates, settled, finished) = team.step();
                if let Some(notifier) = &team.notifier {
                    for (text, details) in &updates {
                        notifier(UPDATE_MESSAGE_TYPE, text, details.clone());
                    }
                }
                if finished {
                    break;
                }
                if settled {
                    team.wake.notified().await;
                } else {
                    tokio::select! {
                        () = tokio::time::sleep(POLL) => {}
                        () = team.wake.notified() => {}
                    }
                }
            }
        });
    }

    fn select<'a>(&self, inner: &'a Inner, member: &str) -> Result<Vec<&'a MemberEntry>, String> {
        if member == "*" {
            return Ok(inner.file.members.iter().collect());
        }
        inner
            .file
            .members
            .iter()
            .find(|m| m.name == member)
            .map(|m| vec![m])
            .ok_or_else(|| {
                format!(
                    "Unknown member \"{member}\" of team {}. Members: {}",
                    inner.file.name,
                    inner
                        .file
                        .members
                        .iter()
                        .map(|m| m.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    /// Sends `message` to `member` (`*` = everyone) (spec §2.3); one line per member.
    ///
    /// # Errors
    /// Unknown or stopped member, empty message, or a resume that fails.
    pub fn message(&self, member: &str, message: &str) -> Result<Vec<String>, String> {
        let message = message.trim();
        if message.is_empty() {
            return Err("Missing required parameter: message".into());
        }
        let mut inner = lock(&self.inner);
        let names: Vec<String> = self
            .select(&inner, member)?
            .iter()
            .map(|m| m.name.clone())
            .collect();
        let mut lines = Vec::new();
        for name in names {
            let Some(index) = inner.file.members.iter().position(|m| m.name == name) else {
                continue;
            };
            let entry = inner.file.members[index].clone();
            match self.state_of(&inner, &entry) {
                MemberState::Stopped => {
                    if member == "*" {
                        lines.push(format!("{name}: stopped, not delivered"));
                        continue;
                    }
                    return Err(format!("Member \"{name}\" is stopped"));
                }
                MemberState::Idle(_) if entry.queue.is_empty() => {
                    let id = entry.task_id.clone().unwrap_or_default();
                    let record = self.manager.resume(&id, Some(message), true)?;
                    inner.seen_runs.insert(id.clone(), record.runs - 1);
                    lines.push(format!(
                        "{name}: delivered (task {id}, run {} started)",
                        record.runs
                    ));
                }
                _ => {
                    inner.file.members[index].queue.push(message.to_owned());
                    lines.push(format!(
                        "{name}: queued ({} waiting for the current run to finish)",
                        inner.file.members[index].queue.len()
                    ));
                }
            }
        }
        Self::mark_changed(&mut inner);
        self.persist(&inner);
        drop(inner);
        self.wake.notify_one();
        Ok(lines)
    }

    fn stop_members(&self, member: Option<&str>, final_status: TeamStatus) -> Vec<String> {
        let mut inner = lock(&self.inner);
        let names: Vec<String> = match member {
            Some(member) => match self.select(&inner, member) {
                Ok(selected) => selected.iter().map(|m| m.name.clone()).collect(),
                Err(error) => return vec![error],
            },
            None => inner.file.members.iter().map(|m| m.name.clone()).collect(),
        };
        let mut lines = Vec::new();
        for name in names {
            let was_pending = inner.pending.iter().any(|(n, _)| *n == name);
            inner.pending.retain(|(n, _)| *n != name);
            let Some(index) = inner.file.members.iter().position(|m| m.name == name) else {
                continue;
            };
            let entry = inner.file.members[index].clone();
            inner.file.members[index].queue.clear();
            if entry.stopped {
                lines.push(format!("{name}: already stopped"));
                continue;
            }
            inner.file.members[index].stopped = true;
            let line = if was_pending {
                "not started".to_owned()
            } else {
                match entry.task_id.as_deref().and_then(|id| self.manager.get(id)) {
                    Some(record) if record.status.is_active() => self.manager.cancel(&record.id),
                    Some(record) => format!("idle ({})", record.status.as_str()),
                    None => "no task".to_owned(),
                }
            };
            lines.push(format!("{name}: {line}"));
        }
        if inner.file.members.iter().all(|m| m.stopped) {
            inner.file.status = final_status;
        }
        self.persist(&inner);
        drop(inner);
        self.wake.notify_one();
        lines
    }

    /// Stops one member or all (spec §2.5); one line per member.
    pub fn stop(&self, member: Option<&str>) -> Vec<String> {
        self.stop_members(member, TeamStatus::Stopped)
    }

    fn all_done(&self, selected: &[String]) -> bool {
        let inner = lock(&self.inner);
        inner
            .file
            .members
            .iter()
            .filter(|m| selected.contains(&m.name))
            .all(|m| {
                m.queue.is_empty()
                    && matches!(
                        self.state_of(&inner, m),
                        MemberState::Idle(_) | MemberState::Stopped
                    )
            })
    }

    /// Waits for the selected members (all when `None`) and returns the summary line and their
    /// result envelopes (spec §2.4).
    ///
    /// # Errors
    /// Unknown member names.
    pub async fn wait(
        &self,
        members: Option<Vec<String>>,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<String, String> {
        let selected: Vec<String> = {
            let inner = lock(&self.inner);
            match members.filter(|m| !m.is_empty()) {
                Some(names) => {
                    for name in &names {
                        self.select(&inner, name)?;
                    }
                    names
                }
                None => inner.file.members.iter().map(|m| m.name.clone()).collect(),
            }
        };
        {
            let mut inner = lock(&self.inner);
            for name in &selected {
                *inner.waiting.entry(name.clone()).or_default() += 1;
            }
        }
        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        loop {
            if self.all_done(&selected) {
                break;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                break;
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
                () = cancel.cancelled() => break,
            }
        }
        let mut inner = lock(&self.inner);
        for name in &selected {
            if let Some(count) = inner.waiting.get_mut(name) {
                *count = count.saturating_sub(1);
            }
        }
        // Settlements this wait returned are not announced again.
        for member in inner
            .file
            .members
            .clone()
            .iter()
            .filter(|m| selected.contains(&m.name))
        {
            if let Some(record) = self.record(member).filter(|r| !r.status.is_active()) {
                inner.seen_runs.insert(record.id.clone(), record.runs);
            }
        }
        let everyone = inner
            .file
            .members
            .iter()
            .all(|m| selected.contains(&m.name));
        if everyone && !timed_out && !cancel.is_cancelled() {
            inner.collected = true;
        }
        let mut done = 0;
        let mut failed = 0;
        let mut sections = Vec::new();
        for member in inner
            .file
            .members
            .iter()
            .filter(|m| selected.contains(&m.name))
        {
            let state = self.state_of(&inner, member);
            match state {
                MemberState::Idle(TaskStatus::Completed) => done += 1,
                MemberState::Idle(_) => {
                    done += 1;
                    failed += 1;
                }
                MemberState::Stopped => done += 1,
                _ => {}
            }
            let body = match member
                .task_id
                .as_deref()
                .and_then(|id| self.manager.result_text(id))
            {
                Some(text) if !matches!(state, MemberState::Active(_)) => text,
                _ => format!("({} is {})", member.name, state.label()),
            };
            sections.push(format!("## {}\n{body}", member.name));
        }
        let mut summary = format!("team {}: {done}/{} done", inner.file.name, selected.len());
        if failed > 0 {
            summary.push_str(&format!(", {failed} failed"));
        }
        if timed_out {
            summary.push_str(&format!(
                ", timed out after {}",
                maho_task::summary::format_duration(
                    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
                )
            ));
        }
        drop(inner);
        Ok(format!("{summary}\n\n{}", sections.join("\n\n")))
    }

    /// The member listing (spec §2.2).
    pub fn describe(&self, tail: usize, panes: Option<&PaneObserver>) -> String {
        let inner = lock(&self.inner);
        let file = &inner.file;
        let mut lines = vec![format!(
            "team {} ({}, {} members)",
            file.name,
            file.status.as_str(),
            file.members.len()
        )];
        for member in &file.members {
            let state = self.state_of(&inner, member);
            let record = self.record(member);
            let mut line = format!("- {}: {}", member.name, state.label());
            if let Some(id) = &member.task_id {
                line.push_str(&format!(", task {id}"));
            }
            if let Some(record) = &record {
                line.push_str(&format!(
                    ", agent {}, model {}, runs {}",
                    record.agent,
                    record.model.as_deref().unwrap_or("(default)"),
                    record.runs
                ));
            }
            if !member.queue.is_empty() {
                line.push_str(&format!(", messages {}", member.queue.len()));
            }
            if let (Some(panes), Some(id)) = (panes, &member.task_id) {
                if let Some(handle) = panes.pane_of(id) {
                    line.push_str(&format!(", pane {}", handle.id));
                } else if let Some(error) = panes.error_of(id) {
                    line.push_str(&format!(", pane unavailable ({error})"));
                }
            }
            line.push_str(&format!(" — {}", member.summary));
            lines.push(line);
            if tail > 0
                && let Some(id) = &member.task_id
            {
                let state = self.manager.state();
                let output = state.read_output(id);
                let text = if output.trim().is_empty() {
                    state.read_log_tail(id, 16_000)
                } else {
                    output
                };
                let shown = tail_lines(&text, tail);
                if !shown.is_empty() {
                    for row in shown.lines() {
                        lines.push(format!("    | {row}"));
                    }
                }
            }
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_helpers() {
        assert!(validate_name("team", "a-1").is_ok());
        assert!(validate_name("team", "A").is_err());
        assert!(validate_name("team", "").is_err());
        assert!(validate_name("team", &"a".repeat(33)).is_err());
        assert_eq!(tail_lines("a\nb\nc\n", 2), "b\nc");
        assert_eq!(preview("abcdef", 3), "abc…");
    }
}
