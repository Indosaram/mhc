use super::types::SearchAttempt;
fn theme(source:&maho_ext_api::Theme)->maho_interactive::theme::Theme {
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
}
fn text<'a>(value:&'a serde_json::Value,key:&str)->&'a str {value[key].as_str().unwrap_or("")}
fn shorten(value:&str,max:usize)->String {
    if value.encode_utf16().count()<=max {return value.into();}
    let mut units=0;let prefix=value.chars().take_while(|character|{units+=character.len_utf16();units<max}).collect::<String>();format!("{prefix}…")
}
fn provider(value:&serde_json::Value)->String {super::search::provider_entry_label(text(value,"provider"),None,value["entryId"].as_str())}
fn attempt(value:&serde_json::Value)->String {format!("{}:{}",provider(value),if !text(value,"error").is_empty(){"failed".into()}else{value["resultsCount"].as_u64().unwrap_or(0).to_string()})}
pub fn renderers()->maho_ext_api::ToolRenderers<(),serde_json::Value> {
    use maho_interactive::theme::ThemeColor;
    use maho_tui::components::text::Text;
    use std::sync::Arc;
    maho_ext_api::ToolRenderers {
        render_call:Some(Arc::new(|args,source,_|{
            let theme=theme(source);let domains=args.get("allowed_domains").or_else(||args.get("blocked_domains")).and_then(serde_json::Value::as_array);
            let filter=domains.filter(|domains|!domains.is_empty()).map(|domains|theme.fg(ThemeColor::Muted,&format!(" domains:{}",domains.len()))).unwrap_or_default();
            Box::new(Text::with_padding(format!("{}{}{}",theme.fg(ThemeColor::ToolTitle,&theme.bold("web_search ")),theme.fg(ThemeColor::Accent,&format!("\"{}\"",shorten(text(args,"query"),90))),filter),0,0))
        })),
        render_result:Some(Arc::new(|result,options,source,_|{
            let theme=theme(source);let details=&result.details;
            let fallback=result.content.first().and_then(|block|match block {maho_ext_api::ContentBlock::Text(text)=>Some(text.text.as_str()),_=>None});
            let output=if options.is_partial {
                if text(details,"phase")=="searching" {
                    let labels=details["providerLabels"].as_array().cloned().unwrap_or_default();
                    let current=text(details,"currentProvider");
                    let route=if !current.is_empty(){current.into()}else if labels.is_empty(){"configured providers".into()}else{labels.iter().filter_map(serde_json::Value::as_str).collect::<Vec<_>>().join(" -> ")};
                    let mut rows=vec![theme.fg(ThemeColor::Warning,&format!("Searching \"{}\" via {route}",shorten(text(details,"query"),80)))];
                    if options.expanded&&!current.is_empty() {
                        let labels=details["routeLabels"].as_array().unwrap_or(&labels);
                        let attempts=details["attempts"].as_array().cloned().unwrap_or_default();
                        let route=labels.iter().enumerate().map(|(index,label)|format!("{}:{}",label.as_str().unwrap_or(""),attempts.get(index).map(|a|if !text(a,"error").is_empty(){"failed".into()}else{a["resultsCount"].as_u64().unwrap_or(0).to_string()}).unwrap_or_else(||if index==attempts.len(){"searching".into()}else{"pending".into()}))).collect::<Vec<_>>().join(" -> ");
                        if !route.is_empty(){rows.push(theme.fg(ThemeColor::Muted,&format!("route {route}")));}
                    }
                    rows.join("\n")
                }else{theme.fg(ThemeColor::Warning,fallback.unwrap_or("Searching the web..."))}
            }else if details.is_null()||text(details,"phase")=="searching" {theme.fg(ThemeColor::Muted,fallback.unwrap_or(""))}
            else if text(details,"phase")=="error"||!text(details,"error").is_empty(){theme.fg(ThemeColor::Error,text(details,"error"))}
            else {
                let results=details["results"].as_array().cloned().unwrap_or_default();let count=results.len();
                let summary=format!("{}{}{}",theme.fg(ThemeColor::Success,&format!("{count} result{}",if count==1{""}else{"s"})),theme.fg(ThemeColor::Muted,&format!(" via {} in {}",provider(details),duration_text(details["durationMs"].as_f64().unwrap_or(0.)))),if details["truncated"]==true{theme.fg(ThemeColor::Warning," (truncated)")}else{String::new()});
                let mut rows=vec![summary];
                if count>0 {
                    if options.expanded&&let Some(attempts)=details["attempts"].as_array(){let route=attempts.iter().map(attempt).collect::<Vec<_>>().join(" -> ");if !route.is_empty(){rows.push(theme.fg(ThemeColor::Muted,&format!("route {route}")));}}
                    let limit=if options.expanded{8}else{3};
                    for item in results.iter().take(limit){rows.push(format!("{} {}",theme.fg(ThemeColor::Accent,&shorten(text(item,"title"),80)),theme.fg(ThemeColor::Dim,&shorten(text(item,"url"),100))));if !text(item,"snippet").is_empty(){rows.push(theme.fg(ThemeColor::Muted,&format!("  {}",shorten(text(item,"snippet"),140))));}}
                    if count>limit{rows.push(theme.fg(ThemeColor::Dim,&format!("… {} more sources",count-limit)));}
                }
                rows.join("\n")
            };
            Box::new(Text::with_padding(output,0,0))
        })),
    }
}
pub fn duration_text(duration_ms:f64)->String { if duration_ms>=1000.0 { format!("{}s",(duration_ms/1000.0).round()) } else { format!("{duration_ms}ms") } }
fn attempt_status(attempt:&SearchAttempt)->String { if attempt.error.as_ref().is_some_and(|error|!error.is_empty()) { "failed".into() } else { attempt.results_count.to_string() } }
pub fn attempt_label(attempts:Option<&[SearchAttempt]>)->String { attempts.unwrap_or(&[]).iter().map(|attempt|format!("{}:{}",super::search::provider_entry_label(attempt.provider.as_str(),None,attempt.entry_id.as_deref()),attempt_status(attempt))).collect::<Vec<_>>().join(" -> ") }
pub fn route_state_label(provider_labels:&[String],route_labels:Option<&[String]>,attempts:&[SearchAttempt])->String {
    route_labels.unwrap_or(provider_labels).iter().enumerate().map(|(index,label)|format!("{label}:{}",attempts.get(index).map_or_else(||if index==attempts.len() { "searching".into() } else { "pending".into() },attempt_status))).collect::<Vec<_>>().join(" -> ")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn duration_threshold_rounds_seconds() { assert_eq!(duration_text(999.0),"999ms"); assert_eq!(duration_text(1500.0),"2s"); }
    #[test] fn route_override_preserves_empty_array() { let labels=["one".into(),"two".into()]; assert_eq!(route_state_label(&labels,None,&[]),"one:searching -> two:pending"); assert_eq!(route_state_label(&labels,Some(&[]),&[]),""); }
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
            println!("CONSUMER_PREFIX_JSON={}",json!({"consumer":"maho-ext-websearch/src/websearch/renderers.rs","mode":format!("{mode:?}"),"call":call,"result":result}));
        }
    }
}
