use crate::{
    binary_path::{BinaryResolver, find_sg_cli_path},
    downloader::{cached_binary_path, ensure_ast_grep_binary},
    tools::tool_with_resolver,
};
use maho_ext_api::{Extension, ExtensionApi, NotificationType};
use std::{path::PathBuf, sync::Arc};

/// `package_entry` stands in for the TS module's `import.meta.url`: `@ast-grep/cli` and the
/// platform package are resolved from its ancestors' `node_modules`. `None` keeps the cwd fallback.
pub struct AstGrep {
    pub package_entry: Option<PathBuf>,
}
impl Extension for AstGrep {
    fn register(&self, api: &mut ExtensionApi) {
        self.register_with_env(api, crate::downloader::process_env());
    }
}
impl AstGrep {
    /// `register` with the environment read through `env`: `PATH`, `PI_OFFLINE` and the cache
    /// variables, at registration and on every `/ast-grep` call.
    pub fn register_with_env(&self, api: &mut ExtensionApi, env: crate::downloader::EnvReader) {
        let platform = crate::downloader::current_platform();
        let platform_key = crate::downloader::current_platform_key();
        let cache = crate::downloader::get_cache_dir_in(&*env);
        let source = self
            .package_entry
            .clone()
            .unwrap_or_else(|| api.cwd.join("extension.rs"));
        let path = env("PATH");
        let offline = crate::downloader::is_offline_in(&*env);
        let resolver = Arc::new(BinaryResolver::new(
            source.clone(),
            cache,
            path,
            platform_key.clone(),
            offline,
        ));
        let version = resolver.version.clone();
        for replace in [false, true] {
            if let Err(error) = api.register_tool_with_renderers(
                tool_with_resolver(replace, Arc::clone(&resolver)),
                crate::render::renderers(replace),
            ) {
                maho_ext_api::diagnostic(&format!("pi-ast-grep: {error}"));
            }
        }
        api.register_command("ast-grep", Some("Show ast-grep binary path, version, and cache directory".into()), None, Arc::new(move |args, ctx| {
            // Upstream calls getCacheDir() per invocation, so cache env changes apply.
            let cache = crate::downloader::get_cache_dir_in(&*env);
            let platform_key = platform_key.clone();
            let source = source.clone();
            // Upstream findSgCliPathSync() reads PATH per invocation.
            let path = env("PATH");
            let version = version.clone();
            let env = Arc::clone(&env);
            Box::pin(async move {
                let cached = cached_binary_path(&cache, platform);
                let local = find_sg_cli_path(&source, cached.as_deref(), path.as_deref());
                if matches!(args.trim(), "install" | "download") {
                    ctx.ui.set_status("pi-ast-grep", Some("Downloading sg binary..."));
                    let offline = crate::downloader::is_offline_in(&*env);
                    let binary = ensure_ast_grep_binary(&cache, &platform_key, &version, offline).await;
                    ctx.ui.set_status("pi-ast-grep", None);
                    if let Some(binary) = binary { ctx.ui.notify(&format!("ast-grep ready: {}", binary.display()), NotificationType::Info); }
                    else { ctx.ui.notify("Auto-download failed. Try: npm install -g @ast-grep/cli or brew install ast-grep", NotificationType::Error); }
                } else {
                    ctx.ui.notify(&format!("pi-ast-grep\n  Cache dir : {}\n  Cached sg : {}\n  Local sg  : {}", cache.display(), cached.map_or_else(|| "not downloaded".into(), |path| path.to_string_lossy().into_owned()), local.map_or_else(|| "not on PATH".into(), |path| path.to_string_lossy().into_owned())), NotificationType::Info);
                }
                Ok(())
            })
        }));
    }
}
