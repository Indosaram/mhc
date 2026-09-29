//! Team-mode data model (the zod schemas of types.ts) with hand-written validation.
//!
//! zod's `parse`/`safeParse` become `parse` / `safe_parse` returning [`SchemaIssues`]; defaults
//! and strict-key checks match the TypeScript schemas.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::clock::now_ms;
use crate::member_parser::{self, MemberParseError};

pub const MESSAGE_KINDS: [&str; 5] = [
    "message",
    "shutdown_request",
    "shutdown_approved",
    "shutdown_rejected",
    "announcement",
];
pub const MEMBER_KINDS: [&str; 2] = ["category", "subagent_type"];
pub const TASK_STATUSES: [&str; 5] = ["pending", "claimed", "in_progress", "completed", "deleted"];
pub const RUNTIME_STATUSES: [&str; 7] = [
    "creating",
    "active",
    "shutdown_requested",
    "deleting",
    "deleted",
    "failed",
    "orphaned",
];

pub(crate) const MISSING_TEAM_LEAD_MESSAGE: &str = "leadAgentId required (or write a \u{60}lead: {...}\u{60} field, or mark one member with \u{60}isLead: true\u{60})";
const MAX_MESSAGE_BODY_UNITS: usize = 32 * 1024;

// ---------------------------------------------------------------------------------------------
// Validation issues
// ---------------------------------------------------------------------------------------------

/// One zod issue: a dotted path plus a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaIssue {
    pub path: Vec<String>,
    pub message: String,
}

/// A failed `safeParse`: every issue, in schema order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaIssues(pub Vec<SchemaIssue>);

impl SchemaIssues {
    #[must_use]
    pub fn first(&self) -> Option<&SchemaIssue> {
        self.0.first()
    }
}

impl fmt::Display for SchemaIssues {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered: Vec<Value> = self
            .0
            .iter()
            .map(|issue| serde_json::json!({ "path": issue.path, "message": issue.message }))
            .collect();
        write!(
            formatter,
            "{}",
            serde_json::to_string_pretty(&rendered).unwrap_or_default()
        )
    }
}

impl std::error::Error for SchemaIssues {}

#[derive(Default)]
pub(crate) struct Checker {
    path: Vec<String>,
    issues: Vec<SchemaIssue>,
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// A JSON number that is an integer (`z.number().int()`).
pub(crate) fn as_integer(value: &Value) -> Option<i64> {
    let number = value.as_number()?;
    if let Some(integer) = number.as_i64() {
        return Some(integer);
    }
    let float = number.as_f64()?;
    #[allow(clippy::cast_possible_truncation)]
    let truncated = float as i64;
    #[allow(clippy::cast_precision_loss)]
    (float.fract() == 0.0 && (truncated as f64) == float).then_some(truncated)
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

pub(crate) fn is_member_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

impl Checker {
    fn issue(&mut self, key: Option<&str>, message: impl Into<String>) {
        let mut path = self.path.clone();
        if let Some(key) = key {
            path.push(key.to_owned());
        }
        self.issues.push(SchemaIssue {
            path,
            message: message.into(),
        });
    }

    fn nested<T>(&mut self, key: &str, run: impl FnOnce(&mut Self) -> T) -> T {
        self.path.push(key.to_owned());
        let result = run(self);
        self.path.pop();
        result
    }

    fn object<'a>(
        &mut self,
        key: Option<&str>,
        value: &'a Value,
    ) -> Option<&'a Map<String, Value>> {
        let object = value.as_object();
        if object.is_none() {
            self.issue(
                key,
                format!(
                    "Invalid input: expected object, received {}",
                    type_name(value)
                ),
            );
        }
        object
    }

    fn strict(&mut self, object: &Map<String, Value>, allowed: &[&str]) {
        let unknown: Vec<&String> = object
            .keys()
            .filter(|key| !allowed.contains(&key.as_str()))
            .collect();
        if !unknown.is_empty() {
            let keys: Vec<String> = unknown.iter().map(|key| format!("\"{key}\"")).collect();
            let noun = if unknown.len() == 1 { "key" } else { "keys" };
            self.issue(None, format!("Unrecognized {noun}: {}", keys.join(", ")));
        }
    }

    fn string(&mut self, object: &Map<String, Value>, key: &str, required: bool) -> Option<String> {
        match object.get(key) {
            None if required => {
                self.issue(
                    Some(key),
                    "Invalid input: expected string, received undefined",
                );
                None
            }
            None => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(other) => {
                self.issue(
                    Some(key),
                    format!(
                        "Invalid input: expected string, received {}",
                        type_name(other)
                    ),
                );
                None
            }
        }
    }

    fn min_len(&mut self, key: &str, value: Option<&String>, min: usize) {
        if value.is_some_and(|text| text.encode_utf16().count() < min) {
            self.issue(
                Some(key),
                format!("Too small: expected string to have >={min} characters"),
            );
        }
    }

    fn name_pattern(&mut self, key: &str, value: Option<&String>) {
        if value.is_some_and(|text| !text.is_empty() && !is_member_name(text)) {
            self.issue(
                Some(key),
                "Invalid string: must match pattern /^[a-z0-9-]+$/",
            );
        }
    }

    fn uuid(&mut self, key: &str, value: Option<&String>) {
        if value.is_some_and(|text| !is_uuid(text)) {
            self.issue(Some(key), "Invalid UUID");
        }
    }

    fn integer(
        &mut self,
        object: &Map<String, Value>,
        key: &str,
        required: bool,
        positive: bool,
    ) -> Option<i64> {
        match object.get(key) {
            None => {
                if required {
                    self.issue(
                        Some(key),
                        "Invalid input: expected number, received undefined",
                    );
                }
                None
            }
            Some(value @ Value::Number(_)) => match as_integer(value) {
                Some(integer) if positive && integer <= 0 => {
                    self.issue(Some(key), "Too small: expected number to be >0");
                    None
                }
                Some(integer) => Some(integer),
                None => {
                    self.issue(Some(key), "Invalid input: expected int, received number");
                    None
                }
            },
            Some(other) => {
                self.issue(
                    Some(key),
                    format!(
                        "Invalid input: expected number, received {}",
                        type_name(other)
                    ),
                );
                None
            }
        }
    }

    fn boolean(&mut self, object: &Map<String, Value>, key: &str, required: bool) -> Option<bool> {
        match object.get(key) {
            None => {
                if required {
                    self.issue(
                        Some(key),
                        "Invalid input: expected boolean, received undefined",
                    );
                }
                None
            }
            Some(Value::Bool(flag)) => Some(*flag),
            Some(other) => {
                self.issue(
                    Some(key),
                    format!(
                        "Invalid input: expected boolean, received {}",
                        type_name(other)
                    ),
                );
                None
            }
        }
    }

    fn string_array(
        &mut self,
        object: &Map<String, Value>,
        key: &str,
        required: bool,
    ) -> Option<Vec<String>> {
        match object.get(key) {
            None => {
                if required {
                    self.issue(
                        Some(key),
                        "Invalid input: expected array, received undefined",
                    );
                }
                None
            }
            Some(Value::Array(items)) => {
                let mut strings = Vec::with_capacity(items.len());
                self.nested(key, |checker| {
                    for (index, item) in items.iter().enumerate() {
                        match item {
                            Value::String(text) => strings.push(text.clone()),
                            other => checker.issue(
                                Some(&index.to_string()),
                                format!(
                                    "Invalid input: expected string, received {}",
                                    type_name(other)
                                ),
                            ),
                        }
                    }
                });
                Some(strings)
            }
            Some(other) => {
                self.issue(
                    Some(key),
                    format!(
                        "Invalid input: expected array, received {}",
                        type_name(other)
                    ),
                );
                None
            }
        }
    }

    fn enumeration<T: Copy>(
        &mut self,
        object: &Map<String, Value>,
        key: &str,
        options: &[(&str, T)],
        default: Option<T>,
    ) -> Option<T> {
        match object.get(key) {
            None if default.is_some() => default,
            Some(Value::String(text)) => {
                if let Some((_, value)) = options.iter().find(|(name, _)| name == text) {
                    return Some(*value);
                }
                self.enum_issue(key, options);
                None
            }
            _ => {
                self.enum_issue(key, options);
                None
            }
        }
    }

    fn enum_issue<T>(&mut self, key: &str, options: &[(&str, T)]) {
        let names: Vec<String> = options
            .iter()
            .map(|(name, _)| format!("\"{name}\""))
            .collect();
        self.issue(
            Some(key),
            format!("Invalid option: expected one of {}", names.join("|")),
        );
    }

    fn literal_one(
        &mut self,
        object: &Map<String, Value>,
        key: &str,
        default: bool,
    ) -> Option<i64> {
        match object.get(key) {
            None if default => Some(1),
            Some(value) if as_integer(value) == Some(1) => Some(1),
            _ => {
                self.issue(Some(key), "Invalid input: expected 1");
                None
            }
        }
    }

    fn finish<T>(self, value: Option<T>) -> Result<T, SchemaIssues> {
        match value {
            Some(value) if self.issues.is_empty() => Ok(value),
            _ => Err(SchemaIssues(self.issues)),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Members and team specs
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BackendType {
    #[default]
    #[serde(rename = "in-process")]
    InProcess,
    #[serde(rename = "tmux")]
    Tmux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    #[default]
    Category,
    SubagentType,
}

impl MemberKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Category => "category",
            Self::SubagentType => "subagent_type",
        }
    }
}

/// A team member (`MemberSchema`, the category/subagent_type discriminated union).
///
/// Category members always carry `category` and `prompt`; subagent members carry
/// `subagent_type` and an optional `prompt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Member {
    pub kind: MemberKind,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(rename = "worktreePath", skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscriptions: Option<Vec<String>>,
    #[serde(rename = "backendType")]
    pub backend_type: BackendType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(rename = "isActive")]
    pub is_active: bool,
}

impl Member {
    /// A category member with schema defaults.
    #[must_use]
    pub fn category(name: &str, category: &str, prompt: &str) -> Self {
        Self {
            kind: MemberKind::Category,
            name: name.to_owned(),
            category: Some(category.to_owned()),
            prompt: Some(prompt.to_owned()),
            is_active: true,
            ..Self::default()
        }
    }

    /// A subagent_type member with schema defaults.
    #[must_use]
    pub fn subagent(name: &str, subagent_type: &str) -> Self {
        Self {
            kind: MemberKind::SubagentType,
            name: name.to_owned(),
            subagent_type: Some(subagent_type.to_owned()),
            is_active: true,
            ..Self::default()
        }
    }

    /// `MemberSchema.safeParse`.
    pub fn safe_parse(value: &Value) -> Result<Self, SchemaIssues> {
        let mut checker = Checker::default();
        let member = check_member(&mut checker, value);
        checker.finish(member)
    }
}

const MEMBER_BASE_KEYS: [&str; 8] = [
    "name",
    "cwd",
    "worktreePath",
    "task_summary",
    "subscriptions",
    "backendType",
    "color",
    "isActive",
];

fn check_member(checker: &mut Checker, value: &Value) -> Option<Member> {
    let object = checker.object(None, value)?;
    let kind = match object.get("kind").and_then(Value::as_str) {
        Some("category") => MemberKind::Category,
        Some("subagent_type") => MemberKind::SubagentType,
        _ => {
            checker.issue(
                Some("kind"),
                "Invalid input: expected \"category\" | \"subagent_type\"",
            );
            return None;
        }
    };
    let issues_before = checker.issues.len();
    let name = checker.string(object, "name", true);
    checker.min_len("name", name.as_ref(), 1);
    checker.name_pattern("name", name.as_ref());
    let cwd = checker.string(object, "cwd", false);
    let worktree_path = checker.string(object, "worktreePath", false);
    let task_summary = checker.string(object, "task_summary", false);
    if task_summary
        .as_ref()
        .is_some_and(|summary| summary.encode_utf16().count() > 80)
    {
        checker.issue(
            Some("task_summary"),
            "Too big: expected string to have <=80 characters",
        );
    }
    let subscriptions = checker.string_array(object, "subscriptions", false);
    let backend_type = checker.enumeration(
        object,
        "backendType",
        &[
            ("in-process", BackendType::InProcess),
            ("tmux", BackendType::Tmux),
        ],
        Some(BackendType::InProcess),
    );
    let color = checker.string(object, "color", false);
    let is_active = checker.boolean(object, "isActive", false).unwrap_or(true);
    let (category, subagent_type, prompt) = match kind {
        MemberKind::Category => {
            let category = checker.string(object, "category", true);
            checker.min_len("category", category.as_ref(), 1);
            let prompt = checker.string(object, "prompt", true);
            checker.min_len("prompt", prompt.as_ref(), 1);
            (category, None, prompt)
        }
        MemberKind::SubagentType => {
            let subagent_type = checker.string(object, "subagent_type", true);
            checker.min_len("subagent_type", subagent_type.as_ref(), 1);
            let prompt = checker.string(object, "prompt", false);
            (None, subagent_type, prompt)
        }
    };
    let kind_keys: &[&str] = match kind {
        MemberKind::Category => &["kind", "category", "prompt"],
        MemberKind::SubagentType => &["kind", "subagent_type", "prompt"],
    };
    let allowed: Vec<&str> = MEMBER_BASE_KEYS.iter().chain(kind_keys).copied().collect();
    checker.strict(object, &allowed);
    if checker.issues.len() != issues_before {
        return None;
    }
    Some(Member {
        kind,
        name: name?,
        category,
        subagent_type,
        prompt,
        cwd,
        worktree_path,
        task_summary,
        subscriptions,
        backend_type: backend_type?,
        color,
        is_active,
    })
}

/// A parsed team spec (`TeamSpecSchema` output: `leadAgentId` is always resolved).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamSpec {
    pub version: i64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    #[serde(rename = "leadAgentId")]
    pub lead_agent_id: String,
    #[serde(rename = "teamAllowedPaths", skip_serializing_if = "Option::is_none")]
    pub team_allowed_paths: Option<Vec<String>>,
    #[serde(rename = "sessionPermission", skip_serializing_if = "Option::is_none")]
    pub session_permission: Option<String>,
    pub members: Vec<Member>,
}

impl TeamSpec {
    /// `TeamSpecSchema.safeParse`: defaults `version`/`createdAt`, requires a lead for multi-member
    /// teams and falls back to the first member as lead.
    pub fn safe_parse(value: &Value) -> Result<Self, SchemaIssues> {
        let mut checker = Checker::default();
        let spec = check_team_spec(&mut checker, value);
        checker.finish(spec)
    }
}

fn check_team_spec(checker: &mut Checker, value: &Value) -> Option<TeamSpec> {
    let object = checker.object(None, value)?;
    let version = checker.literal_one(object, "version", true);
    let name = checker.string(object, "name", true);
    checker.min_len("name", name.as_ref(), 1);
    checker.name_pattern("name", name.as_ref());
    let description = checker.string(object, "description", false);
    let created_at = if object.contains_key("createdAt") {
        checker.integer(object, "createdAt", true, true)
    } else {
        Some(now_ms())
    };
    let lead_agent_id = checker.string(object, "leadAgentId", false);
    let team_allowed_paths = checker.string_array(object, "teamAllowedPaths", false);
    let session_permission = checker.string(object, "sessionPermission", false);
    let members = match object.get("members") {
        Some(Value::Array(items)) => {
            let members: Vec<Option<Member>> = checker.nested("members", |checker| {
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        checker.nested(&index.to_string(), |checker| check_member(checker, item))
                    })
                    .collect()
            });
            if items.is_empty() {
                checker.issue(
                    Some("members"),
                    "Too small: expected array to have >=1 items",
                );
            }
            if items.len() > 8 {
                checker.issue(Some("members"), "Too big: expected array to have <=8 items");
            }
            members.into_iter().collect::<Option<Vec<Member>>>()
        }
        Some(other) => {
            checker.issue(
                Some("members"),
                format!(
                    "Invalid input: expected array, received {}",
                    type_name(other)
                ),
            );
            None
        }
        None => {
            checker.issue(
                Some("members"),
                "Invalid input: expected array, received undefined",
            );
            None
        }
    };
    if !checker.issues.is_empty() {
        return None;
    }
    let members = members?;
    if lead_agent_id.is_none() && members.len() > 1 {
        checker.issue(Some("leadAgentId"), MISSING_TEAM_LEAD_MESSAGE);
        return None;
    }
    let lead_agent_id =
        lead_agent_id.or_else(|| members.first().map(|member| member.name.clone()))?;
    Some(TeamSpec {
        version: version?,
        name: name?,
        description,
        created_at: created_at?,
        lead_agent_id,
        team_allowed_paths,
        session_permission,
        members,
    })
}

// ---------------------------------------------------------------------------------------------
// Messages and tasks
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Message,
    ShutdownRequest,
    ShutdownApproved,
    ShutdownRejected,
    Announcement,
}

const MESSAGE_KIND_OPTIONS: [(&str, MessageKind); 5] = [
    ("message", MessageKind::Message),
    ("shutdown_request", MessageKind::ShutdownRequest),
    ("shutdown_approved", MessageKind::ShutdownApproved),
    ("shutdown_rejected", MessageKind::ShutdownRejected),
    ("announcement", MessageKind::Announcement),
];

impl MessageKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        MESSAGE_KIND_OPTIONS
            .iter()
            .find(|(_, kind)| *kind == self)
            .map_or("message", |(name, _)| name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamReference {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A mailbox message (`MessageSchema`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub version: i64,
    #[serde(rename = "messageId")]
    pub message_id: String,
    pub from: String,
    pub to: String,
    pub kind: MessageKind,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references: Option<Vec<TeamReference>>,
    pub timestamp: i64,
    #[serde(rename = "correlationId", skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

impl Message {
    /// `MessageSchema.safeParse`.
    pub fn safe_parse(value: &Value) -> Result<Self, SchemaIssues> {
        let mut checker = Checker::default();
        let message = check_message(&mut checker, value);
        checker.finish(message)
    }

    /// `MessageSchema.parse` of an already-typed message (re-validates the constraints).
    pub fn validated(self) -> Result<Self, SchemaIssues> {
        Self::safe_parse(&serde_json::to_value(&self).unwrap_or(Value::Null))
    }
}

fn check_message(checker: &mut Checker, value: &Value) -> Option<Message> {
    let object = checker.object(None, value)?;
    let version = checker.literal_one(object, "version", false);
    let message_id = checker.string(object, "messageId", true);
    checker.uuid("messageId", message_id.as_ref());
    let from = checker.string(object, "from", true);
    let to = checker.string(object, "to", true);
    let kind = checker.enumeration(object, "kind", &MESSAGE_KIND_OPTIONS, None);
    let body = checker.string(object, "body", true);
    if body
        .as_ref()
        .is_some_and(|body| body.encode_utf16().count() > MAX_MESSAGE_BODY_UNITS)
    {
        checker.issue(
            Some("body"),
            "Too big: expected string to have <=32768 characters",
        );
    }
    let summary = checker.string(object, "summary", false);
    let references = match object.get("references") {
        None => None,
        Some(Value::Array(items)) => {
            let parsed: Vec<Option<TeamReference>> = checker.nested("references", |checker| {
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        checker.nested(&index.to_string(), |checker| {
                            let object = checker.object(None, item)?;
                            checker.strict(object, &["path", "description"]);
                            let path = checker.string(object, "path", true);
                            let description = checker.string(object, "description", false);
                            Some(TeamReference {
                                path: path?,
                                description,
                            })
                        })
                    })
                    .collect()
            });
            parsed.into_iter().collect::<Option<Vec<_>>>()
        }
        Some(other) => {
            checker.issue(
                Some("references"),
                format!(
                    "Invalid input: expected array, received {}",
                    type_name(other)
                ),
            );
            None
        }
    };
    let timestamp = checker.integer(object, "timestamp", true, true);
    let correlation_id = checker.string(object, "correlationId", false);
    checker.uuid("correlationId", correlation_id.as_ref());
    let color = checker.string(object, "color", false);
    if !checker.issues.is_empty() {
        return None;
    }
    Some(Message {
        version: version?,
        message_id: message_id?,
        from: from?,
        to: to?,
        kind: kind?,
        body: body?,
        summary,
        references,
        timestamp: timestamp?,
        correlation_id,
        color,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Claimed,
    InProgress,
    Completed,
    Deleted,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Deleted => "deleted",
        })
    }
}

/// A shared-tasklist task (`TaskSchema`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub version: i64,
    pub id: String,
    pub subject: String,
    pub description: String,
    #[serde(rename = "activeForm", skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    pub status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default)]
    pub blocks: Vec<String>,
    #[serde(rename = "blockedBy", default)]
    pub blocked_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    #[serde(rename = "claimedAt", skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<i64>,
}

impl Task {
    /// `TaskSchema.parse` of raw JSON: structural decode plus the positive-integer checks.
    pub fn safe_parse(value: &Value) -> Result<Self, SchemaIssues> {
        let task: Self = serde_json::from_value(value.clone()).map_err(|error| {
            SchemaIssues(vec![SchemaIssue {
                path: Vec::new(),
                message: error.to_string(),
            }])
        })?;
        task.validated()
    }

    /// Re-check the constraints serde cannot express.
    pub fn validated(self) -> Result<Self, SchemaIssues> {
        let mut issues = Vec::new();
        let mut positive = |key: &str, value: Option<i64>| {
            if value.is_some_and(|value| value <= 0) {
                issues.push(SchemaIssue {
                    path: vec![key.to_owned()],
                    message: "Too small: expected number to be >0".to_owned(),
                });
            }
        };
        positive("createdAt", Some(self.created_at));
        positive("updatedAt", Some(self.updated_at));
        positive("claimedAt", self.claimed_at);
        if self.version != 1 {
            issues.push(SchemaIssue {
                path: vec!["version".to_owned()],
                message: "Invalid input: expected 1".to_owned(),
            });
        }
        if issues.is_empty() {
            Ok(self)
        } else {
            Err(SchemaIssues(issues))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Runtime state
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStatus {
    Creating,
    Active,
    ShutdownRequested,
    Deleting,
    Deleted,
    Failed,
    Orphaned,
}

impl RuntimeStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Active => "active",
            Self::ShutdownRequested => "shutdown_requested",
            Self::Deleting => "deleting",
            Self::Deleted => "deleted",
            Self::Failed => "failed",
            Self::Orphaned => "orphaned",
        }
    }
}

impl fmt::Display for RuntimeStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentType {
    #[serde(rename = "leader")]
    Leader,
    #[serde(rename = "general-purpose")]
    GeneralPurpose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberStatus {
    Pending,
    Running,
    Idle,
    Errored,
    Completed,
    ShutdownApproved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecSource {
    Project,
    User,
}

impl SpecSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingType {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeThinking {
    #[serde(rename = "type")]
    pub thinking_type: ThinkingType,
    #[serde(rename = "budgetTokens", skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStateMemberModel {
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(rename = "reasoningEffort", skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<Number>,
    #[serde(rename = "maxTokens", skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<RuntimeThinking>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStateMember {
    pub name: String,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(rename = "tmuxPaneId", skip_serializing_if = "Option::is_none")]
    pub tmux_pane_id: Option<String>,
    #[serde(rename = "tmuxGridPaneId", skip_serializing_if = "Option::is_none")]
    pub tmux_grid_pane_id: Option<String>,
    #[serde(rename = "agentType")]
    pub agent_type: AgentType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<RuntimeStateMemberModel>,
    pub status: MemberStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(rename = "worktreePath", skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(
        rename = "lastInjectedTurnMarker",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_injected_turn_marker: Option<String>,
    #[serde(rename = "pendingInjectedMessageIds", default)]
    pub pending_injected_message_ids: Vec<String>,
}

impl RuntimeStateMember {
    /// A pending member with no session yet.
    #[must_use]
    pub fn new(name: &str, agent_type: AgentType) -> Self {
        Self {
            name: name.to_owned(),
            session_id: None,
            tmux_pane_id: None,
            tmux_grid_pane_id: None,
            agent_type,
            subagent_type: None,
            category: None,
            model: None,
            status: MemberStatus::Pending,
            color: None,
            worktree_path: None,
            last_injected_turn_marker: None,
            pending_injected_message_ids: Vec::new(),
        }
    }
}

fn default_max_members() -> i64 {
    8
}
fn default_max_parallel_members() -> i64 {
    4
}
fn default_max_messages_per_run() -> i64 {
    10_000
}
fn default_max_wall_clock_minutes() -> i64 {
    120
}
fn default_max_member_turns() -> i64 {
    500
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeBounds {
    #[serde(rename = "maxMembers", default = "default_max_members")]
    pub max_members: i64,
    #[serde(
        rename = "maxParallelMembers",
        default = "default_max_parallel_members"
    )]
    pub max_parallel_members: i64,
    #[serde(rename = "maxMessagesPerRun", default = "default_max_messages_per_run")]
    pub max_messages_per_run: i64,
    #[serde(
        rename = "maxWallClockMinutes",
        default = "default_max_wall_clock_minutes"
    )]
    pub max_wall_clock_minutes: i64,
    #[serde(rename = "maxMemberTurns", default = "default_max_member_turns")]
    pub max_member_turns: i64,
}

impl Default for RuntimeBounds {
    fn default() -> Self {
        Self {
            max_members: 8,
            max_parallel_members: 4,
            max_messages_per_run: 10_000,
            max_wall_clock_minutes: 120,
            max_member_turns: 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownRequest {
    #[serde(rename = "memberId")]
    pub member_id: String,
    #[serde(rename = "requesterName")]
    pub requester_name: String,
    #[serde(rename = "requestedAt")]
    pub requested_at: i64,
    #[serde(rename = "approvedAt", skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<i64>,
    #[serde(rename = "rejectedReason", skip_serializing_if = "Option::is_none")]
    pub rejected_reason: Option<String>,
    #[serde(rename = "rejectedAt", skip_serializing_if = "Option::is_none")]
    pub rejected_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStateTmuxLayout {
    #[serde(rename = "ownedSession")]
    pub owned_session: bool,
    #[serde(rename = "targetSessionId")]
    pub target_session_id: String,
    #[serde(rename = "focusWindowId", skip_serializing_if = "Option::is_none")]
    pub focus_window_id: Option<String>,
    #[serde(rename = "gridWindowId", skip_serializing_if = "Option::is_none")]
    pub grid_window_id: Option<String>,
    #[serde(rename = "paneIds", skip_serializing_if = "Option::is_none")]
    pub pane_ids: Option<Vec<String>>,
}

/// Persisted team-run state (`RuntimeStateSchema`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeState {
    pub version: i64,
    #[serde(rename = "teamRunId")]
    pub team_run_id: String,
    #[serde(rename = "teamName")]
    pub team_name: String,
    #[serde(rename = "specSource")]
    pub spec_source: SpecSource,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub status: RuntimeStatus,
    #[serde(rename = "leadSessionId", skip_serializing_if = "Option::is_none")]
    pub lead_session_id: Option<String>,
    #[serde(rename = "tmuxLayout", skip_serializing_if = "Option::is_none")]
    pub tmux_layout: Option<RuntimeStateTmuxLayout>,
    pub members: Vec<RuntimeStateMember>,
    #[serde(rename = "shutdownRequests", default)]
    pub shutdown_requests: Vec<ShutdownRequest>,
    pub bounds: RuntimeBounds,
}

impl RuntimeState {
    /// `RuntimeStateSchema.safeParse`: structural decode plus uuid/positive/literal checks.
    pub fn safe_parse(value: &Value) -> Result<Self, SchemaIssues> {
        let state: Self = serde_json::from_value(value.clone()).map_err(|error| {
            SchemaIssues(vec![SchemaIssue {
                path: Vec::new(),
                message: error.to_string(),
            }])
        })?;
        state.validated()
    }

    /// Re-check the constraints serde cannot express.
    pub fn validated(self) -> Result<Self, SchemaIssues> {
        let mut issues = Vec::new();
        let mut push = |path: &[&str], message: &str| {
            issues.push(SchemaIssue {
                path: path.iter().map(|segment| (*segment).to_owned()).collect(),
                message: message.to_owned(),
            });
        };
        if self.version != 1 {
            push(&["version"], "Invalid input: expected 1");
        }
        if !is_uuid(&self.team_run_id) {
            push(&["teamRunId"], "Invalid UUID");
        }
        if self.created_at <= 0 {
            push(&["createdAt"], "Too small: expected number to be >0");
        }
        for request in &self.shutdown_requests {
            let times = [
                Some(request.requested_at),
                request.approved_at,
                request.rejected_at,
            ];
            if times.iter().flatten().any(|time| *time <= 0) {
                push(&["shutdownRequests"], "Too small: expected number to be >0");
            }
        }
        for member in &self.members {
            let budget = member
                .model
                .as_ref()
                .and_then(|model| model.thinking.as_ref())
                .and_then(|thinking| thinking.budget_tokens);
            if budget.is_some_and(|budget| budget <= 0) {
                push(&["members"], "Too small: expected number to be >0");
            }
        }
        if issues.is_empty() {
            Ok(self)
        } else {
            Err(SchemaIssues(issues))
        }
    }
}

/// A row of `listActiveTeams`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActiveTeamSummary {
    #[serde(rename = "teamRunId")]
    pub team_run_id: String,
    #[serde(rename = "teamName")]
    pub team_name: String,
    pub status: RuntimeStatus,
    #[serde(rename = "leadSessionId", skip_serializing_if = "Option::is_none")]
    pub lead_session_id: Option<String>,
    #[serde(rename = "memberCount")]
    pub member_count: usize,
    pub scope: SpecSource,
}

// ---------------------------------------------------------------------------------------------
// Agent eligibility
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EligibilityVerdict {
    Eligible,
    Conditional,
    HardReject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentEligibility {
    pub verdict: EligibilityVerdict,
    pub rejection_message: Option<&'static str>,
}

const fn eligible() -> AgentEligibility {
    AgentEligibility {
        verdict: EligibilityVerdict::Eligible,
        rejection_message: None,
    }
}

const fn rejected(message: &'static str) -> AgentEligibility {
    AgentEligibility {
        verdict: EligibilityVerdict::HardReject,
        rejection_message: Some(message),
    }
}

pub const AGENT_ELIGIBILITY_REGISTRY: [(&str, AgentEligibility); 11] = [
    ("sisyphus", eligible()),
    (
        "hephaestus",
        AgentEligibility {
            verdict: EligibilityVerdict::Conditional,
            rejection_message: Some(
                "Agent 'hephaestus' lacks teammate permission. Either apply D-36 (add teammate: \"allow\" in tool-config-handler.ts) or use subagent_type: \"sisyphus\" instead.",
            ),
        },
    ),
    (
        "oracle",
        rejected(
            "Agent 'oracle' is read-only (cannot write files). Team members must write to mailbox inbox files. Use delegate-task with subagent_type: 'oracle' for read-only analysis instead.",
        ),
    ),
    (
        "librarian",
        rejected(
            "Agent 'librarian' is read-only (write/edit denied). Cannot write to mailbox as team member. Use delegate-task for research queries instead.",
        ),
    ),
    (
        "explore",
        rejected(
            "Agent 'explore' is read-only (write/edit denied). Cannot write to mailbox as team member. Use delegate-task for codebase exploration instead.",
        ),
    ),
    (
        "multimodal-looker",
        rejected(
            "Agent 'multimodal-looker' has read-only tool access (only 'read' allowed). Cannot write to mailbox as team member.",
        ),
    ),
    (
        "metis",
        rejected(
            "Agent 'metis' is read-only (pre-planning consultant). Cannot write to mailbox as team member. Use delegate-task for pre-planning analysis instead.",
        ),
    ),
    (
        "momus",
        rejected(
            "Agent 'momus' is read-only (plan reviewer). Cannot write to mailbox as team member. Use delegate-task for plan review instead.",
        ),
    ),
    ("atlas", eligible()),
    (
        "prometheus",
        rejected(
            "Agent 'prometheus' is plan-mode-only; can only write to .omo/*.md (enforced by prometheusMdOnly hook). Cannot write to team mailbox. Use delegate-task with subagent_type: 'plan' instead.",
        ),
    ),
    ("sisyphus-junior", eligible()),
];

/// `AGENT_ELIGIBILITY_REGISTRY[agent]`.
#[must_use]
pub fn agent_eligibility(agent: &str) -> Option<AgentEligibility> {
    AGENT_ELIGIBILITY_REGISTRY
        .iter()
        .find(|(name, _)| *name == agent)
        .map(|(_, entry)| *entry)
}

/// `parseMember`: hard-rejected subagents fail with their rejection message; everything else
/// goes through the member parser with its translated validation errors.
pub fn parse_member(input: &Value) -> Result<Member, MemberParseError> {
    if let Some(subagent_type) = input.get("subagent_type").and_then(Value::as_str)
        && let Some(entry) = agent_eligibility(subagent_type)
        && entry.verdict == EligibilityVerdict::HardReject
    {
        return Err(MemberParseError::Rejected(
            entry.rejection_message.unwrap_or_default().to_owned(),
        ));
    }
    member_parser::parse_member_base(input)
}
