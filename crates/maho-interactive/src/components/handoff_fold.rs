//! Port of senpi `packages/coding-agent/src/modes/interactive/components/handoff-fold.ts`.
//!
//! Finds the handoff block (`Ask: ... - wanted: ... For you: ... Now: ... Next: ...`, or the GPT
//! presets' `... You need: ... Now: ... Next: ...`) in an assistant text so the transcript can fold
//! its bookkeeping into one muted line (senpi#3323). The labels stay English by prompt contract,
//! so they are matched literally; the slot contents are any language.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffSegment {
    /// The block exactly as written, shown when the fold is open.
    pub block: String,
    /// What the reader needs, kept in the transcript as answer text: the `For you:` slot, or for
    /// the GPT shape the leading outcome followed by the `You need:` slot.
    pub answer: Option<String>,
    pub now: Option<String>,
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistantTextSegment {
    Text(String),
    Handoff(HandoffSegment),
}

static LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\*{1,2})?\b(Ask|wanted|For you|You need|Now|Next)(?:\*{1,2})?\s*[:\x{FF1A}](?:\*{1,2})?")
        .expect("valid handoff label regex")
});
static TRAILING_DASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+-$").expect("valid trailing dash regex"));
static PARAGRAPH_BREAK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n[ \t]*\n").expect("valid paragraph regex"));
static FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(?:```|~~~)").expect("valid fence regex"));

/// The two label sequences the prompts produce, matched exactly so a paragraph that merely uses
/// some of these words ("You need: Node 22. Now: run install. Next: run tests.") stays prose.
const CLAUDE_SHAPES: [&str; 2] = ["Ask,wanted,For you,Now,Next", "Ask,For you,Now,Next"];
const GPT_SHAPE: &str = "You need,Now,Next";

struct LabelHit<'a> {
    label: &'a str,
    start: usize,
    end: usize,
}

fn labels_of(paragraph: &str) -> Vec<LabelHit<'_>> {
    LABEL
        .captures_iter(paragraph)
        .filter_map(|captures| {
            let whole = captures.get(0)?;
            Some(LabelHit {
                label: captures.get(1)?.as_str(),
                start: whole.start(),
                end: whole.end(),
            })
        })
        .collect()
}

fn slot(paragraph: &str, hits: &[LabelHit<'_>], label: &str) -> Option<String> {
    let at = hits.iter().position(|hit| hit.label == label)?;
    let start = hits[at].end;
    let end = hits.get(at + 1).map_or(paragraph.len(), |hit| hit.start);
    let text = TRAILING_DASH
        .replace(paragraph[start..end].trim(), "")
        .into_owned();
    (!text.is_empty()).then_some(text)
}

fn parse_handoff(paragraph: &str) -> Option<HandoffSegment> {
    let hits = labels_of(paragraph);
    let shape = hits
        .iter()
        .map(|hit| hit.label)
        .collect::<Vec<_>>()
        .join(",");
    let before_first = &paragraph[..hits.first().map_or(0, |hit| hit.start)];
    let segment = |answer| HandoffSegment {
        block: paragraph.to_string(),
        answer,
        now: slot(paragraph, &hits, "Now"),
        next: slot(paragraph, &hits, "Next"),
    };
    if CLAUDE_SHAPES.contains(&shape.as_str()) {
        if !before_first.trim().is_empty() {
            return None;
        }
        return Some(segment(slot(paragraph, &hits, "For you")));
    }
    if shape != GPT_SHAPE {
        return None;
    }
    let outcome = before_first.trim();
    if outcome.is_empty() {
        return None;
    }
    let answer = match slot(paragraph, &hits, "You need") {
        None => outcome.to_string(),
        Some(needs) => format!("{outcome} {needs}"),
    };
    Some(segment(Some(answer)))
}

/// Splits an assistant text into prose and handoff blocks. A block is one paragraph (blank-line
/// separated) whose labels are exactly `Ask:` [`wanted:`] `For you:` `Now:` `Next:`, or an outcome
/// sentence followed by exactly `You need:` `Now:` `Next:`; a paragraph inside a code fence never
/// counts. Text with no block comes back as one text segment. A block still streaming stays text
/// until its `Next:` arrives, then folds.
pub fn split_handoff_blocks(text: &str) -> Vec<AssistantTextSegment> {
    let mut segments = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let flush = |prose: &mut Vec<&str>, segments: &mut Vec<AssistantTextSegment>| {
        let joined = prose.join("\n\n").trim().to_string();
        if !joined.is_empty() {
            segments.push(AssistantTextSegment::Text(joined));
        }
        prose.clear();
    };
    for paragraph in PARAGRAPH_BREAK.split(text) {
        let fences = FENCE.find_iter(paragraph).count();
        let handoff = if !in_fence && fences == 0 {
            parse_handoff(paragraph.trim())
        } else {
            None
        };
        if fences % 2 == 1 {
            in_fence = !in_fence;
        }
        match handoff {
            None => prose.push(paragraph),
            Some(handoff) => {
                flush(&mut prose, &mut segments);
                segments.push(AssistantTextSegment::Handoff(handoff));
            }
        }
    }
    flush(&mut prose, &mut segments);
    segments
}

#[cfg(test)]
mod tests {
    //! Port of the `splitHandoffBlocks` cases of senpi `assistant-message-handoff-fold.test.ts`.

    use super::*;

    const BLOCK: &str = "Ask: fix the login redirect - wanted: users land on the dashboard. For you: the redirect now reads the saved path; 12 tests pass. Now: Post milestones and report. Next: Open the PR.";
    const GPT_BLOCK: &str = "Redirect fixed and the login works for every user. You need: 3/3 done, no blockers. Now: Report the fix. Next: none.";

    fn text(text: &str) -> Vec<AssistantTextSegment> {
        vec![AssistantTextSegment::Text(text.to_string())]
    }

    fn handoff(segment: &AssistantTextSegment) -> &HandoffSegment {
        match segment {
            AssistantTextSegment::Handoff(handoff) => handoff,
            AssistantTextSegment::Text(text) => {
                panic!("expected a handoff block, got text {text:?}")
            }
        }
    }

    #[test]
    fn a_plain_answer_is_one_text_segment() {
        let input = "The function parses the header.\n\nNow: it also checks the length.";
        assert_eq!(split_handoff_blocks(input), text(input));
    }

    #[test]
    fn finds_the_block_and_its_slots_between_prose_paragraphs() {
        let segments = split_handoff_blocks(&format!("Intro line.\n\n{BLOCK}\n\nDetail after."));
        assert_eq!(segments.len(), 3);
        assert_eq!(
            segments[0],
            AssistantTextSegment::Text("Intro line.".to_string())
        );
        assert_eq!(
            segments[2],
            AssistantTextSegment::Text("Detail after.".to_string())
        );
        let block = handoff(&segments[1]);
        assert_eq!(
            block.answer.as_deref(),
            Some("the redirect now reads the saved path; 12 tests pass.")
        );
        assert_eq!(block.now.as_deref(), Some("Post milestones and report."));
        assert_eq!(block.next.as_deref(), Some("Open the PR."));
    }

    #[test]
    fn the_gpt_presets_block_keeps_its_outcome_sentence_as_the_answer() {
        let segments = split_handoff_blocks(GPT_BLOCK);
        let block = handoff(&segments[0]);
        assert_eq!(
            block.answer.as_deref(),
            Some("Redirect fixed and the login works for every user. 3/3 done, no blockers.")
        );
        assert_eq!(block.now.as_deref(), Some("Report the fix."));
        assert_eq!(block.next.as_deref(), Some("none."));
    }

    #[test]
    fn prose_that_only_uses_the_labels_stays_text() {
        for input in [
            "You need: Node 22. Now: run install. Next: run tests.",
            "Setup is short. Next: run tests. Now: run install. You need: Node 22.",
            "For you: a gift. Now: open it. Next: say thanks.",
            "Ask: nothing. Now: rest. Next: sleep.",
        ] {
            assert_eq!(split_handoff_blocks(input), text(input), "{input}");
        }
    }

    #[test]
    fn a_for_you_that_opens_with_a_bullet_list_keeps_its_markers() {
        let input = "Ask: tidy up - wanted: clean tree. For you:\n- removed the temp dir\n- kept the worktree\nNow: none. Next: none.";
        let segments = split_handoff_blocks(input);
        assert_eq!(
            handoff(&segments[0]).answer.as_deref(),
            Some("- removed the temp dir\n- kept the worktree")
        );
    }

    #[test]
    fn a_block_still_streaming_stays_text_until_next_arrives() {
        let partial = "Ask: fix it - wanted: fixed. For you: done. Now: a.";
        assert_eq!(split_handoff_blocks(partial), text(partial));
    }

    #[test]
    fn a_block_inside_a_code_fence_stays_text() {
        let input = format!("```\n{BLOCK}\n```");
        assert_eq!(split_handoff_blocks(&input), text(&input));
    }
}
