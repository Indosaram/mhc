use std::collections::BTreeSet;

use maho_cli::cli::extension_registry::{
    BUILTIN_EXTENSION_IDS, GLOBAL_DEFAULT_EXTENSION_IDS, INLINE_EXTENSION_IDS, assemble_extensions,
    builtin_extensions, deferred_builtin_extensions, deferred_user_extensions, inline_extensions,
    installed_user_extension_ids, load_native_extensions, native_extension_factories,
    native_extension_factories_for, user_extensions,
};

#[test]
fn pinned_builtin_order_matches_upstream() {
    // senpi src/core/extensions/builtin/index.ts includes relocate-session in v5.1.47.
    assert_eq!(BUILTIN_EXTENSION_IDS.len(), 45);
    let unique: BTreeSet<&str> = BUILTIN_EXTENSION_IDS.iter().copied().collect();
    assert_eq!(unique.len(), 45);
    assert_eq!(BUILTIN_EXTENSION_IDS[0], "loop-guard");
    assert_eq!(BUILTIN_EXTENSION_IDS[44], "mcp");
    assert_eq!(
        GLOBAL_DEFAULT_EXTENSION_IDS,
        ["diff", "files", "prompt-url-widget", "tps"]
    );
    assert_eq!(INLINE_EXTENSION_IDS, ["llama.cpp"]);
}

#[test]
fn registered_builtin_ids_follow_pinned_order() {
    let registered: Vec<&str> = builtin_extensions()
        .iter()
        .map(|extension| extension.id)
        .collect();
    let expected: Vec<&str> = BUILTIN_EXTENSION_IDS
        .iter()
        .copied()
        .filter(|id| registered.contains(id))
        .collect();
    assert_eq!(registered, expected);
}

#[test]
fn search_fallback_runs_before_provider_payload_injection() {
    let search = BUILTIN_EXTENSION_IDS
        .iter()
        .position(|id| *id == "websearch")
        .unwrap();
    for provider in ["anthropic-web-search", "openai-web-search"] {
        assert!(
            search
                < BUILTIN_EXTENSION_IDS
                    .iter()
                    .position(|id| *id == provider)
                    .unwrap()
        );
    }
}

#[test]
fn every_pinned_id_is_linked_or_recorded_as_deferred() {
    let mut covered: Vec<&str> = builtin_extensions()
        .iter()
        .map(|entry| entry.id)
        .chain(deferred_builtin_extensions().iter().map(|entry| entry.id))
        .filter(|id| BUILTIN_EXTENSION_IDS.contains(id))
        .collect();
    covered.sort_unstable();
    let mut pinned: Vec<&str> = BUILTIN_EXTENSION_IDS.to_vec();
    pinned.sort_unstable();
    assert_eq!(covered, pinned);
}

#[test]
fn linked_builtin_ids_keep_the_pinned_relative_order() {
    let positions: Vec<usize> = builtin_extensions()
        .iter()
        .map(|entry| {
            BUILTIN_EXTENSION_IDS
                .iter()
                .position(|id| *id == entry.id)
                .expect("linked id is pinned")
        })
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{positions:?}"
    );
}

#[test]
fn deferred_records_name_a_crate_and_a_requirement() {
    for deferred in deferred_builtin_extensions()
        .iter()
        .chain(deferred_user_extensions().iter())
    {
        assert!(!deferred.crate_name.is_empty(), "{}", deferred.id);
        assert!(!deferred.requirement.is_empty(), "{}", deferred.id);
        assert!(
            !builtin_extensions()
                .iter()
                .any(|entry| entry.id == deferred.id)
                && !user_extensions()
                    .iter()
                    .any(|entry| entry.id == deferred.id),
            "{}",
            deferred.id
        );
    }
}

#[test]
fn linked_factories_register_through_the_loader() {
    let dir = tempfile::tempdir().expect("isolated cwd");
    let result =
        load_native_extensions(dir.path(), maho_ext_api::ExtensionSessionProfile::default());
    let errors: Vec<&str> = result
        .errors
        .iter()
        .map(|error| error.error.as_str())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(result.extensions.len(), native_extension_factories().len());
    let ids: Vec<&str> = result
        .extensions
        .iter()
        .map(|extension| extension.identity.path.as_str())
        .collect();
    for expected in [
        "<builtin:loop-guard>",
        "<builtin:hooks>",
        "<builtin:prompt-preset>",
        "<builtin:terminal>",
        "<builtin:goal>",
        "<builtin:loop>",
        "<builtin:websearch>",
        "<builtin:webfetch>",
        "<inline:llama.cpp>",
    ] {
        assert!(ids.contains(&expected), "{expected} missing from {ids:?}");
    }
}

#[test]
fn inline_llama_factory_registers_the_llama_command() {
    let dir = tempfile::tempdir().expect("isolated cwd");
    let result =
        load_native_extensions(dir.path(), maho_ext_api::ExtensionSessionProfile::default());
    let llama = result
        .extensions
        .iter()
        .find(|extension| extension.identity.path == "<inline:llama.cpp>")
        .expect("llama.cpp inline extension loaded");
    assert_eq!(llama.commands[0].name, "llama");
}

#[test]
fn loop_guard_registers_the_pinned_notice_renderer() {
    let dir = tempfile::tempdir().expect("isolated cwd");
    let result =
        load_native_extensions(dir.path(), maho_ext_api::ExtensionSessionProfile::default());
    let loop_guard = result
        .extensions
        .iter()
        .find(|extension| extension.identity.path == "<builtin:loop-guard>")
        .expect("loop-guard loaded");
    assert!(
        loop_guard
            .message_renderers
            .contains_key(maho_ext_loop_guard::notice::LOOP_GUARD_NOTICE_CUSTOM_TYPE)
    );
}

#[test]
fn hooks_factory_registers_the_pinned_handlers_and_command() {
    let dir = tempfile::tempdir().expect("isolated cwd");
    let result =
        load_native_extensions(dir.path(), maho_ext_api::ExtensionSessionProfile::default());
    let hooks = result
        .extensions
        .iter()
        .find(|extension| extension.identity.path == "<builtin:hooks>")
        .expect("hooks loaded");
    for kind in [
        maho_ext_api::EventKind::Input,
        maho_ext_api::EventKind::BeforeAgentStart,
        maho_ext_api::EventKind::ToolCall,
        maho_ext_api::EventKind::ToolResult,
    ] {
        assert_eq!(hooks.handlers[&kind].len(), 1, "{kind:?}");
    }
    assert_eq!(hooks.commands[0].name, "hooks");
}

#[test]
fn help_factory_registers_the_keybindings_command() {
    let dir = tempfile::tempdir().expect("isolated cwd");
    let result =
        load_native_extensions(dir.path(), maho_ext_api::ExtensionSessionProfile::default());
    let help = result
        .extensions
        .iter()
        .find(|extension| extension.identity.path == "<builtin:help>")
        .expect("help loaded");
    assert_eq!(help.commands[0].name, "keybindings");
}

#[test]
fn assemble_extensions_tags_builtin_and_user_kinds() {
    let assembled = assemble_extensions();
    assert_eq!(
        assembled.len(),
        builtin_extensions().len() + user_extensions().len() + inline_extensions().len()
    );
    assert!(
        assembled
            .iter()
            .any(|(id, kind, _)| *id == "hooks" && *kind == "builtin")
    );
    assert!(
        assembled
            .iter()
            .any(|(id, kind, _)| *id == "pi-ast-grep" && *kind == "user")
    );
    assert!(
        assembled
            .iter()
            .any(|(id, kind, _)| *id == "llama.cpp" && *kind == "inline")
    );
}

#[test]
fn deferred_user_records_cover_the_unlinked_ports() {
    let linked: BTreeSet<&str> = user_extensions().iter().map(|entry| entry.id).collect();
    for deferred in deferred_user_extensions() {
        assert!(!linked.contains(deferred.id), "{}", deferred.id);
    }
}

#[test]
fn user_ports_register_only_when_installed() {
    let agent = tempfile::tempdir().expect("isolated agent dir");
    let bare: Vec<String> = native_extension_factories_for(agent.path())
        .into_iter()
        .map(|factory| factory.path)
        .collect();
    assert!(
        !bare.iter().any(|path| path.starts_with("<user:")),
        "{bare:?}"
    );
    std::fs::create_dir_all(agent.path().join("extensions")).expect("extensions dir");
    std::fs::write(agent.path().join("extensions/orca-titlebar-spinner.ts"), "")
        .expect("extension file");
    std::fs::write(agent.path().join("settings.json"), r#"{"packages":["git:github.com/code-yeongyu/pi-ast-grep",{"source":"npm:omo-zcode-oauth@1.0.0"}]}"#).expect("settings");
    let installed = installed_user_extension_ids(agent.path());
    assert_eq!(
        installed.into_iter().collect::<Vec<_>>(),
        ["omo-zcode-oauth", "orca-titlebar-spinner", "pi-ast-grep"]
    );
    let paths: Vec<String> = native_extension_factories_for(agent.path())
        .into_iter()
        .map(|factory| factory.path)
        .collect();
    for expected in [
        "<user:pi-ast-grep>",
        "<user:orca-titlebar-spinner>",
        "<user:omo-zcode-oauth>",
    ] {
        assert!(
            paths.contains(&expected.to_owned()),
            "{expected} missing from {paths:?}"
        );
    }
    assert!(!paths.contains(&"<user:orca-prefill>".to_owned()));
}
