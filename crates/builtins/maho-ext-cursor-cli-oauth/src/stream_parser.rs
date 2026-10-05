use serde_json::{Value, json};

#[derive(Clone, Copy)]
pub struct ParserOptions {
    pub max_pending_bytes: usize,
    pub max_diagnostics: usize,
    pub max_diagnostic_characters: usize,
}
impl Default for ParserOptions {
    fn default() -> Self { Self { max_pending_bytes: 1024 * 1024, max_diagnostics: 20, max_diagnostic_characters: 2048 } }
}
pub struct CursorCliStreamParser {
    options: ParserOptions,
    undecoded: Vec<u8>,
    pending: String,
    discarding: bool,
    finalized: bool,
    saw_result: bool,
    diagnostics: Vec<String>,
    unknown_count: usize,
}

fn malformed(reason: &str, message: &str) -> Value {
    json!({"type":"malformed_stream","kind":"malformed_stream","reason":reason,"message":message})
}

impl CursorCliStreamParser {
    pub fn new(mut options: ParserOptions) -> Self {
        let defaults = ParserOptions::default();
        if options.max_pending_bytes == 0 { options.max_pending_bytes = defaults.max_pending_bytes; }
        if options.max_diagnostics == 0 { options.max_diagnostics = defaults.max_diagnostics; }
        if options.max_diagnostic_characters == 0 { options.max_diagnostic_characters = defaults.max_diagnostic_characters; }
        Self { options, undecoded: Vec::new(), pending: String::new(), discarding: false, finalized: false,
            saw_result: false, diagnostics: Vec::new(), unknown_count: 0 }
    }
    pub fn diagnostics(&self) -> &[String] { &self.diagnostics }
    pub fn unknown_event_count(&self) -> usize { self.unknown_count }
    pub fn reset(&mut self) {
        self.undecoded.clear(); self.pending.clear(); self.discarding = false;
        self.finalized = false; self.saw_result = false; self.diagnostics.clear(); self.unknown_count = 0;
    }
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Value> {
        if self.finalized { self.reset(); }
        self.undecoded.extend_from_slice(chunk);
        let decoded = self.decode(false);
        self.consume(&decoded)
    }
    fn decode(&mut self, finish: bool) -> String {
        let mut decoded = String::new();
        loop {
            match std::str::from_utf8(&self.undecoded) {
                Ok(text) => { decoded.push_str(text); self.undecoded.clear(); break; }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if let Ok(prefix) = std::str::from_utf8(&self.undecoded[..valid]) { decoded.push_str(prefix); }
                    self.undecoded.drain(..valid);
                    match error.error_len() {
                        Some(length) => { decoded.push('\u{fffd}'); self.undecoded.drain(..length); }
                        None => { if finish { decoded.push('\u{fffd}'); self.undecoded.clear(); } break; }
                    }
                }
            }
        }
        decoded
    }
    pub fn finish(&mut self) -> Vec<Value> {
        if self.finalized { return Vec::new(); }
        let decoded = self.decode(true);
        let mut events = self.consume(&decoded);
        if self.discarding { self.discarding = false; }
        else if !self.pending.is_empty() {
            let tail = std::mem::take(&mut self.pending);
            events.extend(self.parse_line(&tail, true));
        }
        if !self.saw_result { events.push(malformed("incomplete_stream", "cursor-agent stream ended without a result event")); }
        self.finalized = true;
        events
    }
    fn consume(&mut self, decoded: &str) -> Vec<Value> {
        let mut events = Vec::new();
        for fragment in decoded.split_inclusive('\n') {
            let newline = fragment.ends_with('\n');
            let fragment = fragment.strip_suffix('\n').unwrap_or(fragment);
            if !fragment.is_empty() && !self.discarding {
                if self.pending.len().saturating_add(fragment.len()) > self.options.max_pending_bytes {
                    self.record_diagnostic(&format!("{}{fragment}", self.pending));
                    self.pending.clear(); self.discarding = true;
                    events.push(malformed("line_overflow", &format!("stream line exceeded {} bytes", self.options.max_pending_bytes)));
                } else { self.pending.push_str(fragment); }
            }
            if newline {
                if self.discarding { self.discarding = false; }
                else {
                    let line = std::mem::take(&mut self.pending);
                    events.extend(self.parse_line(line.strip_suffix('\r').unwrap_or(&line), false));
                }
            }
        }
        events
    }
    fn record_diagnostic(&mut self, line: &str) {
        let units: Vec<_> = line.encode_utf16().take(self.options.max_diagnostic_characters).collect();
        self.diagnostics.push(String::from_utf16_lossy(&units));
        if self.diagnostics.len() > self.options.max_diagnostics { self.diagnostics.remove(0); }
    }
    fn parse_line(&mut self, line: &str, tail: bool) -> Vec<Value> {
        if line.trim().is_empty() { return Vec::new(); }
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => {
                self.record_diagnostic(line);
                return vec![if tail { malformed("truncated_tail", "truncated JSON tail") }
                    else { malformed("invalid_json", "non-JSON stream line") }];
            }
        };
        let Some(kind) = value["type"].as_str() else { return vec![malformed("invalid_event", "stream line is not an event object")]; };
        if kind == "user" { return Vec::new(); }
        if !["system","thinking","assistant","tool_call","result"].contains(&kind) {
            self.unknown_count += 1; return Vec::new();
        }
        match normalize_event(&value) {
            Some(event) => { if kind == "result" { self.saw_result = true; } vec![event] }
            None => vec![malformed("invalid_event", &format!("invalid {kind} event"))],
        }
    }
}

fn strings(value: &Value, fields: &[&str]) -> bool { fields.iter().all(|key| value[*key].is_string()) }
fn numbers(value: &Value, fields: &[&str]) -> bool { fields.iter().all(|key| value[*key].is_number()) }
fn normalize_event(value: &Value) -> Option<Value> {
    match value["type"].as_str()? {
        "system" if value["subtype"] == "init" && strings(value, &["session_id","model","apiKeySource","permissionMode","cwd"]) => {
            Some(json!({"type":"system","subtype":"init","session_id":value["session_id"],"model":value["model"],
                "apiKeySource":value["apiKeySource"],"permissionMode":value["permissionMode"],"cwd":value["cwd"]}))
        }
        "thinking" if ["delta","completed"].contains(&value["subtype"].as_str()?) => {
            let text = match value.get("text") { Some(value) => value.as_str()?, None => "" };
            Some(json!({"type":"thinking","subtype":value["subtype"],"text":text}))
        }
        "assistant" => {
            let content = value["message"]["content"].as_array()?;
            let mut normalized = Vec::new();
            for block in content {
                if block["type"] != "text" { return None; }
                normalized.push(json!({"type":"text","text":block["text"].as_str()?}));
            }
            Some(json!({"type":"assistant","message":{"content":normalized}}))
        }
        "tool_call" if ["started","completed"].contains(&value["subtype"].as_str()?) => {
            let id = value["call_id"].as_str()?;
            let (kind, details) = value["tool_call"].as_object()?.iter().find(|(key, value)| key.ends_with("ToolCall") && value.is_object())?;
            let mut normalized = serde_json::Map::new();
            if details["args"].is_object() { normalized.insert("args".into(), details["args"].clone()); }
            let result = &details["result"];
            let success = &result["success"];
            let rejected = &result["rejected"];
            if success.is_object() && numbers(success, &["exitCode","executionTime"]) && strings(success, &["stdout","stderr"]) {
                normalized.insert("result".into(), json!({"success":{"exitCode":success["exitCode"],"stdout":success["stdout"],
                    "stderr":success["stderr"],"executionTime":success["executionTime"]}}));
            } else if rejected.is_object() && strings(rejected, &["command","reason"]) && rejected["isReadonly"].is_boolean() {
                normalized.insert("result".into(), json!({"rejected":{"command":rejected["command"],"reason":rejected["reason"],"isReadonly":rejected["isReadonly"]}}));
            }
            Some(json!({"type":"tool_call","subtype":value["subtype"],"call_id":id,"tool_call":{kind:normalized}}))
        }
        "result" if ["success","error"].contains(&value["subtype"].as_str()?) => {
            let usage = &value["usage"];
            if !usage.is_object() || !numbers(usage, &["inputTokens","outputTokens","cacheReadTokens","cacheWriteTokens"])
                || !value["request_id"].is_string() || !value["duration_ms"].is_number() || !value["is_error"].is_boolean() { return None; }
            Some(json!({"type":"result","subtype":value["subtype"],"result":value["result"],"usage":{
                "inputTokens":usage["inputTokens"],"outputTokens":usage["outputTokens"],"cacheReadTokens":usage["cacheReadTokens"],"cacheWriteTokens":usage["cacheWriteTokens"]},
                "request_id":value["request_id"],"duration_ms":value["duration_ms"],"is_error":value["is_error"]}))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture(text: &str) -> String {
        text.lines().filter(|line| !line.is_empty()).map(|line| line.split_once(' ').unwrap().1)
            .collect::<Vec<_>>().join("\n") + "\n"
    }
    fn replay(text: &str, width: usize) -> Vec<Value> {
        let mut parser = CursorCliStreamParser::new(Default::default());
        let mut events: Vec<_> = text.as_bytes().chunks(width).flat_map(|chunk| parser.push(chunk)).collect();
        events.extend(parser.finish()); events
    }
    #[test]
    fn replays_all_upstream_captures() {
        for source in [include_str!("../tests/fixtures/run-a-events.jsonl"), include_str!("../tests/fixtures/run-c-noforce.jsonl"),
            include_str!("../tests/fixtures/run-d-force.jsonl"), include_str!("../tests/fixtures/run-f-events.jsonl")] {
            let text = capture(source); let expected = replay(&text, text.len());
            assert_eq!(replay(&text, 1), expected); assert_eq!(replay(&text, 17), expected); assert_eq!(replay(&text, 113), expected);
            assert_eq!(expected.last().unwrap()["type"], "result");
            assert!(expected.iter().all(|event| event["type"] != "malformed_stream"));
        }
    }
    #[test]
    fn extracts_real_dialect_fields() {
        let events = replay(&capture(include_str!("../tests/fixtures/run-a-events.jsonl")), 17);
        assert_eq!(events[0]["session_id"], "14e3d8df-06e5-48d5-a1de-00edae06bddd");
        assert_eq!(events.iter().filter(|e| e["type"] == "thinking").count(), 15);
        assert_eq!(events.iter().filter(|e| e["type"] == "assistant").count(), 7);
        assert_eq!(events.last().unwrap()["usage"], json!({"inputTokens":10389,"outputTokens":642,"cacheReadTokens":8928,"cacheWriteTokens":0}));
    }
    #[test]
    fn extracts_successful_and_rejected_tool_results() {
        let forced = replay(&capture(include_str!("../tests/fixtures/run-d-force.jsonl")), 17);
        let completed = forced.iter().find(|e| e["type"] == "tool_call" && e["subtype"] == "completed").unwrap();
        assert_eq!(completed["tool_call"]["shellToolCall"]["result"], json!({"success":{"exitCode":0,"stdout":"tooltest-force-77\n","stderr":"","executionTime":1299}}));
        let rejected = replay(&capture(include_str!("../tests/fixtures/run-c-noforce.jsonl")), 17);
        let rejection = rejected.iter().find(|e| e["tool_call"]["shellToolCall"]["result"].get("rejected").is_some()).unwrap();
        assert_eq!(rejection["tool_call"]["shellToolCall"]["result"], json!({"rejected":{"command":"echo tooltest-42","reason":"","isReadonly":false}}));
    }
    #[test]
    fn preserves_all_incremental_fragments() {
        let events = replay(&capture(include_str!("../tests/fixtures/run-f-events.jsonl")), 17);
        assert_eq!(events.iter().filter(|e| e["type"] == "assistant").count(), 188);
    }
    fn result() -> String { format!("{}\n", json!({"type":"result","subtype":"success","result":"ok","usage":{
        "inputTokens":0,"outputTokens":1,"cacheReadTokens":0,"cacheWriteTokens":0},"request_id":"req","duration_ms":1,"is_error":false})) }
    #[test]
    fn adversarial_unicode_chunks() {
        let input = format!("{}\n{}", json!({"type":"assistant","message":{"content":[{"type":"text","text":"한글 🍀"}]}}), result());
        let mut baseline = CursorCliStreamParser::new(Default::default());
        let expected = baseline.push(input.as_bytes());
        let mut parser = CursorCliStreamParser::new(Default::default());
        let actual: Vec<_> = input.as_bytes().iter().flat_map(|byte| parser.push(&[*byte])).collect();
        assert_eq!(actual, expected); assert!(parser.finish().is_empty());
    }
    #[test]
    fn truncated_and_garbage() {
        let mut parser = CursorCliStreamParser::new(Default::default());
        parser.push(b"{\"type\":\"assistant\",\"message\":");
        assert_eq!(parser.finish()[0]["reason"], "truncated_tail");
        assert_eq!(parser.push(b"banner\n")[0]["reason"], "invalid_json");
        assert_eq!(parser.diagnostics(), ["banner"]);
    }
    #[test]
    fn unknown_events_counted() {
        let mut parser = CursorCliStreamParser::new(Default::default());
        let events = parser.push(format!("{{\"type\":\"future\"}}\n{}", result()).as_bytes());
        assert_eq!(events.len(), 1); assert_eq!(parser.unknown_event_count(), 1);
    }
    #[test]
    fn pending_and_diagnostics_bounded() {
        let mut parser = CursorCliStreamParser::new(ParserOptions { max_pending_bytes:32, max_diagnostics:2, max_diagnostic_characters:8 });
        let events = parser.push(format!("{}\nfirst garbage\nsecond garbage\nthird garbage\n", "x".repeat(40)).as_bytes());
        assert_eq!(events.len(), 4); assert_eq!(parser.diagnostics(), ["second g", "third ga"]);
    }
    #[test]
    fn incomplete_stream() {
        let mut parser = CursorCliStreamParser::new(Default::default());
        parser.push(b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n");
        assert_eq!(parser.finish().last().unwrap()["reason"], "incomplete_stream");
    }
    #[test]
    fn reuse_does_not_leak_state() {
        let mut parser = CursorCliStreamParser::new(Default::default());
        parser.push(b"{\"type\":\"assistant\""); parser.finish();
        assert_eq!(parser.push(result().as_bytes()).len(), 1); assert!(parser.finish().is_empty());
        assert!(parser.diagnostics().is_empty());
    }
}
