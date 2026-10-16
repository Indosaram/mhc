use maho_codemode::kernels::session_env::*;
use maho_codemode::kernels::shared::runtime_asset::*;
use std::path::Path;

#[test]
fn resolves_every_session_variable() {
    let result = session_environment_from(&SessionEnvironmentSource {
        cwd: "/w", goal_store_file: Some("/g/x.json"), session_id: "session-77",
        session_file: Some("/tmp/sessions/session-77.jsonl"), model: Some(("fake-provider", "fake-model")), thinking_level: Some("high"),
    });
    assert_eq!(result, SessionEnvironment::from([
        ("PI_SESSION_ID".into(), "session-77".into()), ("PI_SESSION_CWD".into(), "/w".into()),
        ("PI_GOAL_STORE_FILE".into(), "/g/x.json".into()), ("PI_SESSION_FILE".into(), "/tmp/sessions/session-77.jsonl".into()),
        ("PI_PROVIDER".into(), "fake-provider".into()), ("PI_MODEL".into(), "fake-model".into()), ("PI_REASONING_LEVEL".into(), "high".into()),
    ]));
}

#[test]
fn omits_optional_session_variables() {
    let result = session_environment_from(&SessionEnvironmentSource {
        cwd: "/w", goal_store_file: None, session_id: "ephemeral-1", session_file: None, model: None, thinking_level: None,
    });
    assert_eq!(result, SessionEnvironment::from([("PI_SESSION_ID".into(), "ephemeral-1".into()), ("PI_SESSION_CWD".into(), "/w".into())]));
}

#[test]
fn clears_stale_cwd_and_goal() {
    let base = SessionEnvironment::from([("PI_SESSION_CWD".into(), "stale".into()), ("PI_GOAL_STORE_FILE".into(), "stale".into())]);
    assert!(apply_session_environment(&base, Some(&SessionEnvironment::new())).is_empty());
}

#[test]
fn replaces_inherited_values_without_mutating_base() {
    let mut base = SessionEnvironment::new();
    for key in SESSION_ENVIRONMENT_KEYS { base.insert(key.into(), "stale".into()); }
    base.insert("PATH".into(), "/usr/bin".into());
    let session = SessionEnvironment::from([("PI_SESSION_ID".into(), "session-77".into())]);
    let applied = apply_session_environment(&base, Some(&session));
    assert_eq!(applied, SessionEnvironment::from([("PATH".into(), "/usr/bin".into()), ("PI_SESSION_ID".into(), "session-77".into())]));
    assert_eq!(base.get("PI_SESSION_ID").unwrap(), "stale");
}

#[test]
fn empty_optional_strings_omitted() {
    let result = session_environment_from(&SessionEnvironmentSource { cwd: "/w", session_id: "s", goal_store_file: Some(""), session_file: Some(""), model: None, thinking_level: Some("") });
    assert_eq!(result.len(), 2);
}

#[test]
fn extracts_live_native_tool_context_session() {
    struct Context;
    impl maho_ext_api::ToolSessionManager for Context {
        fn session_id(&self) -> &str { "native-session" }
        fn session_file(&self) -> Option<&Path> { Some(Path::new("/sessions/native.jsonl")) }
    }
    impl maho_ext_api::ToolContext for Context {
        fn cwd(&self) -> &Path { Path::new("/workspace") }
        fn model(&self) -> Option<&maho_ext_api::Model> { None }
        fn thinking_level(&self) -> Option<maho_ext_api::ThinkingLevel> { Some(maho_ext_api::ThinkingLevel::Xhigh) }
        fn session_manager(&self) -> &dyn maho_ext_api::ToolSessionManager { self }
        fn goal_store_file(&self) -> Option<&Path> { Some(Path::new("/goals/native.json")) }
    }
    assert_eq!(session_environment_from_context(&Context), SessionEnvironment::from([
        ("PI_SESSION_ID".into(),"native-session".into()),
        ("PI_SESSION_CWD".into(),"/workspace".into()),
        ("PI_SESSION_FILE".into(),"/sessions/native.jsonl".into()),
        ("PI_GOAL_STORE_FILE".into(),"/goals/native.json".into()),
        ("PI_REASONING_LEVEL".into(),"xhigh".into()),
    ]));
}

#[test]
fn every_interpreter_resolves_its_embedded_asset_without_the_build_tree() {
    let absent = Path::new("/absent/override");
    for relative in ["kernels/rb/runner.rb", "kernels/jl/runner.jl", "kernels/py/prelude.py", "kernels/js/worker-entry.js", "kernels/js/inline-worker-entry.js"] {
        let resolved = require_codemode_runtime_asset(Some(absent), Path::new(relative)).unwrap();
        assert!(resolved.ends_with(relative), "{relative} resolved to {}", resolved.display());
        assert_eq!(std::fs::read(&resolved).unwrap(), std::fs::read(build_tree_runtime_asset_path(Path::new(relative))).unwrap());
        let wrapper = match relative {
            "kernels/rb/runner.rb" => maho_codemode::kernels::rb::kernel::resolve_ruby_runner_path_with(Some(absent)),
            "kernels/jl/runner.jl" => maho_codemode::kernels::jl::kernel::resolve_julia_runner_path_with(Some(absent)),
            "kernels/py/prelude.py" => maho_codemode::kernels::py::transport::resolve_python_prelude_path(maho_codemode::kernels::py::transport::PythonPreludePathOptions { local_path: Some(absent) }),
            "kernels/js/worker-entry.js" => maho_codemode::kernels::js::worker_startup::resolve_js_worker_entry_path(Some(absent)),
            "kernels/js/inline-worker-entry.js" => maho_codemode::kernels::js::inline_worker::resolve_inline_worker_entry_path(Some(absent)),
            _ => unreachable!("fixture runner list"),
        };
        assert_eq!(wrapper.unwrap(), resolved);
    }
}

#[test]
fn existing_override_precedes_embedded_asset() {
    let root = tempfile::tempdir().unwrap();
    let local = root.path().join("runner.rb");
    std::fs::write(&local, "runner").unwrap();
    assert_eq!(require_codemode_runtime_asset(Some(&local), Path::new("kernels/rb/runner.rb")).unwrap(), local);
    assert_eq!(maho_codemode::kernels::rb::kernel::resolve_ruby_runner_path_with(Some(&local)).unwrap(), local);
}

#[test]
fn missing_required_asset_returns_paths() {
    // Embedded kernel scripts back every shipped path, so only an unknown asset can be missing.
    let error = require_codemode_runtime_asset(Some(Path::new("/absent/runner")), Path::new("kernels/rb/absent.rb")).unwrap_err();
    assert_eq!(error.local_path, Path::new("/absent/runner"));
    assert_eq!(error.package_relative_path, Path::new("kernels/rb/absent.rb"));
    assert!(!error.to_string().contains("node_modules"), "{error}");
}
