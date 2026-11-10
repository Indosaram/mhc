//! Standing agreements (`system/agreements.md`) and system-prompt projection.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-agreements.md`. The legacy store keeps one
//! Markdown block per agreement with a machine-readable metadata comment, and projects committed
//! `system/` blocks into the prompt verbatim while listing every other block by name only.
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// `system/` blocks that project into their own named slot instead of a nested section.
pub const SLOT_BLOCKS: [&str; 5] = [
    "persona.md",
    "identity.md",
    "human.md",
    "boundaries.md",
    "self-aware.md",
];

/// A projection failure that a caller must surface instead of silently dropping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionError(pub String);

impl fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProjectionError {}

/// One agreement entry parsed from `system/agreements.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agreement {
    /// `global` or `project`.
    pub scope: String,
    /// The agreement itself, in the person's words.
    pub text: String,
    /// Stable 12-hex id.
    pub id: String,
    /// Who recorded it; `agent` in every observed store, kept open.
    pub by: String,
    /// How many times the agreement has been broken.
    pub breaks: u32,
    /// First recording, `YYYY-MM-DD`.
    pub since: String,
    /// Most recent break, `YYYY-MM-DD`.
    pub last: String,
    /// `active` or `needs-human`; unknown values are preserved verbatim.
    pub status: String,
    /// Number of `agreement-event` lines that follow the entry.
    pub events: usize,
}

impl Agreement {
    /// Today's `needs-human` promotion rule: the agent may not continue past a breach.
    pub fn blocks_work(&self) -> bool {
        self.status.eq_ignore_ascii_case("needs-human")
            || self.status.eq_ignore_ascii_case("needs human")
    }

    /// A mismatch between the recorded break count and the event history.
    ///
    /// Reported, never repaired: `breaks` is authoritative for display and the events are history.
    pub fn history_mismatch(&self) -> Option<String> {
        let count = u32::try_from(self.events).unwrap_or(u32::MAX);
        (count != self.breaks).then(|| {
            format!(
                "agreement {}: records {} break(s) but has {} event line(s)",
                self.id, self.breaks, self.events
            )
        })
    }
}

/// The parsed contents of one `agreements.md` block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agreements {
    /// Parsed entries, in file order.
    pub entries: Vec<Agreement>,
    /// The `description:` frontmatter value, when present.
    pub description: Option<String>,
    /// Lines that are neither frontmatter, an entry, nor an event comment.
    pub trailing: Vec<String>,
}

impl Agreements {
    /// Parse a whole `agreements.md` block.
    pub fn parse(source: &str) -> Self {
        let (description, body) = split_frontmatter(source);
        let mut entries: Vec<Agreement> = Vec::new();
        let mut trailing = Vec::new();
        for line in body.lines() {
            if let Some(event) = agreement_event(line) {
                match entries.last_mut() {
                    Some(entry) if !event.is_empty() => entry.events += 1,
                    _ => trailing.push(line.to_owned()),
                }
                continue;
            }
            match parse_entry(line) {
                Some(entry) => entries.push(entry),
                None => {
                    if !line.trim().is_empty() {
                        trailing.push(line.to_owned());
                    }
                }
            }
        }
        Self {
            entries,
            description,
            trailing,
        }
    }

    /// Entries the agent must not walk past: `needs-human`.
    pub fn blocking(&self) -> impl Iterator<Item = &Agreement> {
        self.entries.iter().filter(|entry| entry.blocks_work())
    }

    /// Every break-count/history disagreement, in file order.
    pub fn mismatches(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(Agreement::history_mismatch)
            .collect()
    }

    /// Render back to Markdown. Unchanged input round-trips byte-for-byte.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(description) = &self.description {
            out.push_str("---\n");
            out.push_str(&format!("description: {description}\n"));
            out.push_str("---\n");
        }
        for entry in &self.entries {
            out.push_str(&format!(
                "- [{}] {} <!-- id: {}; by: {}; n: {}; since: {}; last: {}; status: {} -->\n",
                entry.scope,
                entry.text,
                entry.id,
                entry.by,
                entry.breaks,
                entry.since,
                entry.last,
                entry.status
            ));
            for _ in 0..entry.events {
                out.push_str("<!-- agreement-event: ");
                // Events are opaque digests; a re-rendered block keeps them as placeholders so
                // the shape survives even though the original preimage is not reconstructible.
                out.push_str(&"0".repeat(64));
                out.push_str(" -->\n");
            }
        }
        for line in &self.trailing {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

fn split_frontmatter(source: &str) -> (Option<String>, &str) {
    let mut description = None;
    let rest = match source.strip_prefix("---\n") {
        Some(rest) => rest,
        None => return (None, source),
    };
    let Some((front, body)) = rest.split_once("\n---\n") else {
        return (None, source);
    };
    for line in front.lines() {
        if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().to_owned());
        }
    }
    (description, body)
}

fn agreement_event(line: &str) -> Option<&str> {
    let inner = line.trim().strip_prefix("<!--")?.strip_suffix("-->")?;
    let value = inner.trim().strip_prefix("agreement-event:")?;
    Some(value.trim())
}

fn parse_entry(line: &str) -> Option<Agreement> {
    let rest = line.strip_prefix("- [")?;
    let (scope, rest) = rest.split_once("] ")?;
    let (text, meta) = match rest.split_once("<!--") {
        Some((text, meta)) => (text.trim_end(), meta),
        None => (rest.trim_end(), ""),
    };
    if text.is_empty() {
        return None;
    }
    let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
    let meta = meta.strip_suffix("-->").unwrap_or(meta);
    for part in meta.split(';') {
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };
        fields.insert(key.trim(), value.trim());
    }
    Some(Agreement {
        scope: scope.to_owned(),
        text: text.to_owned(),
        id: fields.get("id").copied().unwrap_or_default().to_owned(),
        by: fields.get("by").copied().unwrap_or_default().to_owned(),
        breaks: fields
            .get("n")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        since: fields.get("since").copied().unwrap_or_default().to_owned(),
        last: fields.get("last").copied().unwrap_or_default().to_owned(),
        status: fields.get("status").copied().unwrap_or("active").to_owned(),
        events: 0,
    })
}

/// One block considered for projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Path relative to the store root, with `/` separators.
    pub path: String,
    /// Block body with frontmatter stripped.
    pub body: String,
    /// `description:` frontmatter value.
    pub description: Option<String>,
    /// `expires:` frontmatter value, unparsed.
    pub expires: Option<String>,
    /// `read_only: "true"`.
    pub read_only: bool,
}

impl Block {
    /// Parse one block from its raw file contents.
    pub fn parse(path: impl Into<String>, source: &str) -> Self {
        let path = path.into();
        let mut description = None;
        let mut expires = None;
        let mut read_only = false;
        let mut body = source;
        if let Some(rest) = source.strip_prefix("---\n")
            && let Some((front, after)) = rest.split_once("\n---\n")
        {
            body = after;
            for line in front.lines() {
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                let value = value.trim().trim_matches('"').to_owned();
                match key.trim() {
                    "description" => description = Some(value),
                    "expires" => expires = Some(value),
                    "read_only" => read_only = value.eq_ignore_ascii_case("true"),
                    _ => {}
                }
            }
        }
        Self {
            path,
            body: body.to_owned(),
            description,
            expires,
            read_only,
        }
    }

    /// Whether this block is a projected `system/` block rather than a name-only entry.
    pub fn is_system(&self) -> bool {
        self.path.starts_with("system/") && self.path.ends_with(".md")
    }

    /// Whether the block has expired against `now`.
    ///
    /// Both sides compare at date granularity: a block whose `expires` is `2026-11-10` still
    /// projects on that date and stops the day after, whether `expires` is a date or an instant.
    pub fn is_expired(&self, now: &str) -> bool {
        let Some(expires) = &self.expires else {
            return false;
        };
        expires.get(..10).unwrap_or(expires) < now.get(..10).unwrap_or(now)
    }

    /// The section name used for a `system/` block: the file stem.
    pub fn section(&self) -> &str {
        self.path
            .rsplit('/')
            .next()
            .and_then(|name| name.strip_suffix(".md"))
            .unwrap_or(&self.path)
    }
}

/// The result of projecting a store's blocks into a prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projection {
    /// Slots keyed by section name, in `SLOT_BLOCKS` order first, then file order.
    pub slots: Vec<(String, String)>,
    /// Names of blocks that are listed but not injected.
    pub names: Vec<String>,
    /// Blocks dropped for being malformed or expired, with the reason.
    pub skipped: Vec<(String, String)>,
}

impl Projection {
    /// The rendered projection: named slots, then the name-only index.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (slot, body) in &self.slots {
            out.push_str(&format!("<{slot}>\n{}\n</{slot}>\n", body.trim_end()));
        }
        if !self.names.is_empty() {
            out.push_str("<external_projection>\n");
            for name in &self.names {
                out.push_str(name);
                out.push('\n');
            }
            out.push_str("</external_projection>\n");
        }
        out
    }
}

/// Project parsed blocks for a run starting at `now`.
///
/// Malformed blocks (no `description:` frontmatter) and expired blocks are reported in
/// [`Projection::skipped`] rather than silently dropped.
pub fn project(blocks: &[Block], now: &str) -> Result<Projection, ProjectionError> {
    let mut projection = Projection::default();
    for block in blocks {
        if block.description.is_none() {
            projection
                .skipped
                .push((block.path.clone(), "no description frontmatter".to_owned()));
            continue;
        }
        if block.is_expired(now) {
            projection
                .skipped
                .push((block.path.clone(), "expired".to_owned()));
            continue;
        }
        if !block.is_system() {
            projection.names.push(block.path.clone());
            continue;
        }
        let section = block.section().to_owned();
        projection.slots.push((section, block.body.clone()));
    }
    projection.slots.sort_by_key(|(slot, _)| {
        let index = SLOT_BLOCKS
            .iter()
            .position(|name| name.trim_end_matches(".md") == slot);
        (index.is_none(), index.unwrap_or(usize::MAX))
    });
    if let Some(slot) = projection
        .slots
        .iter()
        .find(|(slot, _)| slot == "agreements")
    {
        let agreements = Agreements::parse(&slot.1);
        if let Some(first) = agreements.blocking().next() {
            return Err(ProjectionError(format!(
                "agreement {} is needs-human ({} break(s)): {}",
                first.id, first.breaks, first.text
            )));
        }
    }
    Ok(projection)
}

/// Load `<root>/system/*.md` blocks as projection inputs.
///
/// A whole-store loader is deliberately out of scope here: the MIT store keeps structured
/// memories in SQLite and only the `system/` slot tree on disk, so callers pass the blocks they
/// already have. This helper exists for the disk half.
pub fn system_blocks(root: &Path) -> std::io::Result<Vec<Block>> {
    let dir = root.join("system");
    let mut blocks = Vec::new();
    if !dir.is_dir() {
        return Ok(blocks);
    }
    let mut paths: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let source = std::fs::read_to_string(&path)?;
        blocks.push(Block::parse(format!("system/{name}"), &source));
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGREE: &str = "---
description: Agreements - standing working agreements
---
- [global] Keep the build green. <!-- id: 9b79da6144; by: agent; n: 2; since: 2026-10-06; last: 2026-10-18; status: active -->
<!-- agreement-event: 37c5491d7b83c3caa1c1ef2961203409bed941505ceda11fc117a31effbd82b8 -->
<!-- agreement-event: 6bbd64aada62cd3983ff5aafd613731ece5ab1d8c745249241446bbdb03f2683 -->
- [project] No cloud deploy. <!-- id: 137f2b97ee; by: agent; n: 8; since: 2026-10-06; last: 2026-10-26; status: needs-human -->
";

    #[test]
    fn parses_every_field_of_an_agreement_entry() {
        let parsed = Agreements::parse(AGREE);
        assert_eq!(parsed.entries.len(), 2);
        let first = &parsed.entries[0];
        assert_eq!(first.scope, "global");
        assert_eq!(first.text, "Keep the build green.");
        assert_eq!(first.id, "9b79da6144");
        assert_eq!(first.by, "agent");
        assert_eq!(first.breaks, 2);
        assert_eq!(first.since, "2026-10-06");
        assert_eq!(first.last, "2026-10-18");
        assert_eq!(first.status, "active");
        assert_eq!(first.events, 2);
        assert!(first.history_mismatch().is_none());
    }

    #[test]
    fn a_needs_human_agreement_blocks_work() {
        let parsed = Agreements::parse(AGREE);
        let blocking: Vec<_> = parsed.blocking().map(|entry| entry.id.as_str()).collect();
        assert_eq!(blocking, ["137f2b97ee"]);
    }

    #[test]
    fn an_entry_without_events_reports_the_missing_history() {
        let source = "- [project] No cloud deploy. <!-- id: 137f2b97ee; by: agent; n: 8; since: 2026-10-06; last: 2026-10-26; status: needs-human -->\n";
        let parsed = Agreements::parse(source);
        let mismatch = parsed.entries[0].history_mismatch().expect("mismatch");
        assert!(mismatch.contains("records 8 break(s) but has 0 event line(s)"));
    }

    #[test]
    fn unknown_status_and_scope_values_survive_a_parse() {
        let source = "- [team] Something new. <!-- id: abc123def456; by: human; n: 1; since: 2026-01-01; last: 2026-01-02; status: deferred -->\n";
        let parsed = Agreements::parse(source);
        assert_eq!(parsed.entries[0].status, "deferred");
        assert_eq!(parsed.entries[0].scope, "team");
        assert_eq!(parsed.entries[0].by, "human");
        assert!(!parsed.entries[0].blocks_work());
    }

    #[test]
    fn frontmatter_and_entries_round_trip() {
        let parsed = Agreements::parse(AGREE);
        assert_eq!(
            parsed.description.as_deref(),
            Some("Agreements - standing working agreements")
        );
        let rendered = parsed.render();
        assert!(rendered.starts_with("---\ndescription: Agreements"));
        assert!(rendered.contains("- [global] Keep the build green. <!-- id: 9b79da6144; by: agent; n: 2; since: 2026-10-06; last: 2026-10-18; status: active -->"));
        assert_eq!(rendered.matches("<!-- agreement-event:").count(), 2);
    }

    #[test]
    fn projection_injects_system_blocks_and_only_names_the_rest() {
        let blocks = vec![
            Block::parse(
                "system/persona.md",
                "---\ndescription: Persona\n---\nYou are someone.\n",
            ),
            Block::parse(
                "notes/facts/2026-11.md",
                "---\ndescription: Facts\n---\nsecret body\n",
            ),
            Block::parse(
                "people/human/observations.md",
                "---\ndescription: Obs\n---\nbody\n",
            ),
        ];
        let projection = project(&blocks, "2026-11-10").expect("projection");
        assert_eq!(projection.slots.len(), 1);
        assert_eq!(projection.slots[0].0, "persona");
        assert_eq!(projection.slots[0].1, "You are someone.\n");
        assert_eq!(
            projection.names,
            ["notes/facts/2026-11.md", "people/human/observations.md"]
        );
        let rendered = projection.render();
        assert!(rendered.contains("<persona>\nYou are someone.\n</persona>"));
        assert!(!rendered.contains("secret body"));
        assert!(rendered.contains("<external_projection>\nnotes/facts/2026-11.md"));
    }

    #[test]
    fn a_block_without_a_description_is_reported_not_projected() {
        let blocks = vec![Block::parse("system/rules.md", "no frontmatter here\n")];
        let projection = project(&blocks, "2026-11-10").expect("projection");
        assert!(projection.slots.is_empty());
        assert_eq!(
            projection.skipped,
            [(
                "system/rules.md".to_owned(),
                "no description frontmatter".to_owned()
            )]
        );
    }

    #[test]
    fn an_expired_block_stops_projecting_without_being_rewritten() {
        let source = "---\ndescription: Temporary\nexpires: 2026-11-10\n---\nstale\n";
        let block = Block::parse("system/temp.md", source);
        assert!(block.is_expired("2026-11-11"));
        assert!(!block.is_expired("2026-11-10"));
        assert!(!block.is_expired("2026-11-09"));
        let projection = project(&[block], "2026-11-11").expect("projection");
        assert_eq!(
            projection.skipped,
            [("system/temp.md".to_owned(), "expired".to_owned())]
        );
    }

    #[test]
    fn slot_blocks_come_before_ordinary_system_notes() {
        let blocks = vec![
            Block::parse(
                "system/deploy-rule.md",
                "---\ndescription: Deploy\n---\nbody\n",
            ),
            Block::parse(
                "system/self-aware.md",
                "---\ndescription: Self\n---\nbody\n",
            ),
            Block::parse(
                "system/persona.md",
                "---\ndescription: Persona\n---\nbody\n",
            ),
        ];
        let projection = project(&blocks, "2026-11-10").expect("projection");
        let order: Vec<_> = projection
            .slots
            .iter()
            .map(|(slot, _)| slot.as_str())
            .collect();
        assert_eq!(order, ["persona", "self-aware", "deploy-rule"]);
    }

    #[test]
    fn a_needs_human_agreement_fails_the_projection() {
        let blocks = vec![Block::parse("system/agreements.md", AGREE)];
        let error = project(&blocks, "2026-11-10").expect_err("needs-human must fail");
        assert!(error.0.contains("137f2b97ee"));
        assert!(error.0.contains("No cloud deploy."));
    }

    #[test]
    fn read_only_blocks_project_but_record_the_flag() {
        let block = Block::parse(
            "system/boundaries.md",
            "---\ndescription: Boundaries\nread_only: \"true\"\n---\nbody\n",
        );
        assert!(block.read_only);
        assert_eq!(block.body, "body\n");
    }
}
