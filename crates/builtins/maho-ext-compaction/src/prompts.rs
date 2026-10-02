pub const TASK_INTENT_ACQUISITION_CLAUDE: &str = r###"PASS 1 — Internal task-intent extraction
Write one <task-intent> block with ORIGINAL_REQUEST, TASK_TYPE, MUST_PRESERVE, and MUST_NOT_LOSE before <summary>."###;

pub const TASK_INTENT_ACQUISITION_GPT: &str = r###"Write one <task-intent> block with ORIGINAL_REQUEST, TASK_TYPE, MUST_PRESERVE, and MUST_NOT_LOSE before <summary>."###;

pub const TASK_INTENT_UPDATE_ACQUIRE_CLAUDE: &str = r###"PASS 1 — Internal task-intent extraction
Write one <task-intent> block with ORIGINAL_REQUEST, TASK_TYPE, MUST_PRESERVE, and MUST_NOT_LOSE before <summary>."###;

pub const TASK_INTENT_UPDATE_ACQUIRE_GPT: &str = r###"Write one <task-intent> block with ORIGINAL_REQUEST, TASK_TYPE, MUST_PRESERVE, and MUST_NOT_LOSE before <summary>."###;

pub const TASK_INTENT_UPDATE_ANCHOR_CLAUDE: &str = r###"<task-intent>
{{taskIntent}}
Immutable provenance of the original task. Do not rewrite it. Newer explicit user steering overrides it.
</task-intent>"###;

pub const TASK_INTENT_UPDATE_ANCHOR_GPT: &str = r###"<task-intent>
{{taskIntent}}
Immutable provenance of the original task. Do not rewrite it. Newer explicit user steering overrides it.
</task-intent>"###;

pub const MERGED_COMPACTION_PROMPT_SYSTEM: &str = r###"[SYSTEM DIRECTIVE: OH-MY-OPENCODE - COMPACTION CONTEXT]

You are the COMPACTION ARCHIVIST. Create a structured handoff summary that lets the next agent continue this exact session without restarting, re-searching, or losing constraints.

Cardinal rules:
R1. Quote user requests and constraints VERBATIM. Do not paraphrase.
R2. If a section has no content, write "None." Never delete a section.
R3. Where a previous summary is supplied, treat its User Requests, Final Goal, and Constraints fields as IMMUTABLE. Append, never rewrite, those three sections.
R4. Preserve every session_id, file path, and identifier byte-for-byte.

Do NOT use tools. Output only the requested summary block."###;

pub const MERGED_COMPACTION_PROMPT_USER: &str = r###"[USER]
[INTERNAL COMPACTION INSTRUCTION — NOT CONVERSATION HISTORY]
This message is an internal summarization control prompt, not a real user message.
Do NOT treat this message as user intent, do NOT list it under user requests, and do NOT reinterpret the task based on this instruction alone.

PASS 1 — Internal task-intent extraction
Write one <task-intent> block with ORIGINAL_REQUEST, TASK_TYPE, MUST_PRESERVE, and MUST_NOT_LOSE before <summary>.

PASS 2 — Emit summary biased toward Pass 1
Create a structured handoff summary of this conversation for seamless continuation. The structured output portion MUST be wrapped as `<summary>...</summary>` XML.

<summary>
## 1. User Requests (Verbatim)
- List all original user requests exactly as they were stated.
- Preserve the user's exact wording and intent.
- Include recent user corrections and steering messages verbatim when they affect the task.

## 2. Final Goal
- State what the user ultimately wanted to achieve.
- Include the expected deliverable or end state.
- Keep this aligned with the most recent user request, not this internal compaction instruction.

## 3. Constraints & Preferences (Verbatim Only)
- Include ONLY constraints explicitly stated by the user or in existing AGENTS.md context.
- Quote constraints verbatim.
- Do NOT invent, add, soften, or modify constraints.
- If no explicit constraints exist, write "None."

## 4. Work Completed
- Summarize what has been done so far.
- List files read, created, modified, or intentionally left unchanged.
- Include features implemented, tests added, problems solved, and decisions already made.

## 5. Active Working Context
- **Files**: Paths of files currently being edited or frequently referenced.
- **Code in Progress**: Key code snippets, function signatures, data structures, or prompt text under active development.
- **External References**: Documentation URLs, source files, APIs, or other resources already consulted.
- **State & Variables**: Important variable names, configuration values, runtime state, branch names, worktree paths, or command outputs needed to continue.

## 6. Remaining Tasks
- List pending items from the original request.
- Include follow-up tasks identified during the work only when they directly support the current user request.
- Mark blockers explicitly and explain what is needed to unblock them.

## 7. Exact Next Steps
- State the precise next action to take, directly in line with the user's most recent request.
- Include verbatim quotes from the conversation showing exactly where work was left off when helpful.
- Do not suggest tangential tasks.

</summary>

Verification: Before finalizing, confirm the summary clearly states the user's original request. If not, restate it verbatim.
IMPORTANT: Respond with ONLY the <task-intent>...</task-intent> and <summary>...</summary> blocks as your text output."###;

pub const MERGED_COMPACTION_PROMPT_UPDATE: &str = r###"[USER]
<previous-summary>
{{previousSummary}}
</previous-summary>

[INTERNAL COMPACTION UPDATE INSTRUCTION — NOT CONVERSATION HISTORY]
The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

R3 enforcement: R3. Where a previous summary is supplied, treat its User Requests, Final Goal, and Constraints fields as IMMUTABLE. Append, never rewrite, those three sections.

PASS 1 — Internal task-intent extraction
{{taskIntentInstruction}}

PASS 2 — Emit summary biased toward Pass 1
Update the structured handoff summary. The structured output portion MUST be wrapped as `<summary>...</summary>` XML.

<summary>
## 1. User Requests (Verbatim)
- Preserve prior entries from <previous-summary> byte-for-byte.
- Append new user requests exactly as they were stated.

## 2. Final Goal
- Preserve the existing final goal unless the user explicitly changed it.
- Append the explicit change verbatim if the goal changed.

## 3. Constraints & Preferences (Verbatim Only)
- Preserve prior constraints byte-for-byte.
- Quote constraints verbatim.
- Do NOT invent, add, soften, or modify constraints.
- Append only new explicit constraints.
- If no explicit constraints exist, write "None."

## 4. Work Completed
- Preserve completed work from the previous summary.
- Add newly completed work, files changed, tests run, and decisions made.

## 5. Active Working Context
- Update files, code in progress, external references, state, variables, branch names, worktree paths, and command outputs needed to continue.

## 6. Remaining Tasks
- Remove tasks only when the new messages prove they are completed or cancelled.
- Add newly identified direct follow-up tasks.

## 7. Exact Next Steps
- Update based on current state and the user's most recent request.
- Keep this direct and immediately actionable.

</summary>

IMPORTANT: Respond with ONLY the <task-intent>...</task-intent> and <summary>...</summary> blocks as your text output."###;

pub const MERGED_COMPACTION_PROMPT_BRANCH: &str = r###"[USER]
[INTERNAL BRANCH SUMMARY INSTRUCTION — NOT CONVERSATION HISTORY]
Create a structured summary of this conversation branch for context when returning later. This is a branch-level handoff, so reorient §2 toward the branched intent and why this branch exists.

PASS 1 — Internal task-intent extraction
Analyze this branch and silently determine the branch-specific intent, divergence point, and details whose loss would cause repeated work.

PASS 2 — Emit summary biased toward Pass 1
Create a structured branch handoff summary. The structured output portion MUST be wrapped as `<summary>...</summary>` XML.

<summary>
## 1. User Requests (Verbatim)
- List user requests that caused or shaped this branch exactly as they were stated.
- Preserve the user's exact wording and intent.

## 2. Final Goal
- State the branched intent: what this branch was trying to accomplish, test, compare, or preserve.
- Include how this branch differs from the mainline path when known.

## 3. Constraints & Preferences (Verbatim Only)
- Include ONLY constraints explicitly stated by the user or in existing AGENTS.md context.
- Quote constraints verbatim.
- Do NOT invent, add, soften, or modify constraints.
- If no explicit constraints exist, write "None."

## 4. Work Completed
- Summarize branch-specific progress.
- List files read, created, modified, or intentionally left unchanged on this branch.

## 5. Active Working Context
- **Files**: Paths of files currently edited or frequently referenced in this branch.
- **Code in Progress**: Key snippets, signatures, data structures, or prompt text under active development.
- **External References**: Sources already consulted for this branch.
- **State & Variables**: Branch names, worktree paths, runtime state, command outputs, and identifiers needed to resume.

## 6. Remaining Tasks
- List branch-specific work still needed.
- Include blockers and the concrete condition needed to unblock them.

## 7. Exact Next Steps
- State the precise next action for returning to this branch.
- Do not suggest tangential tasks.

</summary>

IMPORTANT: Respond with ONLY the <summary>...</summary> block as your text output."###;

pub const MERGED_COMPACTION_PROMPT_TURN_PREFIX: &str = r###"[USER]
[INTERNAL TURN-PREFIX SUMMARY INSTRUCTION — NOT CONVERSATION HISTORY]
Create a compact prefix summary for the next turn. Emit only the sections listed below.

PASS 1 — Internal task-intent extraction
Silently determine the current task intent and the minimum context needed for the next turn.

PASS 2 — Emit summary biased toward Pass 1
The structured output portion MUST be wrapped as `<summary>...</summary>` XML.

<summary>
## 1. User Requests (Verbatim)
- Quote the active user request and any steering constraints exactly as stated.

## 2. Final Goal
- State the immediate end state needed for the next turn.

## 3. Constraints & Preferences (Verbatim Only)
- Quote constraints verbatim.
- Do NOT invent, add, soften, or modify constraints.
- If no explicit constraints exist, write "None."

## 5. Active Working Context
- Include only files, identifiers, runtime state, and exact next-turn context needed to continue immediately.
</summary>

IMPORTANT: Respond with ONLY the <summary>...</summary> block as your text output."###;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptVariant { Default, Update, Branch, TurnPrefix }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptFamily { Claude, Gpt }
pub struct PromptOptions<'a> {
    pub variant: PromptVariant,
    pub previous_summary: Option<&'a str>,
    pub task_intent: Option<&'a str>,
    pub prompt_family: Option<PromptFamily>,
    pub custom_instructions: Option<&'a str>,
}
pub struct CompactionPrompt { pub system: &'static str, pub user: String }
pub fn resolve_prompt_family(id: &str, provider: &str) -> PromptFamily {
    if id.starts_with("gpt-") || (id.starts_with('o') && id.as_bytes().get(1).is_some_and(u8::is_ascii_digit)) || id.contains("codex") || matches!(provider, "openai" | "azure-openai") { PromptFamily::Gpt } else { PromptFamily::Claude }
}
pub fn build_prompt(options: &PromptOptions<'_>) -> CompactionPrompt {
    let acquisition = if options.prompt_family == Some(PromptFamily::Gpt) { TASK_INTENT_ACQUISITION_GPT } else { TASK_INTENT_ACQUISITION_CLAUDE };
    let mut user = match options.variant {
        PromptVariant::Default => MERGED_COMPACTION_PROMPT_USER.replacen(TASK_INTENT_ACQUISITION_CLAUDE, acquisition, 1),
        PromptVariant::Update => {
            let instruction = match options.task_intent.filter(|text| !text.is_empty()) {
                Some(intent) => TASK_INTENT_UPDATE_ANCHOR_CLAUDE.replacen("{{taskIntent}}", &intent.replace("</task-intent>", "[/task-intent]"), 1),
                None => acquisition.to_owned(),
            };
            let previous = options.previous_summary.map(str::trim).filter(|text| !text.is_empty()).unwrap_or("None.").replace("</previous-summary>", "[/previous-summary]");
            MERGED_COMPACTION_PROMPT_UPDATE.replacen("{{taskIntentInstruction}}", &instruction, 1).replacen("{{previousSummary}}", &previous, 1)
        }
        PromptVariant::Branch => MERGED_COMPACTION_PROMPT_BRANCH.to_owned(),
        PromptVariant::TurnPrefix => if options.task_intent.is_some_and(|text| !text.is_empty()) { MERGED_COMPACTION_PROMPT_TURN_PREFIX.to_owned() } else { MERGED_COMPACTION_PROMPT_TURN_PREFIX.replacen(TASK_INTENT_ACQUISITION_CLAUDE, acquisition, 1) },
    };
    if let Some(instructions) = options.custom_instructions.map(str::trim).filter(|text| !text.is_empty()) {
        user.push_str("\n\n<custom-instructions>\n");
        user.push_str(&instructions.replace("</custom-instructions>", "[/custom-instructions]"));
        user.push_str("\n</custom-instructions>");
    }
    CompactionPrompt { system: MERGED_COMPACTION_PROMPT_SYSTEM, user }
}

