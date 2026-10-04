#[tokio::test]
async fn concrete_default_factories_register_tools_and_commands() {
    let root = tempfile::tempdir().expect("isolated factory directory");
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let factories = maho_cli::cli::default_extensions::factories(sender, std::sync::Arc::new(std::sync::OnceLock::new()));
    let loaded = maho_ext_host::loader::load_extensions_async(factories, root.path(), Default::default()).await;
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let commands = loaded.extensions.iter().flat_map(|extension| &extension.commands).map(|command| command.name.as_str()).collect::<Vec<_>>();
    for command in ["account", "answer", "fallback", "help", "history", "mcp", "todo"] {
        assert!(commands.contains(&command), "Missing command {command}: {commands:?}");
    }
    let names = loaded.extensions.iter().map(|extension| extension.identity.path.as_str()).collect::<Vec<_>>();
    for path in ["<builtin:codemode>", "<builtin:compaction>", "<builtin:task>", "<builtin:webfetch>", "<builtin:websearch>", "<builtin:look-at>"] {
        assert!(names.contains(&path), "Missing factory {path}");
    }
    let tools = loaded.extensions.iter().flat_map(|extension| &extension.tools).map(|tool| tool.definition.name.as_str()).collect::<Vec<_>>();
    for name in ["eval", "look_at", "todo", "webfetch", "websearch"] { assert!(tools.contains(&name), "Missing tool {name}: {tools:?}"); }
}
