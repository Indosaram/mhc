use serde_json::Value;
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
    fn renderer_omits_model_only_parts() {
        use maho_tools::definition::ToolContent;
        assert_eq!(visible_result_text(&[ToolContent::text("visible"),ToolContent::Text {text:"hidden".to_owned(),audience:Some("model".to_owned())},ToolContent::text("next")]),"visible\nnext");
    }
}
