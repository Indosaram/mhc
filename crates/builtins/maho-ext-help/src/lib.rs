pub mod panel;

use maho_ext_api::{Extension,ExtensionApi,ExtensionContext,ExtensionFuture,ExtensionMode,NotificationType};
use std::sync::Arc;
pub fn help_markdown(commands:Vec<maho_ext_api::SlashCommandInfo>)->String{
    let commands=commands.into_iter().map(|command|maho_core::slash_commands::SlashCommandInfo{name:command.name,description:command.description,source:maho_core::slash_commands::SlashCommandSource::Extension,source_info:Default::default()}).collect::<Vec<_>>();
    maho_interactive::help_content::build_help_markdown(&commands)
}
pub type HelpDisplay=Arc<dyn Fn(ExtensionContext,String)->ExtensionFuture<'static,()>+Send+Sync>;
pub struct Help{pub display:HelpDisplay}
impl Extension for Help{
    fn register(&self,api:&mut ExtensionApi){
        api.register_command("keybindings",Some("Open your keybindings.json in $EDITOR and reload it live".into()),None,Arc::new(|_,ctx|Box::pin(async move{
            if ctx.mode!=ExtensionMode::Tui{ctx.ui.notify(&format!("Interactive /keybindings opens your config in $EDITOR and reloads it; run {} in TUI mode to use it.",maho_core::config::app_name()),NotificationType::Info);}
            Ok(())
        })));
        let runtime=api.runtime.clone();
        let display=self.display.clone();
        api.register_command("help",Some("Show usage, keybindings, and all commands".into()),None,Arc::new(move|_,ctx|{
            let runtime=runtime.clone();let display=display.clone();Box::pin(async move{
                if ctx.mode!=ExtensionMode::Tui{ctx.ui.notify(&format!("Interactive /help is available in TUI mode; run {} --help for CLI usage.",maho_core::config::app_name()),NotificationType::Info);return Ok(());}
                let markdown=help_markdown(runtime.session_actions()?.get_commands()?);
                display(ctx.clone(),markdown).await
            })
        }));
    }
}

