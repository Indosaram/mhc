//! Classifies and writes the OpenCode assets [`super::opencode_assets`] planned (upstream
//! `omo-native/bin/lib/setup-assets-import.js`): MCP servers into the engine's GLOBAL
//! `<agentDir>/mcp.json` and skills into the GLOBAL `<agentDir>/skills` root. A name that already
//! exists is never overwritten. [`plan_assets`] only reads; [`apply_assets`] is the one writer.

use super::opencode_assets::{McpServer, Refused, SkillSource, plan_opencode_assets};
use super::{SetupRuntime, list};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// The plugin's bundled skills reach the engine after `<agentDir>/skills`, and the first skill of a
// name wins - so a same-named copy imported into the user root would silently replace the bundled
// skill in every session.
fn bundled_skill_names(skills_dir: &Path) -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return BTreeSet::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().join("SKILL.md").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// The engine's mcp.json as the plan read it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpTarget {
    pub document: Map<String, Value>,
    pub bytes: Option<String>,
    pub malformed: bool,
}

fn read_mcp_target(path: &Path) -> McpTarget {
    let Ok(bytes) = std::fs::read_to_string(path) else {
        return McpTarget::default();
    };
    match serde_json::from_str::<Value>(&bytes) {
        // A non-object `mcpServers` is a file the engine already rejects; it is left alone.
        Ok(Value::Object(document)) if document.get("mcpServers").is_none_or(Value::is_object) => {
            McpTarget {
                document,
                bytes: Some(bytes),
                malformed: false,
            }
        }
        _ => McpTarget {
            document: Map::new(),
            bytes: Some(bytes),
            malformed: true,
        },
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerResult {
    pub added: Vec<McpServer>,
    pub skipped_existing: Vec<String>,
    pub blocked: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkillResult {
    pub added: Vec<SkillSource>,
    pub skipped_existing: Vec<String>,
    pub skipped_bundled: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssetResult {
    pub servers: ServerResult,
    pub skills: SkillResult,
    pub target: McpTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetPaths {
    pub mcp: PathBuf,
    pub skills: PathBuf,
}

fn classify_assets(
    servers: &[McpServer],
    skills: &[SkillSource],
    paths: &AssetPaths,
    bundled: &BTreeSet<String>,
) -> AssetResult {
    let target = read_mcp_target(&paths.mcp);
    let existing = target
        .document
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut result = AssetResult::default();
    for server in servers {
        if target.malformed {
            result.servers.blocked.push(server.name.clone());
        } else if existing.contains_key(&server.name) {
            result.servers.skipped_existing.push(server.name.clone());
        } else {
            result.servers.added.push(server.clone());
        }
    }
    for skill in skills {
        if paths.skills.join(&skill.name).exists() {
            result.skills.skipped_existing.push(skill.name.clone());
        } else if bundled.contains(&skill.name) {
            result.skills.skipped_bundled.push(skill.name.clone());
        } else {
            result.skills.added.push(skill.clone());
        }
    }
    result.target = target;
    result
}

fn write_mcp(path: &Path, target: &McpTarget, added: &[McpServer]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        super::create_private_dir(parent)?;
    }
    if target.bytes.is_some() {
        std::fs::copy(
            path,
            format!("{}.bak-{}", path.display(), super::timestamp()),
        )?;
    }
    let mut next = target.document.clone();
    let mut servers = next
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for server in added {
        servers.insert(server.name.clone(), server.config.clone());
    }
    next.insert("mcpServers".into(), Value::Object(servers));
    // Server env and headers can carry tokens, so this file gets the same 0600 the auth store does.
    let text = serde_json::to_string_pretty(&Value::Object(next)).map_err(std::io::Error::other)?;
    super::write_private(path, text.as_bytes())
}

/// `cpSync(source, target, { recursive: true, force: false })`: never overwrites a file.
fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    let meta = std::fs::metadata(source)?;
    if meta.is_dir() {
        std::fs::create_dir_all(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
        Ok(())
    } else if target.exists() {
        Ok(())
    } else {
        std::fs::copy(source, target).map(|_| ())
    }
}

/// The asset stage's plan.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetPlan {
    pub agent_dir: PathBuf,
    pub paths: AssetPaths,
    pub notices: Vec<String>,
    pub present: bool,
    pub result: AssetResult,
    pub refused_servers: Vec<Refused>,
    pub skipped_skills: Vec<Refused>,
    pub pending: usize,
}

/// The asset stage's one writer: the servers and skills [`plan_assets`] classified as new.
///
/// # Errors
/// A filesystem step failed.
pub fn apply_assets(plan: &AssetPlan) -> Result<(), String> {
    let result = &plan.result;
    if !result.servers.added.is_empty() {
        write_mcp(&plan.paths.mcp, &result.target, &result.servers.added)
            .map_err(|e| e.to_string())?;
    }
    for skill in &result.skills.added {
        // classify already skipped every name that exists; never overwriting only keeps a directory
        // created between that check and this copy from being replaced file by file.
        copy_tree(&skill.source, &plan.paths.skills.join(&skill.name))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn asset_plan_lines(plan: &AssetPlan) -> Vec<String> {
    let r = &plan.result;
    vec![
        list(
            "planned-mcp",
            &r.servers
                .added
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>(),
        ),
        list("mcp-skipped-existing", &r.servers.skipped_existing),
        list(
            "planned-skills",
            &r.skills
                .added
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>(),
        ),
        list("skills-skipped-existing", &r.skills.skipped_existing),
        list("skills-skipped-bundled", &r.skills.skipped_bundled),
    ]
}

pub fn asset_counts(plan: &AssetPlan) -> Vec<String> {
    let r = &plan.result;
    vec![
        format!("mcp-imported: {}", r.servers.added.len()),
        format!("mcp-skipped-existing: {}", r.servers.skipped_existing.len()),
        format!("skills-imported: {}", r.skills.added.len()),
        format!(
            "skills-skipped-existing: {}",
            r.skills.skipped_existing.len()
        ),
        format!("skills-skipped-bundled: {}", r.skills.skipped_bundled.len()),
    ]
}

pub fn asset_question(plan: &AssetPlan) -> String {
    format!(
        "Import {} MCP server(s) and {} skill(s) into {}? [y/N] ",
        plan.result.servers.added.len(),
        plan.result.skills.added.len(),
        plan.agent_dir.display()
    )
}

/// The plugin root whose `skills/` the bundled skills ship in: the runtime's override, else the
/// bundled-skills directory the native harness resolves (its parent), else nothing bundled.
fn bundled_skills_dir(runtime: &SetupRuntime) -> Option<PathBuf> {
    if let Some(root) = &runtime.plugin_root {
        return Some(root.join("skills"));
    }
    maho_omo_bundled_skills::index::resolve_bundled_skills_dir(&runtime.env)
}

/// The asset stage of `omo setup`, read-only. `present` is false when opencode declares neither.
pub fn plan_assets(runtime: &SetupRuntime, agent_dir: &Path) -> AssetPlan {
    let source = plan_opencode_assets(runtime);
    let mut notices = source.notices.clone();
    let present = !source.mcp_servers.is_empty() || !source.skills.is_empty();
    let paths = AssetPaths {
        mcp: agent_dir.join("mcp.json"),
        skills: agent_dir.join("skills"),
    };
    let bundled = bundled_skills_dir(runtime)
        .map(|dir| bundled_skill_names(&dir))
        .unwrap_or_default();
    let result = classify_assets(&source.mcp_servers, &source.skills, &paths, &bundled);
    if !result.servers.blocked.is_empty() {
        notices.push(format!(
            "WARN senpi: malformed mcp.json; these servers were not imported: {}",
            result.servers.blocked.join(", ")
        ));
    }
    let pending = result.servers.added.len() + result.skills.added.len();
    AssetPlan {
        agent_dir: agent_dir.to_path_buf(),
        paths,
        notices,
        present,
        result,
        refused_servers: source.refused_servers,
        skipped_skills: source.skipped_skills,
        pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn runtime(root: &Path) -> SetupRuntime {
        let mut runtime = SetupRuntime::new(root.join("home").to_string_lossy(), BTreeMap::new());
        runtime.plugin_root = Some(root.join("plugin"));
        runtime
    }

    #[test]
    fn new_servers_and_skills_import_existing_and_bundled_ones_are_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let home = root.join("home");
        write(
            &home.join(".config/opencode/opencode.json"),
            r#"{"mcp":{"ctx":{"type":"local","command":["npx","ctx"]},"mine":{"type":"local","command":["x"]}}}"#,
        );
        for name in ["new-skill", "kept", "bundled"] {
            write(
                &home.join(format!(".config/opencode/skills/{name}/SKILL.md")),
                "---\ndescription: d\n---\n",
            );
        }
        write(
            &home.join(".config/opencode/skills/new-skill/extra/notes.md"),
            "n",
        );
        let agent_dir = root.join("agent");
        write(
            &agent_dir.join("mcp.json"),
            r#"{"other":1,"mcpServers":{"mine":{"command":"keep"}}}"#,
        );
        std::fs::create_dir_all(agent_dir.join("skills/kept")).expect("mkdir");
        write(&root.join("plugin/skills/bundled/SKILL.md"), "x");
        let plan = plan_assets(&runtime(root), &agent_dir);
        assert!(plan.present);
        assert_eq!(plan.pending, 2);
        assert_eq!(
            asset_plan_lines(&plan),
            vec![
                "planned-mcp: ctx",
                "mcp-skipped-existing: mine",
                "planned-skills: new-skill",
                "skills-skipped-existing: kept",
                "skills-skipped-bundled: bundled",
            ]
        );
        apply_assets(&plan).expect("apply");
        let mcp: Value = serde_json::from_str(
            &std::fs::read_to_string(agent_dir.join("mcp.json")).expect("read"),
        )
        .expect("json");
        assert_eq!(
            mcp,
            json!({"other": 1, "mcpServers": {"mine": {"command": "keep"}, "ctx": {"type": "stdio", "command": "npx", "args": ["ctx"]}}})
        );
        assert!(agent_dir.join("skills/new-skill/extra/notes.md").exists());
        assert!(!agent_dir.join("skills/bundled").exists());
    }

    #[test]
    fn a_malformed_mcp_json_blocks_every_server() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(
            &root.join("home/.config/opencode/opencode.json"),
            r#"{"mcp":{"ctx":{"type":"local","command":["npx"]}}}"#,
        );
        write(&root.join("agent/mcp.json"), r#"{"mcpServers":[1]}"#);
        let plan = plan_assets(&runtime(root), &root.join("agent"));
        assert_eq!(plan.result.servers.blocked, vec!["ctx"]);
        assert_eq!(plan.pending, 0);
        assert!(plan.notices.contains(
            &"WARN senpi: malformed mcp.json; these servers were not imported: ctx".to_owned()
        ));
    }
}
