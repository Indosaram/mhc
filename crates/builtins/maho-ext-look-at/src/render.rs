use std::sync::LazyLock;
use regex::Regex;
use crate::arguments::LookAtArgs;
pub fn renderers()->maho_ext_api::ToolRenderers<(),serde_json::Value> {
    use maho_interactive::theme::ThemeColor;
    use maho_tui::{components::text::Text,utils::truncate_to_width};
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
            let theme=theme(source);let args:LookAtArgs=serde_json::from_value(args.clone()).unwrap_or_else(|error|std::panic::panic_any(error));let sources=source_labels(&args);
            let summary=if sources.is_empty(){"no sources".into()}else{sources.join(", ")};
            Box::new(Text::with_padding(format!("{}{}\n{}{}",theme.fg(ThemeColor::ToolTitle,&theme.bold("look_at ")),theme.fg(ThemeColor::Accent,&summary),theme.fg(ThemeColor::Muted,"goal: "),theme.fg(ThemeColor::ToolOutput,&goal_preview(&args.goal))),0,0))
        })),
        render_result:Some(Arc::new(move |result,options,source,_|{
            let theme=theme(source);let model=result.details["model"].as_str().unwrap_or("").trim_matches(js_whitespace);let text=result.content.iter().find_map(|block|match block {maho_ext_api::ContentBlock::Text(text)=>Some(text.text.as_str()),_=>None}).unwrap_or("");let mut rows=vec![];
            if !model.is_empty(){rows.push(theme.fg(ThemeColor::Accent,&format!("[vision {model}]")));}
            if text.is_empty(){rows.push(theme.fg(if options.is_partial{ThemeColor::Warning}else{ThemeColor::Dim},if options.is_partial{"Analyzing media..."}else{"empty response"}));}
            else if options.expanded{rows.extend(text.split('\n').map(|line|theme.fg(ThemeColor::ToolOutput,line)));}
            else {let preview=text.split('\n').map(|line|line.trim_matches(js_whitespace)).filter(|line|!line.is_empty()).take(4).collect::<Vec<_>>();if preview.is_empty(){rows.push(theme.fg(ThemeColor::Dim,"empty response"));}else{rows.extend(preview.iter().map(|line|theme.fg(ThemeColor::ToolOutput,&format!("  {}",truncate_to_width(line,120,"…",false)))));}}
            Box::new(Text::with_padding(rows.join("\n"),0,0))
        })),
    }
}
static IMAGE_REFERENCE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?i)^[\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*(?:\[?Image #([1-9][0-9]*)(?:,[^\]\n]*)?\]?|(?:attachment|image)://([1-9][0-9]*))[\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*$").expect("literal pattern"));
fn js_whitespace(c:char)->bool { matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }
pub fn path_label(path:&str)->String {
    if let Some(captures)=IMAGE_REFERENCE.captures(path) { return format!("Image #{}",captures.get(1).or(captures.get(2)).expect("image index").as_str()); }
    let path=path.trim_end_matches('/'); path.rsplit('/').next().unwrap_or("").into()
}
pub fn source_labels(args:&LookAtArgs)->Vec<String> {
    let mut sources:Vec<_>=args.file_paths.iter().flatten().chain(args.file_path.iter()).filter(|value|!value.trim_matches(js_whitespace).is_empty()).map(|value|path_label(value)).collect();
    sources.extend(args.image_data_list.iter().flatten().chain(args.image_data.iter()).filter(|value|!value.trim_matches(js_whitespace).is_empty()).map(|_|"base64 input".into())); sources
}
pub fn goal_preview(goal:&str)->String {
    let goal=goal.split(js_whitespace).filter(|part|!part.is_empty()).collect::<Vec<_>>().join(" "); if goal.is_empty() { return "pending".into(); }
    if goal.encode_utf16().count()<=110 { return goal; }
    let mut units=0; let mut preview=String::new(); for character in goal.chars() { if units+character.len_utf16()>109 { break; } units+=character.len_utf16(); preview.push(character); } preview.push('…'); preview
}
pub fn text_content(content:&[serde_json::Value])->&str { content.iter().find(|block|block.get("type").and_then(serde_json::Value::as_str)==Some("text")).and_then(|block|block.get("text")).and_then(serde_json::Value::as_str).unwrap_or("") }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_callbacks_render_sources_progress_and_expansion() {
        use maho_ext_api::{AgentToolResult,ToolRenderContext,ToolRendererSession};
        use serde_json::json;
        for width in [40,80,120] {
            let context=ToolRenderContext {args:json!({"file_path":"image://2","goal":"extract chart labels"}),tool_call_id:"look-render".into(),invalidate:std::rc::Rc::new(||{}),last_component:None,state:(),cwd:Default::default(),execution_started:false,args_complete:true,is_partial:true,expanded:false,show_images:false,image_protocol:None,is_error:false,has_result:None,spinner_frame:None};
            let mut slots=ToolRendererSession {renderers:std::sync::Arc::new(renderers()),context}.into_slots();
            let theme=maho_ext_api::Theme::default();let mut states=vec![slots.render_call(&theme,width).unwrap()];
            let mut result=AgentToolResult::text("");states.push(slots.render_result(&result,&theme,width).unwrap());
            result=AgentToolResult::text("one\ntwo\nthree\nfour\nfive");result.details=json!({"model":"test/vision"});slots.session.context.is_partial=false;
            states.push(slots.render_result(&result,&theme,width).unwrap());assert!(!states.last().unwrap().join("\n").contains("five"));
            slots.session.context.expanded=true;states.push(slots.render_result(&result,&theme,width).unwrap());assert!(states.last().unwrap().join("\n").contains("five"));
            for lines in &states {assert!(lines.iter().all(|line|maho_tui::utils::visible_width(line)<=width));}
            println!("LOOK_RENDER_JSON={}",json!({"width":width,"states":states}));
        }
    }
    #[test] fn attachment_labels_and_posix_basename() { assert_eq!(path_label("[Image #2, attached]"),"Image #2"); assert_eq!(path_label("image://3"),"Image #3"); assert_eq!(path_label("/tmp/photo.png/"),"photo.png"); assert_eq!(path_label("\\tmp\\photo.png"),"\\tmp\\photo.png"); }
    #[test] fn labels_preserve_plural_then_singular_order() { let args=LookAtArgs{file_paths:Some(vec!["a".into()," ".into()]),file_path:Some("b".into()),image_data:Some("abc".into()),..Default::default()}; assert_eq!(source_labels(&args),["a","b","base64 input"]); }
    #[test] fn first_text_block_is_authoritative() { assert_eq!(text_content(&[serde_json::json!({"type":"text"}),serde_json::json!({"type":"text","text":"later"})]),""); }
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
            let context=ToolRenderContext {args:json!({"goal":"inspect","file_path":"image://1"}),tool_call_id:"prefix-proof".into(),invalidate:std::rc::Rc::new(||{}),last_component:None,state:(),cwd:Default::default(),execution_started:false,args_complete:true,is_partial:false,expanded:true,show_images:false,image_protocol:None,is_error:false,has_result:None,spinner_frame:None};
            let mut slots=ToolRendererSession {renderers:std::sync::Arc::new(renderers()),context}.into_slots();
            let call=slots.render_call(&exported,80).unwrap().join("
");
            assert!(call.contains(&native.get_fg_ansi(ThemeColor::ToolTitle)));
            let result=slots.render_result(&AgentToolResult::text("native output"),&exported,80).unwrap().join("
");
            for output in [&call,&result] {assert!(!output.is_empty());if mode==ColorMode::Color256 {assert!(!output.contains("\x1b[38;2;"));assert!(!output.contains("\x1b[48;2;"));}}
            println!("CONSUMER_PREFIX_JSON={}",json!({"consumer":"maho-ext-look-at/src/render.rs","mode":format!("{mode:?}"),"call":call,"result":result}));
        }
    }
}
