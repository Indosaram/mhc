use serde_json::{Value, json};
use crate::engine::{bm25::Bm25Result,document::ToolSearchSource};
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct HiddenToolHint { pub name:String,pub hint:String }
pub const TOOL_SEARCH_TOOL_NAME: &str="tool_search";
pub fn renderers()->maho_ext_api::ToolRenderers<(),Value> {
    use maho_interactive::theme::ThemeColor;
    use maho_tui::components::text::Text;
    use std::sync::Arc;
    let theme=|source:&maho_ext_api::Theme| {
    use maho_interactive::theme::{theme::ColorMode,theme_json::{ColorValue,ThemeJson}};
    let colors=source.colors.iter().chain(&source.backgrounds).map(|(key,prefix)| {
        let code=prefix.strip_prefix("\x1b[").and_then(|value|value.strip_suffix('m')).unwrap_or_else(||panic!("Expected exported ANSI theme prefix"));
        let fields=code.split(';').collect::<Vec<_>>();
        let value=match fields.as_slice() {
            ["39"|"49"]=>ColorValue::Text(String::new()),
            ["38"|"48","5",index]=>ColorValue::Index(index.parse().unwrap_or_else(|error|std::panic::panic_any(error))),
            ["38"|"48","2",red,green,blue]=>{
                let channels=[red,green,blue].map(|channel|channel.parse::<u8>().unwrap_or_else(|error|std::panic::panic_any(error)));
                ColorValue::Text(format!("#{:02x}{:02x}{:02x}",channels[0],channels[1],channels[2]))
            },
            _=>panic!("Unsupported exported ANSI theme prefix"),
        };
        (key.clone(),value)
    }).collect();
    let mode=if source.colors.values().chain(source.backgrounds.values()).any(|prefix|prefix.starts_with("\x1b[38;2;")||prefix.starts_with("\x1b[48;2;")){ColorMode::Truecolor}else{ColorMode::Color256};
    maho_interactive::theme::Theme::from_json(ThemeJson {name:source.name.clone().unwrap_or_default(),colors,vars:Default::default(),export_colors:Default::default()},mode).unwrap_or_else(|error|std::panic::panic_any(error))
    };
    maho_ext_api::ToolRenderers {
        render_call:Some(Arc::new(move |args,source,_|{
            let theme=theme(source);let source=args["source"].as_str().map(|source|format!(" source:{source}")).unwrap_or_default();let group=args["group"].as_str().map(|group|format!(" @{group}")).unwrap_or_default();
            Box::new(Text::with_padding(theme.fg(ThemeColor::ToolTitle,&theme.bold(&format!("{TOOL_SEARCH_TOOL_NAME} \"{}\"{source}{group}",args["query"].as_str().unwrap_or("")))),0,0))
        })),
        render_result:Some(Arc::new(move |result,options,source,_|{
            let theme=theme(source);let count=result.details["matched"].as_array().map_or(0,Vec::len);
            let title=if options.is_partial{format!("{TOOL_SEARCH_TOOL_NAME}: searching")}else{format!("{TOOL_SEARCH_TOOL_NAME}: {count} tool(s) found")};
            Box::new(Text::with_padding(theme.fg(ThemeColor::ToolOutput,&title),0,0))
        })),
    }
}
pub fn parameters()->Value { json!({"type":"object","properties":{"query":{"type":"string","description":"Natural-language description of the capability you need."},"source":{"anyOf":[{"const":"mcp","type":"string"},{"const":"extension","type":"string"}],"description":"Optional: restrict the search to MCP or extension tools."},"group":{"type":"string","description":"Optional: restrict the search to one catalog group."}},"required":["query"]}) }
pub fn create_tool_search_tool(service:std::sync::Arc<tokio::sync::Mutex<crate::service::ToolSearchService>>)->maho_tools::definition::ToolDefinition {
    use maho_tools::definition::{ToolDefinition,ToolError,ToolResult,ToolContent,ToolExecutionMode}; use std::sync::Arc;
    let mut tool=ToolDefinition::new(TOOL_SEARCH_TOOL_NAME,"Search the catalog of deferred tools by capability. Returns matching tool names with their parameter schemas and never changes your active tool set; call a returned tool by name and it activates on that first call.",parameters(),Arc::new(move |call| {
        let service=service.clone(); Box::pin(async move {
            let query=call.params.get("query").and_then(Value::as_str).ok_or_else(||ToolError::Message("query is required".into()))?;
            let source=match call.params.get("source").and_then(Value::as_str) { Some("mcp")=>Some(ToolSearchSource::Mcp),Some("extension")=>Some(ToolSearchSource::Extension),_=>None }; let group=call.params.get("group").and_then(Value::as_str);
            let mut service=service.lock().await;
            let matches=service.search(query,5,&crate::engine::bm25::Bm25SearchOptions{source,group:group.map(String::from),..Default::default()}).map_err(|error|ToolError::Message(error.to_string()))?;
            let hints=service.hidden_tool_hints(query); let mut parameters=std::collections::BTreeMap::new(); for item in &matches { parameters.insert(item.name.clone(),service.get_tool_parameters(&item.name).map_err(|error|ToolError::Message(error.to_string()))?); }
            let text=build_tool_search_result_text(query,&matches,&hints,source,group,|name|parameters.get(name).cloned().flatten());
            Ok(ToolResult{content:vec![ToolContent::text(text)],details:Some(json!({"matched":matches.iter().map(|item|&item.name).collect::<Vec<_>>(),"query":query}))})
        })
    }));
    tool.label="Tool search".into(); tool.execution_mode=Some(ToolExecutionMode::Parallel); tool.prepare_arguments=Some(Arc::new(|args|Ok(prepare_tool_search_arguments(args)))); tool.prompt_snippet=Some("Search deferred tool catalogs by capability; call a returned tool by name to use it.".into()); tool
}
pub fn prepare_tool_search_arguments(mut args: Value) -> Value {
    if let Some(obj)=args.as_object_mut() {
        let server=obj.remove("server");
        if (obj.contains_key("source") || server.is_some()) && obj.get("source").is_none_or(Value::is_null) { obj.insert("source".into(),json!("mcp")); }
        if (obj.contains_key("group") || server.is_some()) && obj.get("group").is_none_or(Value::is_null) { match server { Some(server)=>{obj.insert("group".into(),server);},None=>{obj.remove("group");} } }
    }
    args
}
pub fn build_tool_search_result_text(query: &str, matches: &[Bm25Result], hints: &[HiddenToolHint], source: Option<ToolSearchSource>, group: Option<&str>, parameters_of: impl Fn(&str)->Option<Value>) -> String {
    let mut scope=Vec::new();
    if let Some(source)=source { scope.push(format!("source \"{}\"",match source { ToolSearchSource::Mcp=>"mcp",ToolSearchSource::Extension=>"extension" })); }
    if let Some(group)=group { scope.push(format!("group \"{group}\"")); }
    let scope=if scope.is_empty() { String::new() } else { format!(" in {}",scope.join(" in ")) };
    let hint_lines:Vec<_>=hints.iter().map(|h|format!("- {}: {}",h.name,h.hint)).collect();
    if matches.is_empty() {
        let head=format!("No catalog tools matched \"{query}\"{scope}. Your active tool set is unchanged.");
        if hints.is_empty() { return format!("{head} Try different keywords or a broader query."); }
        return format!("{head}\n\nThe query names a tool that is hidden in this session:\n{}",hint_lines.join("\n"));
    }
    let mut lines=vec![format!("Found {} tool(s) matching \"{query}\"{scope}. Nothing was activated; call one by name and it activates on that first call:",matches.len()),String::new()];
    for m in matches {
        let description=m.doc.description.as_deref().map(|s|s.split(|character|matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).filter(|piece|!piece.is_empty()).collect::<Vec<_>>().join(" ")).filter(|s|!s.is_empty()).unwrap_or_else(||"(no description)".into());
        let description=if description.encode_utf16().count()>160 { format!("{}...",String::from_utf16_lossy(&description.encode_utf16().take(157).collect::<Vec<_>>())) } else { description };
        lines.push(format!("- {} — {description}",m.name));
        if let Some(parameters)=parameters_of(&m.name).filter(Value::is_object) { lines.push(format!("  parameters: {parameters}")); }
    }
    if !hint_lines.is_empty() { lines.extend([String::new(),"The query also names a tool that is hidden in this session:".into()]); lines.extend(hint_lines); } lines.join("\n")
}
#[cfg(test)]
mod argument_tests {
    use super::*;
    #[test]
    fn native_renderer_reports_progress_and_machine_match_count() {
        use maho_ext_api::{AgentToolResult,ToolRenderContext,ToolRendererSession};
        for width in [40,80,120] {
            let context=ToolRenderContext {args:json!({"query":"documentation","source":"mcp","group":"docs"}),tool_call_id:"tool-search-render".into(),invalidate:std::rc::Rc::new(||{}),last_component:None,state:(),cwd:Default::default(),execution_started:false,args_complete:true,is_partial:true,expanded:false,show_images:false,image_protocol:None,is_error:false,has_result:None,spinner_frame:None};
            let mut slots=ToolRendererSession {renderers:std::sync::Arc::new(renderers()),context}.into_slots();let theme=maho_ext_api::Theme::default();
            let mut states=vec![slots.render_call(&theme,width).unwrap()];let mut result=AgentToolResult::text("");states.push(slots.render_result(&result,&theme,width).unwrap());
            slots.session.context.is_partial=false;result.details=json!({"matched":["read_docs","find_docs"]});states.push(slots.render_result(&result,&theme,width).unwrap());
            assert!(states.last().unwrap().join("\n").contains("2 tool(s)"));for lines in &states {assert!(lines.iter().all(|line|maho_tui::utils::visible_width(line)<=width));}
            println!("TOOL_SEARCH_RENDER_JSON={}",json!({"width":width,"states":states}));
        }
    }
    #[test] fn null_source_without_server_still_defaults_to_mcp() { assert_eq!(prepare_tool_search_arguments(json!({"query":"files","source":null})),json!({"query":"files","source":"mcp"})); }
    #[test] fn null_group_without_server_is_omitted() { assert_eq!(prepare_tool_search_arguments(json!({"query":"files","group":null})),json!({"query":"files"})); }
}

#[cfg(test)]
mod exported_prefix_tests {
    use super::*;
    use maho_ext_api::{ToolRenderContext,ToolRendererSession,AgentToolResult};
    use maho_interactive::theme::{Theme,ThemeColor,ThemeBg,theme::ColorMode};
    use serde_json::json;
    #[test]
    fn callbacks_preserve_current_exported_prefix_bytes() {
        for mode in [ColorMode::Color256,ColorMode::Truecolor] {
            let native=Theme::builtin("dark",mode).unwrap();
            let exported=maho_ext_api::Theme {name:Some(native.name.clone()),colors:ThemeColor::ALL.iter().map(|color|(color.key().into(),native.get_fg_ansi(*color))).collect(),backgrounds:ThemeBg::ALL.iter().map(|bg|(bg.key().into(),native.get_bg_ansi(*bg))).collect(),vars:Default::default()};
            let context=ToolRenderContext {args:json!({"query":"documentation"}),tool_call_id:"prefix-proof".into(),invalidate:std::rc::Rc::new(||{}),last_component:None,state:(),cwd:Default::default(),execution_started:false,args_complete:true,is_partial:false,expanded:true,show_images:false,image_protocol:None,is_error:false,has_result:None,spinner_frame:None};
            let mut slots=ToolRendererSession {renderers:std::sync::Arc::new(renderers()),context}.into_slots();
            let call=slots.render_call(&exported,80).unwrap().join("
");
            assert!(call.contains(&native.get_fg_ansi(ThemeColor::ToolTitle)));
            let result=slots.render_result(&AgentToolResult::text("native output"),&exported,80).unwrap().join("
");
            for output in [&call,&result] {assert!(!output.is_empty());if mode==ColorMode::Color256 {assert!(!output.contains("\x1b[38;2;"));assert!(!output.contains("\x1b[48;2;"));}}
            println!("CONSUMER_PREFIX_JSON={}",json!({"consumer":"maho-ext-tool-search/src/tool.rs","mode":format!("{mode:?}"),"call":call,"result":result}));
        }
    }
}
