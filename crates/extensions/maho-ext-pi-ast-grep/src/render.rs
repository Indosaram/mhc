use maho_ext_api::{AgentToolResult, Theme, ToolRenderers};
use maho_tui::{components::text::Text, utils::truncate_to_width};
use serde_json::Value;
use std::sync::Arc;

const MAX_COLLAPSED_ERROR_LENGTH: usize = 180;
const MAX_COLLAPSED_FILES: usize = 3;
const MAX_EXPANDED_MATCHES: usize = 15;
const MAX_PATH_LENGTH: usize = 42;
const MAX_SNIPPET_LENGTH: usize = 160;

/// Applies upstream `theme.fg` / `theme.bold`; `None` renders the same text without ANSI styling.
#[derive(Clone, Copy)]
struct Paint<'a>(Option<&'a Theme>);
impl Paint<'_> {
    fn fg(&self, color: &str, text: &str) -> String {
        self.0
            .map_or_else(|| text.to_owned(), |theme| theme.fg(color, text))
    }
    fn bold(&self, text: &str) -> String {
        self.0
            .map_or_else(|| text.to_owned(), |theme| theme.bold(text))
    }
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}
fn shorten(path: &str) -> String {
    let path = path.replace('\\', "/");
    let home = std::env::var("HOME").unwrap_or_default().replace('\\', "/");
    let path = if !home.is_empty() {
        path.strip_prefix(&home)
            .map(|rest| format!("~{rest}"))
            .unwrap_or(path)
    } else {
        path
    };
    let units = path.encode_utf16().collect::<Vec<_>>();
    if units.len() > MAX_PATH_LENGTH {
        format!(
            "…{}",
            String::from_utf16_lossy(&units[units.len() - (MAX_PATH_LENGTH - 1)..])
        )
    } else if path.is_empty() {
        ".".into()
    } else {
        path
    }
}
fn truncate_error(message: &str) -> String {
    let units = message.encode_utf16().collect::<Vec<_>>();
    if units.len() <= MAX_COLLAPSED_ERROR_LENGTH {
        message.into()
    } else {
        format!(
            "{}…",
            String::from_utf16_lossy(&units[..MAX_COLLAPSED_ERROR_LENGTH - 1])
        )
    }
}
fn plural(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}
fn strings<'a>(value: &'a Value, key: &str) -> Option<Vec<&'a str>> {
    value
        .get(key)?
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect()
}

fn format_glob_badge(args: &Value, paint: Paint<'_>) -> String {
    let Some(globs) = strings(args, "globs") else {
        return String::new();
    };
    let Some(first) = globs.first() else {
        return String::new();
    };
    let suffix = if globs.len() > 1 {
        format!(" +{}", globs.len() - 1)
    } else {
        String::new()
    };
    paint.fg("dim", &format!(" [glob {first}{suffix}]"))
}

fn format_call(args: &Value, replace: bool, paint: Paint<'_>) -> String {
    let paths = strings(args, "paths").unwrap_or_default();
    let mut path = shorten(paths.first().copied().unwrap_or("."));
    if paths.len() > 1 {
        path.push_str(&format!(" +{}", paths.len() - 1));
    }
    let pattern = string(args, "pattern");
    let mut text = paint.fg(
        "toolTitle",
        &paint.bold(if replace {
            "ast_grep_replace "
        } else {
            "ast_grep_search "
        }),
    );
    text.push_str(&paint.fg("accent", &format!("/{pattern}/")));
    if replace {
        text.push_str(&paint.fg("dim", " → "));
        text.push_str(&paint.fg("accent", string(args, "rewrite")));
    }
    text.push_str(&paint.fg("toolOutput", &format!(" in {path}")));
    let lang = string(args, "lang");
    if !lang.is_empty() {
        text.push_str(&paint.fg("dim", &format!(" [{lang}]")));
    }
    text.push_str(&format_glob_badge(args, paint));
    if replace {
        if args.get("dryRun") != Some(&Value::Bool(false)) {
            text.push_str(&paint.fg("warning", " [dry-run]"));
        }
    } else if let Some(context) = args.get("context").and_then(Value::as_f64) {
        text.push_str(&paint.fg("dim", &format!(" [context {context}]")));
    }
    text
}

/// Plain (unstyled) `renderSearchCall` / `renderReplaceCall` text.
pub fn call_text(args: &Value, replace: bool) -> String {
    format_call(args, replace, Paint(None))
}
/// Themed `renderSearchCall` / `renderReplaceCall` text.
pub fn render_call_text(args: &Value, replace: bool, theme: &Theme) -> String {
    format_call(args, replace, Paint(Some(theme)))
}

fn valid_details(details: &Value, replace: bool) -> bool {
    details.get("pattern").is_some_and(Value::is_string)
        && details.get("lang").is_some_and(Value::is_string)
        && strings(details, "paths").is_some()
        && details.get("totalMatches").is_some_and(Value::is_number)
        && details.get("truncated").is_some_and(Value::is_boolean)
        && details.get("matches").and_then(Value::as_array).is_some_and(|matches| {
            matches.iter().all(|item| {
                item.get("file").is_some_and(Value::is_string)
                    && item.get("text").is_some_and(Value::is_string)
                    && item.get("lines").is_some_and(Value::is_string)
                    && item.pointer("/range/start/line").is_some_and(Value::is_number)
                    && item.pointer("/range/start/column").is_some_and(Value::is_number)
            })
        })
        && details.get("error").is_none_or(Value::is_string)
        // Upstream `isSearchDetails` checks `hint`; `isReplaceDetails` does not.
        && (replace || details.get("hint").is_none_or(Value::is_string))
        && details.get("truncatedReason").is_none_or(|value| {
            matches!(value.as_str(), Some("max_matches" | "max_output_bytes" | "timeout"))
        })
        && (!replace
            || (details.get("rewrite").is_some_and(Value::is_string)
                && details.get("dryRun").is_some_and(Value::is_boolean)))
}

fn format_result(
    result: &AgentToolResult,
    expanded: bool,
    is_error: bool,
    replace: bool,
    paint: Paint<'_>,
) -> String {
    let details = &result.details;
    if !valid_details(details, replace) {
        let output = result
            .content
            .iter()
            .find_map(|content| match content {
                maho_ext_api::ContentBlock::Text(content) => Some(content.text.trim()),
                _ => None,
            })
            .unwrap_or_default();
        return if is_error && !output.is_empty() {
            paint.fg("error", &format!("Error: {}", truncate_error(output)))
        } else if output.is_empty() {
            paint.fg("dim", "No output")
        } else {
            paint.fg("toolOutput", output)
        };
    }
    let error = string(details, "error");
    if !error.is_empty() {
        return paint.fg("error", &format!("Error: {}", truncate_error(error)));
    }
    let total = details["totalMatches"].as_f64().unwrap_or_default() as usize;
    if total == 0 {
        if replace {
            return paint.fg("dim", "No matches found to replace");
        }
        let mut text = paint.fg("dim", "No matches found");
        let hint = string(details, "hint");
        if !hint.is_empty() {
            text.push_str(&format!("\n{}", paint.fg("muted", hint)));
        }
        return text;
    }
    let matches = details["matches"].as_array().expect("validated array");
    let mut groups: Vec<(&str, Vec<&Value>)> = Vec::new();
    for item in matches {
        let file = string(item, "file");
        if let Some((_, matches)) = groups.iter_mut().find(|(existing, _)| *existing == file) {
            matches.push(item);
        } else {
            groups.push((file, vec![item]));
        }
    }
    let files = paint.fg(
        "muted",
        &format!(" • {}", plural(groups.len(), "file", "files")),
    );
    let mut text = if replace {
        let replacements = plural(total, "replacement", "replacements");
        if details["dryRun"] == true {
            paint.fg("warning", &format!("[DRY RUN] {replacements} previewed"))
        } else {
            paint.fg("success", &format!("Applied {replacements}"))
        }
    } else {
        paint.fg("success", &plural(total, "match", "matches"))
    };
    text.push_str(&files);
    if details["truncated"] == true {
        let reason = match string(details, "truncatedReason") {
            "max_matches" => "match limit reached",
            "max_output_bytes" => "output exceeded 1MB limit",
            "timeout" => "search timed out",
            _ => "results truncated",
        };
        text.push_str(&if expanded {
            format!(
                "\n{}",
                paint.fg("warning", &format!("[Truncated: {reason}]"))
            )
        } else {
            format!(
                " {}",
                paint.fg("warning", &format!("[truncated: {reason}]"))
            )
        });
    }
    if !expanded {
        let mut lines = Vec::new();
        for (file, matches) in groups.iter().take(MAX_COLLAPSED_FILES) {
            lines.push(paint.fg(
                "muted",
                &format!(
                    "  {} ({})",
                    shorten(file),
                    plural(matches.len(), "match", "matches")
                ),
            ));
        }
        if groups.len() > MAX_COLLAPSED_FILES {
            lines.push(paint.fg(
                "dim",
                &format!("  … {} more files", groups.len() - MAX_COLLAPSED_FILES),
            ));
        }
        if !lines.is_empty() {
            text.push_str(&format!("\n{}", lines.join("\n")));
        }
    } else {
        let mut lines = Vec::new();
        let mut count = 0;
        for (file, matches) in groups {
            if count >= MAX_EXPANDED_MATCHES {
                break;
            }
            lines.push(paint.fg("accent", &shorten(file)));
            for item in matches {
                if count >= MAX_EXPANDED_MATCHES {
                    break;
                }
                let snippet = string(item, "lines").trim();
                let snippet = if snippet.is_empty() {
                    string(item, "text").trim()
                } else {
                    snippet
                };
                let position = format!(
                    "{}:{}",
                    item["range"]["start"]["line"].as_f64().unwrap_or_default() + 1.0,
                    item["range"]["start"]["column"]
                        .as_f64()
                        .unwrap_or_default()
                        + 1.0
                );
                // pi-tui `truncateToWidth(text, maxWidth)` defaults its ellipsis to "...".
                lines.push(format!(
                    "  {}  {}",
                    paint.fg("muted", &position),
                    paint.fg(
                        "toolOutput",
                        &truncate_to_width(snippet, MAX_SNIPPET_LENGTH, "...", false)
                    )
                ));
                count += 1;
            }
        }
        if total > count {
            lines.push(paint.fg(
                "dim",
                &format!("… {} more matches not shown", total - count),
            ));
        }
        if !lines.is_empty() {
            text.push_str(&format!("\n\n{}", lines.join("\n")));
        }
    }
    text
}

/// Plain (unstyled) `renderSearchResult` / `renderReplaceResult` text.
pub fn result_text(
    result: &AgentToolResult,
    expanded: bool,
    is_error: bool,
    replace: bool,
) -> String {
    format_result(result, expanded, is_error, replace, Paint(None))
}
/// Themed `renderSearchResult` / `renderReplaceResult` text.
pub fn render_result_text(
    result: &AgentToolResult,
    expanded: bool,
    is_error: bool,
    replace: bool,
    theme: &Theme,
) -> String {
    format_result(result, expanded, is_error, replace, Paint(Some(theme)))
}

/// Upstream `reuseText(context)` reuses a previous `Text`; `Component` has no downcast here, so
/// each render returns a fresh zero-padding `Text` with the same content.
pub fn renderers(replace: bool) -> ToolRenderers<(), Value> {
    ToolRenderers {
        render_call: Some(Arc::new(move |args, theme, _| {
            Box::new(Text::with_padding(
                render_call_text(args, replace, theme),
                0,
                0,
            ))
        })),
        render_result: Some(Arc::new(move |result, options, theme, context| {
            Box::new(Text::with_padding(
                render_result_text(result, options.expanded, context.is_error, replace, theme),
                0,
                0,
            ))
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> AgentToolResult {
        serde_json::from_value(json!({"content":[{"type":"text","text":""}],"details":{"pattern":"console.$METHOD($$$)","lang":"typescript","paths":["src"],"matches":[
            {"file":"src/logger.ts","lines":"console.log(message);","text":"console.log(message);","range":{"start":{"line":0,"column":0}}},
            {"file":"src/console.ts","lines":"console.error(message);","text":"console.error(message);","range":{"start":{"line":7,"column":1}}},
            {"file":"src/logger.ts","lines":"console.warn(message);","text":"console.warn(message);","range":{"start":{"line":12,"column":2}}}],"totalMatches":3,"truncated":false}})).unwrap()
    }
    fn theme() -> Theme {
        let mut theme = Theme::default();
        for color in [
            "accent",
            "success",
            "error",
            "warning",
            "muted",
            "dim",
            "toolTitle",
            "toolOutput",
        ] {
            theme
                .colors
                .insert(color.into(), "\u{1b}[38;2;255;255;255m".into());
        }
        theme
    }
    fn strip_ansi(text: &str) -> String {
        regex::Regex::new("\u{1b}\\[[0-9;]*m")
            .unwrap()
            .replace_all(text, "")
            .into_owned()
    }
    #[test]
    fn collapsed_groups_match_reference() {
        let output = result_text(&fixture(), false, false, false);
        for expected in [
            "3 matches",
            "2 files",
            "src/logger.ts (2 matches)",
            "src/console.ts (1 match)",
        ] {
            assert!(output.contains(expected));
        }
    }
    #[test]
    fn expanded_locations_match_reference() {
        let output = result_text(&fixture(), true, false, false);
        for expected in [
            "3 matches",
            "2 files",
            "1:1",
            "13:3",
            "8:2",
            "console.error(message);",
        ] {
            assert!(output.contains(expected));
        }
        assert!(output.find("13:3").unwrap() < output.find("8:2").unwrap());
    }
    #[test]
    fn byte_truncation_matches_reference() {
        let mut result = fixture();
        result.details["truncated"] = json!(true);
        result.details["truncatedReason"] = json!("max_output_bytes");
        result.details["totalMatches"] = json!(15);
        let output = result_text(&result, false, false, false);
        assert!(output.contains("output exceeded 1MB limit"));
        assert!(!output.contains("max_output_bytes"));
    }
    #[test]
    fn fallback_error_remains_visible() {
        let mut result = fixture();
        result.details = json!({});
        result.content = serde_json::from_value(
            json!([{"type":"text","text":"Output too large and could not be parsed"}]),
        )
        .unwrap();
        assert_eq!(
            result_text(&result, false, true, false),
            "Error: Output too large and could not be parsed"
        );
    }
    #[test]
    fn dry_run_replacement_matches_reference() {
        let mut result = fixture();
        result.details["rewrite"] = json!("logger.info($MSG)");
        result.details["dryRun"] = json!(true);
        let output = result_text(&result, false, false, true);
        assert!(output.contains("[DRY RUN] 3 replacements previewed"));
        assert!(output.contains("2 files"));
    }
    #[test]
    fn themed_result_strips_to_plain_reference_text() {
        let theme = theme();
        for expanded in [false, true] {
            let themed = render_result_text(&fixture(), expanded, false, false, &theme);
            assert!(themed.contains('\u{1b}'));
            assert_eq!(
                strip_ansi(&themed),
                result_text(&fixture(), expanded, false, false)
            );
        }
    }
    #[test]
    fn themed_call_uses_bold_title_and_strips_to_plain_text() {
        let args = json!({"pattern":"console.log($MSG)","rewrite":"logger.info($MSG)","lang":"typescript","paths":["src","lib"],"globs":["*.ts"]});
        let themed = render_call_text(&args, true, &theme());
        assert!(themed.contains("\u{1b}[1mast_grep_replace \u{1b}[22m"));
        assert_eq!(
            strip_ansi(&themed),
            "ast_grep_replace /console.log($MSG)/ → logger.info($MSG) in src +1 [typescript] [glob *.ts] [dry-run]"
        );
    }
}
