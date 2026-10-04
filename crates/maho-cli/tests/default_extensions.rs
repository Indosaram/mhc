#[tokio::test]
async fn concrete_default_factories_register_tools_and_commands() {
    let root = tempfile::tempdir().expect("isolated factory directory");
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let factories = maho_cli::cli::default_extensions::factories(sender, std::sync::Arc::new(std::sync::OnceLock::new()));
    let loaded = maho_ext_host::loader::load_extensions_async(factories, root.path(), Default::default()).await;
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let commands = loaded.extensions.iter().flat_map(|extension| &extension.commands).map(|command| command.name.as_str()).collect::<Vec<_>>();
    for command in ["account", "answer", "fallback", "goal", "rules", "reload-rules", "help", "history", "mcp", "todo"] {
        assert!(commands.contains(&command), "Missing command {command}: {commands:?}");
    }
    let names = loaded.extensions.iter().map(|extension| extension.identity.path.as_str()).collect::<Vec<_>>();
    for path in ["<builtin:codemode>", "<builtin:compaction>", "<builtin:webfetch>", "<builtin:websearch>", "<builtin:rules>", "<builtin:goal>", "<builtin:look-at>"] {
        assert!(names.contains(&path), "Missing factory {path}");
    }
    let tools = loaded.extensions.iter().flat_map(|extension| &extension.tools).map(|tool| tool.definition.name.as_str()).collect::<Vec<_>>();
    for name in ["eval", "look_at", "todo", "webfetch", "websearch", "create_goal", "update_goal", "get_goal"] { assert!(tools.contains(&name), "Missing tool {name}: {tools:?}"); }
}

#[tokio::test]
async fn assembled_factories_preserve_sdk_and_cli_extensions_once() {
    let root = tempfile::tempdir().expect("isolated factory directory");
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let factories = maho_cli::cli::default_extensions::assembled_factories(sender, std::sync::Arc::new(std::sync::OnceLock::new()));
    let paths = factories.iter().map(|factory| factory.path.clone()).collect::<std::collections::BTreeSet<_>>();
    assert_eq!(paths.len(), factories.len());
    for path in ["<builtin:gpt-apply-patch>", "<builtin:todotools>", "<builtin:omo>", "<builtin:codemode>", "<builtin:hooks>", "<builtin:terminal>", "<user:pi-ast-grep>"] {
        assert!(paths.contains(path), "Missing factory {path}");
    }
    let loaded = maho_ext_host::loader::load_extensions_async(factories, root.path(), Default::default()).await;
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    assert_eq!(loaded.extensions.len(), paths.len());
}
