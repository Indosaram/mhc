use serde_json::Value;

pub const JSON_TREE_MAX_DEPTH_COLLAPSED: usize = 2;
pub const JSON_TREE_MAX_DEPTH_EXPANDED: usize = 6;
pub const JSON_TREE_MAX_LINES_COLLAPSED: usize = 6;
pub const JSON_TREE_MAX_LINES_EXPANDED: usize = 200;
pub const JSON_TREE_SCALAR_LEN_COLLAPSED: usize = 60;
pub const JSON_TREE_SCALAR_LEN_EXPANDED: usize = 2000;

fn truncate_to_columns(text: &str, limit: usize) -> String {
    let width: usize = text
        .chars()
        .map(|character| if character == '\t' { 4 } else { 1 })
        .sum();
    if width <= limit {
        return text.into();
    }
    if limit == 0 {
        return String::new();
    }
    let mut prefix = String::new();
    let mut width = 0;
    for character in text.chars() {
        let next = if character == '\t' { 4 } else { 1 };
        if width + next > limit - 1 {
            break;
        }
        prefix.push(character);
        width += next;
    }
    prefix.push('…');
    prefix
}

pub fn format_scalar(value: &Value, max_len: usize) -> String {
    match value {
        Value::String(text) => format!(
            "\"{}\"",
            truncate_to_columns(&text.replace('\n', "\\n").replace('\t', "\\t"), max_len)
        ),
        Value::Array(values) => format!("[{} items]", values.len()),
        Value::Object(values) => format!("{{{} keys}}", values.len()),
        _ => value.to_string(),
    }
}

pub struct JsonTreeResult {
    pub lines: Vec<String>,
    pub truncated: bool,
}

struct RenderState<'a> {
    lines: Vec<String>,
    theme: Option<&'a maho_ext_api::Theme>,
    max_depth: usize,
    max_lines: usize,
    max_scalar_len: usize,
    truncated: bool,
}
impl RenderState<'_> {
    /// json-tree.ts `style(theme, color, text)`: `theme.fg` when a theme is present.
    fn style(&self, color: &str, text: &str) -> String {
        self.theme
            .map_or_else(|| text.to_owned(), |theme| theme.fg(color, text))
    }
    fn tree_prefix(&self, ancestors: &[bool]) -> String {
        ancestors
            .iter()
            .map(|next| self.style("dim", if *next { "│  " } else { "   " }))
            .collect()
    }
    fn push(&mut self, line: String) -> bool {
        if self.lines.len() >= self.max_lines {
            self.truncated = true;
            return false;
        }
        self.lines.push(line);
        true
    }
    fn closing(&self, ancestors: &[bool], text: &str) -> String {
        format!(
            "{}{} {}",
            self.tree_prefix(ancestors),
            self.style("dim", "└─"),
            self.style("dim", text)
        )
    }
    fn node(
        &mut self,
        value: &Value,
        key: Option<&str>,
        ancestors: &mut Vec<bool>,
        last: bool,
        depth: usize,
    ) {
        if self.lines.len() >= self.max_lines {
            self.truncated = true;
            return;
        }
        let prefix = format!(
            "{}{} ",
            self.tree_prefix(ancestors),
            self.style("dim", if last { "└─" } else { "├─" })
        );
        ancestors.push(!last);
        match value {
            Value::Array(values) => {
                let label = key.filter(|key| !key.is_empty()).unwrap_or("array");
                let line = format!(
                    "{prefix}{} {}",
                    self.style("muted", "◇"),
                    self.style("muted", label)
                );
                if self.push(line) {
                    if values.is_empty() {
                        let line = self.closing(ancestors, "[]");
                        self.push(line);
                    } else if depth >= self.max_depth {
                        let line = self.closing(ancestors, "…");
                        self.push(line);
                    } else {
                        for (index, value) in values.iter().enumerate() {
                            self.node(
                                value,
                                Some(&format!("[{index}]")),
                                ancestors,
                                index == values.len() - 1,
                                depth + 1,
                            );
                            if self.lines.len() >= self.max_lines {
                                self.truncated = true;
                                break;
                            }
                        }
                    }
                }
            }
            Value::Object(values) => {
                let label = key.filter(|key| !key.is_empty()).unwrap_or("object");
                let line = format!(
                    "{prefix}{} {}",
                    self.style("muted", "◆"),
                    self.style("muted", label)
                );
                if self.push(line) {
                    if depth >= self.max_depth {
                        let line = self.closing(ancestors, "…");
                        self.push(line);
                    } else if values.is_empty() {
                        let line = self.closing(ancestors, "{}");
                        self.push(line);
                    } else {
                        for (index, (key, value)) in values.iter().enumerate() {
                            self.node(
                                value,
                                Some(key),
                                ancestors,
                                index == values.len() - 1,
                                depth + 1,
                            );
                            if self.lines.len() >= self.max_lines {
                                self.truncated = true;
                                break;
                            }
                        }
                    }
                }
            }
            Value::String(text) if text.contains('\n') => {
                let label = self.style(
                    "muted",
                    key.filter(|key| !key.is_empty()).unwrap_or("value"),
                );
                let lines: Vec<_> = text.split('\n').collect();
                let visible = lines
                    .len()
                    .min(self.max_lines.saturating_sub(self.lines.len() + 1).max(1));
                let first = format!(
                    "{prefix}{} {label}: {}",
                    self.style("muted", "•"),
                    self.style(
                        "dim",
                        &format!("\"{}", truncate_to_columns(lines[0], self.max_scalar_len))
                    )
                );
                if self.push(first) {
                    let continued = self.tree_prefix(ancestors);
                    let mut open = true;
                    for line in lines.iter().take(visible).skip(1) {
                        let line = format!(
                            "{continued}   {}",
                            self.style(
                                "dim",
                                &format!(" {}", truncate_to_columns(line, self.max_scalar_len))
                            )
                        );
                        if !self.push(line) {
                            open = false;
                            break;
                        }
                    }
                    if open {
                        if lines.len() > visible {
                            self.truncated = true;
                            let line = format!(
                                "{continued}   {}",
                                self.style(
                                    "dim",
                                    &format!(" …({} more lines)\"", lines.len() - visible)
                                )
                            );
                            self.push(line);
                        } else {
                            let quote = self.style("dim", "\"");
                            if let Some(last) = self.lines.last_mut() {
                                last.push_str(&quote);
                            }
                        }
                    }
                }
            }
            _ => {
                let label = self.style(
                    "muted",
                    key.filter(|key| !key.is_empty()).unwrap_or("value"),
                );
                let line = format!(
                    "{prefix}{} {label}: {}",
                    self.style("muted", "•"),
                    self.style("dim", &format_scalar(value, self.max_scalar_len))
                );
                self.push(line);
            }
        }
        ancestors.pop();
    }
}

pub fn render_json_tree_lines(
    value: &Value,
    max_depth: usize,
    max_lines: usize,
    max_scalar_len: usize,
) -> JsonTreeResult {
    render_json_tree_lines_themed(value, None, max_depth, max_lines, max_scalar_len)
}

/// json-tree.ts `renderJsonTreeLines(value, theme, ...)`: glyphs and labels are `muted`, connectors and scalars `dim`.
pub fn render_json_tree_lines_themed(
    value: &Value,
    theme: Option<&maho_ext_api::Theme>,
    max_depth: usize,
    max_lines: usize,
    max_scalar_len: usize,
) -> JsonTreeResult {
    let mut state = RenderState {
        lines: vec![],
        theme,
        max_depth,
        max_lines,
        max_scalar_len,
        truncated: false,
    };
    match value {
        Value::Object(values) => {
            let keys: Vec<_> = values
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "i" | "__partialJson"))
                .collect();
            for (index, (key, value)) in keys.iter().enumerate() {
                state.node(value, Some(key), &mut vec![], index == keys.len() - 1, 1);
                if state.lines.len() >= state.max_lines {
                    state.truncated = true;
                    break;
                }
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                state.node(
                    value,
                    Some(&format!("[{index}]")),
                    &mut vec![],
                    index == values.len() - 1,
                    1,
                );
                if state.lines.len() >= state.max_lines {
                    state.truncated = true;
                    break;
                }
            }
        }
        _ => state.node(value, None, &mut vec![], true, 0),
    }
    JsonTreeResult {
        lines: state.lines,
        truncated: state.truncated,
    }
}
