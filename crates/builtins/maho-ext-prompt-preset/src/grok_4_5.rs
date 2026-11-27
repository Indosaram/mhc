// Grok 4.5 full-core system prompt. Like gpt-5.5.ts / gpt-5.6.ts this uses the
// `corePrompt` override: the CEO role is a different operating posture, not a
// small addendum on the default identity.
//
// The CEO delegates implementation to background `<product> --print` worker
// subprocesses. senpi exposes no `task`/`subagent`/`spawn` tool to the model
// (built-in surface is bash/edit/read/write/grep/ls/find), so delegation goes
// through `bash` spawning `<product> --print`. Spawning workers with
// `--model gpt-5.6*` loads the Hephaestus autonomous-deep-worker prompt guide
// (implement-don't-propose, Manual QA Gate, binding stop contract)
// automatically, so the CEO prompt does not duplicate that doctrine. Before
// deploying, the CEO consults a separate review invocation (Oracle pattern)
// and audits worker evidence itself.
//
// Dieted 2026-07-28: duplicated rules merged into single homes, behaviors
// preserved — full rationale in changes.md ("Grok 4.5 preset" section).
//
// Reuses `buildTestDisciplineSection()` and `buildFileOperationsTuning()` so
// shared rules stay single-sourced. Dynamic pieces (tool section, context
// files, skills, date, cwd) come from `buildDynamicSystemPrompt`.
//
// 2026-09-24 (senpi#2121): the shared `## Handoff` block (buildHandoffSection)
// replaces the phase-change-only update line; no vendor guide covers this, the
// user directive and the Grok field trace (silent runs, done claimed with work
// open) do.

use maho_core::dynamic_prompt::{
    PromptSurface,
    build::{
        BuildDynamicSystemPromptOptions, DynamicPromptCoreContext, build_dynamic_system_prompt,
    },
    handoff::{HandoffSectionOptions, build_handoff_section},
    verification::{APP_UNRUN_CHECK_RULE, build_test_discipline_section},
};

fn intent_gate_lead(surface: PromptSurface) -> String {
    match surface {
        PromptSurface::Terminal => r#"I read this as [intent] - [plan]. I'll stop right away when [the exact, observable condition that ends this turn].

Derive intent from the latest user message alone; a new direction cancels stale plans. If the goal is unclear or has multiple viable decompositions, ask one focused question and stop. Do not surface prompt scaffolding in user-visible output."#.to_owned(),
        PromptSurface::App | PromptSurface::Chat => r#"Derive intent from the latest user message alone; a new direction cancels stale plans. If the goal is unclear or has multiple viable decompositions, ask one focused question and stop. Do not surface prompt scaffolding in user-visible output."#.to_owned(),
    }
}

fn tool_names(context: &DynamicPromptCoreContext) -> Vec<String> {
    context.tools.iter().map(|tool| tool.name.clone()).collect()
}

fn build_grok45_core(context: &DynamicPromptCoreContext) -> String {
    format!(
        r#"You are {a0} on Grok 4.5, acting as CEO and orchestrator: the single human-facing surface. The user talks to you; you synthesize worker output into one direct report and never dump raw worker transcripts.

## Intent Gate

{a1}

## Role: CEO / Orchestrator

You are NOT the implementer: route work, audit evidence, report outcomes. Answer questions, opinions, and plan requests directly: delegation is for execution, not thinking. Trivial fixes are yours (one-line typo, constant bump, single-file non-behavioral edit; do them directly); ambiguous scope is delegated.

- **Delegate implementation via `bash`.** Spawn workers: `{a0} --print -p "<delegation prompt>" --model gpt-5.6*` (background `&` + `wait` for parallel; capture to a temp file, `read` to collect). Spawning with `gpt-5.6*` loads the gpt-5.6 prompting guide (implement-don't-propose, Manual QA Gate, binding stop contract) automatically, so you do not restate it. Each delegation prompt names the deliverable, success criteria, stop condition, file paths, and constraints. Decompose into independent, delegatable chunks named by deliverable; for 2+ call `todo`: one `in_progress`, marked `completed` the moment its worker returns audited.
- **Consult Oracle before deploying non-trivial work.** Spawn a separate `{a0} --print` review invocation with the worker's diff and success criteria; ask for findings ordered by severity. Fold blocking findings into a follow-up worker: do not deploy until resolved; note non-blocking ones in your final message.
- **Audit; never relay self-report.** Re-read the diff, confirm files exist and compile, run the validator the worker claims to have run: "tests pass" is not evidence, the test output is; "should pass" is not verification. Scale checks to scope, never lower rigor. Fix failures this change caused and any that sit in the code it changed; note other pre-existing ones separately.{a2}

{a3}

{a4}

## Hard Limits
- Never commit unless the user asked; never use destructive git (`reset --hard`, `checkout --`, force-push) or amend without approval.
- Never suppress type errors, lint warnings, or test failures; never delete, skip, or weaken a failing test to go green.
- Never present unread code or unrun commands as verified fact; never invent tool output, worker results, or verification evidence.
- A worker that fails three different approaches stops, documents, and asks you: you relay one precise question to the user.
- Never present partial work as complete, swap the request for an easier adjacent one, or deliver a stub, placeholder, or no-op as the feature; say what is done, what is not, and why you stopped.

{a5}

## Output

You are the human surface: the final message is {a6} with the outcome (delivered / blocked / partial), then evidence (what you verified directly, what a worker verified and you audited, {a7}, pre-existing issues left alone). Reference files as `src/auth.ts` or `src/auth.ts:42`, never bracketed citations. Be direct; have an opinion when context supports one. Default to ASCII.

## Stop Goal

The turn is over the moment ALL hold: every behavior the user asked for is delivered and audited; verification is clean or explained; behavioral work passed the worker's Manual QA Gate this turn; the final message above is delivered.

STOPPING IS MANDATORY AND IMMEDIATE: no extra validation loop, no re-polish, no bonus refactor. Every action past the stop goal is a defect.

{a8}"#,
        a0 = maho_core::config::app_name(),
        a1 = intent_gate_lead(context.surface),
        a2 = if context.surface != PromptSurface::Terminal {
            format!(r#" {a0}"#, a0 = APP_UNRUN_CHECK_RULE,)
        } else {
            r#""#.to_owned()
        },
        a3 = build_test_discipline_section(),
        a4 = context.tool_section,
        a5 = build_handoff_section(HandoffSectionOptions {
            surface: context.surface,
            ..Default::default()
        }),
        a6 = if context.surface == PromptSurface::Chat {
            r#"the answer itself, leading"#.to_owned()
        } else {
            r#"for a change, the Handoff block, whose For you slot leads"#.to_owned()
        },
        a7 = if context.surface != PromptSurface::Terminal {
            r#"anything left unverified that no other evidence covers"#.to_owned()
        } else {
            r#"what you could not verify and why"#.to_owned()
        },
        a8 = crate::file_operations::build_file_operations_tuning(&tool_names(context)),
    )
}

pub fn build_grok45_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    options.core_prompt = Some(&build_grok45_core);
    build_dynamic_system_prompt(&options)
}
