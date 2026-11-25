//! Port of `test/suite/prompt-presets-generation.test.ts` "matches every forced preset and fallback
//! captured from immutable main": every preset (and the default prompt) on every surface and baseline
//! input renders the bytes recorded in the pinned senpi checkout's
//! `prompt-presets-native-baseline.json`. senpi pins the workstation facts through a module mock; here
//! the rendered fact lines are replaced by the same fixture facts, and the host brand is mapped back to
//! senpi's app name, which upstream's host-identity test shows is the only brand-dependent text.

use maho_core::dynamic_prompt::PromptSurface;
use maho_core::dynamic_prompt::build::BuildDynamicSystemPromptOptions;
use maho_core::skills::Skill;
use maho_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use maho_core::system_prompt::ContextFile;
use maho_ext_prompt_preset::public::{build_dynamic_system_prompt, build_preset};
use maho_ext_prompt_preset::settings::parse_prompt_preset;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const FIXTURE_FACTS: &str = "<workstation>\n- OS: linux (kernel Linux fixture)\n- Arch: x64\n- CPU: Fixture CPU (4 cores)\n- Terminal: fixture-terminal\n</workstation>";

fn baseline_input(
    index: usize,
    surface: PromptSurface,
) -> BuildDynamicSystemPromptOptions<'static> {
    let mut options = BuildDynamicSystemPromptOptions {
        custom_prompt: None,
        append_system_prompt: None,
        on_parts: None,
        cwd: String::new(),
        selected_tools: Vec::new(),
        tool_snippets: HashMap::new(),
        prompt_guidelines: Vec::new(),
        full_tool_prompt: None,
        shared_sections: Default::default(),
        native_search_available: false,
        context_files: Vec::new(),
        skills: Vec::new(),
        tuning_section: None,
        core_prompt: None,
        workstation_dialect: None,
        surface,
    };
    if index == 1 {
        options.cwd = "/fixture".into();
        options.selected_tools = [
            "read",
            "bash",
            "grep",
            "eval",
            "apply_patch",
            "todo",
            "ask_user_question",
            "web_search",
        ]
        .map(String::from)
        .to_vec();
        options.tool_snippets = HashMap::from([
            ("read".to_owned(), "READ_SENTINEL".to_owned()),
            ("eval".to_owned(), "EVAL_SENTINEL".to_owned()),
        ]);
        options.prompt_guidelines = vec!["GUIDELINE_SENTINEL".into()];
        options.context_files = vec![ContextFile {
            path: "/fixture/AGENTS.md".into(),
            content: "CONTEXT_SENTINEL".into(),
            scope: None,
        }];
        options.skills = vec![Skill {
            name: "fixture-skill".into(),
            description: "A deterministic fixture skill.".into(),
            file_path: "/fixture/skill/SKILL.md".into(),
            base_dir: "/fixture/skill".into(),
            source_info: SourceInfo {
                path: "/fixture/skill".into(),
                source: "fixture".into(),
                scope: SourceScope::Project,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
            },
            disable_model_invocation: false,
            provenance: None,
            allowed_set: None,
        }];
    }
    options
}

/// The prompt as senpi's baseline hashed it: fixture workstation facts, senpi's app name, and senpi's executor
/// phrase where mhc names only `eval` because bash runs inside eval cells (docs/divergences.md `EXEC-EVAL`).
fn as_recorded(prompt: &str) -> String {
    let start = prompt.find("<workstation>\n").expect("workstation block");
    let end = prompt.find("</workstation>").expect("workstation end") + "</workstation>".len();
    let prompt = format!("{}{FIXTURE_FACTS}{}", &prompt[..start], &prompt[end..]);
    prompt
        .replace(&maho_core::config::app_name(), "senpi")
        .replace("`eval` executes", "`bash` and `eval` execute")
}

#[test]
fn every_preset_matches_the_pinned_native_baseline() {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .expect("HOME or USERPROFILE");
    let source = std::env::var("SENPI_SRC").unwrap_or_else(|_| format!("{home}/code/senpi"));
    let path = std::path::Path::new(&source)
        .join("packages/coding-agent/test/suite/prompt-presets-native-baseline.json");
    let baseline: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("pinned native baseline"))
            .expect("baseline json");
    let hashes = baseline["hashes"].as_object().expect("baseline hashes");
    assert_eq!(hashes.len(), 210);
    let mut mismatched = Vec::new();
    for (key, expected) in hashes {
        let mut fields = key.split(':');
        let (name, surface, index) = (
            fields.next().unwrap(),
            fields.next().unwrap(),
            fields.next().unwrap().parse::<usize>().unwrap(),
        );
        let surface = match surface {
            "terminal" => PromptSurface::Terminal,
            "app" => PromptSurface::App,
            "chat" => PromptSurface::Chat,
            other => panic!("unknown surface {other}"),
        };
        let input = baseline_input(index, surface);
        let prompt = if name == "auto" {
            Some(build_dynamic_system_prompt(&input))
        } else {
            parse_prompt_preset(Some(name))
                .and_then(|preset| build_preset(preset, input))
                .map(|preset| preset.prompt)
        };
        let actual = prompt.map(|prompt| hex::encode(Sha256::digest(as_recorded(&prompt))));
        if actual.as_deref() != expected.as_str() {
            mismatched.push(key.clone());
        }
    }
    assert!(
        mismatched.is_empty(),
        "{} of 210 renders differ from the pinned baseline: {mismatched:?}",
        mismatched.len()
    );
}
