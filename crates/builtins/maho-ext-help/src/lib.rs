pub mod panel;

use maho_ext_api::{Extension,ExtensionApi,ExtensionMode,NotificationType,ExtensionFailure};
use std::sync::Arc;
pub struct Help;
impl Extension for Help{
    fn register(&self,api:&mut ExtensionApi){
        api.register_command("keybindings",Some("Open your keybindings.json in $EDITOR and reload it live".into()),None,Arc::new(|_,ctx|Box::pin(async move{
            if ctx.mode!=ExtensionMode::Tui{ctx.ui.notify(&format!("Interactive /keybindings opens your config in $EDITOR and reloads it; run {} in TUI mode to use it.",maho_core::config::app_name()),NotificationType::Info);}
            Ok(())
        })));
        let runtime=api.runtime.clone();
        api.register_command("help",Some("Show usage, keybindings, and all commands".into()),None,Arc::new(move|_,ctx|{
            let runtime=runtime.clone();Box::pin(async move{
                if ctx.mode!=ExtensionMode::Tui{ctx.ui.notify(&format!("Interactive /help is available in TUI mode; run {} --help for CLI usage.",maho_core::config::app_name()),NotificationType::Info);return Ok(());}
                let _commands=runtime.session_actions()?.get_commands()?;
                Err(ExtensionFailure::new("Interactive help requires the shared help-content builder and custom UI completion bridge"))
            })
        }));
    }
}

