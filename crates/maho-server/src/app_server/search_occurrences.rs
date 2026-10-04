use super::{history_pagination::inclusive_turn_history_cursor,registry::JsonRpcError,turn_log::LoggedTurn};
use maho_tui::components::{markdown_lexer::Lexer,markdown_token::Token};
use serde_json::{Value,json};

fn token_text(token: &Token) -> String {
    let joined = |tokens: &[Token],separator|tokens.iter().map(token_text).collect::<Vec<_>>().join(separator);
    match token {
        Token::Space {..}|Token::Hr {..}|Token::Br {..}=>" ".into(),
        Token::Code {text,..}|Token::Codespan {text,..}|Token::Escape {text,..}|Token::Html {text,..}|Token::LatexBlock {text,..}|Token::LatexInline {text,..}|Token::LatexLiteral {text,..}=>text.clone(),
        Token::List {items,..}=>joined(items," "),
        Token::Blockquote {tokens,..}|Token::ListItem {tokens,..}=>joined(tokens,""),
        Token::Table {header,rows,..}=>header.iter().chain(rows.iter().flatten()).map(|cell|joined(cell.tokens.tokens(),"")).collect::<Vec<_>>().join(" "),
        Token::Heading {tokens,..}|Token::Paragraph {tokens,..}|Token::Link {tokens,..}|Token::Image {tokens,..}|Token::Strong {tokens,..}|Token::Em {tokens,..}|Token::Del {tokens,..}=>joined(tokens.tokens(),""),
        Token::Text {text,tokens,..}=>tokens.as_ref().map_or_else(||text.clone(),|tokens|joined(tokens.tokens(),"")),
        Token::Def {..}|Token::Checkbox {..}=>String::new(),
    }
}
pub fn markdown_to_search_text(markdown: &str) -> String {Lexer::new().lex(markdown.trim()).iter().map(token_text).collect::<Vec<_>>().join(" ").split_whitespace().collect::<Vec<_>>().join(" ")}
pub(super) fn parse_occurrence_params(params: &Value) -> Result<(&str,&str,Option<&String>,usize),JsonRpcError> {
    let invalid = |message: &str|JsonRpcError::new(-32600,message);
    let id = params["threadId"].as_str().filter(|id|!id.is_empty()).ok_or_else(||invalid("thread/searchOccurrences requires a non-empty threadId"))?;
    let term = params["searchTerm"].as_str().filter(|term|!maho_ai::utils::js::trim(term).is_empty()).ok_or_else(||invalid("thread/searchOccurrences requires a non-empty searchTerm"))?;
    let cursor = match params.get("cursor") {None|Some(Value::Null)=>None,Some(Value::String(value))=>Some(value),_=>return Err(invalid("thread/searchOccurrences received an invalid cursor"))};
    let limit = match params.get("limit").filter(|value|!value.is_null()) {None=>50,Some(value)=>value.as_f64().filter(|value|value.fract() == 0.0 && *value >= 0.0 && *value <= f64::from(u32::MAX)).map(|value|value.clamp(1.0,250.0) as usize).ok_or_else(||invalid("thread/searchOccurrences received an invalid limit"))?};
    Ok((id,term,cursor,limit))
}
pub fn occurrences_response(params: &Value,turns: &[LoggedTurn]) -> Result<Value,JsonRpcError> {
    let (id,term,cursor,limit) = parse_occurrence_params(params)?;
    let invalid = |message: &str|JsonRpcError::new(-32600,message);
    let mut occurrences = Vec::new();
    for turn in turns {
        let final_agent = turn.items.iter().rposition(|item|item.get("type").is_some_and(|value|value == "agentMessage"));
        for (index,item) in turn.items.iter().enumerate() {
            let item = Value::Object(item.clone());
            if item["type"] != "userMessage" && (item["type"] != "agentMessage" || Some(index) != final_agent) {continue;}
            let Some(item_id) = item["id"].as_str().filter(|id|!id.is_empty()) else {continue};
            let text = if item["type"] == "agentMessage" {markdown_to_search_text(item["text"].as_str().unwrap_or_default())} else if let Some(text) = item["content"].as_str() {text.into()} else {item["content"].as_array().into_iter().flatten().filter(|input|input["type"] == "text").filter_map(|input|input["text"].as_str()).collect()};
            let lower = text.to_lowercase();let needle = term.to_lowercase();let mut offset = 0;let mut occurrence_index = 0;
            let mut spans = Vec::new();let mut lower_offset = 0;let mut original_offset = 0;
            for character in text.chars() {let folded = character.to_lowercase().collect::<String>();spans.push((lower_offset,lower_offset+folded.len(),original_offset,original_offset+character.len_utf8()));lower_offset += folded.len();original_offset += character.len_utf8();}
            while let Some(found) = lower[offset..].find(&needle) {
                let start = offset+found;let end = start+needle.len();offset = end;
                let first = spans.iter().find(|span|span.0 <= start && start < span.1);let last = spans.iter().find(|span|span.0 < end && end <= span.1);
                if let (Some(first),Some(last)) = (first,last) {
                    let prefix = &text[..first.2];let suffix = &text[last.3..];let before = prefix.chars().count().saturating_sub(49);
                    let snippet_start = prefix.char_indices().nth(before).map_or(prefix.len(),|(index,_)|index);
                    let snippet_end = last.3+suffix.char_indices().nth(96).map_or(suffix.len(),|(index,_)|index);
                    let leading = snippet_start > 0;let matched_start = usize::from(leading)*4+text[snippet_start..first.2].encode_utf16().count();
                    let value = json!({"turnId":turn.turn_id,"itemId":item_id,"snippet":format!("{}{}{}",if leading {"... "} else {""},&text[snippet_start..snippet_end],if snippet_end < text.len() {" ..."} else {""}),"snippetMatchRange":{"start":matched_start,"end":matched_start+text[first.2..last.3].encode_utf16().count()},"turnCursor":inclusive_turn_history_cursor(id.into(),turn.turn_id.clone())?});
                    occurrences.push((format!("{}\0{item_id}",turn.turn_id),occurrence_index,value));occurrence_index += 1;
                }
            }
        }
    }
    let start = if let Some(cursor) = cursor {
        let parsed: Value = serde_json::from_str(cursor).map_err(|_|invalid(&format!("invalid cursor: {cursor}")))?;
        if parsed["threadId"] != id || parsed["searchTerm"] != term || !parsed["candidateKey"].is_string() || !parsed["occurrenceIndex"].is_u64() {return Err(invalid(&format!("invalid cursor: {cursor}")));}
        occurrences.iter().position(|(key,index,_)|parsed["candidateKey"] == *key && parsed["occurrenceIndex"] == *index).ok_or_else(||invalid("invalid cursor: anchor is no longer present"))?
    } else {0};
    let end = start.saturating_add(limit).min(occurrences.len());
    let next = occurrences.get(end).map(|(key,index,_)|json!({"threadId":id,"searchTerm":term,"candidateKey":key,"occurrenceIndex":index}).to_string());
    Ok(json!({"data":occurrences[start..end].iter().map(|(_,_,value)|value).collect::<Vec<_>>(),"nextCursor":next}))
}
