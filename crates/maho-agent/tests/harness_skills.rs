use maho_agent::harness::context::background_context;
use maho_agent::harness::env::nodejs::NodeExecutionEnv;
use maho_agent::harness::skills::{format_skill_invocation, load_skills, load_sourced_skills};
use maho_agent::harness::types::Skill;

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("maho-skills-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&path).expect("fixture invariant");
        Self(path)
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().expect("fixture invariant"))
            .expect("fixture invariant");
        std::fs::write(path, text).expect("fixture invariant");
    }
    fn env(&self) -> NodeExecutionEnv {
        NodeExecutionEnv::new(self.0.to_string_lossy().into_owned())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("fixture invariant");
    }
}

#[tokio::test]
async fn loads_skill_through_execution_environment() {
    let f = Fixture::new("load");
    f.write(".agents/skills/example/SKILL.md", "---\nname: example\ndescription: Example skill\ndisable-model-invocation: true\n---\nUse this skill.\n");
    let result = load_skills(&f.env(), &[".agents/skills"], &background_context()).await;
    assert!(result.diagnostics.is_empty());
    assert_eq!(
        result.skills,
        vec![Skill {
            name: "example".into(),
            description: "Example skill".into(),
            content: "Use this skill.".into(),
            file_path: f
                .0
                .join(".agents/skills/example/SKILL.md")
                .to_string_lossy()
                .into_owned(),
            disable_model_invocation: Some(true)
        }]
    );
}

#[tokio::test]
async fn loads_symlinked_directories() {
    let f = Fixture::new("symlink");
    f.write(
        "actual/example/SKILL.md",
        "---\nname: example\ndescription: Example skill\n---\nUse this skill.",
    );
    std::os::unix::fs::symlink(f.0.join("actual"), f.0.join("skills-link"))
        .expect("fixture invariant");
    let result = load_skills(&f.env(), &["skills-link"], &background_context()).await;
    assert_eq!(result.skills[0].name, "example");
    assert_eq!(
        result.skills[0].file_path,
        f.0.join("skills-link/example/SKILL.md").to_string_lossy()
    );
}

#[tokio::test]
async fn preserves_source_info() {
    let f = Fixture::new("source");
    f.write(
        "user/example/SKILL.md",
        "---\nname: example\ndescription: Example skill\n---\nUse this skill.",
    );
    let result = load_sourced_skills(
        &f.env(),
        &[("user", serde_json::json!({"type":"user"}))],
        |skill, _, _| skill,
        &background_context(),
    )
    .await;
    assert!(result.diagnostics.is_empty());
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].source, serde_json::json!({"type":"user"}));
    assert_eq!(result.skills[0].skill.disable_model_invocation, Some(false));
    assert_eq!(result.skills[0].skill.name, "example");
    assert_eq!(result.skills[0].skill.description, "Example skill");
    assert_eq!(result.skills[0].skill.content, "Use this skill.");
    assert_eq!(
        result.skills[0].skill.file_path,
        f.0.join("user/example/SKILL.md").to_string_lossy()
    );
}

#[tokio::test]
async fn attaches_sources_to_diagnostics() {
    let f = Fixture::new("diagnostic");
    f.write(
        "user/broken/SKILL.md",
        "---\nname: broken\n---\nMissing description.",
    );
    let result = load_sourced_skills(
        &f.env(),
        &[("user", "user")],
        |skill, _, _| skill,
        &background_context(),
    )
    .await;
    assert!(result.skills.is_empty());
    assert_eq!(result.diagnostics.len(), 1);
    let d = &result.diagnostics[0];
    assert_eq!(d.source, "user");
    assert_eq!(d.diagnostic.code, "invalid_metadata");
    assert_eq!(d.diagnostic.r#type, "warning");
    assert_eq!(d.diagnostic.message, "description is required");
    assert_eq!(
        d.diagnostic.path,
        f.0.join("user/broken/SKILL.md").to_string_lossy()
    );
}

#[tokio::test]
async fn loads_direct_markdown_only_at_root() {
    let f = Fixture::new("root");
    f.write(
        "skills/root.md",
        "---\ndescription: Root skill\n---\nRoot content",
    );
    f.write(
        "skills/nested/ignored.md",
        "---\ndescription: Ignored\n---\nIgnored content",
    );
    let result = load_skills(&f.env(), &["skills"], &background_context()).await;
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "skills");
    assert_eq!(result.skills[0].content, "Root content");
}

#[tokio::test]
async fn ignores_undeclared_root_documents() {
    let f = Fixture::new("docs");
    f.write("skills/README.md", "# Shared skills\n\nDocumentation.");
    f.write("skills/AGENTS.md", "# Agent notes\n\nDocumentation.");
    f.write(
        "skills/CLAUDE.md",
        "---\ndescription: [invalid\n---\n\nDocumentation.",
    );
    f.write(
        "skills/root.md",
        "---\ndescription: Root skill\n---\nRoot content",
    );
    f.write(
        "skills/nested-skill/SKILL.md",
        "---\nname: nested-skill\ndescription: Nested skill\n---\nNested content",
    );
    let result = load_skills(&f.env(), &["skills"], &background_context()).await;
    assert!(result.diagnostics.is_empty());
    let mut names: Vec<_> = result.skills.iter().map(|s| s.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["nested-skill", "skills"]);
}

#[test]
fn formats_skill_with_additional_instructions() {
    let skill = Skill {
        name: "inspect".into(),
        description: "Inspect things".into(),
        content: "Use inspection tools.".into(),
        file_path: "/project/.pi/skills/inspect/SKILL.md".into(),
        disable_model_invocation: None,
    };
    assert_eq!(
        format_skill_invocation(&skill, Some("Check errors.")),
        "<skill name=\"inspect\" location=\"/project/.pi/skills/inspect/SKILL.md\">\nReferences are relative to /project/.pi/skills/inspect.\n\nUse inspection tools.\n</skill>\n\nCheck errors."
    );
}
