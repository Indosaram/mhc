//! Port of the `grok-mermaid` `source-box.ts`.
use super::labels::{src_lines, strip_controls};
use super::width::{measured, string_width};
use super::{Cls, MermaidArt, Span};

fn sat(a: usize, b: usize) -> usize {
    a.saturating_sub(b)
}

pub fn source_box(src: &str, max_width: Option<usize>) -> MermaidArt {
    let src = strip_controls(src);
    let header = src.split_whitespace().next().unwrap_or("diagram");
    let title = format!(" mermaid: {header} ");
    let limit = max_width.map(|max_width| 8.max(sat(max_width, 4)));

    let mut started = false;
    let mut body: Vec<String> = Vec::new();
    for line in src_lines(&src) {
        let line = line.trim_end().to_owned();
        if !started && line.is_empty() {
            continue;
        }
        started = true;
        body.extend(chunk_line(&line, limit));
    }

    let content_width = body
        .iter()
        .map(|line| string_width(line))
        .chain(std::iter::once(string_width(&title)))
        .max()
        .unwrap_or(0);
    let inner = content_width + 2;

    let mut plain: Vec<String> = Vec::new();
    let mut styled: Vec<Vec<Span>> = Vec::new();

    let rule = "─".repeat(sat(inner, string_width(&title)));
    plain.push(format!("╭{title}{rule}╮"));
    styled.push(vec![
        Span { text: String::from("╭"), cls: Cls::Border },
        Span { text: title.clone(), cls: Cls::Title },
        Span { text: format!("{rule}╮"), cls: Cls::Border },
    ]);

    for line in &body {
        let pad = " ".repeat(sat(content_width, string_width(line)));
        plain.push(format!("│ {line}{pad} │"));
        styled.push(vec![
            Span { text: String::from("│ "), cls: Cls::Border },
            Span { text: line.clone(), cls: Cls::Text },
            Span { text: format!("{pad} │"), cls: Cls::Border },
        ]);
    }

    let bottom = format!("╰{}╯", "─".repeat(inner));
    plain.push(bottom.clone());
    styled.push(vec![Span { text: bottom, cls: Cls::Border }]);

    MermaidArt { plain, styled, width: inner + 2, warnings: Vec::new() }
}

fn chunk_line(line: &str, limit: Option<usize>) -> Vec<String> {
    let Some(limit) = limit else {
        return vec![line.to_owned()];
    };
    if string_width(line) <= limit {
        return vec![line.to_owned()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for (cluster, cluster_width) in measured(line) {
        if current_width + cluster_width > limit && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(cluster);
        current_width += cluster_width;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}
