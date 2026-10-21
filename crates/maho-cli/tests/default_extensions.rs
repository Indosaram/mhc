use maho_ext_api::Extension;

#[cfg(windows)]
#[tokio::test]
async fn memory_report_command_loads_from_default_factories_without_model_tools() {
    let root = tempfile::tempdir().expect("isolated factory directory");
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut factories = maho_cli::cli::default_extensions::factories(
        sender,
        std::sync::Arc::new(std::sync::OnceLock::new()),
    );
    maho_cli::cli::default_extensions::retain_no_extensions_factories(&mut factories);
    factories.retain(|factory| factory.path == "<builtin:memory-report>");

    let loaded = maho_ext_host::loader::load_extensions_async(
        factories, root.path(), Default::default(),
    ).await;

    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    assert_eq!(loaded.extensions.len(), 1);
    let extension = &loaded.extensions[0];
    assert_eq!(extension.commands.len(), 1);
    assert_eq!(extension.commands[0].name, "memory-report");
    assert!(extension.tools.is_empty());
}

#[tokio::test]
async fn concrete_default_factories_register_tools_and_commands() {
    let root = tempfile::tempdir().expect("isolated factory directory");
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let factories = maho_cli::cli::default_extensions::factories(sender, std::sync::Arc::new(std::sync::OnceLock::new()));
    let search = factories.iter().find(|factory| factory.path == "<builtin:tool-search>").expect("native search factory");
    assert_eq!(search.source_info.source, "builtin");
    assert_eq!(search.source_info.path, search.path);
    let loaded = maho_ext_host::loader::load_extensions_async(factories, root.path(), Default::default()).await;
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let commands = loaded.extensions.iter().flat_map(|extension| &extension.commands).map(|command| command.name.as_str()).collect::<Vec<_>>();
    for command in ["account", "answer", "fallback", "goal", "rules", "reload-rules", "help", "history", "mcp", "todo"] {
        assert!(commands.contains(&command), "Missing command {command}: {commands:?}");
    }
    let names = loaded.extensions.iter().map(|extension| extension.identity.path.as_str()).collect::<Vec<_>>();
    assert!(names.contains(&"<builtin:service-tier>"), "Missing builtin:service-tier: {names:?}");
    for id in ["reasoning", "redraws", "import-repro", "gpt-account", "diff", "files", "prompt-url-widget", "tps"] {
        assert_eq!(names.iter().filter(|path| **path == format!("<builtin:{id}>")).count(), 1);
    }
    for path in ["<builtin:codemode>", "<builtin:compaction>", "<builtin:webfetch>", "<builtin:websearch>", "<builtin:rules>", "<builtin:goal>", "<builtin:look-at>"] {
        assert!(names.contains(&path), "Missing factory {path}");
    }
    let tools = loaded.extensions.iter().flat_map(|extension| &extension.tools).map(|tool| tool.definition.name.as_str()).collect::<Vec<_>>();
    for name in ["eval", "look_at", "todo", "webfetch", "web_search", "create_goal", "update_goal", "get_goal"] { assert!(tools.contains(&name), "Missing tool {name}: {tools:?}"); }
}

#[tokio::test]
async fn assembled_factories_preserve_sdk_and_cli_extensions_once() {
    let root = tempfile::tempdir().expect("isolated factory directory");
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let settings = maho_core::settings_manager::SettingsManager::in_memory(Default::default(), false);
    let factories = maho_cli::cli::default_extensions::assembled_factories(sender, std::sync::Arc::new(std::sync::OnceLock::new()), &settings, root.path(), &root.path().to_string_lossy());
    let paths = factories.iter().map(|factory| factory.path.clone()).collect::<std::collections::BTreeSet<_>>();
    assert_eq!(paths.len(), factories.len());
    for path in ["<builtin:gpt-apply-patch>", "<builtin:todowrite>", "<builtin:omo>", "<builtin:codemode>", "<builtin:hooks>", "<builtin:terminal>", "<builtin:claude-sdk-oauth>", "<builtin:cursor-cli-oauth>"] {
        assert!(paths.contains(path), "Missing factory {path}");
    }
    let registered = factories.iter().filter_map(|factory| factory.path.strip_prefix("<builtin:").and_then(|path| path.strip_suffix('>')))
        .filter(|id| maho_cli::cli::extension_registry::BUILTIN_EXTENSION_IDS.contains(id)).collect::<Vec<_>>();
    let expected = maho_cli::cli::extension_registry::BUILTIN_EXTENSION_IDS.iter().copied().filter(|id| registered.contains(id)).collect::<Vec<_>>();
    assert_eq!(registered, expected);
    // senpi resource-loader.ts:933-949: global defaults load through agent-dir shims after every
    // builtin, bundled and inline factory.
    let defaults = factories.iter().rev().take(4).rev().map(|factory| factory.path.as_str()).collect::<Vec<_>>();
    assert_eq!(defaults, ["<builtin:diff>", "<builtin:files>", "<builtin:prompt-url-widget>", "<builtin:tps>"]);
    let position = |path: &str| factories.iter().position(|factory| factory.path == path).expect(path);
    assert!(position("<builtin:mcp>") < position("<builtin:diff>"));
    assert!(position("<builtin:omo>") < position("<builtin:diff>"));
    let loaded = maho_ext_host::loader::load_extensions_async(factories, root.path(), Default::default()).await;
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    assert_eq!(loaded.extensions.len(), paths.len());
}

#[tokio::test]
async fn oauth_lanes_sit_before_config_reload_and_mcp_stays_last_builtin() {
    let root = tempfile::tempdir().expect("isolated factory directory");
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let settings = maho_core::settings_manager::SettingsManager::in_memory(Default::default(), false);
    let factories = maho_cli::cli::default_extensions::assembled_factories(sender, std::sync::Arc::new(std::sync::OnceLock::new()), &settings, root.path(), &root.path().to_string_lossy());
    let position = |path: &str| factories.iter().position(|factory| factory.path == path).expect(path);
    assert!(position("<builtin:gpt-account>") < position("<builtin:claude-sdk-oauth>"));
    assert!(position("<builtin:claude-sdk-oauth>") < position("<builtin:cursor-cli-oauth>"));
    assert!(position("<builtin:cursor-cli-oauth>") < position("<builtin:config-reload>"));
    let builtins = factories.iter().filter(|factory| factory.path.strip_prefix("<builtin:").and_then(|id| id.strip_suffix('>'))
        .is_some_and(|id| maho_cli::cli::extension_registry::BUILTIN_EXTENSION_IDS.contains(&id))).collect::<Vec<_>>();
    assert_eq!(builtins.last().map(|factory| factory.path.as_str()), Some("<builtin:mcp>"));
}

#[tokio::test]
async fn no_extensions_drops_global_defaults_and_keeps_builtins() {
    let root = tempfile::tempdir().expect("isolated factory directory");
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let settings = maho_core::settings_manager::SettingsManager::in_memory(Default::default(), false);
    let mut factories = maho_cli::cli::default_extensions::assembled_factories(sender, std::sync::Arc::new(std::sync::OnceLock::new()), &settings, root.path(), &root.path().to_string_lossy());
    maho_cli::cli::default_extensions::retain_no_extensions_factories(&mut factories);
    let paths = factories.iter().map(|factory| factory.path.as_str()).collect::<Vec<_>>();
    for id in maho_cli::cli::extension_registry::GLOBAL_DEFAULT_EXTENSION_IDS {
        assert!(!paths.contains(&format!("<builtin:{id}>").as_str()), "{paths:?}");
    }
    assert!(factories.iter().all(|factory| factory.source_info.source != "user"), "{paths:?}");
    assert!(paths.contains(&"<builtin:mcp>") && paths.contains(&"<builtin:codemode>") && paths.contains(&"<builtin:omo>"), "{paths:?}");
}

#[tokio::test]
async fn settings_allowlist_and_denylist_filter_builtin_ids_only() {
    let root = tempfile::tempdir().expect("isolated factory directory");
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    // senpi resource-loader.ts:1211 filters only `builtinExtensions`/bundled ids; global defaults
    // (`tps`, builtin/index.ts:55) load through agent-dir shims and ignore the denylist.
    let global = serde_json::json!({"disabledBuiltinExtensions":["redraws","cursor-cli-oauth","tps"]});
    let settings = maho_core::settings_manager::SettingsManager::in_memory(global.as_object().expect("object").clone(), false);
    let factories = maho_cli::cli::default_extensions::assembled_factories(sender, std::sync::Arc::new(std::sync::OnceLock::new()), &settings, root.path(), &root.path().to_string_lossy());
    let paths = factories.iter().map(|factory| factory.path.as_str()).collect::<Vec<_>>();
    assert!(!paths.contains(&"<builtin:redraws>") && !paths.contains(&"<builtin:cursor-cli-oauth>"), "{paths:?}");
    assert!(paths.contains(&"<builtin:tps>"), "{paths:?}");
    assert!(paths.contains(&"<builtin:claude-sdk-oauth>") && paths.contains(&"<builtin:omo>") && paths.contains(&"<inline:llama.cpp>"), "{paths:?}");
    let allow = maho_cli::cli::default_extensions::BuiltinExtensionSelection { enabled: Some(vec!["mcp".into()]), disabled: Vec::new() };
    assert!(allow.is_active("mcp") && !allow.is_active("help") && !allow.is_active("codemode") && allow.is_active("omo"));
}

#[tokio::test]
async fn task_component_registers_session_start_handler_for_non_member() {
    let parent: maho_cli::cli::default_extensions::TaskParent = std::sync::Arc::new(std::sync::OnceLock::new());
    let mut env = std::collections::BTreeMap::new();
    env.insert("HOME".to_string(), "/tmp".to_string());
    let component = maho_cli::cli::default_extensions::task_entry(parent, env);
    assert_eq!(component.name, "task");

    let mut api = maho_ext_api::ExtensionApi::new(
        maho_ext_api::LoadedExtension::new("test", std::path::PathBuf::from("/tmp"), maho_ext_api::SourceInfo::default()),
        maho_ext_api::ExtensionSessionProfile::default(),
        maho_ext_api::EventBus::default(),
        maho_ext_api::ExtensionRuntime::default(),
    );
    let omo = maho_omo::OmoExtension::new(vec![component]);
    omo.register(&mut api);

    // The task entrypoint registers a SessionStart handler on the host api.
    // When SessionStart fires, it registers the inner TaskComponent and process sweep.
    assert!(
        api.registered.handlers.contains_key(&maho_ext_api::EventKind::SessionStart),
        "SessionStart handler must be registered on host api"
    );
    let host_start_handlers = &api.registered.handlers[&maho_ext_api::EventKind::SessionStart];
    assert!(!host_start_handlers.is_empty());
}

