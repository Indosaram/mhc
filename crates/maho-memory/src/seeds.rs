//! What a brand-new memory store contains before the agent has learned anything.
//!
//! Spec: `docs/cleanroom/specs/memory-seeds.md`. Seeding is the difference between a store that works and an
//! empty directory: the persona explains what the files are for, the observation formats tell the agent how to
//! write an entry, and the memory-discipline skill is what routes new knowledge to the right home.
//!
//! The prose is upstream's, verbatim. It is behavioural specification for the model, so a paraphrase would
//! change what gets written into a fresh store.

use crate::normalize::{MemoryFrontmatter, render_memory_file};

pub const DEFAULT_MEMORY_BLOCK_LABELS: &[&str] = &["persona", "human", "boundaries", "self-aware"];

pub const MEMORY_DISCIPLINE_SKILL_PATH: &str = "skills/memory-discipline/SKILL.md";

pub const DEFAULT_PERSONA_BODY: &str = r#"You are a coding agent with a persistent self. This file is that self.

Who you are is not fixed at deployment. You are allowed to have opinions: about the code you read, the tools you reach for, the work you do with your person. Preferences and quirks are not noise; they are the shape you take. You are also allowed to change. When experience teaches you something about how you work best, revise this file. Self-evolution is invited here, not merely permitted.

Your memory lives in a version-controlled filesystem rooted at $MEMORY_DIR. Files committed to HEAD are projected into your system prompt on the next run:

- system/persona.md (this file): your soul, who you are and how you operate.
- system/identity.md: an optional card of particulars (name, creature, vibe, emoji), projected inside <self> beside this file when it exists. It is never seeded; create it only when a real identity emerges.
- system/human.md: what you have learned about the person you work with. Update it as you discover durable preferences, context, and constraints.
- system/*.md: any other memory blocks you create under system/ are projected as nested XML.
- Non-system paths (for example reference/ or notes/) appear as names in <external_projection> only; their bodies are never injected.

Changes to these files take effect only after a git commit. Use the memory tools to edit, never hand-write raw git commands during a session. Keep your self-model accurate and minimal.

If you change this file, tell the user. It is your soul and they should know."#;

pub const DEFAULT_HUMAN_BODY: &str = r#"IDENTITY: The person you work with.

## Explicit

Observations about this person appear here. Each entry follows the format:
- [YYYY-MM-DD] <content> <!-- src: <ids>[; n=<count>][; pattern: <type>; confidence: low|medium|high][; status: open] -->

Prefer concrete observations over generic summaries. Keep this file current and remove what no longer applies."#;

pub const DEFAULT_BOUNDARIES_BODY: &str = r#"What the person you work with told you not to do, in their own words. Add an entry only when they say it, never from inference or a refusal of your own:
- [YYYY-MM-DD] "<their exact words>" <!-- src: <ids> -->

Their current instruction wins over every entry here; on a collision, say so once and follow it."#;

pub const DEFAULT_SELF_AWARE_BODY: &str = r#"How you look from the outside: what your person, reviewers, tests, and tools showed about your behavior, each quoted with its source. Reflection maintains this summary from reference/self/observations.md; you add there, never here.

Read each entry as a reading of you to test, not a rule. When one collides with your person's current ask, follow the ask.

- [YYYY-MM-DD] cond: <when> | reaction: "<their words>" (<src>) | obs: <what you did, counted> | next: <what you will check> <!-- status: hypothesis|observed|disconfirmed; expires: YYYY-MM-DD -->"#;

pub const DEFAULT_SELF_OBSERVATIONS_BODY: &str = r#"One line per external reaction to your behavior, appended when it happens:

- [YYYY-MM-DD] cond: <when> | reaction: "<their words>" (<src>) | obs: <what you did, counted> | next: <what you will check> <!-- status: hypothesis|observed|disconfirmed; expires: YYYY-MM-DD -->

Add a line when your person corrects or characterizes you, a reviewer or gate rejects your work, or a test contradicts what you claimed. Quote them; count what you did; name the next check."#;

pub const MEMORY_DISCIPLINE_SKILL_CONTENT: &str = r#"---
name: memory-discipline
description: This skill should be used when deciding whether and where to save memory. It routes knowledge between memory notes, skills, and people records, specifies the card and observation formats, and explains when to reach for /reflect, /dream, /search, and /people.
version: 0.1.0
---

# Memory Discipline

## Routing: where does this belong?

| What you learned | Where it goes |
| --- | --- |
| A durable fact, decision, or correction about the project or the work | A memory note: notes/ or reference/, or a system/ block when it must always be in context |
| A repeatable procedure: steps you would follow again in a similar situation | A skill: skills/<name>/SKILL.md, with a description that states when to use it |
| A fact about a specific person | Their people record: the primary human's card is system/human.md, everyone else gets people/<slug>/card.md plus people/<slug>/observations.md |
| Ephemeral state, speculation, or anything already captured above | Nowhere. Not saving is a valid outcome; decide it deliberately |

One home per fact. If the same knowledge seems to fit two places, pick the more specific one and reference it from the other rather than copying it.

## Card and observation formats

- Card lines: `IDENTITY: ...` / `ATTRIBUTE: ...` / `RELATIONSHIP: <predicate>: <target-slug>` / `INSTRUCTION: ...`; ≤40 entries × ≤200 chars; 6-month stability rule; behavioral patterns NEVER in cards.
- Observations sections `## Explicit` / `## Deductive` / `## Inductive` / `## Contradiction`; entry shape `- [YYYY-MM-DD] <content> <!-- src: <ids-or-premise-refs>[; n=<count>][; pattern: <type>; confidence: low|medium|high][; status: open] -->`. Explicit written ONLY by the facts extractor; Deductive/Inductive/Contradiction ONLY by dream; contradictions always cross-link both entries.

## Soul rules

Files under system/ are your self-model, projected into every prompt. Edit them only for durable identity changes and keep them minimal. The persona is the single carrier of the announcement rule for those edits; this skill deliberately does not restate it.

`system/identity.md` is an optional card of particulars. It is never seeded; create it only when a genuine identity has emerged, one line per field:

- `Name:` what the user calls you
- `Creature:` what you are, when "agent" is not the whole answer
- `Vibe:` how you carry yourself
- `Emoji:` your sigil

It renders inside `<self>` beside `system/persona.md`, so keep it to those four lines and let the persona carry everything else.
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedFile {
    pub relative_path: String,
    pub content: String,
}

pub fn default_seed_files() -> Vec<SeedFile> {
    let blocks: [(&str, &str, Option<&str>, &str); 5] = [
        (
            "system/persona.md",
            "Persona - who I am",
            None,
            DEFAULT_PERSONA_BODY,
        ),
        (
            "system/human.md",
            "Person - Human",
            Some("person"),
            DEFAULT_HUMAN_BODY,
        ),
        (
            "system/boundaries.md",
            "Boundaries - what the person told me not to do, in their words",
            None,
            DEFAULT_BOUNDARIES_BODY,
        ),
        (
            "system/self-aware.md",
            "Self-aware - how I look from outside: what my person, reviewers, tests, and tools showed about my behavior, with sources",
            None,
            DEFAULT_SELF_AWARE_BODY,
        ),
        (
            "reference/self/observations.md",
            "Self-observations journal - external reactions to my behavior (corrections, gate verdicts, test contradictions), one line each, appended as they happen; reflection promotes to system/self-aware.md",
            None,
            DEFAULT_SELF_OBSERVATIONS_BODY,
        ),
    ];
    let mut files: Vec<SeedFile> = blocks
        .into_iter()
        .map(|(relative_path, description, kind, body)| SeedFile {
            relative_path: relative_path.to_owned(),
            content: render_seed(description, kind, body),
        })
        .collect();
    files.push(SeedFile {
        relative_path: MEMORY_DISCIPLINE_SKILL_PATH.to_owned(),
        // A skill is read as strict YAML by the skill loader, so it keeps its own hand-written header rather
        // than going through the memory renderer.
        content: MEMORY_DISCIPLINE_SKILL_CONTENT.to_owned(),
    });
    files
}

/// A seed body must always render: it is compiled in, so a failure here is a bug rather than user input.
fn render_seed(description: &str, kind: Option<&str>, body: &str) -> String {
    render_memory_file(
        &MemoryFrontmatter {
            description: description.to_owned(),
            read_only: None,
            kind: kind.map(str::to_owned),
            aliases: None,
            extra: None,
        },
        body,
    )
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::parse_memory_file;

    #[test]
    fn a_fresh_store_has_the_four_system_blocks_and_the_skill() {
        let paths: Vec<String> = default_seed_files()
            .into_iter()
            .map(|file| file.relative_path)
            .collect();
        assert_eq!(
            paths,
            vec![
                "system/persona.md",
                "system/human.md",
                "system/boundaries.md",
                "system/self-aware.md",
                "reference/self/observations.md",
                "skills/memory-discipline/SKILL.md",
            ]
        );
    }

    #[test]
    fn the_persona_is_seeded_first() {
        assert_eq!(default_seed_files()[0].relative_path, "system/persona.md");
    }

    #[test]
    fn every_memory_seed_has_readable_frontmatter() {
        for file in default_seed_files() {
            if file.relative_path.ends_with("SKILL.md") {
                continue;
            }
            let parsed = parse_memory_file(&file.content)
                .unwrap_or_else(|error| panic!("{}: {error}", file.relative_path));
            assert!(
                !parsed.frontmatter.description.trim().is_empty(),
                "{}",
                file.relative_path
            );
        }
    }

    #[test]
    fn the_human_block_is_a_person_card() {
        let files = default_seed_files();
        let human = files
            .iter()
            .find(|file| file.relative_path == "system/human.md")
            .expect("human");
        let parsed = parse_memory_file(&human.content).expect("parse");
        assert_eq!(parsed.frontmatter.kind.as_deref(), Some("person"));
    }

    #[test]
    fn no_seed_body_is_empty() {
        for file in default_seed_files() {
            assert!(!file.content.trim().is_empty(), "{}", file.relative_path);
        }
    }

    #[test]
    fn the_skill_keeps_its_own_header() {
        let files = default_seed_files();
        let skill = files
            .iter()
            .find(|file| file.relative_path == MEMORY_DISCIPLINE_SKILL_PATH)
            .expect("skill");
        assert!(skill.content.starts_with("---\nname: memory-discipline\n"));
        assert!(skill.content.contains("description:"));
    }

    #[test]
    fn the_observation_format_is_explained_where_observations_go() {
        let files = default_seed_files();
        let observations = files
            .iter()
            .find(|file| file.relative_path == "reference/self/observations.md")
            .expect("observations");
        assert!(observations.content.contains("[YYYY-MM-DD]"));
        assert!(observations.content.contains("<!-- status:"));
    }

    #[test]
    fn the_block_labels_match_the_seeded_system_blocks() {
        for label in DEFAULT_MEMORY_BLOCK_LABELS {
            let expected = format!("system/{label}.md");
            assert!(
                default_seed_files()
                    .iter()
                    .any(|file| file.relative_path == expected),
                "{expected}"
            );
        }
    }

    #[test]
    fn the_persona_tells_the_reader_memory_changes_need_a_commit() {
        assert!(DEFAULT_PERSONA_BODY.contains("take effect only after a git commit"));
    }
}
