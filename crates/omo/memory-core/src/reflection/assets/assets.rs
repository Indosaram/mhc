//! Embedded persona assets and section parsing for background reflection agents.

/// Verbatim markdown persona definition for the standard reflection agent.
pub const REFLECTION_PERSONA_MARKDOWN: &str = include_str!("reflection-persona.md");

/// Verbatim markdown persona definition for the dream maintenance agent.
pub const DREAM_PERSONA_MARKDOWN: &str = include_str!("dream-persona.md");

/// Markdown heading section parsed from an agent persona.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectionPersonaSection {
    pub heading: String,
    pub level: usize,
    pub body: String,
}

/// Parsed markdown persona structure with its raw text and heading sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectionPersona {
    pub markdown: &'static str,
    pub sections: Vec<ReflectionPersonaSection>,
}

/// Parses heading sections from a markdown document.
pub fn parse_sections(markdown: &str) -> Vec<ReflectionPersonaSection> {
    let mut sections = Vec::new();
    let mut current: Option<ReflectionPersonaSection> = None;
    let mut in_fence = false;

    for line in markdown.lines() {
        if line.starts_with("```") {
            in_fence = !in_fence;
        }

        if !in_fence
            && line.starts_with('#')
            && let Some(parsed) = parse_heading_line(line)
        {
            if let Some(sec) = current.take() {
                sections.push(sec);
            }
            current = Some(ReflectionPersonaSection {
                heading: parsed.0,
                level: parsed.1,
                body: String::new(),
            });
            continue;
        }

        if let Some(ref mut sec) = current {
            sec.body.push_str(line);
            sec.body.push('\n');
        }
    }

    if let Some(sec) = current {
        sections.push(sec);
    }

    sections
}

fn parse_heading_line(line: &str) -> Option<(String, usize)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    let heading = rest.trim();
    if heading.is_empty() {
        return None;
    }
    Some((heading.to_string(), hashes))
}

/// Loads the standard reflection persona embedded asset.
pub fn load_reflection_persona() -> ReflectionPersona {
    ReflectionPersona {
        markdown: REFLECTION_PERSONA_MARKDOWN,
        sections: parse_sections(REFLECTION_PERSONA_MARKDOWN),
    }
}

/// Loads the dream persona embedded asset.
pub fn load_dream_persona() -> ReflectionPersona {
    ReflectionPersona {
        markdown: DREAM_PERSONA_MARKDOWN,
        sections: parse_sections(DREAM_PERSONA_MARKDOWN),
    }
}

#[cfg(test)]
#[path = "assets_tests.rs"]
mod tests;
