//! Session-history types (upstream `session-history/types.ts`). Wire shapes use camelCase keys.

use std::{collections::BTreeMap, sync::Arc};

use serde_json::{Map, Value, json};

use super::session_history_fs::SessionHistoryFs;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentSessionSource {
    ClaudeAgent,
    Codex,
}

impl AgentSessionSource {
    pub const ALL: [Self; 2] = [Self::ClaudeAgent, Self::Codex];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeAgent => "claudeAgent",
            Self::Codex => "codex",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claudeAgent" => Some(Self::ClaudeAgent),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProjectSource {
    Agent(AgentSessionSource),
    Omo,
}

impl ProjectSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent(source) => source.as_str(),
            Self::Omo => "omo",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentHome {
    pub source: AgentSessionSource,
    pub home_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedProject {
    pub id: String,
    pub workspace_root: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectGit {
    pub remote_key: Option<String>,
    pub repository: Option<String>,
}

impl ProjectGit {
    pub fn to_json(&self) -> Value {
        json!({"remoteKey": self.remote_key, "repository": self.repository})
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectCandidate {
    pub path: String,
    pub title: String,
    pub project_id: Option<String>,
    pub sources: Vec<ProjectSource>,
    pub thread_count: usize,
    pub last_active_at: Option<String>,
    pub already_imported: bool,
    pub git: Option<ProjectGit>,
}

impl ProjectCandidate {
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("path".into(), json!(self.path));
        out.insert("title".into(), json!(self.title));
        if let Some(id) = &self.project_id {
            out.insert("projectId".into(), json!(id));
        }
        out.insert(
            "sources".into(),
            Value::Array(self.sources.iter().map(|s| json!(s.as_str())).collect()),
        );
        out.insert("threadCount".into(), json!(self.thread_count));
        out.insert("lastActiveAt".into(), json!(self.last_active_at));
        out.insert("alreadyImported".into(), json!(self.already_imported));
        out.insert(
            "git".into(),
            self.git.as_ref().map_or(Value::Null, ProjectGit::to_json),
        );
        Value::Object(out)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanCounts {
    pub found: usize,
    pub read: usize,
    pub cap: usize,
}

impl ScanCounts {
    pub fn to_json(self) -> Value {
        json!({"found": self.found, "read": self.read, "cap": self.cap})
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectScanResult {
    pub candidates: Vec<ProjectCandidate>,
    pub scanned_at: String,
    pub truncated: bool,
    pub counts: ScanCounts,
}

impl ProjectScanResult {
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert(
            "candidates".into(),
            Value::Array(
                self.candidates
                    .iter()
                    .map(ProjectCandidate::to_json)
                    .collect(),
            ),
        );
        out.insert("scannedAt".into(), json!(self.scanned_at));
        if self.truncated {
            out.insert("truncated".into(), json!(true));
        }
        out.insert("counts".into(), self.counts.to_json());
        Value::Object(out)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSessionThreadMessage {
    pub role: MessageRole,
    pub text: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSessionThread {
    pub source: AgentSessionSource,
    pub provider_session_id: String,
    pub title: String,
    pub model: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub messages: Vec<AgentSessionThreadMessage>,
}

impl AgentSessionThread {
    pub fn to_json(&self) -> Value {
        json!({
            "source": self.source.as_str(),
            "providerSessionId": self.provider_session_id,
            "title": self.title,
            "model": self.model,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
            "messages": self.messages.iter().map(|m| json!({
                "role": m.role.as_str(), "text": m.text, "createdAt": m.created_at,
            })).collect::<Vec<_>>(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptSource {
    pub provider: AgentSessionSource,
    pub home_path: String,
    pub provider_session_id: String,
    pub file_path: String,
    pub size: u64,
    pub mtime_ms: Option<i64>,
    pub device: u64,
    pub inode: Option<u64>,
    pub birthtime_ms: Option<i64>,
}

impl TranscriptSource {
    pub fn to_json(&self) -> Value {
        json!({
            "provider": self.provider.as_str(),
            "homePath": self.home_path,
            "providerSessionId": self.provider_session_id,
            "filePath": self.file_path,
            "size": self.size,
            "mtimeMs": self.mtime_ms,
            "device": self.device,
            "inode": self.inode,
            "birthtimeMs": self.birthtime_ms,
        })
    }
}

/// A transcript the caller already imported, named by the identity it had then
/// (a [`TranscriptSource`] without `homePath`).
#[derive(Clone, Debug, PartialEq)]
pub struct CompletedTranscript {
    pub provider: AgentSessionSource,
    pub provider_session_id: String,
    pub file_path: String,
    pub size: u64,
    pub mtime_ms: Option<i64>,
    pub device: u64,
    pub inode: Option<u64>,
    pub birthtime_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionToolUse {
    pub name: String,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDigest {
    pub session_key: String,
    pub source: AgentSessionSource,
    pub provider_session_id: String,
    pub cwd: String,
    pub title: String,
    pub model: Option<String>,
    pub started_at: String,
    pub last_active_at: String,
    pub message_count: usize,
    pub first_request: String,
    pub tools_used: Vec<SessionToolUse>,
    pub files_changed: Vec<String>,
    pub outcome: String,
    pub truncated: bool,
}

impl SessionDigest {
    pub fn to_json(&self) -> Value {
        json!({
            "sessionKey": self.session_key,
            "source": self.source.as_str(),
            "providerSessionId": self.provider_session_id,
            "cwd": self.cwd,
            "title": self.title,
            "model": self.model,
            "startedAt": self.started_at,
            "lastActiveAt": self.last_active_at,
            "messageCount": self.message_count,
            "firstRequest": self.first_request,
            "toolsUsed": self.tools_used.iter().map(|t| json!({"name": t.name, "count": t.count})).collect::<Vec<_>>(),
            "filesChanged": self.files_changed,
            "outcome": self.outcome,
            "truncated": self.truncated,
        })
    }

    /// `JSON.stringify` order: the declaration order of the upstream interface.
    pub fn to_json_string(&self) -> String {
        fn s(value: &str) -> String {
            Value::String(value.to_owned()).to_string()
        }
        let model = self.model.as_deref().map_or_else(|| "null".to_owned(), s);
        let tools = self
            .tools_used
            .iter()
            .map(|t| format!("{{\"name\":{},\"count\":{}}}", s(&t.name), t.count))
            .collect::<Vec<_>>()
            .join(",");
        let files = self
            .files_changed
            .iter()
            .map(|f| s(f))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"sessionKey\":{},\"source\":{},\"providerSessionId\":{},\"cwd\":{},\"title\":{},\"model\":{},\"startedAt\":{},\"lastActiveAt\":{},\"messageCount\":{},\"firstRequest\":{},\"toolsUsed\":[{}],\"filesChanged\":[{}],\"outcome\":{},\"truncated\":{}}}",
            s(&self.session_key),
            s(self.source.as_str()),
            s(&self.provider_session_id),
            s(&self.cwd),
            s(&self.title),
            model,
            s(&self.started_at),
            s(&self.last_active_at),
            self.message_count,
            s(&self.first_request),
            tools,
            files,
            s(&self.outcome),
            self.truncated,
        )
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key)?.as_str().map(str::to_owned);
        Some(Self {
            session_key: text("sessionKey")?,
            source: AgentSessionSource::parse(value.get("source")?.as_str()?)?,
            provider_session_id: text("providerSessionId")?,
            cwd: text("cwd")?,
            title: text("title")?,
            model: value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned),
            started_at: text("startedAt")?,
            last_active_at: text("lastActiveAt")?,
            message_count: usize::try_from(value.get("messageCount")?.as_u64()?).ok()?,
            first_request: text("firstRequest")?,
            tools_used: value
                .get("toolsUsed")?
                .as_array()?
                .iter()
                .map(|t| {
                    Some(SessionToolUse {
                        name: t.get("name")?.as_str()?.to_owned(),
                        count: usize::try_from(t.get("count")?.as_u64()?).ok()?,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
            files_changed: value
                .get("filesChanged")?
                .as_array()?
                .iter()
                .map(|f| f.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()?,
            outcome: text("outcome")?,
            truncated: value.get("truncated")?.as_bool()?,
        })
    }
}

#[derive(Clone, Debug)]
pub enum RecentSessionOutcome {
    Importable {
        thread: AgentSessionThread,
        source: TranscriptSource,
        cwd: String,
        digest: Box<SessionDigest>,
    },
    AlreadyImported {
        source: TranscriptSource,
    },
    Duplicate {
        source: TranscriptSource,
    },
    Skipped,
}

impl RecentSessionOutcome {
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Importable { .. } => "Importable",
            Self::AlreadyImported { .. } => "AlreadyImported",
            Self::Duplicate { .. } => "Duplicate",
            Self::Skipped => "Skipped",
        }
    }
}

pub type NowFn = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Clone, Default)]
pub struct SessionHistoryOptions {
    /// Agent homes to read. Defaults to `CLAUDE_CONFIG_DIR`/`~/.claude` and `CODEX_HOME`/`~/.codex`.
    pub homes: Option<Vec<AgentHome>>,
    /// omo CLI session roots. Defaults to the engine's own sessions directory.
    pub omo_sessions_roots: Option<Vec<String>>,
    pub imported_projects: Option<Vec<ImportedProject>>,
    /// App data directories whose subtree is never a project. Defaults to `~/.omo`.
    pub excluded_directories: Option<Vec<String>>,
    pub managed_worktree_directories: Option<Vec<String>>,
    pub home_dir: Option<String>,
    pub tmp_dir: Option<String>,
    pub env: Option<BTreeMap<String, String>>,
    /// Node-style platform name (`"win32"`, `"linux"`, `"darwin"`).
    pub platform: Option<String>,
    pub fs: Option<Arc<dyn SessionHistoryFs>>,
    pub now: Option<NowFn>,
}

#[derive(Clone, Debug, Default)]
pub struct RecentSessionsQuery {
    /// Restrict to one project; `None` means every non-excluded project.
    pub workspace_root: Option<String>,
    /// How far back a session counts as recent. Defaults to 30 days.
    pub recent_window_ms: Option<i64>,
    /// Transcripts still identical to one of these come back `AlreadyImported`, unread and outside the read caps.
    pub completed: Option<Vec<CompletedTranscript>>,
}
