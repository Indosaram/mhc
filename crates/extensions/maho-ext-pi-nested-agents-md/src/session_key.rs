use maho_ext_api::ExtensionContext;

pub fn get_session_key(ctx: &ExtensionContext) -> String {
    ctx.session_manager.session_file().filter(|path| !path.as_os_str().is_empty()).map_or_else(
        || "__pi_nested_agents_md_singleton__".to_owned(),
        |path| path.to_string_lossy().into_owned(),
    )
}
