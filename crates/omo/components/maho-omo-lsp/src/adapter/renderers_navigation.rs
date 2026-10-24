use super::render_format::{shorten, uri_to_path};
use super::renderers_common::{
    COLLAPSED_HEAD, EXPANDED_HEAD, PATH_BUDGET, RenderResultOptions, RendererTheme, loc_text,
    position_args, render_position_call, result_details, unique,
};
use super::rendering::Text;
use serde_json::Value;
pub fn render_goto_definition_call(args: &Value, theme: &dyn RendererTheme) -> Text {
    Text::new(render_position_call(
        "lsp_goto_definition",
        &position_args(args),
        theme,
    ))
}
pub fn render_goto_definition_result(
    result: &Value,
    _options: RenderResultOptions,
    theme: &dyn RendererTheme,
) -> Text {
    let details = result_details(result);
    if let Some(error) = details
        .and_then(|d| d.get("error"))
        .and_then(Value::as_str)
        .filter(|e| !e.is_empty())
    {
        return Text::new(theme.fg("warning", error.split('\n').next().unwrap_or("error")));
    }
    let Some(locations) = details
        .and_then(|d| d.get("locations"))
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
    else {
        return Text::new(theme.fg("dim", "No definition found"));
    };
    let more = locations.len().saturating_sub(1);
    let head = format!(
        "{}{}",
        theme.fg("success", "→ "),
        theme.fg("accent", &loc_text(&locations[0]).unwrap_or_default())
    );
    let tail = if more > 0 {
        theme.fg("dim", &format!(" (+{more} more)"))
    } else {
        String::new()
    };
    Text::new(format!("{head}{tail}"))
}
pub fn render_find_references_call(args: &Value, theme: &dyn RendererTheme) -> Text {
    Text::new(render_position_call(
        "lsp_find_references",
        &position_args(args),
        theme,
    ))
}
pub fn render_find_references_result(
    result: &Value,
    options: RenderResultOptions,
    theme: &dyn RendererTheme,
) -> Text {
    let details = result_details(result);
    if let Some(error) = details
        .and_then(|d| d.get("error"))
        .and_then(Value::as_str)
        .filter(|e| !e.is_empty())
    {
        return Text::new(theme.fg("warning", error.split('\n').next().unwrap_or("error")));
    }
    let Some(details) = details else {
        return Text::new(theme.fg("dim", "No references"));
    };
    let total = details
        .get("totalReferences")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if total == 0 {
        return Text::new(theme.fg("dim", "No references"));
    }
    let references = details
        .get("references")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let unique_references = unique(references.clone(), |r| {
        r.get("uri")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    });
    let file_count = unique_references.len();
    let mut summary = theme.fg(
        "success",
        &format!("{total} reference{}", if total == 1 { "" } else { "s" }),
    );
    summary.push_str(&theme.fg(
        "muted",
        &format!(
            " • {file_count} file{}",
            if file_count == 1 { "" } else { "s" }
        ),
    ));
    if details
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        summary.push_str(&theme.fg("warning", " (truncated)"));
    }
    if !options.expanded {
        let mut lines = vec![summary];
        for reference in unique_references.iter().take(COLLAPSED_HEAD) {
            lines.push(theme.fg(
                "muted",
                &format!(
                        "  {}",
                        shorten(
                            &uri_to_path(
                                reference.get("uri").and_then(Value::as_str).unwrap_or("")
                            )
                            .unwrap_or_default(),
                            PATH_BUDGET
                        )
                    ),
            ));
        }
        if file_count > COLLAPSED_HEAD {
            lines.push(theme.fg(
                "dim",
                &format!("  … {} more files", file_count - COLLAPSED_HEAD),
            ));
        }
        return Text::new(lines.join("\n"));
    }
    let mut lines = vec![summary, String::new()];
    for reference in references.iter().take(EXPANDED_HEAD) {
        lines.push(theme.fg("accent", &loc_text(reference).unwrap_or_default()));
    }
    if total > EXPANDED_HEAD as u64 {
        lines.push(theme.fg(
            "dim",
            &format!("… {} more references", total - EXPANDED_HEAD as u64),
        ));
    }
    Text::new(lines.join("\n"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::renderers_common::test_theme::TagTheme;
    use serde_json::json;
    fn location(uri: &str, line: u64, character: u64) -> Value {
        json!({"uri":uri,"range":{"start":{"line":line,"character":character}}})
    }
    #[test]
    fn goto_call_uses_position_renderer() {
        assert!(
            render_goto_definition_call(
                &json!({"filePath":"/tmp/a.ts","line":2,"character":3}),
                &TagTheme
            )
            .text
            .ends_with("<accent>/tmp/a.ts:2:3</accent>")
        );
    }
    #[test]
    fn goto_without_locations_is_dim() {
        assert_eq!(
            render_goto_definition_result(
                &json!({"details":{"locations":[]}}),
                RenderResultOptions::default(),
                &TagTheme
            )
            .text,
            "<dim>No definition found</dim>"
        );
    }
    #[test]
    fn goto_head_plus_more() {
        let r = json!({"details":{"locations":[location("file:///tmp/a.ts",0,3),location("file:///tmp/b.ts",4,1)]}});
        assert!(
            render_goto_definition_result(&r, RenderResultOptions::default(), &TagTheme)
                .text
                .ends_with("<dim> (+1 more)</dim>")
        );
    }
    #[test]
    fn goto_error_warns() {
        assert_eq!(
            render_goto_definition_result(
                &json!({"details":{"error":"boom\nline"}}),
                RenderResultOptions::default(),
                &TagTheme
            )
            .text,
            "<warning>boom</warning>"
        );
    }
    #[test]
    fn references_without_total_are_dim() {
        assert_eq!(
            render_find_references_result(
                &json!({"details":{"totalReferences":0,"references":[]}}),
                RenderResultOptions::default(),
                &TagTheme
            )
            .text,
            "<dim>No references</dim>"
        );
    }
    #[test]
    fn references_collapsed_dedupes_files() {
        let r = json!({"details":{"totalReferences":2,"references":[location("file:///tmp/a.ts",0,0),location("file:///tmp/a.ts",1,0)]}});
        assert!(
            render_find_references_result(&r, RenderResultOptions::default(), &TagTheme)
                .text
                .contains("<muted> • 1 file</muted>")
        );
    }
    #[test]
    fn references_expanded_lists_locations() {
        let r = json!({"details":{"totalReferences":2,"references":[location("file:///tmp/a.ts",0,0),location("file:///tmp/b.ts",3,2)]}});
        assert!(
            render_find_references_result(
                &r,
                RenderResultOptions {
                    expanded: true,
                    is_partial: false
                },
                &TagTheme
            )
            .text
            .contains("/tmp/b.ts:4:2")
        );
    }
    #[test]
    fn references_note_beyond_head() {
        let locations = (0..21)
            .map(|i| location(&format!("file:///tmp/f{i}.ts"), 0, 0))
            .collect::<Vec<_>>();
        let r = json!({"details":{"totalReferences":21,"references":locations}});
        assert!(
            render_find_references_result(
                &r,
                RenderResultOptions {
                    expanded: true,
                    is_partial: false
                },
                &TagTheme
            )
            .text
            .ends_with("<dim>… 1 more references</dim>")
        );
    }
    #[test]
    fn position_args_reads_json_numbers() {
        let value = json!({"filePath":"/x","line":7,"character":2});
        let a = position_args(&value);
        assert_eq!((a.file_path, a.line, a.character), ("/x", 7, 2));
    }
}
