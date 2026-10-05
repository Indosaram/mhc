use crate::{binary_path::{find_sg_cli_path, BinaryResolver}, downloader::{cache_dir, cached_binary_path, ensure_ast_grep_binary}, tools::tool_with_resolver};
use maho_ext_api::{Extension, ExtensionApi, NotificationType};
use std::{path::PathBuf, sync::Arc};

pub struct AstGrep;
impl Extension for AstGrep {
    fn register(&self, api: &mut ExtensionApi) {
        let platform = if cfg!(windows) { "win32" } else if cfg!(target_os = "macos") { "darwin" } else { "linux" };
        let arch = if cfg!(target_arch = "x86_64") { "x64" } else if cfg!(target_arch = "aarch64") { "arm64" } else { "ia32" };
        let platform_key = format!("{platform}-{arch}");
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default();
        let override_path = if cfg!(windows) { std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA")) } else { std::env::var_os("XDG_CACHE_HOME") }.map(PathBuf::from);
        let cache = cache_dir(&home, platform, override_path.as_deref());
        let source = api.cwd.join("extension.rs");
        let path = std::env::var_os("PATH");
        let offline = matches!(std::env::var("PI_OFFLINE").ok().as_deref(), Some("1" | "true"));
        let resolver = Arc::new(BinaryResolver::new(source.clone(), cache.clone(), path.clone(), platform_key.clone(), offline));
        let version = resolver.version.clone();
        for replace in [false,true] {
            if let Err(error) = api.register_tool_with_renderers(tool_with_resolver(replace,Arc::clone(&resolver)),crate::render::renderers(replace)) { eprintln!("pi-ast-grep: {error}"); }
        }
        api.register_command("ast-grep", Some("Show ast-grep binary path, version, and cache directory".into()), None, Arc::new(move |args, ctx| {
            let cache = cache.clone();
            let platform_key = platform_key.clone();
            let source = source.clone();
            let path = path.clone();
            let version = version.clone();
            Box::pin(async move {
                let cached = cached_binary_path(&cache, platform);
                let local = find_sg_cli_path(&source, cached.as_deref(), path.as_deref());
                if matches!(args.trim(), "install" | "download") {
                    ctx.ui.set_status("pi-ast-grep", Some("Downloading sg binary..."));
                    let offline = matches!(std::env::var("PI_OFFLINE").ok().as_deref(), Some("1" | "true"));
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
