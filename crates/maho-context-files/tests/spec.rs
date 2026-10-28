//! Tests written from docs/cleanroom/specs/init-deep.md.

use maho_context_files::cli;
use maho_context_files::generate::{Outcome, Runner};
use maho_context_files::prompt::{clean_answer, file_prompt, session_request, summary};
use maho_context_files::{Action, Settings, advisor, analyze, choose, generate, plan};
use std::path::{Path, PathBuf};

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    std::fs::write(path, text).expect("write");
}

fn many(root: &Path, dir: &str, ext: &str, n: usize) {
    for i in 0..n {
        write(root, &format!("{dir}/f{i}.{ext}"), "x");
    }
}

/// root: Cargo.toml + README; crates/core (manifest, 25 rs, tests/); crates/tiny (manifest, 1 rs);
/// web/src/app (30 ts below a pass-through `web`), docs (md only), ignored/ (gitignored, 40 rs),
/// node_modules (40 js), .hidden (40 rs).
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path();
    std::fs::create_dir_all(root.join(".git")).expect("git");
    write(root, ".gitignore", "ignored/\n");
    write(root, "Cargo.toml", "[workspace]\n");
    write(root, "README.md", "# demo\n");
    write(root, "crates/core/Cargo.toml", "[package]\n");
    write(root, "crates/core/src/lib.rs", "x");
    many(root, "crates/core/src", "rs", 24);
    write(root, "crates/core/tests/it.rs", "x");
    write(root, "crates/tiny/Cargo.toml", "[package]\n");
    write(root, "crates/tiny/src/main.rs", "x");
    many(root, "web/src/app", "ts", 30);
    write(root, "web/src/app/index.ts", "x");
    write(root, "docs/guide.md", "x");
    many(root, "ignored", "rs", 40);
    many(root, "node_modules/pkg", "js", 40);
    many(root, ".hidden", "rs", 40);
    dir
}

#[test]
fn analysis_honours_ignores_and_records_facts() {
    let dir = fixture();
    let a = analyze(dir.path());
    assert!(!a.dirs.contains_key("ignored"));
    assert!(!a.dirs.contains_key("node_modules"));
    assert!(!a.dirs.contains_key(".hidden"));
    let root = &a.dirs["."];
    assert_eq!(root.subtree_code_files, 25 + 1 + 1 + 31);
    assert_eq!(root.manifests, ["Cargo.toml"]);
    let core = &a.dirs["crates/core"];
    assert!(core.has_tests);
    assert_eq!(core.depth, 2);
    assert_eq!(a.dirs["crates/core/src"].entry_points, ["lib.rs"]);
    assert_eq!(a.dirs["web/src/app"].entry_points, ["index.ts"]);
    assert_eq!(root.top_languages(), ["TypeScript", "Rust"]);
}

#[test]
fn choice_follows_manifests_thresholds_and_collapses() {
    let dir = fixture();
    let a = analyze(dir.path());
    let chosen = choose(&a, &Settings::default());
    assert_eq!(chosen[0], ".");
    // crates/core (manifest + tests), crates/tiny (manifest), web/src/app (30 files).
    assert!(chosen.contains(&"crates/core".to_owned()), "{chosen:?}");
    assert!(chosen.contains(&"crates/tiny".to_owned()), "{chosen:?}");
    assert!(chosen.contains(&"web/src/app".to_owned()), "{chosen:?}");
    // `web` is a pass-through, `web/src` collapses into its single child, `crates/core/src`
    // collapses too? no: it has 25 files but its parent has a manifest, both stay eligible.
    assert!(!chosen.contains(&"web".to_owned()), "{chosen:?}");
    assert!(!chosen.contains(&"web/src".to_owned()), "{chosen:?}");
    assert!(!chosen.contains(&"docs".to_owned()));
    let small = Settings {
        max_dirs: 2,
        ..Settings::default()
    };
    let top = choose(&a, &small);
    assert_eq!(top, [".", "crates/core"]);
    let shallow = Settings {
        max_depth: 1,
        ..Settings::default()
    };
    assert_eq!(choose(&a, &shallow), ["."]);
}

#[test]
fn plan_orders_deepest_first_with_actions_and_children() {
    let dir = fixture();
    write(dir.path(), "crates/tiny/AGENTS.md", "# tiny\n\nA tiny binary.\n");
    let a = analyze(dir.path());
    let read = |rel: &str, name: &str| std::fs::read_to_string(dir.path().join(rel).join(name)).ok();
    let p = plan(&a, &Settings::default(), false, &read);
    let depths: Vec<usize> = p.targets.iter().map(|t| t.depth).collect();
    let mut sorted = depths.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(depths, sorted);
    let tiny = p.targets.iter().find(|t| t.path == "crates/tiny").expect("tiny");
    assert_eq!(tiny.action, Action::Keep);
    let root = p.targets.iter().find(|t| t.path == ".").expect("root");
    assert_eq!(root.action, Action::Create);
    assert!(root.children.contains(&"crates/core".to_owned()));
    assert!(root.children.contains(&"web/src/app".to_owned()));
    assert!(!root.children.contains(&"crates/core/src".to_owned()));
    let updated = plan(&a, &Settings::default(), true, &read);
    let tiny = updated.targets.iter().find(|t| t.path == "crates/tiny").expect("tiny");
    assert_eq!(tiny.action, Action::Update);
    assert!(tiny.existing.as_deref().is_some_and(|e| e.contains("A tiny binary")));
    let json = serde_json::to_value(&updated).expect("json");
    assert!(json["targets"][0].get("existing").is_none());
    assert!(json["targets"][0]["action"].is_string());
}

#[test]
fn prompts_carry_facts_children_and_rules() {
    let dir = fixture();
    let a = analyze(dir.path());
    let p = plan(&a, &Settings::default(), false, &|_, _| None);
    let root = p.targets.iter().find(|t| t.path == ".").expect("root");
    let text = file_prompt(root, &[("crates/core".into(), "core — the engine".into())]);
    assert!(text.contains("the root of this repository"));
    assert!(text.contains("Cargo.toml"));
    assert!(text.contains("`crates/core`: core — the engine"));
    assert!(text.contains("about 120 lines"));
    let core = p.targets.iter().find(|t| t.path == "crates/core").expect("core");
    let text = file_prompt(core, &[]);
    assert!(text.contains("`crates/core`"));
    assert!(text.contains("tests: present"));
    assert!(text.contains("Do not repeat"));
    let request = session_request(&p);
    assert!(request.contains("## `crates/core/AGENTS.md` (new file)"));
}

#[test]
fn answers_are_cleaned_and_summarised() {
    assert_eq!(clean_answer("```markdown\n# A\n\nbody\n```\n"), "# A\n\nbody\n");
    assert_eq!(clean_answer("  # A\nbody  "), "# A\nbody\n");
    assert_eq!(clean_answer("```rust\nfn main(){}\n```"), "```rust\nfn main(){}\n```\n");
    assert_eq!(clean_answer("  \n"), "");
    assert_eq!(summary("# Core\n\nThe engine.\nIt runs.\n\n## Next\n"), "Core — The engine. It runs.");
    assert_eq!(summary(&format!("# T\n\n{}", "a".repeat(700))).chars().count(), 600);
}

struct Fake {
    calls: Vec<(PathBuf, String)>,
    fail_in: Option<String>,
}

impl Runner for Fake {
    fn run(&mut self, dir: &Path, prompt: &str) -> Result<String, String> {
        self.calls.push((dir.to_path_buf(), prompt.to_owned()));
        if self.fail_in.as_deref().is_some_and(|f| dir.ends_with(f)) {
            return Err("boom".into());
        }
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(format!("```md\n# {name}\n\nAbout {name}.\n```"))
    }
}

#[test]
fn generation_writes_deepest_first_and_feeds_summaries_up() {
    let dir = fixture();
    let a = analyze(dir.path());
    let p = plan(&a, &Settings::default(), false, &|_, _| None);
    let mut runner = Fake {
        calls: Vec::new(),
        fail_in: Some("tiny".into()),
    };
    let report = generate(&p, &mut runner, false);
    assert_eq!(report.failed(), 1);
    assert!(report.summary_line().ends_with("kept, 1 failed"));
    let core = std::fs::read_to_string(dir.path().join("crates/core/AGENTS.md")).expect("core");
    assert_eq!(core, "# core\n\nAbout core.\n");
    assert!(dir.path().join("AGENTS.md").is_file());
    assert!(!dir.path().join("crates/tiny/AGENTS.md").exists());
    let (last_dir, last_prompt) = runner.calls.last().expect("calls");
    assert_eq!(last_dir, dir.path());
    assert!(last_prompt.contains("`crates/core`: core — About core."));
    // dry run writes nothing
    let fresh = fixture();
    let a = analyze(fresh.path());
    let p = plan(&a, &Settings::default(), false, &|_, _| None);
    let report = generate(&p, &mut Fake { calls: Vec::new(), fail_in: None }, true);
    assert!(report.outcomes.iter().all(|(_, o)| matches!(o, Outcome::Preview(..))));
    assert!(!fresh.path().join("AGENTS.md").exists());
}

#[test]
fn cli_plan_json_and_errors() {
    let dir = fixture();
    let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let mut never = |_model: Option<String>| -> Box<dyn Runner> { panic!("no model run for --plan") };
    let out = cli::run(&args(&["--plan", "--json"]), dir.path(), Settings::default(), &mut never);
    assert_eq!(out.code, 0, "{out:?}");
    let value: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(value["targets"].as_array().map(|t| t.last().map(|l| l["path"].clone())), Some(Some(".".into())));
    let out = cli::run(&args(&["--plan", "--max-dirs", "1"]), &dir.path().join("crates"), Settings::default(), &mut never);
    assert!(out.stdout.contains("create  ."), "{}", out.stdout);
    assert_eq!(out.stdout.lines().count(), 2);
    let out = cli::run(&args(&["--frob"]), dir.path(), Settings::default(), &mut never);
    assert_eq!(out.code, 2);
    let out = cli::run(&args(&["--max-depth", "x"]), dir.path(), Settings::default(), &mut never);
    assert_eq!(out.code, 2);
    let mut fake = |model: Option<String>| -> Box<dyn Runner> {
        assert_eq!(model.as_deref(), Some("p/m"));
        Box::new(Fake { calls: Vec::new(), fail_in: None })
    };
    let out = cli::run(&args(&["--model", "p/m"]), dir.path(), Settings::default(), &mut fake);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(out.stdout.ends_with("written, 0 kept, 0 failed"));
}

#[test]
fn settings_from_config_and_environment() {
    let view = serde_json::json!({"init_deep": {"advise": true, "max_depth": 5, "max_dirs": 0, "min_files": 7}});
    let s = Settings::from_view(Some(&view), &|_| None);
    assert_eq!((s.max_depth, s.max_dirs, s.min_files, s.advise), (5, 1, 7, true));
    let off = Settings::from_view(Some(&view), &|k| (k == "MAHO_CONTEXT_ADVICE").then(|| "0".into()));
    assert!(!off.advise);
}

#[test]
fn advisor_suggests_once_per_root_and_respects_opt_outs() {
    let dir = fixture();
    let home = tempfile::tempdir().expect("home");
    let settings = Settings::default();
    let text = advisor::advise_once(home.path(), dir.path(), &settings).expect("advice");
    assert!(text.contains("/init-deep"));
    assert!(advisor::advise_once(home.path(), dir.path(), &settings).is_none());
    let other = fixture();
    let off = Settings { advise: false, ..Settings::default() };
    assert!(advisor::advise_once(home.path(), other.path(), &off).is_none());
    write(other.path(), "AGENTS.md", "# x\n");
    assert!(advisor::advise_once(home.path(), other.path(), &settings).is_none());
    let tiny = tempfile::tempdir().expect("tiny");
    write(tiny.path(), "a.rs", "x");
    assert!(advisor::advise_once(home.path(), tiny.path(), &settings).is_none());
}
