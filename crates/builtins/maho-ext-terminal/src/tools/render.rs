use serde_json::Value;
use maho_tui::tui::Component;
pub struct BashOutputResultComponent {pub text:String,pub is_partial:bool,pub expanded:bool}
impl Component for BashOutputResultComponent {
    fn invalidate(&mut self) {}
    fn render(&mut self,width:usize)->Vec<String> {
        if !self.is_partial||self.expanded {return self.text.split('\n').map(str::to_owned).collect();}
        if self.text.is_empty() {return vec![];}
        let mut text=maho_tui::components::text::Text::with_padding(&self.text,0,0);
        let lines=text.render(width.max(1));lines.iter().skip(lines.len().saturating_sub(8)).map(|line|line.trim_end().to_owned()).collect()
    }
}
pub fn output_renderers()->maho_ext_api::types::ToolRenderers<(),Value> {
    maho_ext_api::types::ToolRenderers {render_call:Some(std::sync::Arc::new(|args,theme,_|call_component(&bash_output_call_label(args),theme))),render_result:Some(std::sync::Arc::new(|result,options,_,_| {
        let text=result.content.iter().filter_map(|part|match part {
            maho_ai::types::ContentBlock::Text(part) if part.audience!=Some(maho_ai::types::TextAudience::Model)=>Some(part.text.as_str()),_=>None,
        }).collect::<Vec<_>>().join("\n");
        Box::new(BashOutputResultComponent {text,is_partial:options.is_partial,expanded:options.expanded})
    }))}
}
fn call_component(label:&str,theme:&maho_ext_api::types::Theme)->Box<dyn Component> {
    let prefix=theme.colors.get("toolTitle").and_then(|color|color.strip_prefix('#')).filter(|hex|hex.len()==6&&hex.is_ascii()).and_then(|hex| {
        Some(format!("\x1b[38;2;{};{};{}m",u8::from_str_radix(&hex[..2],16).ok()?,u8::from_str_radix(&hex[2..4],16).ok()?,u8::from_str_radix(&hex[4..],16).ok()?))
    }).unwrap_or_else(||"\x1b[39m".to_owned());
    Box::new(maho_tui::components::text::Text::with_padding(format!("{prefix}\x1b[1m{label}\x1b[22m\x1b[39m"),0,0))
}
pub fn monitor_renderers()->maho_ext_api::types::ToolRenderers<(),Value> {
    maho_ext_api::types::ToolRenderers {render_call:Some(std::sync::Arc::new(|args,theme,_|call_component(&monitor_call_label(args),theme))),render_result:None}
}
pub fn bash_output_call_label(args:&Value)->String {format!("bash_output {}",args.get("bash_id").and_then(Value::as_str).unwrap_or(""))}
pub fn monitor_call_label(args:&Value)->String {
    if args.get("action").and_then(Value::as_str)==Some("rearm") {return format!("monitor rearm {}",args.get("bash_id").and_then(Value::as_str).unwrap_or("")).trim_end().to_owned();}
    format!("monitor {}",args.get("description").or_else(||args.get("command")).and_then(Value::as_str).unwrap_or("")).trim_end().to_owned()
}
pub fn visible_result_text(content:&[maho_tools::definition::ToolContent])->String {
    content.iter().filter_map(|content|match content {maho_tools::definition::ToolContent::Text {text,audience} if audience.as_deref()!=Some("model")=>Some(text.as_str()),_=>None}).collect::<Vec<_>>().join("\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn themed_call_components_keep_tool_title_color_and_bold() {
        let theme=maho_ext_api::types::Theme {colors:[("toolTitle".to_owned(),"#123456".to_owned())].into(),..Default::default()};
        for label in [bash_output_call_label(&serde_json::json!({"bash_id":"bash_1"})),monitor_call_label(&serde_json::json!({"action":"rearm","bash_id":"mon_1"}))] {
            let mut component=call_component(&label,&theme);let lines=component.render(80);
            assert!(lines[0].contains("\x1b[38;2;18;52;86m\x1b[1m"));assert!(lines[0].contains(&label));assert!(lines[0].contains("\x1b[22m\x1b[39m"));
        }
        assert!(output_renderers().render_call.is_some());assert!(monitor_renderers().render_call.is_some());assert!(monitor_renderers().render_result.is_none());
    }
    #[test]
    fn partial_results_keep_last_eight_visual_rows_and_full_results_keep_raw_rows() {
        let mut component=BashOutputResultComponent {text:(0..12).map(|index|format!("row{index}")).collect::<Vec<_>>().join("\n"),is_partial:true,expanded:false};
        assert_eq!(component.render(40),(4..12).map(|index|format!("row{index}")).collect::<Vec<_>>());
        component.expanded=true;assert_eq!(component.render(40).len(),12);
        component.text="abcdefghij".to_owned();component.expanded=false;assert_eq!(component.render(5),["abcde","fghij"]);
        component.text.clear();assert!(component.render(5).is_empty());component.expanded=true;assert_eq!(component.render(5),[""]);
    }
    #[test]
    fn renderer_omits_model_only_parts() {
        use maho_tools::definition::ToolContent;
        assert_eq!(visible_result_text(&[ToolContent::text("visible"),ToolContent::Text {text:"hidden".to_owned(),audience:Some("model".to_owned())},ToolContent::text("next")]),"visible\nnext");
    }
}
