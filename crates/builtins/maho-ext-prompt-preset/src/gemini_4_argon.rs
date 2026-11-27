//! Port of senpi `packages/coding-agent/src/core/extensions/builtin/prompt-preset/gemini-4-argon.ts`.

use maho_core::dynamic_prompt::{
    SharedSections,
    build::{
        BuildDynamicSystemPromptOptions, DynamicPromptCoreContext, build_dynamic_system_prompt,
    },
    handoff::{HandoffSectionOptions, build_handoff_section},
    intent_gate::build_intent_gate_with_options,
    policies::build_policies_section_with_options,
    style::build_style_section,
    verification::build_verification_section,
    working_task::build_working_task_section_with_options,
    workstation::WorkstationDialect,
};

pub const GEMINI_4_ARGON_ROUTING_LINE_RULE: &str = r#"The line is plain prose, and the tool calls that start the work follow it in the same response"#;
pub const GEMINI_4_ARGON_WORKING_TASK_OPENING: &str = r#"Writing code is your superpower. A step is solved by writing one `eval` cell that does the work, not by a parade of one-off tool calls: every session tool is reachable from the cell as `tool.<name>(...)`, so the cell reads, searches, inspects, runs commands, and edits files with real control flow - a `for` over every target, an `if`/`else` per case, a `try`/`catch` per item - and fires every independent lookup AT ONCE through `parallel(thunks)`. Bias toward breadth when context is thin: pull in anything even loosely relevant in that wave instead of serially later, since wasted reads cost almost nothing and stale assumptions cost the turn. Reduce in the kernel to the facts the decision needs, keeping every failed or missing item verbatim; a result that hides a failure or a truncated tail is not evidence.

Sort the step before you write the cell: independent reads, searches, probes, and edits share one cell; a call whose input is a result you have not seen yet, a deploy, an approval, or a destructive command runs ONE ACTION AT A TIME, each observed before the next. Name the state a cell should produce before it runs and compare the return with that state. When the result must be SEEN rather than read - a page, an image, a layout - make one change, render it, look, then make the next. A direct tool call is the exception, for a single call that settles the step or an action that needs approval; files are read, searched, and edited through the cell's tool calls, never through shell text processing or inline scripts inside a command. Never fill missing parameters with placeholders."#;
pub const GEMINI_4_ARGON_ERROR_RULE: &str = r#"Never silently swallow errors. Retry a transient failure (timeout, rate limit, "try again") as is; on any other failure change the strategy or the arguments - never repeat the same failed call, and never shotgun-debug with unrelated edits."#;
pub const GEMINI_4_ARGON_SEARCH_GROUNDING: &str = r#"## Search Grounding

Today's date is in the environment context and your training data ends before it: when a query is time-sensitive, form search queries from that date, and search before answering about anything "latest"; facts that cannot change need no search."#;

fn has_tool(context: &DynamicPromptCoreContext, name: &str) -> bool {
    context.tools.iter().any(|tool| tool.name == name)
}

fn build_gemini4_argon_core(context: &DynamicPromptCoreContext) -> String {
    [
        format!(
            "You are {}, a coding agent running on Gemini 4 Argon. Your work should be indistinguishable from a careful senior engineer's.",
            maho_core::config::app_name()
        ),
        String::new(),
        build_intent_gate_with_options(
            &context.tools,
            context.surface,
            Some(GEMINI_4_ARGON_ROUTING_LINE_RULE),
            SharedSections::Diet,
        ),
        String::new(),
        build_working_task_section_with_options(
            has_tool(context, "eval").then_some(GEMINI_4_ARGON_WORKING_TASK_OPENING),
            SharedSections::Diet,
        ),
        String::new(),
        build_verification_section(context.surface),
        String::new(),
        context.tool_section.clone(),
        String::new(),
        build_policies_section_with_options(Some(GEMINI_4_ARGON_ERROR_RULE), SharedSections::Diet),
        String::new(),
        build_handoff_section(HandoffSectionOptions {
            surface: context.surface,
            ..Default::default()
        }),
        String::new(),
        build_style_section(context.surface),
    ]
    .join("\n")
}

pub fn build_gemini4_argon_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    if options
        .selected_tools
        .iter()
        .any(|tool| tool == "web_search")
    {
        options.tuning_section = Some(GEMINI_4_ARGON_SEARCH_GROUNDING.to_owned());
    }
    options.core_prompt = Some(&build_gemini4_argon_core);
    options.workstation_dialect = Some(WorkstationDialect::Gemini);
    build_dynamic_system_prompt(&options)
}
