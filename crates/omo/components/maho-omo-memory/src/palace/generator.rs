use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use memory_core::compile::render_external_projection;
use memory_core::git::GitMemoryRepo;
use serde::Serialize;

use super::PalaceError;
use super::entry_collector::{PalaceCoreEntry, PalaceExternalEntry, collect_core, collect_external};
use super::history_collector::{PalaceHistory, collect_history};
use super::people::{PalacePeople, PalacePeopleOptions, collect_people};
use super::reflection_collector::{PalaceReflection, collect_reflection};
use super::template::{
    PALACE_DATA_PLACEHOLDER, PALACE_PEOPLE_PANEL, PALACE_PEOPLE_TAB, PALACE_TEMPLATE,
};
use crate::context::MemoryIdentityContext;

pub const VIEWER_DIR_MODE: u32 = 0o700;
pub const VIEWER_FILE_MODE: u32 = 0o600;
pub const DEFAULT_OUTCOME_LIMIT: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PalaceMetadata {
    pub identity: String,
    pub head_sha: Option<String>,
    pub recompiled_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PalaceCoreSection {
    pub entries: Vec<PalaceCoreEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PalaceExternalSection {
    pub entries: Vec<PalaceExternalEntry>,
    pub tree: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PalaceData {
    pub metadata: PalaceMetadata,
    pub core: PalaceCoreSection,
    pub external: PalaceExternalSection,
    pub history: PalaceHistory,
    pub reflection: PalaceReflection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub people: Option<PalacePeople>,
}

#[derive(Default)]
pub struct GeneratePalaceOptions {
    pub now: Option<Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>>,
    pub outcome_limit: Option<usize>,
    pub people: Option<PalacePeopleOptions>,
}

pub struct GeneratePalaceResult {
    pub path: PathBuf,
    pub data: PalaceData,
}

pub fn generate_palace_html(
    context: &MemoryIdentityContext,
    options: GeneratePalaceOptions,
) -> Result<GeneratePalaceResult, PalaceError> {
    let generated_at = options.now.as_ref().map(|now| now()).unwrap_or_else(Utc::now);
    let data = collect_palace_data(
        context,
        generated_at,
        options.outcome_limit.unwrap_or(DEFAULT_OUTCOME_LIMIT),
        options.people.unwrap_or_default(),
    )?;
    let html = render_palace_html(&data);

    let viewers_dir = context.identity_paths.viewers.clone();
    std::fs::create_dir_all(&viewers_dir)?;
    set_mode(&viewers_dir, VIEWER_DIR_MODE)?;

    let path = viewers_dir.join(format!("palace-{}.html", file_timestamp(generated_at)));
    write_file(&path, &html, VIEWER_FILE_MODE)?;
    Ok(GeneratePalaceResult { path, data })
}

pub fn collect_palace_data(
    context: &MemoryIdentityContext,
    generated_at: DateTime<Utc>,
    outcome_limit: usize,
    people: PalacePeopleOptions,
) -> Result<PalaceData, PalaceError> {
    let repo = GitMemoryRepo::open(context.identity_paths.repo.clone(), context.identity.as_str())?;
    let head = repo.head().ok().flatten();
    let core = collect_core(&repo, head.as_deref())?;
    let external = collect_external(&repo, head.as_deref())?;
    let history = collect_history(&repo)?;
    let reflection = collect_reflection(&context.identity_paths, outcome_limit)?;
    let people = collect_people(&repo, head.as_deref(), &people);
    let tree = render_external_projection(
        &external
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>(),
    );
    Ok(PalaceData {
        metadata: PalaceMetadata {
            identity: context.identity.clone(),
            head_sha: head,
            recompiled_at: generated_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        },
        core: PalaceCoreSection { entries: core },
        external: PalaceExternalSection {
            entries: external,
            tree,
        },
        history,
        reflection,
        people,
    })
}

pub fn render_palace_html(data: &PalaceData) -> String {
    let (tab, panel) = if data.people.is_none() {
        ("", "")
    } else {
        (PALACE_PEOPLE_TAB, PALACE_PEOPLE_PANEL)
    };
    PALACE_TEMPLATE
        .replace(PALACE_DATA_PLACEHOLDER, &encode_palace_data(data))
        .replace(PALACE_PEOPLE_TAB, tab)
        .replace(PALACE_PEOPLE_PANEL, panel)
}

pub fn encode_palace_data(data: &PalaceData) -> String {
    serde_json::to_string(data)
        .expect("palace data is always serializable")
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn file_timestamp(date: DateTime<Utc>) -> String {
    date.to_rfc3339_opts(SecondsFormat::Millis, true)
        .replace(':', "-")
        .replace('.', "-")
}

fn write_file(path: &Path, contents: &str, mode: u32) -> Result<(), PalaceError> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(path)?;
        file.write_all(contents.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::write(path, contents)?;
    }
    set_mode(path, mode)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), PalaceError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), PalaceError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palace::test_support::{INJECTION_PAYLOAD, create_palace_fixture, inline_json};

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-09T12:00:00.000Z")
            .expect("fixture timestamp")
            .with_timezone(&Utc)
    }

    fn options_at_fixed_now() -> GeneratePalaceOptions {
        GeneratePalaceOptions {
            now: Some(Arc::new(fixed_now)),
            ..Default::default()
        }
    }

    fn tabs(html: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mut rest = html;
        while let Some(index) = rest.find("data-tab=\"") {
            let start = index + "data-tab=\"".len();
            let Some(end) = rest[start..].find('"') else {
                break;
            };
            let name = &rest[start..start + end];
            if !name.is_empty() && name.chars().all(|character| character.is_ascii_lowercase()) {
                found.push(name.to_string());
            }
            rest = &rest[start + end..];
        }
        found
    }

    #[test]
    fn inline_data_pins_persona_head_and_identity() {
        let fixture = create_palace_fixture(false);

        let result = generate_palace_html(&fixture.context, options_at_fixed_now()).unwrap();
        let html = std::fs::read_to_string(&result.path).unwrap();
        let data = inline_json(&html);

        assert!(data["core"]["entries"].is_array());
        assert!(data["core"]["entries"].to_string().contains("system/persona.md"));
        assert_eq!(data["metadata"]["identity"].as_str(), Some(fixture.identity.as_str()));
        assert_eq!(data["metadata"]["headSha"].as_str(), Some(fixture.head.as_str()));
        assert_eq!(data["metadata"]["recompiledAt"].as_str(), Some("2026-08-09T12:00:00.000Z"));
        assert!(html.contains(fixture.head.as_str()));
    }

    #[test]
    fn default_people_gate_has_matching_panels() {
        let fixture = create_palace_fixture(false);

        let result = generate_palace_html(&fixture.context, GeneratePalaceOptions::default()).unwrap();
        let html = std::fs::read_to_string(&result.path).unwrap();

        let tabs = tabs(&html);
        assert_eq!(tabs, ["core", "external", "history", "reflection", "people"]);
        for tab in tabs {
            let panel = format!("id=\"panel-{tab}\"");
            assert!(html.contains(panel.as_str()));
        }
    }

    #[test]
    fn disabled_people_keeps_four_tabs() {
        let fixture = create_palace_fixture(false);

        let result = generate_palace_html(
            &fixture.context,
            GeneratePalaceOptions {
                people: Some(PalacePeopleOptions {
                    enabled: false,
                    limits: memory_core::people::PeopleLimits {
                        max_entries: 40,
                        max_entry_chars: 200,
                    },
                }),
                ..Default::default()
            },
        )
        .unwrap();
        let html = std::fs::read_to_string(&result.path).unwrap();

        let tabs = tabs(&html);
        assert_eq!(tabs, ["core", "external", "history", "reflection"]);
        for tab in tabs {
            let panel = format!("id=\"panel-{tab}\"");
            assert!(html.contains(panel.as_str()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn viewer_file_is_0600_inside_0700_directory() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = create_palace_fixture(false);

        let result = generate_palace_html(&fixture.context, GeneratePalaceOptions::default()).unwrap();

        let name = result.path.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("palace-") && name.ends_with(".html"));
        assert_eq!(
            std::fs::metadata(&result.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let dir = result.path.parent().unwrap();
        assert_eq!(
            std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn script_breakout_payload_is_escaped() {
        let fixture = create_palace_fixture(true);

        let result = generate_palace_html(&fixture.context, GeneratePalaceOptions::default()).unwrap();
        let html = std::fs::read_to_string(&result.path).unwrap();

        assert!(!html.contains(INJECTION_PAYLOAD));
        assert!(!html.contains("</script><img"));
        let data_marker = html.find("id=\"omo-palace-data\"").unwrap();
        let first_close = data_marker + html[data_marker..].find("</script").unwrap();
        assert_eq!(first_close, html.find("</script>\n<script>").unwrap());
        assert!(html.contains("\\u003c/script>\\u003cimg src=x onerror=alert(1)>"));
        assert!(inline_json(&html).to_string().contains(INJECTION_PAYLOAD));
    }

    #[test]
    fn line_separators_are_escaped() {
        let mut fixture = create_palace_fixture(false);
        fixture.commit_file(
            "system/separators.md",
            "---\ndescription: separators\n---\n\nline\u{2028}break\u{2029}paragraph\n",
            "chore: add separator fixture",
        );

        let result = generate_palace_html(&fixture.context, GeneratePalaceOptions::default()).unwrap();
        let html = std::fs::read_to_string(&result.path).unwrap();

        assert!(!html.contains('\u{2028}'));
        assert!(!html.contains('\u{2029}'));
        assert!(html.contains("\\u2028"));
        assert!(html.contains("\\u2029"));
    }

    #[test]
    fn viewer_is_self_contained() {
        let fixture = create_palace_fixture(false);

        let result = generate_palace_html(&fixture.context, GeneratePalaceOptions::default()).unwrap();
        let html = std::fs::read_to_string(&result.path).unwrap();

        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
        assert!(!html.contains("@import"));
        assert!(!html.contains("<link"));
    }

    #[test]
    fn committed_binary_lists_size_only() {
        let fixture = create_palace_fixture(false);

        let result = generate_palace_html(&fixture.context, GeneratePalaceOptions::default()).unwrap();
        let html = std::fs::read_to_string(&result.path).unwrap();
        let data = inline_json(&html);
        let external = data["external"]["entries"].as_array().unwrap();

        let binary = external
            .iter()
            .find(|entry| entry["path"] == "reference/logo.png")
            .unwrap();
        assert_eq!(binary["binary"], true);
        assert_eq!(binary["byteSize"], 12);
        assert!(binary.get("body").is_none());
        assert!(!html.contains("\\u0089PNG"));
    }
}
