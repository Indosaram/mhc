//! Resident prompt envelopes (latest `kibitzer/sidecar-prompt.ts` + `sidecar-prompt-reseed.ts`).
//!
//! The seed / wake envelopes carry the read-only contract, the task line, the folded digest, the
//! newest event window and the candidate list; the reseed envelope restarts a replacement child with
//! the state a restart would lose. Three rules hold for every envelope, because the child is a model
//! reading attacker-influenced transcript text: redact, then cap, then escape - in that order; every
//! embedded body has a cap; and the envelope states the read-only contract and the exact five tools.
//! Every length is measured in UTF-16 code units (JS `String.length`), never `chars()`.
//!
//! The event window is an ALREADY-CAPTURED batch: the producer hands the envelope a borrowed
//! `&[KibitzerEvent]` (upstream `KibitzerSidecarEvent[]`), never the live stream, so the prompt never
//! recaptures. Events are ordered by ascending parent cursor and the newest `event_window` are kept;
//! each kept event renders as `<event cursor kind [tool]>` with ONE nested body element chosen by
//! kind (`<text>` for prompt/assistant, `<args>` for tool_call, `<result>` for tool_result), re-capped
//! at render. The outer `cursor-from` / `cursor-to` range spans EVERY event cursor plus the digest
//! range; the digest renders as its own `<digest>` element.

use crate::kibitzer_events::KibitzerEvent;
use crate::kibitzer_prompt_blocks::{
    KibitzerFieldCaps, escape_text, field, open_tag, render_contract, render_task, single_line,
};

/// Newest events carried whole by one envelope; everything older belongs to the folded digest.
pub const KIBITZER_EVENT_WINDOW: usize = 20;
/// Whole-envelope bound on a reseed: a restart must never re-explode the context it is escaping.
pub const KIBITZER_RESEED_MAX_CHARS: usize = 4000;

/// One candidate the envelope offers the child.
#[derive(Clone, Debug, PartialEq)]
pub struct KibitzerSidecarCandidate {
    pub path: String,
    pub description: Option<String>,
    pub excerpt: Option<String>,
    pub score: Option<f64>,
}

/// The one-line fold of every event older than the window; the cursor range survives the fold.
#[derive(Clone, Debug, PartialEq)]
pub struct KibitzerSidecarDigest {
    pub text: String,
    pub cursor_from: usize,
    pub cursor_to: usize,
    pub folded: usize,
}

/// Everything a seed / wake envelope renders.
pub struct KibitzerEnvelopeInput<'a> {
    pub session_id: &'a str,
    /// `memory.recall.max_items`: how many nudges this wake may accept.
    pub max_items: usize,
    /// The already-captured event batch, oldest first; the prompt orders by cursor and windows it.
    pub events: &'a [KibitzerEvent],
    pub candidates: &'a [KibitzerSidecarCandidate],
    pub digest: Option<&'a KibitzerSidecarDigest>,
    /// One line naming what the parent is working on; the seed carries it, a wake usually does not.
    pub task_summary: Option<&'a str>,
    /// `memory.recall.tool_budget`: tool calls this wake may spend.
    pub tool_budget: Option<usize>,
    pub caps: KibitzerFieldCaps,
    pub event_window: Option<usize>,
}

/// Everything a reseed envelope renders.
pub struct KibitzerReseedInput<'a> {
    pub session_id: &'a str,
    pub max_items: usize,
    /// The newest parent cursor the disposed child had seen; the new child resumes from here.
    pub last_cursor: usize,
    pub task_summary: &'a str,
    /// Paths offered to the disposed child that it declined; they are not worth re-judging.
    pub rejected_paths: &'a [String],
    /// Paths already delivered to the parent; they can never be nudged again.
    pub delivered_paths: &'a [String],
    pub tool_budget: Option<usize>,
    pub caps: KibitzerFieldCaps,
    pub max_chars: Option<usize>,
}

/// JS `String.length`: UTF-16 code units.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The first turn of a fresh resident child: contract, live events, folded history, candidates.
pub fn render_kibitzer_seed_prompt(input: &KibitzerEnvelopeInput) -> String {
    render_envelope("kibitzer-seed", input)
}

/// Every later turn of the same child: the same contract over the newest batch of events.
pub fn render_kibitzer_wake_prompt(input: &KibitzerEnvelopeInput) -> String {
    render_envelope("kibitzer-wake", input)
}

fn render_envelope(tag: &str, input: &KibitzerEnvelopeInput) -> String {
    let caps = input.caps;
    let window = input.event_window.unwrap_or(KIBITZER_EVENT_WINDOW);
    // `[...input.events].sort((left, right) => left.cursor - right.cursor)`: ascending parent cursor.
    let mut ordered: Vec<&KibitzerEvent> = input.events.iter().collect();
    ordered.sort_by(|left, right| left.cursor.cmp(&right.cursor));
    // `ordered.length <= window ? ordered : ordered.slice(ordered.length - window)`: newest window.
    let kept: &[&KibitzerEvent] = if ordered.len() <= window { &ordered } else { &ordered[ordered.len() - window..] };

    let mut attributes: Vec<(String, String)> = vec![
        ("version".to_string(), "1".to_string()),
        ("session".to_string(), input.session_id.to_string()),
        ("max-items".to_string(), input.max_items.to_string()),
    ];
    if let Some(budget) = input.tool_budget {
        attributes.push(("tool-budget".to_string(), budget.to_string()));
    }
    // The range spans every cursor the envelope knows about: EVERY event (including the ones the
    // window drops) plus the digest's range. The child must still see the real span.
    let mut cursors: Vec<usize> = ordered.iter().map(|event| event.cursor).collect();
    if let Some(digest) = input.digest {
        cursors.push(digest.cursor_from);
        cursors.push(digest.cursor_to);
    }
    if let (Some(min), Some(max)) = (cursors.iter().min(), cursors.iter().max()) {
        attributes.push(("cursor-from".to_string(), min.to_string()));
        attributes.push(("cursor-to".to_string(), max.to_string()));
    }

    let borrowed: Vec<(&str, &str)> = attributes.iter().map(|(key, value)| (key.as_str(), value.as_str())).collect();
    let mut parts = vec![open_tag(tag, &borrowed), render_contract()];
    if let Some(summary) = input.task_summary {
        parts.push(render_task(summary, caps));
    }
    if let Some(digest) = input.digest {
        parts.push(render_digest(digest, caps));
    }
    parts.push(render_events(kept, ordered.len() - kept.len(), caps));
    parts.push(render_candidates(input.candidates, input.max_items, caps));
    parts.push(format!("</{tag}>"));
    parts.push(String::new());
    parts.join("\n")
}

/// `<digest cursor-from cursor-to folded>` + the single-lined, capped, escaped fold text.
fn render_digest(digest: &KibitzerSidecarDigest, caps: KibitzerFieldCaps) -> String {
    let open = open_tag("digest", &[
        ("cursor-from", &digest.cursor_from.to_string()),
        ("cursor-to", &digest.cursor_to.to_string()),
        ("folded", &digest.folded.to_string()),
    ]);
    format!("{open}{}</digest>", escape_text(&single_line(&field(&digest.text, caps.digest))))
}

/// `<events count=".." omitted="..">`: the newest window, each event one `<event>` block.
fn render_events(events: &[&KibitzerEvent], omitted: usize, caps: KibitzerFieldCaps) -> String {
    let open = open_tag("events", &[("count", &events.len().to_string()), ("omitted", &omitted.to_string())]);
    if events.is_empty() {
        return format!("{open}</events>");
    }
    let mut lines = vec![open];
    for event in events.iter().copied() {
        lines.push(render_event(event, caps));
    }
    lines.push("</events>".to_string());
    lines.join("\n")
}

/// `<event cursor=".." kind=".."[ tool=".."]>` + ONE nested body element chosen by kind, re-capped at
/// render (redact, then cap, then escape). The prompt wire carries no `seq` / `error` / `truncated`.
fn render_event(event: &KibitzerEvent, caps: KibitzerFieldCaps) -> String {
    let cursor = event.cursor.to_string();
    let mut attributes: Vec<(&str, &str)> = vec![("cursor", cursor.as_str()), ("kind", event.kind.as_str())];
    if let Some(tool) = event.tool.as_deref() {
        attributes.push(("tool", tool));
    }
    let open = open_tag("event", &attributes);
    let mut body: Vec<String> = Vec::new();
    match event.kind.as_str() {
        "prompt" => body.push(format!("<text>{}</text>", escape_text(&field(&event.body, caps.prompt)))),
        "assistant" => body.push(format!("<text>{}</text>", escape_text(&field(&event.body, caps.assistant)))),
        "tool_call" => body.push(format!("<args>{}</args>", escape_text(&field(&event.body, caps.tool_args)))),
        "tool_result" => body.push(format!("<result>{}</result>", escape_text(&field(&event.body, caps.result_head)))),
        _ => {}
    }
    let mut lines = vec![open];
    lines.extend(body);
    lines.push("</event>".to_string());
    lines.join("\n")
}

fn render_candidates(candidates: &[KibitzerSidecarCandidate], max_items: usize, caps: KibitzerFieldCaps) -> String {
    let open = open_tag("candidates", &[("count", &candidates.len().to_string()), ("max-items", &max_items.to_string())]);
    if candidates.is_empty() {
        return format!("{open}</candidates>");
    }
    let mut lines = vec![open];
    for candidate in candidates {
        lines.push(render_candidate(candidate, caps));
    }
    lines.push("</candidates>".to_string());
    lines.join("\n")
}

fn render_candidate(candidate: &KibitzerSidecarCandidate, caps: KibitzerFieldCaps) -> String {
    let score = candidate.score.map(|value| value.to_string());
    let mut attributes: Vec<(&str, &str)> = vec![("path", candidate.path.as_str())];
    if let Some(score) = score.as_deref() {
        attributes.push(("score", score));
    }
    let mut body: Vec<String> = Vec::new();
    if let Some(description) = &candidate.description {
        body.push(format!("<description>{}</description>", escape_text(&single_line(&field(description, caps.candidate)))));
    }
    if let Some(excerpt) = &candidate.excerpt {
        body.push(format!("<excerpt>{}</excerpt>", escape_text(&field(excerpt, caps.candidate))));
    }
    let mut lines = vec![open_tag("candidate", &attributes)];
    lines.extend(body);
    lines.push("</candidate>".to_string());
    lines.join("\n")
}

/// The seed of a replacement child after the previous one hit its context budget. Path lists are
/// trimmed whole-line to the bound with the omitted counts reported, and both lists stay INSIDE the
/// envelope.
pub fn render_kibitzer_reseed_prompt(input: &KibitzerReseedInput) -> String {
    let bound = input.max_chars.unwrap_or(KIBITZER_RESEED_MAX_CHARS);
    let mut attributes: Vec<(String, String)> = vec![
        ("version".to_string(), "1".to_string()),
        ("session".to_string(), input.session_id.to_string()),
        ("cursor".to_string(), input.last_cursor.to_string()),
        ("max-items".to_string(), input.max_items.to_string()),
    ];
    if let Some(budget) = input.tool_budget {
        attributes.push(("tool-budget".to_string(), budget.to_string()));
    }
    let borrowed: Vec<(&str, &str)> = attributes.iter().map(|(key, value)| (key.as_str(), value.as_str())).collect();
    let head = [
        open_tag("kibitzer-reseed", &borrowed),
        render_contract(),
        render_task(input.task_summary, input.caps),
    ]
    .join("\n");
    let rejected: Vec<String> = input.rejected_paths.iter().map(|path| format!("<path>{}</path>", escape_text(path))).collect();
    let delivered: Vec<String> = input.delivered_paths.iter().map(|path| format!("<path>{}</path>", escape_text(path))).collect();
    // The fixed part is measured with the LARGEST omitted counts the lists can produce, so the
    // rendered envelope can only be shorter than the length this budget was computed from.
    let fixed = utf16_len(&reseed_envelope(&head, &[], rejected.len(), &[], delivered.len()));
    let budget = bound.saturating_sub(fixed);
    let (kept_rejected, rejected_used) = fit_lines(&rejected, budget / 2);
    let (kept_delivered, _) = fit_lines(&delivered, budget.saturating_sub(rejected_used));
    reseed_envelope(&head, &kept_rejected, rejected.len(), &kept_delivered, delivered.len())
}

fn reseed_envelope(head: &str, rejected: &[String], rejected_total: usize, delivered: &[String], delivered_total: usize) -> String {
    [
        head.to_string(),
        render_path_list("rejected", rejected, rejected_total),
        render_path_list("delivered", delivered, delivered_total),
        "</kibitzer-reseed>".to_string(),
        String::new(),
    ]
    .join("\n")
}

fn render_path_list(tag: &str, lines: &[String], total: usize) -> String {
    let open = open_tag(tag, &[("count", &lines.len().to_string()), ("omitted", &(total - lines.len()).to_string())]);
    if lines.is_empty() {
        format!("{open}</{tag}>")
    } else {
        format!("{open}\n{}\n</{tag}>", lines.join("\n"))
    }
}

/// Greedily keeps whole lines (with their newline) while they fit the budget; never splits one.
fn fit_lines(lines: &[String], budget: usize) -> (Vec<String>, usize) {
    let mut kept = Vec::new();
    let mut used = 0usize;
    for line in lines {
        let cost = utf16_len(line) + 1;
        if used + cost > budget {
            break;
        }
        kept.push(line.clone());
        used += cost;
    }
    (kept, used)
}
