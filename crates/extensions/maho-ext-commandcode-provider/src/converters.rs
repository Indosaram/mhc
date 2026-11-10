use maho_ai::types::{ContentBlock, Message, UserContent};
use serde_json::{Value, json};
use std::collections::HashSet;

pub fn messages_to_cc(messages: &[Message], supports_images: bool) -> Result<Vec<Value>, String> {
    let calls: HashSet<String> = messages
        .iter()
        .filter_map(|m| {
            if let Message::Assistant(a) = m {
                Some(
                    a.content
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::ToolCall(c) = b {
                                Some(c.id.clone())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            }
        })
        .flatten()
        .collect();
    let results: HashSet<String> = messages
        .iter()
        .filter_map(|m| {
            if let Message::ToolResult(r) = m {
                Some(r.tool_call_id.clone())
            } else {
                None
            }
        })
        .collect();
    let mut out = vec![];
    for m in messages {
        match m {
            Message::User(u) => {
                let parts = match &u.content {
                    UserContent::Text(s) => json!(s),
                    UserContent::Blocks(bs) => {
                        let mut p = vec![];
                        for b in bs {
                            match b {
                                ContentBlock::Text(t) => {
                                    p.push(json!({"type":"text","text":t.text}))
                                }
                                ContentBlock::Image(i) => {
                                    if !supports_images {
                                        return Err("Selected Command Code model does not support image content in user".into());
                                    }
                                    p.push(image(i.data.as_str(), i.mime_type.as_str()))
                                }
                                _ => {}
                            }
                        }
                        json!(p)
                    }
                };
                out.push(json!({"role":"user","content":parts}));
            }
            Message::Assistant(a) => {
                let mut p = vec![];
                let mut missing = vec![];
                for b in &a.content {
                    match b{ContentBlock::Text(t)=>p.push(json!({"type":"text","text":t.text})),ContentBlock::ToolCall(c)=>{p.push(json!({"type":"tool-call","toolCallId":c.id,"toolName":c.name,"input":c.arguments}));if !results.contains(&c.id){missing.push(json!({"type":"tool-result","toolCallId":c.id,"toolName":c.name,"output":{"type":"error-text","value":"No result — the tool call did not complete (interrupted or lost)."}}));}},ContentBlock::Image(_)=>return Err("Selected Command Code model does not support image content in assistant".into()),_=>{}}
                }
                if !p.is_empty() {
                    out.push(json!({"role":"assistant","content":p}))
                }
                if !missing.is_empty() {
                    out.push(json!({"role":"tool","content":missing}))
                }
            }
            Message::ToolResult(r) => {
                if !calls.contains(&r.tool_call_id) {
                    continue;
                }
                let mut text = String::new();
                let mut images = vec![];
                for b in &r.content {
                    match b {
                        ContentBlock::Text(t) => {
                            if !text.is_empty() {
                                text.push('\n')
                            }
                            text.push_str(&t.text)
                        }
                        ContentBlock::Image(i) => images.push(i),
                        _ => {}
                    }
                }
                if !images.is_empty() && !supports_images {
                    return Err(
                        "Selected Command Code model does not support image content in toolResult"
                            .into(),
                    );
                }
                if text.is_empty() && !images.is_empty() {
                    text = "[Image omitted: model does not support images]".into()
                }
                out.push(json!({"role":"tool","content":[{"type":"tool-result","toolCallId":r.tool_call_id,"toolName":r.tool_name,"output":{"type":if r.is_error{"error-text"}else{"text"},"value":text}}]}));
                for i in images {
                    out.push(json!({"role":"user","content":[image(&i.data,&i.mime_type)]}));
                }
            }
            _ => {}
        }
    }
    Ok(out)
}
fn image(data: &str, mime: &str) -> Value {
    json!({"type":"image","image":format!("data:{mime};base64,{data}"),"mimeType":mime})
}
pub fn tools_to_json(tools: &[maho_ai::types::Tool]) -> Vec<Value> {
    tools.iter().map(|t|json!({"type":"function","name":t.name,"description":t.description,"input_schema":crate::json_schema::to_json_schema(&t.parameters)})).collect()
}
pub fn system_prompt_to_text(prompt: &Value) -> String {
    fn part(v: &Value, d: u8) -> String {
        if d > 10 {
            return String::new();
        }
        if let Some(s) = v.as_str() {
            return s.into();
        }
        if let Some(a) = v.as_array() {
            return a
                .iter()
                .map(|x| part(x, d + 1))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
        }
        if let Some(o) = v.as_object() {
            if let Some(s) = o.get("text").and_then(Value::as_str) {
                return s.into();
            }
            return o.get("content").map(|x| part(x, d + 1)).unwrap_or_default();
        }
        String::new()
    }
    if prompt.is_null() {
        String::new()
    } else if let Some(a) = prompt.as_array() {
        a.iter()
            .map(|x| part(x, 0))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    } else {
        part(prompt, 0)
    }
}
