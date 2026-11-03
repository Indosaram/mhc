//! Injected texts: contract wrapper, reminder, mass block and the todo advisory.

use maho_ext_api::{CustomMessage, ToolContent};
use serde_json::json;

/// Full core contract, canonical copy under `contract/`.
pub const CORE_CONTRACT: &str = include_str!("../contract/core.md");

pub const DIRECTIVE_OPEN_TAG: &str = "<omo-ultrawork-directive>";
pub const DIRECTIVE_CLOSE_TAG: &str = "</omo-ultrawork-directive>";
pub const DIRECTIVE_MESSAGE_TYPE: &str = "omo-ultrawork:directive";
pub const REMINDER_MESSAGE_TYPE: &str = "omo-ultrawork:reminder";

const REMINDER: &str = "<omo-ultrawork-reminder>\n\
Ultrawork is still active in this conversation. No new contract text follows this notice, \
because the copy delivered earlier in the conversation still applies in full.\n\
</omo-ultrawork-reminder>";

const MASS_BLOCK: &str = "<omo-ultrawork-mass>\n\
Mass mode is on for this task. Run it as waves; this block is complete and needs no file reads.\n\
\n\
Wave 1, parallel lanes: split the work into lanes that each own a disjoint write scope, and \
start them together. Before dispatch, check that no two lanes can touch the same file; if \
they can, merge those lanes or sequence them. A lane returns a compact claim summary of at \
most 3,500 characters, with a concrete file and line behind every claim, so that a reader can \
check each claim without re-running the lane.\n\
\n\
Wave 2, falsification: start separate verification lanes whose only job is to disprove the \
Wave 1 claims. They hunt for contradicting evidence and for edge cases nobody handled. A \
claim that no verifier could break counts as verified; a claim that was broken goes back for \
repair, and the lane that made it does not grade its own work.\n\
\n\
Integration: you reconcile the verified findings yourself and settle anything that crosses lane \
boundaries, such as a shared interface that two lanes each changed. Do not leave integration \
until you hold one agreed list of defects, each with its evidence.\n\
\n\
Fix wave: apply the smallest edits that remove the defects that survived verification. Leave \
working code alone.\n\
\n\
Verification wave: run the final end-to-end check on the combined result before you report, \
and say which checks you ran and which you could not run.\n\
</omo-ultrawork-mass>";

const TODO_ADVISORY: &str = "<omo-ultrawork-reminder>\n\
A task list with several items was just created. Before starting the first item, weigh how \
much work the whole list represents: count the items and estimate the effort each one needs. \
Then choose explicitly between handing items to parallel child agents and doing them yourself. \
Base the choice on two things: whether the items can proceed independently of each other, \
and whether parallel work still saves time once the cost of setting it up is counted. Tell \
the user which way you went and why. For the rest of the run, keep the checklist matching \
what is actually done.\n\
</omo-ultrawork-reminder>";

pub fn mass_block() -> &'static str {
    MASS_BLOCK
}

pub fn todo_advisory() -> &'static str {
    TODO_ADVISORY
}

pub fn wrapped_contract() -> String {
    format!("{DIRECTIVE_OPEN_TAG}\n{CORE_CONTRACT}\n{DIRECTIVE_CLOSE_TAG}")
}

/// Hidden custom message that carries the full contract (plus the mass block when asked).
pub fn directive_message(revision: u64, mass: bool) -> CustomMessage {
    let mut content = vec![ToolContent::text(wrapped_contract())];
    if mass {
        content.push(ToolContent::text(MASS_BLOCK));
    }
    CustomMessage {
        custom_type: DIRECTIVE_MESSAGE_TYPE.into(),
        content,
        display: false,
        details: Some(json!({"revision": format!("{revision:016x}"), "mass": mass})),
    }
}

/// Hidden custom message that replaces the contract on a re-mention.
pub fn reminder_message(mass: bool) -> CustomMessage {
    let mut content = vec![ToolContent::text(REMINDER)];
    if mass {
        content.push(ToolContent::text(MASS_BLOCK));
    }
    CustomMessage {
        custom_type: REMINDER_MESSAGE_TYPE.into(),
        content,
        display: false,
        details: Some(json!({"mass": mass})),
    }
}

/// The same texts as plain prompt text, for inputs that cannot carry a custom message.
pub fn inline_block(full: bool, mass: bool) -> String {
    let mut out = if full {
        wrapped_contract()
    } else {
        REMINDER.to_owned()
    };
    if mass {
        out.push_str("\n\n");
        out.push_str(MASS_BLOCK);
    }
    out
}
