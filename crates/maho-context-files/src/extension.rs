//! The `<builtin:context-files>` extension: `/init-deep [update]` and the session-start advisor.

use crate::plan::{Settings, plan};
use crate::prompt::session_request;
use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionMode, NotificationType,
    SendUserMessageOptions, UserMessageContent,
};
use std::path::PathBuf;
use std::sync::Arc;

/// Command name.
pub const COMMAND: &str = "init-deep";

/// The extension. `home: None` uses `$HOME` for the advisor state.
#[derive(Clone, Debug, Default)]
pub struct ContextFilesExtension {
    pub home: Option<PathBuf>,
}

/// Whether the advisor runs in sessions of `mode` (long-lived ones only).
pub fn advises_in(mode: ExtensionMode) -> bool {
    matches!(
        mode,
        ExtensionMode::Tui | ExtensionMode::Rpc | ExtensionMode::AppServer
    )
}

fn read_existing(root: PathBuf) -> impl Fn(&str, &str) -> Option<String> {
    move |rel, name| {
        let dir = if rel == "." {
            root.clone()
        } else {
            rel.split('/').fold(root.clone(), |p, c| p.join(c))
        };
        std::fs::read_to_string(dir.join(name)).ok()
    }
}

impl Extension for ContextFilesExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let sender = Arc::new(ExtensionApi::new(
            api.registered.clone(),
            api.profile.clone(),
            api.events.clone(),
            api.runtime.clone(),
        ));
        api.register_command(
            COMMAND,
            Some(
                "Write AGENTS.md context files for this repository, one per significant directory"
                    .into(),
            ),
            Some("[update]".into()),
            Arc::new(move |args, ctx| {
                let sender = Arc::clone(&sender);
                Box::pin(async move {
                    let update = args.trim() == "update";
                    let settings = Settings::load(&ctx.cwd);
                    let root = crate::analyze::project_root(&ctx.cwd);
                    let analysis = crate::analyze::analyze(&root);
                    let plan = plan(&analysis, &settings, update, &read_existing(root.clone()));
                    let text = session_request(&plan);
                    if let Err(error) = sender.send_user_message(
                        UserMessageContent::Text(text),
                        SendUserMessageOptions::default(),
                    ) {
                        ctx.ui
                            .notify(&format!("/init-deep: {error}"), NotificationType::Error);
                    }
                    Ok(())
                })
            }),
        );
        let home = self.home.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_event, ctx| {
                let home = home.clone();
                Box::pin(async move {
                    if !advises_in(ctx.mode) {
                        return Ok(EventResult::None);
                    }
                    let Some(home) = home.or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                    else {
                        return Ok(EventResult::None);
                    };
                    let settings = Settings::load(&ctx.cwd);
                    let root = crate::analyze::project_root(&ctx.cwd);
                    if let Some(text) = crate::advisor::advise_once(&home, &root, &settings) {
                        ctx.ui.notify(&text, NotificationType::Info);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
