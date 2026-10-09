//! Port of the `grok-mermaid` package (v0.2.3) as used by `components/mermaid.ts`.
//!
//! Port of the `grok-mermaid` package (v0.2.3), including its parser and diagram layout engine.
pub mod ansi;
pub mod canvas;
pub mod graph;
pub mod labels;
pub mod parse;
pub mod layout;
pub mod layout_seq;
pub mod source_box;
pub mod width;
pub mod width_data;

pub use source_box::source_box;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cls {
    Border,
    Text,
    Edge,
    EdgeLabel,
    Title,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub cls: Cls,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MermaidArt {
    pub plain: Vec<String>,
    pub styled: Vec<Vec<Span>>,
    pub width: usize,
    pub warnings: Vec<String>,
}

pub fn render(src: &str) -> Option<MermaidArt> {
    let src = labels::strip_controls(src);
    if src.trim().is_empty() {
        return None;
    }
    let drawn = attempt(&src)?;
    let lines = drawn.canvas.to_lines();
    Some(MermaidArt {
        plain: lines.plain,
        styled: lines.styled,
        width: lines.width,
        warnings: drawn.warnings,
    })
}

struct Drawn {
    canvas: canvas::Canvas,
    warnings: Vec<String>,
}

fn attempt(src: &str) -> Option<Drawn> {
    if let Some(drawn) = draw(src) {
        return Some(drawn);
    }

    let body = src.trim_end();
    let cut = body.rfind('\n')?;
    let salvaged = draw(&body[..cut])?;
    let dropped = body[cut + 1..].trim();
    Some(Drawn {
        canvas: salvaged.canvas,
        warnings: [salvaged.warnings, vec![format!("dropped, unreadable final line: \"{dropped}\"")]]
            .concat(),
    })
}

fn draw(src: &str) -> Option<Drawn> {
    let plain = |canvas: layout::CanvasResult| canvas.map(|canvas| Drawn {
        canvas,
        warnings: Vec::new(),
    });

    match parse::diagram_kind(src)? {
        "flowchart" => {
            let graph = parse::parse_graph(src)?;
            let canvas = if graph.groups.is_empty() {
                layout::layout_flowchart(&graph)
            } else {
                layout::layout_grouped(&graph)
            }?;
            Some(Drawn {
                canvas,
                warnings: graph.warnings,
            })
        }
        "state" => plain(layout::layout_flowchart(&parse::parse_state(src)?)),
        "class" => {
            let (graph, infos) = parse::parse_class(src)?;
            plain(layout::layout_class(&graph, &infos))
        }
        "er" => {
            let (graph, infos) = parse::parse_er(src)?;
            plain(layout::layout_class(&graph, &infos))
        }
        "sequence" => plain(layout_seq::layout_sequence(&parse::parse_sequence(src)?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::render;

    fn assert_rendered(src: &str) {
        let art = render(src).expect("diagram should render");
        assert!(!art.plain.is_empty());
        assert!(art.plain.iter().any(|line| !line.is_empty()));
    }

    #[test]
    fn renders_flowchart() {
        assert_rendered("flowchart TD\n  A --> B");
    }

    #[test]
    fn renders_sequence() {
        assert_rendered("sequenceDiagram\n  Alice->>Bob: Hello");
    }

    #[test]
    fn renders_class() {
        assert_rendered("classDiagram\n  Animal <|-- Duck");
    }

    #[test]
    fn renders_state() {
        assert_rendered("stateDiagram-v2\n  [*] --> Still\n  Still --> [*]");
    }

    #[test]
    fn renders_er() {
        assert_rendered("erDiagram\n  CUSTOMER ||--o{ ORDER : places");
    }

    #[test]
    fn rejects_unsupported_input() {
        assert!(render("pie\n  title Pets").is_none());
    }
}
