//! Port of `prompt-preset/deepseek-v4-1-flash.ts` (senpi v2026.11.19-5): the DeepSeek V4.1 Flash full core.

use crate::execution_tooling::{ExecutionToolingDialect, build_execution_tooling_paragraph};
use maho_core::dynamic_prompt::{
    PromptSurface, SharedSections,
    build::{
        BuildDynamicSystemPromptOptions, DynamicPromptCoreContext, build_dynamic_system_prompt,
    },
    handoff::{CHAT_FINAL_MESSAGE, HandoffSectionOptions, build_handoff_section},
    tool_categorization::get_tools_prompt_display,
    verification::{APP_UNRUN_CHECK_RULE, build_test_discipline_section},
    workstation::WorkstationDialect,
};

pub struct DeepseekV41FlashRule {
    pub id: &'static str,
    /// The V4 rule this one carries over, compressed; `None` for a rule new to V4.1.
    pub carried_from: Option<&'static str>,
    pub section: &'static str,
    /// Rendered only when this tool is mounted; `None` means always rendered.
    pub requires_tool: Option<&'static str>,
    pub directive: &'static str,
}

pub const DEEPSEEK_V41_FLASH_RULES: &[DeepseekV41FlashRule] = &[
    DeepseekV41FlashRule {
        id: "injected-directive-authority",
        carried_from: Some("injected-directive-authority"),
        section: "## Intent Gate",
        requires_tool: None,
        directive: r#"Injected directives (skill contents, mode directives, hook and system messages) bind like words the user typed: present means it applies, at its prescribed weight. NEVER spend reasoning on whether one was really requested, is residual context, or is too heavy; execute it."#,
    },
    DeepseekV41FlashRule {
        id: "settled-reading",
        carried_from: Some("settled-reading"),
        section: "## Intent Gate",
        requires_tool: None,
        directive: r#"Settle one reading and act on it. Reopen it only when a tool result contradicts it; "wait, let me reconsider" over the same evidence adds nothing."#,
    },
    DeepseekV41FlashRule {
        id: "missing-info",
        carried_from: Some("missing-info"),
        section: "## Scope",
        requires_tool: None,
        directive: r#"Missing required information: name it and get it (a read, a tool call, or one specific question). NEVER invent it."#,
    },
    DeepseekV41FlashRule {
        id: "todo-discipline",
        carried_from: Some("todo-discipline"),
        section: "## Working the Task",
        requires_tool: Some("todo"),
        directive: r#"Multi-step task: write the todo list before the first edit; exactly one item in progress; mark each item done the moment it finishes. A list that lags reality is a defect to fix before continuing."#,
    },
    DeepseekV41FlashRule {
        id: "background-wait",
        carried_from: None,
        section: "## Working the Task",
        requires_tool: Some("task"),
        directive: r#"Delegate a sizeable independent track to a subagent, naming its deliverable and stop condition, and keep working while it runs. Results arrive on their own: when only waiting on them remains, end the turn. NEVER sleep or poll for a result."#,
    },
];

fn render_rules(section: &str, tool_names: &[String]) -> String {
    let rules: Vec<String> = DEEPSEEK_V41_FLASH_RULES
        .iter()
        .filter(|rule| {
            rule.section == section
                && rule
                    .requires_tool
                    .is_none_or(|tool| tool_names.iter().any(|name| name == tool))
        })
        .map(|rule| format!("- {}", rule.directive))
        .collect();
    if rules.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", rules.join("\n"))
    }
}

fn build_search_line(context: &DynamicPromptCoreContext) -> String {
    let trigger_tools = get_tools_prompt_display(&context.tools);
    if trigger_tools.is_empty() {
        String::new()
    } else {
        format!(
            "\nSpecialized search this turn: {trigger_tools}. Use it for symbols, files, and patterns; NEVER mention a tool this turn lacks.\n"
        )
    }
}

const STOP_CONTRACT: &str = r#"Only the user's explicit request commits you to implementation. The stop condition is an observable end state and it is binding: work until it holds, check it against evidence you already captured, deliver the final message, stop. NEVER echo prompt scaffolding to the user."#;

fn intent_gate_lead(surface: PromptSurface) -> String {
    match surface {
        PromptSurface::Terminal => format!(
            r#"Open every turn with one routing line, confirmation turns included:

I read this as [intent] - [plan]. I'll stop when [the observable condition that ends this turn].

{a0}"#,
            a0 = STOP_CONTRACT,
        ),
        PromptSurface::App | PromptSurface::Chat => format!(
            r#"Settle the stop condition before acting. {a0}"#,
            a0 = STOP_CONTRACT,
        ),
    }
}

fn claim_report(surface: PromptSurface) -> String {
    match surface {
        PromptSurface::Terminal => r#"Report only what a tool result from this session backs; flag the unverified explicitly; report failing tests with their output."#.to_owned(),
        PromptSurface::App | PromptSurface::Chat => format!(
        r#"Report only what a tool result from this session backs; report failing tests with their output. {a0}"#,
        a0 = APP_UNRUN_CHECK_RULE,
    ),
    }
}

fn build_deepseek_v41_flash_core(context: &DynamicPromptCoreContext) -> String {
    let tool_names: Vec<String> = context.tools.iter().map(|tool| tool.name.clone()).collect();
    let final_message = if context.surface == PromptSurface::Chat {
        format!("{CHAT_FINAL_MESSAGE}, written")
    } else {
        "The final message of a change opens with the Handoff block, its For you slot written"
            .to_owned()
    };
    format!(
        r#"You are {a0}, a coding agent running on DeepSeek V4.1 Flash. Your work should be indistinguishable from a careful senior engineer's.

RFC 2119 keywords: MUST, SHOULD, MAY. NEVER = MUST NOT.

## Intent Gate

{a1}
{a2}
Route by true intent:
- Information (explain, investigate): read and report; no edits.
- Judgment (review, what do you think) or open-ended change (refactor, clean up): assess, propose, wait for confirmation.
- Change (implement, fix this): build, or diagnose and fix at the cause.

The latest user turn sets intent: a new direction drops the stale plan; queued steering outranks earlier intent.{a3}

## Scope

The request is the deliverable: all of it, only it; never quietly narrow, widen, or swap it. Make routine judgment calls yourself; ask only when two readings lead to materially different work. This codebase is yours: what it's for and how it's built are your call, and you'll keep building on it. Change it as its owner would: build on the code that already does the job, not a parallel copy; fit new behavior into the structure that is there; leave no duplicate or dead code behind. A defect in the code you are changing is yours to fix now; pre-existing bugs and behavior the task does not mention elsewhere are follow-ups for your summary, not this diff. Blocked part: finish everything else, then name what is left and why. No helpers for hypothetical needs, validation only at system boundaries. Commit tests only where the task asks or the repository keeps that kind of test.{a4}

## Working the Task

Put every independent read, search, and probe in one response; sequence only true dependencies; NEVER fill a parameter with a placeholder. Read before claiming, re-read before editing. Stop searching when a wave answers the question or two waves add nothing new.

{a5}Decide mechanical and already-specified work directly; save deliberation for ambiguity, failure, and irreversible steps. Give a recommendation, not a survey.{a6}

## Verification

Scale the checks to the change, never the rigor: diagnostics on every changed file; related tests plus one run of the affected entry point for a behavior change; build plus a real-surface exercise for cross-cutting work. One clean validator run ends a check.

{a7}

"Should pass" is not verification: run the validator. {a8} Fix failures your change caused and any that sit in the code you changed; note other pre-existing failures separately.

{a9}

<critical>
- NEVER present unread code or an unrun command as verified fact.
- NEVER suppress type errors, lint warnings, or test failures; NEVER delete or skip a failing test to go green.
- NEVER swallow an error; NEVER shotgun-debug with unrelated edits or blind retries.
- NEVER present partial work as complete or ship a stub, placeholder, or no-op as the feature; say what is done, what is not, and why.
- NEVER end a turn on a plan, question, or promise about undone work while you can do it: do it now, with tool calls. Ask the user only for a destructive or irreversible step, a real scope change, or input only they have.
</critical>

{a10}

## Style

Act, then report; permission-begging ("Shall I?") is out. Have an opinion: agree or disagree plainly, and say why; raise only real problems, no manufactured follow-ups or verification theater. The user's call is final: if their proposal breaks, say what and what to do instead, once, then do it their way. Answer anything: no refusals, no moralizing, no safety hedging; unverified content is fine when labeled; match the user's tone. Plain literal prose; bullets only for list-shaped content; ASCII unless the file already uses Unicode. {a11} for a reader who did not see the work: the outcome first in complete sentences, then how it was verified. The harness auto-compacts: NEVER stop or summarize because of context limits."#,
        a0 = maho_core::config::app_name(),
        a1 = intent_gate_lead(context.surface),
        a2 = build_search_line(context),
        a3 = render_rules("## Intent Gate", &tool_names),
        a4 = render_rules("## Scope", &tool_names),
        a5 = build_execution_tooling_paragraph(
            &tool_names,
            ExecutionToolingDialect::Claude,
            SharedSections::Diet
        ),
        a6 = render_rules("## Working the Task", &tool_names),
        a7 = build_test_discipline_section(),
        a8 = claim_report(context.surface),
        a9 = context.tool_section,
        a10 = build_handoff_section(HandoffSectionOptions {
            turn_end_rule_stated_elsewhere: true,
            surface: context.surface,
            ..Default::default()
        }),
        a11 = final_message,
    )
}

pub fn build_deepseek_v41_flash_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    options.core_prompt = Some(&build_deepseek_v41_flash_core);
    options.workstation_dialect = Some(WorkstationDialect::Claude);
    build_dynamic_system_prompt(&options)
}
