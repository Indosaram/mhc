//! Port of session-info-format.ts.
use crate::theme::theme::{Theme, ThemeColor};
use serde_json::Value;
fn grouped(number: f64) -> String {
    let raw = format!("{:.0}", number);
    let mut result = String::new();
    for (i, c) in raw.chars().enumerate() {
        if i > 0 && (raw.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result
}
pub fn format_session_info(stats: &Value, name: Option<&str>, theme: &Theme) -> String {
    let number = |key: &str| stats.get(key).and_then(Value::as_f64).unwrap_or(0.);
    let label = |s: &str| theme.fg(ThemeColor::Dim, s);
    let mut info = format!("{}\n\n", theme.bold("Session Info"));
    if let Some(n) = name.filter(|n| !n.is_empty()) {
        info += &format!("{} {n}\n", label("Name:"));
    }
    info += &format!(
        "{} {}\n{} {}\n\n",
        label("File:"),
        stats
            .get("sessionFile")
            .and_then(Value::as_str)
            .unwrap_or("In-memory"),
        label("ID:"),
        stats.get("sessionId").and_then(Value::as_str).unwrap_or("")
    );
    info += &format!("{}\n", theme.bold("Messages"));
    for (label_text, key) in [
        ("User:", "userMessages"),
        ("Assistant:", "assistantMessages"),
        ("Tool Calls:", "toolCalls"),
        ("Tool Results:", "toolResults"),
        ("Total:", "totalMessages"),
    ] {
        info += &format!("{} {}\n", label(label_text), number(key));
    }
    info += &format!("\n{}\n", theme.bold("Tokens"));
    for (label_text, key, optional) in [
        ("Input:", "input", false),
        ("Output:", "output", false),
        ("Cache Read:", "cacheRead", true),
        ("Cache Write:", "cacheWrite", true),
        ("Total:", "total", false),
    ] {
        let n = stats
            .get("tokens")
            .and_then(|v| v.get(key))
            .and_then(Value::as_f64)
            .unwrap_or(0.);
        if !optional || n > 0. {
            info += &format!("{} {}\n", label(label_text), grouped(n));
        }
    }
    if let Some(ctx) = stats.get("contextUsage") {
        let window = ctx
            .get("contextWindow")
            .and_then(Value::as_f64)
            .unwrap_or(0.);
        if window > 0. {
            let used = ctx
                .get("tokens")
                .and_then(Value::as_f64)
                .map_or_else(|| "-".into(), grouped);
            let percent = ctx
                .get("percent")
                .and_then(Value::as_f64)
                .map_or_else(|| "-".into(), |v| format!("{}%", v.round()));
            info += &format!(
                "\n{}\n{} {used} / {} ({percent})",
                theme.bold("Context Window"),
                label("Used:"),
                grouped(window)
            );
        }
    }
    let cost = number("cost");
    if cost > 0. {
        let amount = format!("{cost:.2}");
        let mut parts = amount.split('.');
        let whole = parts.next().unwrap_or("0").parse::<f64>().unwrap_or(0.);
        let cost_unknown = stats
            .get("costUnknown")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let unpriced = if cost_unknown {
            format!(" {}", label("+ unpriced turns"))
        } else {
            String::new()
        };
        info += &format!(
            "\n\n{}\n{} ${}.{}{unpriced}",
            theme.bold("Cost"),
            label("Total:"),
            grouped(whole),
            parts.next().unwrap_or("00")
        );
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_session_info_with_unknown_cost() {
        let theme = Theme::builtin("dark", crate::theme::ColorMode::Truecolor).unwrap();
        let stats = json!({
            "sessionId": "test-id",
            "sessionFile": "session.json",
            "userMessages": 1,
            "assistantMessages": 1,
            "toolCalls": 0,
            "toolResults": 0,
            "totalMessages": 2,
            "tokens": { "input": 100, "output": 20, "cacheRead": 0, "cacheWrite": 0, "total": 120 },
            "cost": 0.5,
            "costUnknown": true
        });
        let output = format_session_info(&stats, Some("Test"), &theme);
        assert!(output.contains("+ unpriced turns"));
        assert!(output.contains("$0.50"));
    }

    #[test]
    fn format_session_info_without_unknown_cost() {
        let theme = Theme::builtin("dark", crate::theme::ColorMode::Truecolor).unwrap();
        let stats = json!({
            "sessionId": "test-id",
            "cost": 0.5,
            "costUnknown": false
        });
        let output = format_session_info(&stats, None, &theme);
        assert!(!output.contains("+ unpriced turns"));
        assert!(output.contains("$0.50"));
    }
}
