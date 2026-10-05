pub mod panel;

use maho_ext_api::{Extension,ExtensionApi,ExtensionContext,ExtensionFuture,ExtensionMode,NotificationType};
use std::sync::Arc;
pub fn help_markdown(commands:Vec<maho_ext_api::SlashCommandInfo>)->String{
    let commands=commands.into_iter().map(|command|maho_core::slash_commands::SlashCommandInfo{name:command.name,description:command.description,source:maho_core::slash_commands::SlashCommandSource::Extension,source_info:Default::default()}).collect::<Vec<_>>();
    maho_interactive::help_content::build_help_markdown(&commands)
}
pub type HelpDisplay=Arc<dyn Fn(ExtensionContext,String)->ExtensionFuture<'static,()>+Send+Sync>;
pub type HelpRowsReader=Arc<dyn Fn(&dyn maho_ext_api::ExtensionTuiHost)->std::rc::Rc<dyn Fn()->usize>+Send+Sync>;
pub type HelpRenderer=Arc<dyn Fn(&dyn maho_ext_api::ExtensionTuiHost)->std::rc::Rc<dyn Fn()>+Send+Sync>;
pub type HelpThemeMapper=Arc<dyn Fn(&maho_ext_api::Theme)->Result<maho_interactive::theme::Theme,maho_ext_api::ExtensionFailure>+Send+Sync>;
pub struct HelpUiBindings{
    pub rows:HelpRowsReader,
    pub render:HelpRenderer,
    pub theme:HelpThemeMapper,
}
pub fn native_display(bindings:HelpUiBindings)->HelpDisplay{
    let bindings=Arc::new(bindings);
    Arc::new(move|ctx,markdown|{let bindings=bindings.clone();Box::pin(async move{
        ctx.ui.custom_factory(Arc::new(move|host,theme,_,done|{
            let rows=(bindings.rows)(host);let render=(bindings.render)(host);let theme=(bindings.theme)(theme);
            let markdown=markdown.clone();
            Box::pin(async move{
                let panel=panel::HelpPanel::new(&markdown,theme?,rows,render,std::rc::Rc::new(move||done(maho_ext_api::JsonValue::Null)));
                Ok(Box::new(panel) as Box<dyn maho_tui::tui::Component>)
            })
        }),maho_ext_api::CustomUiFactoryOptions{overlay:true,overlay_options:Some(maho_ext_api::ExtensionOverlayOptions::Static(Arc::new(||maho_tui::tui::OverlayOptions{
            anchor:Some(maho_tui::tui::OverlayAnchor::TopCenter),width:Some(maho_tui::tui::SizeValue::Percent(90.0)),min_width:Some(60),max_height:Some(maho_tui::tui::SizeValue::Percent(100.0)),margin:Some(maho_tui::tui::OverlayMargin{top:Some(2),right:Some(2),bottom:Some(2),left:Some(2)}),..Default::default()
        }))),..Default::default()}).await?;
        Ok(())
    })})
}
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

