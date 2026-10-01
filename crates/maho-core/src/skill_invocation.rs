//! Port of senpi `packages/coding-agent/src/core/skill-invocation.ts`.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

/// `SkillInvocationPromptSkill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInvocationPromptSkill {
    pub name: String,
    pub file_path: String,
    pub base_dir: String,
    pub body: String,
}

/// `formatSkillInvocationPrompt`.
pub fn format_skill_invocation_prompt(skills: &[SkillInvocationPromptSkill], user_request: Option<&str>) -> String {
    let skill_blocks: Vec<String> = skills
        .iter()
        .map(|skill| {
            format!(
                "The user explicitly invoked the \"{}\" skill. Follow the instructions in <skill-instruction> as binding for this request, while respecting higher-priority instructions.\n\n<skill-instruction name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill-instruction>",
                skill.name, skill.name, skill.file_path, skill.base_dir, skill.body
            )
        })
        .collect();
    let expanded_skills = skill_blocks.join("\n\n");
    match user_request {
        Some(user_request) if user_request.chars().any(|character| !character.is_whitespace()) => {
            format!("{expanded_skills}\n\n<user-request>\n{user_request}\n</user-request>")
        }
        _ => expanded_skills,
    }
}

/// `ParsedSkillBlockSkill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSkillBlockSkill {
    pub name: String,
    pub location: String,
    pub content: String,
}

/// `ParsedSkillBlock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSkillBlock {
    pub name: String,
    pub location: String,
    pub content: String,
    pub skills: Vec<ParsedSkillBlockSkill>,
    pub user_message: Option<String>,
}

static SKILL_INSTRUCTION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^The user explicitly invoked the "([^"]+)" skill\. Follow the instructions in <skill-instruction> as binding for this request, while respecting higher-priority instructions\.\n\n<skill-instruction name="([^"]+)" location="([^"]+)">\n([\s\S]*?)\n</skill-instruction>"#,
    )
    .expect("skill instruction pattern")
});

fn match_skill_instruction(text: &str) -> Option<(ParsedSkillBlockSkill, usize)> {
    let captures = SKILL_INSTRUCTION_PATTERN.captures(text)?;
    if captures.get(1)?.as_str() != captures.get(2)?.as_str() {
        return None;
    }
    let skill = ParsedSkillBlockSkill {
        name: captures.get(1)?.as_str().to_string(),
        location: captures.get(3)?.as_str().to_string(),
        content: captures.get(4)?.as_str().to_string(),
    };
    Some((skill, captures.get(0)?.as_str().chars().count()))
}

fn to_parsed_skill_block(skills: Vec<ParsedSkillBlockSkill>, user_message: Option<String>) -> Option<ParsedSkillBlock> {
    let first = skills.first()?.clone();
    Some(ParsedSkillBlock {
        name: first.name,
        location: first.location,
        content: first.content,
        skills,
        user_message,
    })
}

static USER_REQUEST_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\n\n<user-request>\n([\s\S]*?)\n</user-request>$").expect("user request pattern")
});

static LEGACY_SKILL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^<skill name="([^"]+)" location="([^"]+)">\n([\s\S]*?)\n</skill>(?:\n\n([\s\S]+))?$"#)
        .expect("legacy skill pattern")
});

/// `parseSkillBlock`: every chained skill block, or `None` when the text has none.
pub fn parse_skill_block(text: &str) -> Option<ParsedSkillBlock> {
    if let Some((skill, length)) = match_skill_instruction(text) {
        let mut skills = vec![skill];
        let mut remainder: String = text.chars().skip(length).collect();
        while remainder.starts_with("\n\nThe user explicitly invoked the ") {
            let chained_text: String = remainder.chars().skip(2).collect();
            let (chained, chained_length) = match_skill_instruction(&chained_text)?;
            skills.push(chained);
            remainder = chained_text.chars().skip(chained_length).collect();
        }
        let request_match = USER_REQUEST_PATTERN.captures(&remainder);
        if !remainder.is_empty() && request_match.is_none() {
            return None;
        }
        let user_message = request_match
            .as_ref()
            .and_then(|captures| captures.get(1))
            .map(|group| group.as_str().trim().to_string())
            .filter(|message| !message.is_empty());
        return to_parsed_skill_block(skills, user_message);
    }

    let captures = LEGACY_SKILL_PATTERN.captures(text)?;
    let skill = ParsedSkillBlockSkill {
        name: captures.get(1)?.as_str().to_string(),
        location: captures.get(2)?.as_str().to_string(),
        content: captures.get(3)?.as_str().to_string(),
    };
    let user_message = captures
        .get(4)
        .map(|group| group.as_str().trim().to_string())
        .filter(|message| !message.is_empty());
    to_parsed_skill_block(vec![skill], user_message)
}

/// `SkillInvocationSyntax`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillInvocationSyntax {
    Dollar,
    Slash,
}

/// `SkillInvocationToken`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInvocationToken {
    pub name: String,
    pub syntax: SkillInvocationSyntax,
    pub start: usize,
    pub end: usize,
    pub position: SkillInvocationPosition,
}

/// `position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillInvocationPosition {
    Inline,
    Leading,
}

/// `MAX_SKILL_INVOCATION_TOKENS_PER_PROMPT`.
pub const MAX_SKILL_INVOCATION_TOKENS_PER_PROMPT: usize = 64;

const SKILL_NAMESPACE: &str = "skill:";

fn is_name_start(character: char) -> bool {
    character.is_ascii_alphabetic()
}

fn is_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == ':' || character == '_' || character == '-'
}

fn read_name(chars: &[char], start: usize) -> Option<(String, usize)> {
    if start >= chars.len() || !is_name_start(chars[start]) {
        return None;
    }
    let mut end = start;
    while end < chars.len() && is_name_character(chars[end]) {
        end += 1;
    }
    let name: String = chars[start..end].iter().collect();
    Some((name, end))
}

fn followed_by_separator(chars: &[char], end: usize) -> bool {
    end >= chars.len() || chars[end].is_whitespace()
}

fn match_leading_invocation(chars: &[char], cursor: usize) -> Option<(SkillInvocationSyntax, String, usize)> {
    let rest: String = chars[cursor..].iter().collect();
    if rest.starts_with("/skill:") {
        let (name, name_end) = read_name(chars, cursor + "/skill:".chars().count())?;
        if !followed_by_separator(chars, name_end) {
            return None;
        }
        let consumed = "/skill:".chars().count() + name.chars().count();
        return Some((SkillInvocationSyntax::Slash, name, consumed));
    }
    if rest.starts_with('$') {
        let name_start = cursor + 1;
        let (token, name_end) = read_name(chars, name_start)?;
        if !followed_by_separator(chars, name_end) {
            return None;
        }
        let name = match token.strip_prefix(SKILL_NAMESPACE) {
            Some(stripped) => stripped.to_string(),
            None => token,
        };
        return Some((SkillInvocationSyntax::Dollar, name, name_end - cursor));
    }
    None
}

/// `ParseSkillInvocationOptions`.
#[derive(Debug, Clone, Default)]
pub struct ParseSkillInvocationOptions {
    pub known_skill_names: Option<HashSet<String>>,
}

fn inline_skill_name(token: &str, known_skill_names: Option<&HashSet<String>>) -> Option<String> {
    if let Some(stripped) = token.strip_prefix(SKILL_NAMESPACE) {
        return if stripped.is_empty() { None } else { Some(stripped.to_string()) };
    }
    match known_skill_names {
        Some(known) if known.contains(token) => Some(token.to_string()),
        _ => None,
    }
}

/// `parseSkillInvocationTokens`.
pub fn parse_skill_invocation_tokens(text: &str, options: &ParseSkillInvocationOptions) -> Vec<SkillInvocationToken> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens: Vec<SkillInvocationToken> = Vec::new();
    let mut cursor = 0usize;

    while cursor < chars.len() {
        while cursor < chars.len() && chars[cursor].is_whitespace() {
            cursor += 1;
        }
        if cursor >= chars.len() {
            break;
        }
        let Some((syntax, name, consumed)) = match_leading_invocation(&chars, cursor) else {
            break;
        };
        if name.is_empty() {
            break;
        }
        tokens.push(SkillInvocationToken {
            name,
            syntax,
            start: cursor,
            end: cursor + consumed,
            position: SkillInvocationPosition::Leading,
        });
        if tokens.len() >= MAX_SKILL_INVOCATION_TOKENS_PER_PROMPT {
            return tokens;
        }
        cursor += consumed;
    }

    let mut index = cursor;
    while index < chars.len() {
        let is_dollar = chars[index] == '$' && (index == 0 || chars[index - 1].is_whitespace());
        if !is_dollar {
            index += 1;
            continue;
        }
        let Some((token, name_end)) = read_name(&chars, index + 1) else {
            index += 1;
            continue;
        };
        if !followed_by_separator(&chars, name_end) {
            index += 1;
            continue;
        }
        let Some(name) = inline_skill_name(&token, options.known_skill_names.as_ref()) else {
            index += 1;
            continue;
        };
        tokens.push(SkillInvocationToken {
            name,
            syntax: SkillInvocationSyntax::Dollar,
            start: index,
            end: name_end,
            position: SkillInvocationPosition::Inline,
        });
        if tokens.len() >= MAX_SKILL_INVOCATION_TOKENS_PER_PROMPT {
            break;
        }
        index = name_end;
    }

    tokens
}

fn strip_leading_invocation_separators(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut cursor = 0usize;
    while cursor < chars.len() && (chars[cursor] == ' ' || chars[cursor] == '\t') {
        cursor += 1;
    }
    loop {
        let at_newline = cursor < chars.len()
            && (chars[cursor] == '\n' || (chars[cursor] == '\r' && chars.get(cursor + 1) == Some(&'\n')));
        if !at_newline {
            return chars[cursor..].iter().collect();
        }
        cursor += if chars[cursor] == '\r' { 2 } else { 1 };
        let line_start = cursor;
        while cursor < chars.len() && (chars[cursor] == ' ' || chars[cursor] == '\t') {
            cursor += 1;
        }
        let next_is_newline = cursor < chars.len()
            && (chars[cursor] == '\n' || (chars[cursor] == '\r' && chars.get(cursor + 1) == Some(&'\n')));
        if !next_is_newline {
            return chars[line_start..].iter().collect();
        }
    }
}

/// `removeSkillInvocationTokens`.
pub fn remove_skill_invocation_tokens(text: &str, tokens: &[SkillInvocationToken]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut cursor = 0usize;
    let mut result = String::new();
    for token in tokens {
        result.extend(chars[cursor.min(chars.len())..token.start.min(chars.len())].iter());
        if token.position == SkillInvocationPosition::Inline {
            result.push_str(&format!("[skill: {}]", token.name));
        }
        cursor = token.end;
        let trailing_separator = token.position == SkillInvocationPosition::Inline
            && (result.ends_with(' ') || result.ends_with('\t'))
            && matches!(chars.get(cursor), Some(' ') | Some('\t'));
        if trailing_separator {
            cursor += 1;
        }
    }
    result.extend(chars[cursor.min(chars.len())..].iter());
    if tokens.iter().any(|token| token.position == SkillInvocationPosition::Leading) {
        strip_leading_invocation_separators(&result)
    } else {
        result
    }
}

/// `MAX_SKILL_EXPANSIONS_PER_PROMPT`.
pub const MAX_SKILL_EXPANSIONS_PER_PROMPT: usize = 5;

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt_skill(name: &str, body: &str) -> SkillInvocationPromptSkill {
        SkillInvocationPromptSkill {
            name: name.to_string(),
            file_path: format!("/skills/{name}/SKILL.md"),
            base_dir: format!("/skills/{name}"),
            body: body.to_string(),
        }
    }

    fn known(names: &[&str]) -> ParseSkillInvocationOptions {
        ParseSkillInvocationOptions {
            known_skill_names: Some(names.iter().map(|name| (*name).to_string()).collect()),
        }
    }

    #[test]
    fn a_single_invocation_renders_a_binding_instruction_block() {
        let prompt = format_skill_invocation_prompt(&[prompt_skill("demo", "do the thing")], None);
        assert!(prompt.starts_with("The user explicitly invoked the \"demo\" skill."));
        assert!(prompt.contains("<skill-instruction name=\"demo\" location=\"/skills/demo/SKILL.md\">"));
        assert!(prompt.contains("References are relative to /skills/demo."));
        assert!(prompt.contains("do the thing"));
        assert!(prompt.ends_with("</skill-instruction>"));
        assert!(!prompt.contains("<user-request>"));
    }

    #[test]
    fn a_user_request_is_appended_only_when_it_has_content() {
        let skills = vec![prompt_skill("demo", "body")];
        let with_request = format_skill_invocation_prompt(&skills, Some("please fix it"));
        assert!(with_request.ends_with("<user-request>\nplease fix it\n</user-request>"));
        let blank = format_skill_invocation_prompt(&skills, Some("   "));
        assert!(!blank.contains("<user-request>"));
    }

    #[test]
    fn several_skills_are_joined_in_order() {
        let prompt = format_skill_invocation_prompt(&[prompt_skill("a", "A"), prompt_skill("b", "B")], Some("go"));
        let first = prompt.find("\"a\"").expect("a");
        let second = prompt.find("\"b\"").expect("b");
        assert!(first < second);
        assert!(prompt.contains("</skill-instruction>\n\nThe user explicitly invoked the \"b\" skill."));
    }

    #[test]
    fn a_parsed_block_round_trips_through_the_formatter() {
        let prompt = format_skill_invocation_prompt(&[prompt_skill("demo", "body text")], Some("the request"));
        let parsed = parse_skill_block(&prompt).expect("parsed");
        assert_eq!(parsed.name, "demo");
        assert_eq!(parsed.location, "/skills/demo/SKILL.md");
        assert_eq!(parsed.content, "References are relative to /skills/demo.\n\nbody text");
        assert_eq!(parsed.user_message.as_deref(), Some("the request"));
        assert_eq!(parsed.skills.len(), 1);
    }

    #[test]
    fn chained_invocations_parse_into_every_skill() {
        let first = format_skill_invocation_prompt(&[prompt_skill("a", "A")], None);
        let second = format_skill_invocation_prompt(&[prompt_skill("b", "B")], Some("request"));
        let chained = format!("{first}\n\n{second}");
        let parsed = parse_skill_block(&chained).expect("parsed");
        assert_eq!(parsed.skills.len(), 2);
        assert_eq!(parsed.name, "a");
        assert_eq!(parsed.user_message.as_deref(), Some("request"));
    }

    #[test]
    fn a_legacy_block_still_parses() {
        let parsed = parse_skill_block("<skill name=\"old\" location=\"/old/SKILL.md\">\nbody\n</skill>\n\nrequest").expect("parsed");
        assert_eq!(parsed.name, "old");
        assert_eq!(parsed.content, "body");
        assert_eq!(parsed.user_message.as_deref(), Some("request"));
    }

    #[test]
    fn text_without_a_skill_block_parses_to_none() {
        assert!(parse_skill_block("just a message").is_none());
        assert!(parse_skill_block("").is_none());
    }

    #[test]
    fn leading_tokens_accept_slash_and_dollar_forms() {
        let tokens = parse_skill_invocation_tokens("/skill:demo do it", &known(&[]));
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].name, "demo");
        assert_eq!(tokens[0].syntax, SkillInvocationSyntax::Slash);
        assert_eq!(tokens[0].position, SkillInvocationPosition::Leading);
        assert_eq!((tokens[0].start, tokens[0].end), (0, 11));

        let tokens = parse_skill_invocation_tokens("$skill:demo do it", &known(&[]));
        assert_eq!(tokens[0].name, "demo");
        assert_eq!(tokens[0].syntax, SkillInvocationSyntax::Dollar);

        let tokens = parse_skill_invocation_tokens("$demo rest", &known(&[]));
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].name, "demo");
        assert_eq!(tokens[0].position, SkillInvocationPosition::Leading);
    }

    #[test]
    fn a_leading_run_stops_at_the_first_non_token() {
        let tokens = parse_skill_invocation_tokens("/skill:a /skill:b plain text", &known(&[]));
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].name, "a");
        assert_eq!(tokens[1].name, "b");
        assert_eq!(tokens[1].start, 9);
    }

    #[test]
    fn a_token_must_end_at_a_separator() {
        assert!(parse_skill_invocation_tokens("$HOME/.config", &known(&["HOME"])).is_empty());
        assert!(parse_skill_invocation_tokens("/skill:demo.md", &known(&[])).is_empty());
    }

    #[test]
    fn inline_dollar_tokens_need_a_known_skill_or_the_namespace() {
        assert!(parse_skill_invocation_tokens("please use $demo now", &known(&[])).is_empty());
        let tokens = parse_skill_invocation_tokens("please use $demo now", &known(&["demo"]));
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].position, SkillInvocationPosition::Inline);
        assert_eq!((tokens[0].start, tokens[0].end), (11, 16));

        let tokens = parse_skill_invocation_tokens("please use $skill:demo now", &known(&[]));
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].name, "demo");
    }

    #[test]
    fn inline_scanning_resumes_after_the_leading_run() {
        let tokens = parse_skill_invocation_tokens("/skill:lead and $known here", &known(&["known"]));
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[1].name, "known");
        assert_eq!(tokens[1].position, SkillInvocationPosition::Inline);
    }

    #[test]
    fn removing_tokens_keeps_inline_placeholders_and_drops_leading_ones() {
        let text = "/skill:lead do it";
        let tokens = parse_skill_invocation_tokens(text, &known(&[]));
        assert_eq!(remove_skill_invocation_tokens(text, &tokens), "do it");

        let text = "say $skill:demo loudly";
        let tokens = parse_skill_invocation_tokens(text, &known(&[]));
        assert_eq!(remove_skill_invocation_tokens(text, &tokens), "say [skill: demo] loudly");
    }

    #[test]
    fn removing_leading_tokens_also_strips_the_blank_lines_they_left() {
        let text = "/skill:a\n\n/ skill body";
        let tokens = parse_skill_invocation_tokens(text, &known(&[]));
        let stripped = remove_skill_invocation_tokens(text, &tokens);
        assert_eq!(stripped, "/ skill body");
    }

    #[test]
    fn the_token_cap_bounds_one_prompt() {
        let text = (0..70).map(|index| format!("/skill:s{index}")).collect::<Vec<_>>().join(" ");
        let tokens = parse_skill_invocation_tokens(&text, &known(&[]));
        assert_eq!(tokens.len(), MAX_SKILL_INVOCATION_TOKENS_PER_PROMPT);
    }
}
