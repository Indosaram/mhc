use std::collections::BTreeSet;

use maho_cli::cli::extension_registry::{
    assemble_extensions, builtin_extensions, deferred_builtin_extensions, deferred_user_extensions,
    load_native_extensions, native_extension_factories, user_extensions, BUILTIN_EXTENSION_IDS,
    GLOBAL_DEFAULT_EXTENSION_IDS, INLINE_EXTENSION_IDS,
};

#[test]
fn pinned_builtin_order_matches_upstream() {
    assert_eq!(BUILTIN_EXTENSION_IDS.len(), 43);
    let unique: BTreeSet<&str> = BUILTIN_EXTENSION_IDS.iter().copied().collect();
    assert_eq!(unique.len(), 43);
    assert_eq!(BUILTIN_EXTENSION_IDS[0], "loop-guard");
    assert_eq!(BUILTIN_EXTENSION_IDS[42], "mcp");
    assert_eq!(GLOBAL_DEFAULT_EXTENSION_IDS, ["diff", "files", "prompt-url-widget", "tps"]);
    assert_eq!(INLINE_EXTENSION_IDS, ["llama.cpp"]);
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
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{positions:?}");
}

#[test]
fn deferred_records_name_a_crate_and_a_requirement() {
    for deferred in deferred_builtin_extensions().iter().chain(deferred_user_extensions().iter()) {
        assert!(!deferred.crate_name.is_empty(), "{}", deferred.id);
        assert!(!deferred.requirement.is_empty(), "{}", deferred.id);
        assert!(
            !builtin_extensions().iter().any(|entry| entry.id == deferred.id)
                && !user_extensions().iter().any(|entry| entry.id == deferred.id),
            "{}",
            deferred.id
        );
    }
}

#[test]
fn linked_factories_register_through_the_loader() {
    let dir = tempfile::tempdir().expect("isolated cwd");
    let result = load_native_extensions(dir.path(), maho_ext_api::ExtensionSessionProfile::default());
    let errors: Vec<&str> = result.errors.iter().map(|error| error.error.as_str()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(result.extensions.len(), native_extension_factories().len());
    let ids: Vec<&str> = result.extensions.iter().map(|extension| extension.identity.path.as_str()).collect();
    for expected in ["<builtin:loop-guard>", "<builtin:hooks>", "<builtin:prompt-preset>", "<builtin:terminal>", "<builtin:goal>", "<builtin:loop>", "<builtin:websearch>", "<builtin:webfetch>", "<user:pi-ast-grep>", "<user:orca-titlebar-spinner>"] {
        assert!(ids.contains(&expected), "{expected} missing from {ids:?}");
    }
}

#[test]
fn loop_guard_registers_the_pinned_notice_renderer() {
    let dir = tempfile::tempdir().expect("isolated cwd");
    let result = load_native_extensions(dir.path(), maho_ext_api::ExtensionSessionProfile::default());
    let loop_guard = result
        .extensions
        .iter()
        .find(|extension| extension.identity.path == "<builtin:loop-guard>")
        .expect("loop-guard loaded");
    assert!(loop_guard.message_renderers.contains_key(maho_ext_loop_guard::index::LOOP_GUARD_NOTICE_CUSTOM_TYPE));
}

#[test]
fn hooks_factory_registers_the_pinned_handlers_and_command() {
    let dir = tempfile::tempdir().expect("isolated cwd");
    let result = load_native_extensions(dir.path(), maho_ext_api::ExtensionSessionProfile::default());
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
    let result = load_native_extensions(dir.path(), maho_ext_api::ExtensionSessionProfile::default());
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
    assert_eq!(assembled.len(), builtin_extensions().len() + user_extensions().len());
    assert!(assembled.iter().any(|(id, kind, _)| *id == "hooks" && *kind == "builtin"));
    assert!(assembled.iter().any(|(id, kind, _)| *id == "pi-ast-grep" && *kind == "user"));
}

#[test]
fn deferred_user_records_cover_the_unlinked_ports() {
    let linked: BTreeSet<&str> = user_extensions().iter().map(|entry| entry.id).collect();
    for deferred in deferred_user_extensions() {
        assert!(!linked.contains(deferred.id), "{}", deferred.id);
    }
}
