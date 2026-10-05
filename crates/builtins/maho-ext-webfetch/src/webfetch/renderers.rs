pub fn collect_lines(text:&str,limit:usize)->Vec<String> { text.split('\n').take(limit).map(String::from).collect() }
pub fn collect_non_empty_trimmed_lines(text:&str,limit:usize)->Vec<String> { text.split('\n').map(|line|line.trim_matches(|character:char|matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'))).filter(|line|!line.is_empty()).take(limit).map(String::from).collect() }
pub fn format_bytes(bytes:usize)->String { if bytes<1024 { format!("{bytes} B") } else if bytes<1024*1024 { format!("{:.1} KB",bytes as f64/1024.0) } else { format!("{:.1} MB",bytes as f64/(1024.0*1024.0)) } }
pub fn is_progress_details(details:&serde_json::Value)->bool { matches!(details.get("phase").and_then(serde_json::Value::as_str),Some("fetching"|"downloading"|"converting")) }
pub fn is_result_details(details:&serde_json::Value)->bool { details.get("status").is_some_and(serde_json::Value::is_number) }

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
fn shorten(value:&str,max:usize)->String {
    if value.encode_utf16().count()<=max {return value.into();}
    let mut units=0;let prefix=value.chars().take_while(|character|{units+=character.len_utf16();units<max}).collect::<String>();
    format!("{prefix}…")
}
fn string<'a>(value:&'a serde_json::Value,key:&str)->&'a str { value[key].as_str().unwrap_or("") }
fn number(value:&serde_json::Value,key:&str)->usize {value[key].as_u64().unwrap_or(0) as usize}
pub fn renderers()->maho_ext_api::ToolRenderers<(),serde_json::Value> {
    use maho_interactive::theme::ThemeColor;
    use maho_tui::{components::text::Text,utils::truncate_to_width};
    use std::sync::Arc;
    maho_ext_api::ToolRenderers {
        render_call:Some(Arc::new(|args,source,_|{
            let theme=theme(source);
            let timeout=args["timeout"].as_f64().map(|seconds|theme.fg(ThemeColor::Dim,&format!(" {seconds}s"))).unwrap_or_default();
            Box::new(Text::with_padding(format!("{}{}{}{}",theme.fg(ThemeColor::ToolTitle,&theme.bold("webfetch ")),theme.fg(ThemeColor::Accent,&shorten(string(args,"url"),92)),theme.fg(ThemeColor::Muted,&format!(" [{}]",args["format"].as_str().unwrap_or("markdown"))),timeout),0,0))
        })),
        render_result:Some(Arc::new(|result,options,source,_|{
            let theme=theme(source);let details=&result.details;
            let text=result.content.iter().find_map(|block|match block {maho_ext_api::ContentBlock::Text(text)=>Some(text.text.as_str()),_=>None}).unwrap_or("");
            let output=if options.is_partial {
                let progress=if is_progress_details(details) {
                    let url=shorten(string(details,"url"),92);
                    match string(details,"phase") {
                        "downloading"=>format!("Downloading {url}: {}{}",format_bytes(number(details,"bytesRead")),details.get("totalBytes").map(|_|format!(" / {}",format_bytes(number(details,"totalBytes")))).unwrap_or_default()),
                        "converting"=>format!("Converting {url} to {}",string(details,"format")),
                        _=>format!("Fetching {url} as {} ({}s)",string(details,"format"),details["timeoutSeconds"]),
                    }
                } else {"Fetching...".into()};
                theme.fg(ThemeColor::Warning,&progress)
            } else if !is_result_details(details) {theme.fg(ThemeColor::Muted,&truncate_to_width(text,120,"…",false))}
            else {
                let status=number(details,"status");let reason=string(details,"statusText");
                let header=format!("{} {} {} {} {}{}{}",theme.fg(if (200..300).contains(&status){ThemeColor::Success}else{ThemeColor::Warning},&format!("{status} {}",if reason.is_empty(){"OK"}else{reason})),theme.fg(ThemeColor::Muted,"•"),theme.fg(ThemeColor::Accent,string(details,"format")),theme.fg(ThemeColor::Muted,"•"),theme.fg(ThemeColor::Muted,&format_bytes(number(details,"bytes"))),if details["converted"]==true{theme.fg(ThemeColor::Dim," converted")}else{String::new()},if details["outputTruncated"]==true{theme.fg(ThemeColor::Warning," truncated")}else{String::new()});
                let mut lines=vec![header];
                if options.expanded {
                    lines.push(theme.fg(ThemeColor::Dim,&format!("URL: {}",shorten(string(details,"finalUrl"),92))));
                    lines.push(theme.fg(ThemeColor::Dim,&format!("Content-Type: {}",details["contentType"].as_str().filter(|value|!value.is_empty()).unwrap_or("unknown"))));lines.push(String::new());
                    lines.extend(collect_lines(text,24).iter().map(|line|theme.fg(ThemeColor::ToolOutput,&truncate_to_width(line,120,"…",false))));
                } else {
                    let preview=collect_non_empty_trimmed_lines(text,4);
                    if preview.is_empty(){lines.push(theme.fg(ThemeColor::Dim,"  empty response"));}
                    else {lines.extend(preview.iter().map(|line|theme.fg(ThemeColor::ToolOutput,&format!("  {}",truncate_to_width(line,120,"…",false)))));}
                }
                lines.join("\n")
            };
            Box::new(Text::with_padding(output,0,0))
        })),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn line_collection_preserves_final_empty_line() { assert_eq!(collect_lines("a\n",24),["a",""]); assert_eq!(collect_lines("",24),[""]); assert!(collect_lines("a",0).is_empty()); }
    #[test] fn preview_skips_blanks_and_stops_at_limit() { assert_eq!(collect_non_empty_trimmed_lines(" \n a \n\n b\nc",2),["a","b"]); }
    #[test] fn detail_guards_accept_only_machine_fields() { assert!(is_progress_details(&serde_json::json!({"phase":"downloading"}))); assert!(!is_progress_details(&serde_json::json!({"phase":"other"}))); assert!(is_result_details(&serde_json::json!({"status":404}))); assert!(!is_result_details(&serde_json::json!({"status":"404"}))); }
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
            let context=ToolRenderContext {args:json!({"url":"https://example.test","format":"text"}),tool_call_id:"prefix-proof".into(),invalidate:std::rc::Rc::new(||{}),last_component:None,state:(),cwd:Default::default(),execution_started:false,args_complete:true,is_partial:false,expanded:true,show_images:false,image_protocol:None,is_error:false,has_result:None,spinner_frame:None};
            let mut slots=ToolRendererSession {renderers:std::sync::Arc::new(renderers()),context}.into_slots();
            let call=slots.render_call(&exported,80).unwrap().join("
");
            assert!(call.contains(&native.get_fg_ansi(ThemeColor::ToolTitle)));
            let result=slots.render_result(&AgentToolResult::text("native output"),&exported,80).unwrap().join("
");
            for output in [&call,&result] {assert!(!output.is_empty());if mode==ColorMode::Color256 {assert!(!output.contains("\x1b[38;2;"));assert!(!output.contains("\x1b[48;2;"));}}
            println!("CONSUMER_PREFIX_JSON={}",json!({"consumer":"maho-ext-webfetch/src/webfetch/renderers.rs","mode":format!("{mode:?}"),"call":call,"result":result}));
        }
    }
}
