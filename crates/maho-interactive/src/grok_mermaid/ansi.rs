//! Port of the `grok-mermaid` `ansi.ts`.
use super::{Cls, MermaidArt};

const ESC: char = '\x1b';

/// SGR parameter per semantic class, e.g. `'2'` for dim, `'36'` for cyan,
/// `'38;5;244'` for a 256-colour index. A class left out is printed unstyled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnsiTheme {
    pub border: Option<&'static str>,
    pub text: Option<&'static str>,
    pub edge: Option<&'static str>,
    pub edge_label: Option<&'static str>,
    pub title: Option<&'static str>,
    pub none: Option<&'static str>,
}

impl AnsiTheme {
    pub const fn get(&self, cls: Cls) -> Option<&'static str> {
        match cls {
            Cls::Border => self.border,
            Cls::Text => self.text,
            Cls::Edge => self.edge,
            Cls::EdgeLabel => self.edge_label,
            Cls::Title => self.title,
            Cls::None => self.none,
        }
    }
}

/// Dim frame, plain labels, cyan connectors. Readable on light and dark.
pub const DEFAULT_THEME: AnsiTheme = AnsiTheme {
    border: Some("2"),
    text: None,
    edge: Some("36"),
    edge_label: Some("2;36"),
    title: Some("1"),
    none: None,
};

/// Render art to ANSI-coloured lines.
///
/// A convenience over mapping `art.styled` yourself — reach for that directly
/// when your TUI has its own styling model.
pub fn to_ansi(art: &MermaidArt, theme: Option<&AnsiTheme>) -> Vec<String> {
    let theme = theme.unwrap_or(&DEFAULT_THEME);
    art.styled
        .iter()
        .map(|row| {
            let mut line = String::new();
            for span in row {
                if let Some(sgr) = theme.get(span.cls) {
                    line.push(ESC);
                    line.push('[');
                    line.push_str(sgr);
                    line.push('m');
                    line.push_str(&span.text);
                    line.push(ESC);
                    line.push_str("[0m");
                } else {
                    line.push_str(&span.text);
                }
            }
            line
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grok_mermaid::source_box::source_box;
    use crate::grok_mermaid::Span;

    #[test]
    fn test_to_ansi_default_theme() {
        let art = MermaidArt {
            plain: vec!["hello".to_owned()],
            styled: vec![
                vec![
                    Span {
                        text: "╭─".to_owned(),
                        cls: Cls::Border,
                    },
                    Span {
                        text: " mermaid ".to_owned(),
                        cls: Cls::Title,
                    },
                    Span {
                        text: "─╮".to_owned(),
                        cls: Cls::Border,
                    },
                ],
                vec![
                    Span {
                        text: "│ ".to_owned(),
                        cls: Cls::Border,
                    },
                    Span {
                        text: "A".to_owned(),
                        cls: Cls::Text,
                    },
                    Span {
                        text: " ──▶ ".to_owned(),
                        cls: Cls::Edge,
                    },
                    Span {
                        text: "lbl".to_owned(),
                        cls: Cls::EdgeLabel,
                    },
                    Span {
                        text: " │".to_owned(),
                        cls: Cls::Border,
                    },
                    Span {
                        text: " ".to_owned(),
                        cls: Cls::None,
                    },
                ],
            ],
            width: 10,
            warnings: vec![],
        };

        let lines = to_ansi(&art, None);
        assert_eq!(
            lines,
            vec![
                "\u{1b}[2m╭─\u{1b}[0m\u{1b}[1m mermaid \u{1b}[0m\u{1b}[2m─╮\u{1b}[0m",
                "\u{1b}[2m│ \u{1b}[0mA\u{1b}[36m ──▶ \u{1b}[0m\u{1b}[2;36mlbl\u{1b}[0m\u{1b}[2m │\u{1b}[0m ",
            ]
        );
    }

    #[test]
    fn test_to_ansi_custom_theme() {
        let art = MermaidArt {
            plain: vec!["test".to_owned()],
            styled: vec![vec![
                Span {
                    text: "╭─".to_owned(),
                    cls: Cls::Border,
                },
                Span {
                    text: "node".to_owned(),
                    cls: Cls::Text,
                },
                Span {
                    text: "─╮".to_owned(),
                    cls: Cls::Border,
                },
            ]],
            width: 8,
            warnings: vec![],
        };

        let custom_theme = AnsiTheme {
            border: Some("31"),
            text: Some("32"),
            ..Default::default()
        };

        let lines = to_ansi(&art, Some(&custom_theme));
        assert_eq!(
            lines,
            vec!["\u{1b}[31m╭─\u{1b}[0m\u{1b}[32mnode\u{1b}[0m\u{1b}[31m─╮\u{1b}[0m",]
        );
    }

    #[test]
    fn test_to_ansi_empty_theme() {
        let art = MermaidArt {
            plain: vec!["test".to_owned()],
            styled: vec![vec![
                Span {
                    text: "╭─".to_owned(),
                    cls: Cls::Border,
                },
                Span {
                    text: "title".to_owned(),
                    cls: Cls::Title,
                },
                Span {
                    text: "─╮".to_owned(),
                    cls: Cls::Border,
                },
            ]],
            width: 9,
            warnings: vec![],
        };

        let empty_theme = AnsiTheme::default();
        let lines = to_ansi(&art, Some(&empty_theme));
        assert_eq!(lines, vec!["╭─title─╮"]);
    }

    #[test]
    fn test_to_ansi_with_source_box() {
        let art = source_box("graph TD\n  A --> B", Some(40));
        let lines = to_ansi(&art, None);
        assert_eq!(
            lines,
            vec![
                "\u{1b}[2m╭\u{1b}[0m\u{1b}[1m mermaid: graph \u{1b}[0m\u{1b}[2m──╮\u{1b}[0m",
                "\u{1b}[2m│ \u{1b}[0mgraph TD\u{1b}[2m         │\u{1b}[0m",
                "\u{1b}[2m│ \u{1b}[0m  A --> B\u{1b}[2m        │\u{1b}[0m",
                "\u{1b}[2m╰──────────────────╯\u{1b}[0m",
            ]
        );
    }
}
