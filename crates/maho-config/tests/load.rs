mod common;

use common::Tree;
use maho_config::*;
use serde_json::{Value, json};
use std::path::Path;

fn kinds(result: &LoadResult) -> Vec<DiagnosticKind> {
    result.diagnostics.iter().map(|d| d.kind).collect()
}

// ---- Locations -------------------------------------------------------------------------

#[test]
fn jsonc_wins_over_json_for_the_user_file() {
    let t = Tree::new();
    t.write("home/.omo/omo.jsonc", r#"{"telemetry": {"enabled": true}}"#);
    t.write("home/.omo/omo.json", r#"{"telemetry": {"enabled": false}}"#);
    let r = t.load("home");
    assert_eq!(r.sources.len(), 1);
    assert!(r.sources[0].path.ends_with(".omo/omo.jsonc"));
    assert!(r.sources[0].loaded);
    assert_eq!(r.view["telemetry"]["enabled"], json!(true));
}

#[test]
fn json_user_file_is_used_alone() {
    let t = Tree::new();
    t.write("home/.omo/omo.json", r#"{"telemetry": {"enabled": true}}"#);
    let r = t.load("home");
    assert!(r.sources[0].path.ends_with(".omo/omo.json"));
    assert!(r.sources[0].loaded);
    assert_eq!(r.view["telemetry"]["enabled"], json!(true));
}

#[test]
fn project_files_are_ordered_farthest_first_and_nearest_wins() {
    let t = Tree::new();
    t.write(
        "home/a/.omo/omo.jsonc",
        r#"{"model_profile": "far", "disabled_skills": ["x"]}"#,
    );
    t.write("home/a/b/c/.omo/omo.jsonc", r#"{"model_profile": "near"}"#);
    let r = t.load("home/a/b/c");
    let scopes: Vec<Scope> = r.sources.iter().map(|s| s.scope).collect();
    assert_eq!(scopes, vec![Scope::User, Scope::Project, Scope::Project]);
    assert!(r.sources[1].path.starts_with(t.root.join("home/a/.omo")));
    assert!(
        r.sources[2]
            .path
            .starts_with(t.root.join("home/a/b/c/.omo"))
    );
    assert_eq!(r.view["model_profile"], json!("near"));
    assert_eq!(r.view["disabled_skills"], json!(["x"]));
}

#[test]
fn cwd_outside_home_walks_to_the_root() {
    let t = Tree::new();
    t.write("out/.omo/omo.jsonc", r#"{"model_profile": "outside"}"#);
    let r = t.load("out/x/y");
    assert_eq!(r.view["model_profile"], json!("outside"));
}

#[test]
fn home_is_a_boundary() {
    let t = Tree::new();
    t.write("home/.omo/omo.jsonc", "{}");
    let r = t.load("home");
    assert!(r.sources.iter().all(|s| s.scope == Scope::User));
}

#[test]
fn account_home_is_a_boundary_when_home_is_overridden() {
    let t = Tree::new();
    t.write("acct/.omo/omo.jsonc", r#"{"model_profile": "acct"}"#);
    t.write("acct/p/.omo/omo.jsonc", r#"{"disabled_skills": ["p"]}"#);
    let cwd = t.mkdir("acct/p/q");
    let r = load(
        &LoadOptions::new(&cwd)
            .with_env(t.env())
            .with_account_home(Some(t.root.join("acct"))),
    );
    assert!(r.view.get("model_profile").is_none());
    assert_eq!(r.view["disabled_skills"], json!(["p"]));
    assert!(
        !r.sources
            .iter()
            .any(|s| s.path.starts_with(t.root.join("acct/.omo")))
    );
}

#[cfg(unix)]
#[test]
fn symlinked_project_omo_dir_contributes_nothing() {
    let t = Tree::new();
    t.write("elsewhere/omo.jsonc", r#"{"model_profile": "linked"}"#);
    t.mkdir("home/p");
    std::os::unix::fs::symlink(t.root.join("elsewhere"), t.root.join("home/p/.omo"))
        .expect("symlink");
    let r = t.load("home/p");
    assert!(r.view.get("model_profile").is_none());
}

#[cfg(unix)]
#[test]
fn symlinked_project_file_is_skipped() {
    let t = Tree::new();
    t.write("elsewhere/real.jsonc", r#"{"model_profile": "linked"}"#);
    t.mkdir("home/p/.omo");
    std::os::unix::fs::symlink(
        t.root.join("elsewhere/real.jsonc"),
        t.root.join("home/p/.omo/omo.jsonc"),
    )
    .expect("symlink");
    let r = t.load("home/p");
    assert!(r.view.get("model_profile").is_none());
}

#[test]
fn walk_stops_after_256_directories() {
    let t = Tree::new();
    t.write("deep/.omo/omo.jsonc", r#"{"model_profile": "top"}"#);
    let mut rel = String::from("deep");
    for _ in 0..260 {
        rel.push_str("/d");
    }
    let r = t.load(&rel);
    assert!(r.view.get("model_profile").is_none());
    let shallow = t.load("deep/d/d");
    assert_eq!(shallow.view["model_profile"], json!("top"));
}

#[test]
fn xdg_config_home_is_never_read() {
    let t = Tree::new();
    t.write("xdg/omo.jsonc", r#"{"model_profile": "xdg"}"#);
    t.write("xdg/.omo/omo.jsonc", r#"{"model_profile": "xdg"}"#);
    let cwd = t.mkdir("home");
    let mut env = t.env();
    env.insert(
        "XDG_CONFIG_HOME".into(),
        t.root.join("xdg").to_string_lossy().into_owned(),
    );
    let r = t.load_at(&cwd, env);
    assert!(r.view.get("model_profile").is_none());
}

// ---- Format and validation ---------------------------------------------------------------

#[test]
fn comments_and_trailing_commas_load() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        "{ // c\n \"agents\": {\"a\": {\"model\": \"x/y\",}}, }",
    );
    let r = t.load("home");
    assert!(r.sources[0].loaded);
    assert_eq!(r.view["agents"]["a"]["model"], json!("x/y"));
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
}

#[test]
fn truncated_file_is_one_parse_diagnostic() {
    let t = Tree::new();
    t.write("home/.omo/omo.jsonc", r#"{"agents":"#);
    let r = t.load("home");
    assert!(!r.sources[0].loaded);
    assert_eq!(kinds(&r), vec![DiagnosticKind::Parse]);
}

#[test]
fn array_root_is_rejected() {
    let t = Tree::new();
    t.write("home/.omo/omo.jsonc", "[]");
    let r = t.load("home");
    assert!(!r.sources[0].loaded);
    assert_eq!(kinds(&r), vec![DiagnosticKind::Validation]);
    assert!(r.diagnostics[0].message.contains("root must be an object"));
}

#[test]
fn empty_file_is_not_an_object() {
    let t = Tree::new();
    t.write("home/.omo/omo.jsonc", "  \n");
    let r = t.load("home");
    assert!(!r.sources[0].loaded);
    assert_eq!(kinds(&r), vec![DiagnosticKind::Validation]);
}

#[test]
fn unknown_key_is_removed_and_reported() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"bogus": 1, "categories": {"quick": {"model": "a/b"}}}"#,
    );
    let r = t.load("home");
    assert!(r.sources[0].loaded);
    assert_eq!(kinds(&r), vec![DiagnosticKind::UnknownKeys]);
    assert_eq!(r.diagnostics[0].issues, vec!["bogus".to_string()]);
    assert!(r.view.get("bogus").is_none());
    assert_eq!(r.view["categories"]["quick"]["model"], json!("a/b"));
}

#[test]
fn invalid_value_prunes_only_its_subtree() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"agents": {"x": {"model": 5, "description": "d"}, "y": {"model": "a/b"}}}"#,
    );
    let r = t.load("home");
    assert_eq!(kinds(&r), vec![DiagnosticKind::InvalidValue]);
    assert_eq!(r.diagnostics[0].issues, vec!["agents.x.model".to_string()]);
    assert_eq!(r.view["agents"]["x"], json!({"description": "d"}));
    assert_eq!(r.view["agents"]["y"]["model"], json!("a/b"));
}

#[test]
fn file_with_only_invalid_values_is_rejected_but_user_file_applies() {
    let t = Tree::new();
    t.write("home/.omo/omo.jsonc", r#"{"telemetry": {"enabled": true}}"#);
    t.write(
        "home/p/.omo/omo.jsonc",
        r#"{"telemetry": {"enabled": "yes"}, "model_profile": 3}"#,
    );
    let r = t.load("home/p");
    assert!(!r.sources[1].loaded);
    assert_eq!(kinds(&r), vec![DiagnosticKind::Validation]);
    assert_eq!(r.view["telemetry"]["enabled"], json!(true));
}

#[test]
fn unsafe_keys_are_removed() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"__proto__": {"a": 1}, "models": {"m": {"model": "a/b"}}}"#,
    );
    let r = t.load("home");
    assert_eq!(kinds(&r), vec![DiagnosticKind::UnknownKeys]);
    assert_eq!(r.diagnostics[0].issues, vec!["__proto__".to_string()]);
    assert_eq!(r.view["models"]["m"]["model"], json!("a/b"));
}

#[test]
fn legacy_variant_folds_into_reasoning() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"categories": {"quick": {"model": "a/b", "variant": "high"}}}"#,
    );
    let r = t.load("home");
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert_eq!(
        r.view["categories"]["quick"],
        json!({"model": "a/b", "reasoning": "high"})
    );
}

#[test]
fn legacy_fields_fold_on_every_model_bearing_object() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"categories": {"q": {
            "models": [{"model": "a/b", "reasoningEffort": "low", "maxTokens": 10,
                        "thinking": {"type": "enabled", "budgetTokens": 5}, "textVerbosity": "low"}],
            "thinking": {"type": "disabled"}
        }}}"#,
    );
    let r = t.load("home");
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    let q = &r.view["categories"]["q"];
    assert_eq!(q["reasoning"], json!("off"));
    assert!(q.get("thinking").is_none());
    let entry = &q["models"][0];
    assert_eq!(entry["reasoning"], json!("low"));
    assert_eq!(entry["max_tokens"], json!(10));
    assert_eq!(
        entry["provider_options"]["thinking"]["budgetTokens"],
        json!(5)
    );
    assert_eq!(entry["provider_options"]["textVerbosity"], json!("low"));
}

#[test]
fn spaced_reasoning_token_is_canonicalised() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"agents": {"a": {"model": "openai/gpt-x (high)"}}}"#,
    );
    let r = t.load("home");
    assert_eq!(r.view["agents"]["a"]["model"], json!("openai/gpt-x:high"));
}

#[test]
fn max_without_provider_is_unchanged() {
    assert_eq!(canonical_model_string("gpt-x:max"), "gpt-x:max");
    assert_eq!(canonical_model_string("a/gpt-x (max)"), "a/gpt-x:max");
    assert_eq!(canonical_model_string("gpt-x (max)"), "gpt-x (max)");
    assert_eq!(canonical_model_string("a/b none"), "a/b:off");
    assert_eq!(canonical_model_string("a/b turbo"), "a/b turbo");
    assert_eq!(canonical_model_string("a/b:HIGH"), "a/b:high");
}

// ---- Legacy names --------------------------------------------------------------------------

#[test]
fn legacy_category_is_renamed() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"categories": {"deep": {"model": "a/b"}}}"#,
    );
    let r = t.load("home");
    assert_eq!(kinds(&r), vec![DiagnosticKind::DeprecatedKeys]);
    assert_eq!(r.view["categories"]["deep-low"]["model"], json!("a/b"));
    assert!(r.view["categories"].get("deep").is_none());

    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"categories": {"deep": {"model": "a/b"}, "deep-low": {"model": "c/d"}}}"#,
    );
    let r = t.load("home");
    assert_eq!(r.view["categories"]["deep-low"]["model"], json!("c/d"));
    assert!(r.diagnostics[0].message.contains("ignored"));
}

#[test]
fn legacy_harness_block_applies_and_native_wins() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"[senpi]": {"categories": {"q": {"model": "a/b"}}}}"#,
    );
    let r = t.load("home");
    assert_eq!(r.view["categories"]["q"]["model"], json!("a/b"));

    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"[senpi]": {"model_profile": "old"}, "[native]": {"model_profile": "new"}}"#,
    );
    let r = t.load("home");
    assert_eq!(r.view["model_profile"], json!("new"));
}

// ---- Merge / view / profiles -----------------------------------------------------------------

#[test]
fn disabled_skills_replace_in_view_but_union_for_the_denylist() {
    let t = Tree::new();
    t.write("home/.omo/omo.jsonc", r#"{"disabled_skills": ["a"]}"#);
    t.write(
        "home/p/.omo/omo.jsonc",
        r#"{"disabled_skills": ["b", " a ", ""]}"#,
    );
    let r = t.load("home/p");
    assert_eq!(r.view["disabled_skills"], json!(["b", " a ", ""]));
    assert_eq!(
        collect_disabled_skills(&r.layers, &Harness::Native, None),
        vec!["a", "b"]
    );
}

#[test]
fn disabled_skills_union_reads_harness_and_profile_scopes() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"[native]": {"disabled_skills": ["n"]}, "[codex]": {"disabled_skills": ["c"]},
            "profiles": {"w": {"disabled_skills": ["pw"], "[native]": {"disabled_skills": ["pn"]}}}}"#,
    );
    let r = t.load("home");
    assert_eq!(
        collect_disabled_skills(&r.layers, &Harness::parse("senpi"), Some("w")),
        vec!["n", "pw", "pn"]
    );
}

#[test]
fn env_profile_applies() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"profiles": {"fast": {"categories": {"quick": {"model": "p/m2"}}}}}"#,
    );
    let cwd = t.mkdir("home");
    let mut env = t.env();
    env.insert("OMO_PROFILE".into(), "fast".into());
    let r = t.load_at(&cwd, env);
    assert_eq!(r.view["categories"]["quick"]["model"], json!("p/m2"));
    assert_eq!(r.profile.as_deref(), Some("fast"));
}

#[test]
fn profile_name_precedence() {
    let mut env = Env::new();
    env.insert("OMO_PROFILE".into(), "fast".into());
    assert_eq!(
        resolve_profile_name(&env, Some("slow")).as_deref(),
        Some("slow")
    );
    assert_eq!(resolve_profile_name(&env, None).as_deref(), Some("fast"));
    let mut env = Env::new();
    env.insert("OCX_PROFILE".into(), "ocx".into());
    env.insert("OPENCODE_CONFIG_DIR".into(), "/x/profiles/work".into());
    assert_eq!(resolve_profile_name(&env, None).as_deref(), Some("ocx"));
    let mut env = Env::new();
    env.insert("OPENCODE_CONFIG_DIR".into(), "/x/profiles/work/".into());
    assert_eq!(resolve_profile_name(&env, None).as_deref(), Some("work"));
    env.insert("OPENCODE_CONFIG_DIR".into(), "C:\\x\\profiles\\win".into());
    assert_eq!(resolve_profile_name(&env, None).as_deref(), Some("win"));
    env.insert("OPENCODE_CONFIG_DIR".into(), "profiles/rel".into());
    assert_eq!(resolve_profile_name(&env, None).as_deref(), Some("rel"));
    env.insert("OPENCODE_CONFIG_DIR".into(), "/x/profiles/a/b".into());
    assert_eq!(resolve_profile_name(&env, None), None);
}

#[test]
fn missing_profile_is_reported_and_base_used() {
    let t = Tree::new();
    t.write("home/.omo/omo.jsonc", r#"{"model_profile": "base"}"#);
    let cwd = t.mkdir("home");
    let mut env = t.env();
    env.insert("OMO_PROFILE".into(), "missing".into());
    let r = t.load_at(&cwd, env);
    assert_eq!(kinds(&r), vec![DiagnosticKind::Profile]);
    assert_eq!(
        r.diagnostics[0].target,
        DiagnosticTarget::Config("profiles.missing".into())
    );
    assert_eq!(r.profile, None);
    assert_eq!(r.view["model_profile"], json!("base"));
}

#[test]
fn profile_harness_block_has_highest_precedence() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"telemetry": {"enabled": false}, "[native]": {"telemetry": {"enabled": true}},
            "profiles": {"p": {"telemetry": {"enabled": false}, "[native]": {"telemetry": {"enabled": true}}}}}"#,
    );
    let cwd = t.mkdir("home");
    let r = load(
        &LoadOptions::new(&cwd)
            .with_env(t.env())
            .with_account_home(None)
            .with_profile(Some("p".into())),
    );
    assert_eq!(r.view["telemetry"]["enabled"], json!(true));
    let t2 = Tree::new();
    t2.write(
        "home/.omo/omo.jsonc",
        r#"{"[native]": {"telemetry": {"enabled": true}}, "profiles": {"p": {"telemetry": {"enabled": false}}}}"#,
    );
    let cwd = t2.mkdir("home");
    let r = load(
        &LoadOptions::new(&cwd)
            .with_env(t2.env())
            .with_account_home(None)
            .with_profile(Some("p".into())),
    );
    assert_eq!(r.view["telemetry"]["enabled"], json!(false));
}

#[test]
fn no_file_gives_the_base_defaults() {
    let t = Tree::new();
    let r = t.load("home");
    assert!(r.diagnostics.is_empty());
    assert_eq!(r.view["agents"], json!({}));
    assert_eq!(r.view["categories"], json!({}));
    assert_eq!(r.view["teams"], json!({}));
    let task = &r.view["task"];
    assert_eq!(task["max_depth"], json!(1));
    assert_eq!(task["default_concurrency"], json!(5));
    assert_eq!(task["isolation"]["backend"], json!("auto"));
    assert_eq!(task["wait"]["max_ms"], json!(600_000));
    assert_eq!(task["team"]["max_parallel_members"], json!(4));
    assert_eq!(task["residency_max_children"], json!("unlimited"));
    assert!(r.view.get("profiles").is_none());
    assert!(
        r.view
            .as_object()
            .expect("object")
            .keys()
            .all(|k| !k.starts_with('['))
    );
}

#[test]
fn merged_team_that_stays_invalid_is_dropped() {
    let t = Tree::new();
    t.write(
        "home/a/.omo/omo.jsonc",
        r#"{"teams": {"t": {"description": "half"}}, "model_profile": "x"}"#,
    );
    t.write(
        "home/a/b/.omo/omo.jsonc",
        r#"{"teams": {"t": {"leadAgentId": "lead"}}}"#,
    );
    let r = t.load("home/a/b");
    assert!(r.view["teams"].get("t").is_none());
    let merged: Vec<&Diagnostic> = r
        .diagnostics
        .iter()
        .filter(|d| d.target == DiagnosticTarget::Merged)
        .collect();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].kind, DiagnosticKind::InvalidValue);
    let lines = doctor_lines(&r.diagnostics, &r.home);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("merged config") && l.contains("teams.t")),
        "{lines:?}"
    );
    assert_eq!(r.view["model_profile"], json!("x"));
}

// ---- Catalog ---------------------------------------------------------------------------------

fn catalog(view: Value) -> (Value, Vec<Diagnostic>) {
    resolve_catalog_references(&view)
}

#[test]
fn catalog_alias_with_reasoning_becomes_an_object_entry() {
    let (view, diagnostics) = catalog(json!({
        "models": {"fast": {"model": "p/m", "reasoning": "high"}},
        "agents": {"w": {"models": ["fast", "other/x"], "model": "fast", "reasoning": "low"}},
        "categories": {"c": {"fallback_models": "fast"}, "d": {"model": "fast"}}
    }));
    assert!(diagnostics.is_empty());
    assert_eq!(
        view["agents"]["w"]["models"],
        json!([{"model": "p/m", "reasoning": "high"}, "other/x"])
    );
    assert_eq!(view["agents"]["w"]["model"], json!("p/m"));
    assert_eq!(view["agents"]["w"]["reasoning"], json!("low"));
    assert_eq!(
        view["categories"]["c"]["fallback_models"],
        json!([{"model": "p/m", "reasoning": "high"}])
    );
    assert_eq!(
        view["categories"]["d"],
        json!({"model": "p/m", "reasoning": "high"})
    );
}

#[test]
fn catalog_cycles_are_reported_and_left_unresolved() {
    let (view, diagnostics) = catalog(json!({
        "models": {"b": {"model": "a"}, "a": {"model": "b"}, "s": {"model": "s"}},
        "agents": {"w": {"model": "a"}}
    }));
    let paths: Vec<&DiagnosticTarget> = diagnostics.iter().map(|d| &d.target).collect();
    assert_eq!(
        paths,
        vec![
            &DiagnosticTarget::Config("models.a.model".into()),
            &DiagnosticTarget::Config("models.b.model".into()),
            &DiagnosticTarget::Config("models.s.model".into()),
        ]
    );
    assert!(
        diagnostics
            .iter()
            .all(|d| d.kind == DiagnosticKind::CatalogCycle)
    );
    assert_ne!(
        diagnostics[0].message,
        diagnostics[2]
            .message
            .replace("\"s\"", "\"a\"")
            .replace(".s.", ".a.")
    );
    assert_eq!(view["agents"]["w"]["model"], json!("a"));
}

#[test]
fn catalog_lookup_is_one_hop() {
    let (view, _) = catalog(json!({
        "models": {"x": {"model": "models-y"}, "models-y": {"model": "p/q"}},
        "agents": {"w": {"model": "x"}}
    }));
    assert_eq!(view["agents"]["w"]["model"], json!("models-y"));
}

#[test]
fn catalog_shadowing_and_profile_splices_are_reported_in_order() {
    let (view, diagnostics) = catalog(json!({
        "models": {"mine": {"model": "p/q"}},
        "model_profiles": {"mine": {"models": ["a/b"]}, "team": {}},
        "categories": {"c": {"models": ["team"]}},
        "agents": {"w": {"models": ["daily-normal"]}}
    }));
    let targets: Vec<String> = diagnostics
        .iter()
        .map(|d| match &d.target {
            DiagnosticTarget::Config(p) => p.clone(),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        targets,
        vec![
            "model_profiles.mine",
            "categories.c.models.0",
            "agents.w.models.0"
        ]
    );
    assert_eq!(view["categories"]["c"]["models"], json!(["team"]));
}

// ---- Doctor lines ------------------------------------------------------------------------------

#[test]
fn doctor_line_for_invalid_value() {
    let t = Tree::new();
    t.write("home/.omo/omo.jsonc", r#"{"task": {"max_depth": -1}}"#);
    let r = t.load("home");
    let lines = doctor_lines(&r.diagnostics, &r.home);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("~/.omo/omo.jsonc"));
    assert!(lines[0].contains("task.max_depth"));
    assert!(lines[0].contains("invalid value"));
    assert!(lines[0].starts_with("config:"));
}

#[test]
fn doctor_lines_for_unloaded_files() {
    let t = Tree::new();
    t.write("home/.omo/omo.jsonc", "{");
    let r = t.load("home");
    let lines = doctor_lines(&r.diagnostics, &r.home);
    assert!(lines[0].contains("not loaded") && lines[0].contains("JSONC parse error"));
}

#[test]
fn windows_paths_display_under_home() {
    assert_eq!(
        display_path(
            Path::new("c:\\Users\\me\\.omo\\omo.jsonc"),
            Path::new("C:/Users/Me")
        ),
        "~/.omo/omo.jsonc"
    );
    assert_eq!(display_path(Path::new("/h/u"), Path::new("/h/u")), "~");
    assert_eq!(
        display_path(Path::new("/h/user2/x"), Path::new("/h/user")),
        "/h/user2/x"
    );
}

#[test]
fn load_native_resolves_the_catalog() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"models": {"fast": {"model": "p/m"}}, "categories": {"q": {"model": "fast"}}}"#,
    );
    let cwd = t.mkdir("home");
    let r = load_native(&cwd, &t.env());
    assert_eq!(r.view["categories"]["q"]["model"], json!("p/m"));
}

#[test]
fn mcp_section_is_accepted_with_its_default() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"mcp": {"disabled_builtins": ["grep_app"]}}"#,
    );
    let cwd = t.mkdir("home");
    let r = load(
        &LoadOptions::new(&cwd)
            .with_env(t.env())
            .with_account_home(None),
    );
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert_eq!(r.view["mcp"]["builtins"], json!(true));
    assert_eq!(r.view["mcp"]["disabled_builtins"], json!(["grep_app"]));
    let t2 = Tree::new();
    t2.write("home/.omo/omo.jsonc", r#"{"mcp": {"builtins": "no"}}"#);
    let cwd = t2.mkdir("home");
    let r = load(
        &LoadOptions::new(&cwd)
            .with_env(t2.env())
            .with_account_home(None),
    );
    assert!(!r.diagnostics.is_empty());
}

#[test]
fn disabled_mcp_builtins_are_a_union_across_layers() {
    let t = Tree::new();
    t.write(
        "home/.omo/omo.jsonc",
        r#"{"mcp": {"disabled_builtins": ["context7"]}}"#,
    );
    t.write(
        "home/proj/.omo/omo.jsonc",
        r#"{"mcp": {"disabled_builtins": [" grep_app ", "context7", 3]}}"#,
    );
    let cwd = t.mkdir("home/proj");
    let r = load(
        &LoadOptions::new(&cwd)
            .with_env(t.env())
            .with_account_home(None),
    );
    assert_eq!(
        collect_disabled_mcp_builtins(&r.layers, &Harness::Native, None),
        vec!["context7".to_owned(), "grep_app".to_owned()]
    );
}
