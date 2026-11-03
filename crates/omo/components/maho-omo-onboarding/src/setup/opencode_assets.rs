//! Reads the OpenCode user-scope config and skill tree and converts what it finds into the shapes the
//! engine's global `mcp.json` and global skill root accept (upstream
//! `omo-native/bin/lib/setup-opencode-assets.js`). Read-only: nothing here writes.

use super::SetupRuntime;
use super::jsonc::parse_jsonc;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

// `interpolateString` in the engine's mcp config rejects any string value that looks like command
// substitution - one containing `$(` or starting (after leading whitespace) with `!` - and one bad
// value fails the whole file, so such a server is dropped, not copied.
fn rejected_by_engine(value: &Value) -> bool {
    match value {
        Value::String(text) => text.trim_start().starts_with('!') || text.contains("$("),
        Value::Array(items) => items.iter().any(rejected_by_engine),
        Value::Object(map) => map.values().any(rejected_by_engine),
        _ => false,
    }
}

// OpenCode's global config is every one of these files deep-merged in this order, later keys
// winning - not the first one found.
const GLOBAL_CONFIG_FILES: [&str; 3] = ["config.json", "opencode.json", "opencode.jsonc"];
// Every other user-scope config directory contributes only these two, on top of the global dir.
const DIRECTORY_CONFIG_FILES: [&str; 2] = ["opencode.json", "opencode.jsonc"];

/// Where OpenCode reads user-scope config, in its merge order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigSources {
    pub files: Vec<PathBuf>,
    pub directories: Vec<PathBuf>,
}

fn absolute(path: &str) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path))
}

/// The global dir's files, then `$OPENCODE_CONFIG`, then `~/.opencode` and `$OPENCODE_CONFIG_DIR`.
pub fn opencode_config_sources(runtime: &SetupRuntime) -> ConfigSources {
    let home = Path::new(&runtime.home);
    let global_dir = runtime
        .env_value("XDG_CONFIG_HOME")
        .map_or_else(|| home.join(".config"), PathBuf::from)
        .join("opencode");
    let mut directories = vec![global_dir.clone(), home.join(".opencode")];
    if let Some(explicit) = runtime
        .env
        .get("OPENCODE_CONFIG_DIR")
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        let explicit = absolute(explicit);
        if !directories.contains(&explicit) {
            directories.push(explicit);
        }
    }
    let mut files: Vec<PathBuf> = GLOBAL_CONFIG_FILES
        .iter()
        .map(|name| global_dir.join(name))
        .collect();
    if let Some(explicit) = runtime
        .env
        .get("OPENCODE_CONFIG")
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        files.push(absolute(explicit));
    }
    for directory in &directories[1..] {
        files.extend(
            DIRECTORY_CONFIG_FILES
                .iter()
                .map(|name| directory.join(name)),
        );
    }
    ConfigSources { files, directories }
}

pub(crate) fn merge_deep(
    base: &Map<String, Value>,
    overlay: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged = base.clone();
    for (key, value) in overlay {
        if key == "__proto__" {
            continue;
        }
        let next = match (merged.get(key), value) {
            (Some(Value::Object(left)), Value::Object(right)) => {
                Value::Object(merge_deep(left, right))
            }
            _ => value.clone(),
        };
        merged.insert(key.clone(), next);
    }
    merged
}

fn read_documents(
    files: &[PathBuf],
    label: &str,
    notices: &mut Vec<String>,
) -> Vec<Map<String, Value>> {
    let mut documents = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        match parse_jsonc(&text) {
            Err(error) => notices.push(format!(
                "WARN opencode: could not parse {}: {error}; its {label} were not imported",
                path.display()
            )),
            Ok(Value::Object(map)) => documents.push(map),
            Ok(_) => notices.push(format!(
                "WARN opencode: {} is not an object; its {label} were not imported",
                path.display()
            )),
        }
    }
    documents
}

/// One top-level object section (`mcp`, `provider`) of the merged OpenCode config.
pub fn read_opencode_section(
    files: &[PathBuf],
    section: &str,
    label: &str,
    notices: &mut Vec<String>,
) -> Map<String, Value> {
    let mut declared = Map::new();
    for parsed in read_documents(files, label, notices) {
        if let Some(Value::Object(own)) = parsed.get(section) {
            declared = merge_deep(&declared, own);
        }
    }
    declared
}

/// Every file of `files` deep-merged in order, later keys winning.
pub fn read_opencode_config(
    files: &[PathBuf],
    label: &str,
    notices: &mut Vec<String>,
) -> Map<String, Value> {
    read_documents(files, label, notices)
        .iter()
        .fold(Map::new(), |acc, doc| merge_deep(&acc, doc))
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// OpenCode substitutes `{env:NAME}`; the engine substitutes `${NAME}`. Same intent, same value.
pub fn convert_placeholders(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{env:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 5..];
        match after.find('}') {
            Some(end) if is_env_name(&after[..end]) => {
                out.push_str("${");
                out.push_str(&after[..end]);
                out.push('}');
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str("{env:");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn convert_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(convert_placeholders(text)),
        other => other.clone(),
    }
}

/// `/\{(?:file|env):[^}]+\}/` anywhere in `text`.
fn has_placeholder(text: &str) -> bool {
    ["{file:", "{env:"].iter().any(|open| {
        text.match_indices(open).any(|(index, _)| {
            let after = &text[index + open.len()..];
            after.find('}').is_some_and(|end| end > 0)
        })
    })
}

/// What is left after conversion is a placeholder the engine has no spelling for.
pub fn unconverted_placeholder(value: &Value) -> bool {
    match value {
        Value::String(text) => has_placeholder(text),
        Value::Array(items) => items.iter().any(unconverted_placeholder),
        Value::Object(map) => map.values().any(unconverted_placeholder),
        _ => false,
    }
}

fn convert_record(record: Option<&Value>) -> Option<Map<String, Value>> {
    let record = record?.as_object()?;
    let converted: Map<String, Value> = record
        .iter()
        .filter(|(_, v)| v.is_string())
        .map(|(k, v)| (k.clone(), convert_value(v)))
        .collect();
    (!converted.is_empty()).then_some(converted)
}

fn disabled(entry: &Map<String, Value>, target: &mut Map<String, Value>) {
    if entry.get("enabled") == Some(&Value::Bool(false)) {
        target.insert("enabled".into(), Value::Bool(false));
    }
}

fn convert_local(entry: &Map<String, Value>) -> Option<Value> {
    let command: Vec<&Value> = entry
        .get("command")
        .and_then(Value::as_array)
        .map(|parts| parts.iter().filter(|p| p.is_string()).collect())
        .unwrap_or_default();
    let first = command.first()?;
    let mut config = Map::new();
    config.insert("type".into(), json!("stdio"));
    config.insert("command".into(), convert_value(first));
    if command.len() > 1 {
        config.insert(
            "args".into(),
            Value::Array(command[1..].iter().map(|v| convert_value(v)).collect()),
        );
    }
    if let Some(env) = convert_record(entry.get("environment")) {
        config.insert("env".into(), Value::Object(env));
    }
    // Both sides resolve a relative cwd from the workspace directory the session runs in.
    if let Some(cwd) = entry
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|c| !c.trim().is_empty())
    {
        config.insert("cwd".into(), json!(convert_placeholders(cwd)));
    }
    disabled(entry, &mut config);
    Some(Value::Object(config))
}

// OpenCode's pre-registered OAuth client, in the engine's `oauth` shape. The engine has no client
// secret or per-server redirect URI, so a server that needs either is reported.
fn convert_oauth(name: &str, oauth: Option<&Value>, notices: &mut Vec<String>) -> Option<Value> {
    let oauth = oauth?.as_object()?;
    let mut converted = Map::new();
    if let Some(client) = oauth
        .get("clientId")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
    {
        converted.insert("clientId".into(), json!(convert_placeholders(client)));
    }
    // `Number.isInteger(port) && port >= 0 && port <= 65_535`.
    if let Some(port) = oauth
        .get("callbackPort")
        .and_then(Value::as_f64)
        .filter(|p| p.fract() == 0.0 && (0.0..=65_535.0).contains(p))
    {
        converted.insert("callbackPort".into(), json!(port as u64));
    }
    let scopes: Vec<Value> = oauth
        .get("scope")
        .and_then(Value::as_str)
        .map(|scope| scope.split_whitespace().map(|s| json!(s)).collect())
        .unwrap_or_default();
    if !scopes.is_empty() {
        converted.insert("scopes".into(), Value::Array(scopes));
    }
    let dropped: Vec<&str> = ["clientSecret", "redirectUri"]
        .into_iter()
        .filter(|key| oauth.contains_key(*key))
        .collect();
    if !dropped.is_empty() {
        notices.push(format!(
            "NOTICE opencode: mcp server {name} oauth {} has no omo equivalent and was not carried over; start omo and run /mcp auth {name} to sign in",
            dropped.join(" and ")
        ));
    }
    (!converted.is_empty()).then_some(Value::Object(converted))
}

fn convert_remote(
    name: &str,
    entry: &Map<String, Value>,
    notices: &mut Vec<String>,
) -> Option<Value> {
    let url = entry
        .get("url")
        .and_then(Value::as_str)
        .filter(|u| !u.trim().is_empty())?;
    let mut config = Map::new();
    config.insert("type".into(), json!("http"));
    config.insert("url".into(), json!(convert_placeholders(url)));
    if let Some(headers) = convert_record(entry.get("headers")) {
        config.insert("headers".into(), Value::Object(headers));
    }
    let oauth = convert_oauth(name, entry.get("oauth"), notices);
    // OpenCode's `oauth: false` disables OAuth auto-detection; the engine spells that `auth: false`.
    if entry.get("oauth") == Some(&Value::Bool(false)) {
        config.insert("auth".into(), Value::Bool(false));
    }
    if let Some(oauth) = oauth {
        config.insert("oauth".into(), oauth);
    }
    disabled(entry, &mut config);
    Some(Value::Object(config))
}

/// A server OpenCode declares that this stage did not carry, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub name: String,
    pub reason: String,
}

fn convert_server(
    name: &str,
    entry: &Value,
    notices: &mut Vec<String>,
    refused: &mut Vec<Refused>,
) -> Option<Value> {
    let entry = entry.as_object()?;
    let config = if entry.get("type").and_then(Value::as_str) == Some("remote") {
        convert_remote(name, entry, notices)
    } else {
        convert_local(entry)
    };
    let mut refuse = |notice: String, reason: &str| {
        notices.push(notice);
        refused.push(Refused {
            name: name.to_owned(),
            reason: reason.to_owned(),
        });
        None
    };
    let Some(config) = config else {
        return refuse(
            format!(
                "NOTICE opencode: mcp server {name} has no usable command or url; not imported"
            ),
            "no usable command or url",
        );
    };
    if rejected_by_engine(&config) {
        return refuse(
            format!(
                "NOTICE opencode: mcp server {name} uses command substitution ($(...) or a leading !), which omo refuses to run; not imported - resolve it to a fixed value or a ${{NAME}} environment reference and add the server to mcp.json by hand"
            ),
            "uses $(...) or a leading !",
        );
    }
    if unconverted_placeholder(&config) {
        return refuse(
            format!(
                "NOTICE opencode: mcp server {name} uses a {{file:...}} or {{env:...}} placeholder omo cannot express; not imported - put the value in an environment variable and reference it as ${{NAME}} in mcp.json"
            ),
            "uses a {file:...} or {env:...} placeholder",
        );
    }
    Some(config)
}

// The engine drops a SKILL.md whose frontmatter has no `description`, so copying one would report
// an import the next session never shows.
fn has_description(skill_file: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(skill_file) else {
        return false;
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return false;
    };
    let Some(end) = rest.find("\n---") else {
        return false;
    };
    let frontmatter = rest[..end].trim_end_matches('\r');
    frontmatter
        .lines()
        .any(|line| line.starts_with("description:"))
}

/// A skill directory to copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillSource {
    pub name: String,
    pub source: PathBuf,
}

fn read_skills(
    config_dirs: &[PathBuf],
    notices: &mut Vec<String>,
    skipped: &mut Vec<Refused>,
) -> Vec<SkillSource> {
    let mut skills: Vec<SkillSource> = Vec::new();
    let roots = config_dirs
        .iter()
        .flat_map(|dir| [dir.join("skills"), dir.join("skill")]);
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() && !kind.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let source = root.join(&name);
            let skill_file = source.join("SKILL.md");
            if !skill_file.exists() {
                notices.push(format!("NOTICE opencode: skill {name} has no SKILL.md at its top level; not imported - copy it into the omo skills dir by hand if it holds nested skills"));
                skipped.push(Refused {
                    name,
                    reason: "no SKILL.md".into(),
                });
                continue;
            }
            if !has_description(&skill_file) {
                notices.push(format!("NOTICE opencode: skill {name} has no description in its SKILL.md frontmatter, so omo would not load it; not imported"));
                skipped.push(Refused {
                    name,
                    reason: "no description in SKILL.md".into(),
                });
                continue;
            }
            if skills.iter().any(|skill| skill.name == name) {
                continue;
            }
            skills.push(SkillSource { name, source });
        }
    }
    // readdir order is filesystem-dependent; the printed plan and the import order must not be.
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// A converted MCP server.
#[derive(Clone, Debug, PartialEq)]
pub struct McpServer {
    pub name: String,
    pub config: Value,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpencodeAssets {
    pub mcp_servers: Vec<McpServer>,
    pub refused_servers: Vec<Refused>,
    pub skills: Vec<SkillSource>,
    pub skipped_skills: Vec<Refused>,
    pub notices: Vec<String>,
}

pub fn plan_opencode_assets(runtime: &SetupRuntime) -> OpencodeAssets {
    let sources = opencode_config_sources(runtime);
    let mut result = OpencodeAssets::default();
    let declared = read_opencode_section(&sources.files, "mcp", "mcp servers", &mut result.notices);
    for (name, entry) in &declared {
        if let Some(config) = convert_server(
            name,
            entry,
            &mut result.notices,
            &mut result.refused_servers,
        ) {
            result.mcp_servers.push(McpServer {
                name: name.clone(),
                config,
            });
        }
    }
    result.skills = read_skills(
        &sources.directories,
        &mut result.notices,
        &mut result.skipped_skills,
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn runtime(home: &Path) -> SetupRuntime {
        SetupRuntime::new(home.to_string_lossy(), BTreeMap::new())
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    #[test]
    fn placeholders_convert_and_unconvertible_ones_are_found() {
        assert_eq!(convert_placeholders("a {env:TOKEN} b"), "a ${TOKEN} b");
        assert_eq!(convert_placeholders("{env:1BAD}"), "{env:1BAD}");
        assert!(unconverted_placeholder(&json!({"x": ["{file:~/k}"]})));
        assert!(!unconverted_placeholder(&json!("${TOKEN}")));
    }

    #[test]
    fn servers_from_every_config_file_merge_and_convert() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        write(
            &home.join(".config/opencode/opencode.json"),
            r#"{"mcp":{"local":{"type":"local","command":["npx","srv","{env:ARG}"],"environment":{"K":"{env:K}","N":1},"enabled":false}}}"#,
        );
        write(
            &home.join(".config/opencode/opencode.jsonc"),
            "{\n // comment\n \"mcp\": {\"remote\": {\"type\": \"remote\", \"url\": \"https://x.test/mcp\", \"headers\": {\"A\": \"b\"}, \"oauth\": {\"clientId\": \"c\", \"scope\": \"a b\", \"clientSecret\": \"s\"}},\n \"bad\": {\"type\": \"local\", \"command\": [\"$(evil)\"]},\n \"file\": {\"type\": \"local\", \"command\": [\"{file:~/x}\"]},\n \"none\": {\"type\": \"local\"},},\n}",
        );
        let plan = plan_opencode_assets(&runtime(home));
        let names: Vec<&str> = plan.mcp_servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["local", "remote"]);
        assert_eq!(
            plan.mcp_servers[0].config,
            json!({"type": "stdio", "command": "npx", "args": ["srv", "${ARG}"], "env": {"K": "${K}"}, "enabled": false})
        );
        assert_eq!(
            plan.mcp_servers[1].config,
            json!({"type": "http", "url": "https://x.test/mcp", "headers": {"A": "b"}, "oauth": {"clientId": "c", "scopes": ["a", "b"]}})
        );
        let refused: Vec<&str> = plan
            .refused_servers
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(refused, vec!["bad", "file", "none"]);
        assert!(
            plan.notices
                .iter()
                .any(|n| n.contains("oauth clientSecret has no omo equivalent"))
        );
    }

    #[test]
    fn skills_need_a_skill_md_with_a_description() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        write(
            &home.join(".config/opencode/skills/good/SKILL.md"),
            "---\nname: good\ndescription: d\n---\nbody\n",
        );
        write(
            &home.join(".config/opencode/skills/nodesc/SKILL.md"),
            "---\nname: x\n---\n",
        );
        std::fs::create_dir_all(home.join(".opencode/skill/empty")).expect("mkdir");
        write(
            &home.join(".opencode/skill/good/SKILL.md"),
            "---\ndescription: dup\n---\n",
        );
        let plan = plan_opencode_assets(&runtime(home));
        assert_eq!(plan.skills.len(), 1);
        assert_eq!(plan.skills[0].name, "good");
        assert!(
            plan.skills[0]
                .source
                .ends_with(".config/opencode/skills/good")
        );
        let skipped: Vec<&str> = plan
            .skipped_skills
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(skipped, vec!["nodesc", "empty"]);
    }
}
