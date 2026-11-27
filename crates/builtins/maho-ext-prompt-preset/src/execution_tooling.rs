//! Port of `prompt-preset/execution-tooling.ts` (senpi v2026.11.19-5): the eval routing stance shared by the
//! Claude and Kimi presets, rendered only when eval is selected.

use maho_core::dynamic_prompt::SharedSections;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionToolingDialect {
    Claude,
    Kimi,
}

pub struct DialectText {
    pub claude: &'static str,
    pub kimi: &'static str,
}

pub struct ExecutionToolingRule {
    pub id: &'static str,
    pub concern: &'static str,
    pub directive: DialectText,
    /// The directive as worded before the senpi#5335 diet, rendered for `SharedSections::Full` (senpi#5665).
    pub full_directive: DialectText,
}

pub const EXECUTION_TOOLING_RULES: &[ExecutionToolingRule] = &[
    ExecutionToolingRule {
        id: "eval-routing-decision",
        concern: "code-cell-routing",
        directive: DialectText {
            claude: "Sort each multi-call step first: independent reads/searches/symbol lookups/probes -> ONE `eval` cell via `parallel(thunks)`. Edits, side effects, deploys, approvals, any call fed by an unseen result -> one at a time, observe each before next.",
            kimi: "Sort a multi-call step before you write it: put independent reads, searches, symbol lookups, and probes into one `eval` cell together with `parallel(thunks)`, and run edits, side-effecting commands, deploys, approvals, and any call that depends on a result you have not seen yet one at a time, looking at each result before the next.",
        },
        full_directive: DialectText {
            claude: "Sort a multi-call step before you write it: independent reads, searches, symbol lookups, and probes go into ONE `eval` cell together via `parallel(thunks)` - an extra read-only call in that wave is nearly free, a stale assumption costs the turn - while edits, side-effecting commands, deploys, approvals, and any call whose input is a result you have not seen yet run one at a time, each observed before the next.",
            kimi: "Sort a multi-call step before you write it: put independent reads, searches, symbol lookups, and probes into one `eval` cell together with `parallel(thunks)`, and run edits, side-effecting commands, deploys, approvals, and any call that depends on a result you have not seen yet one at a time, looking at each result before the next.",
        },
    },
    ExecutionToolingRule {
        id: "eval-evidence-return",
        concern: "code-cell-routing",
        directive: DialectText {
            claude: "Name expected state before running a cell; on return COMPARE evidence to it; for a mutating cell also confirm nothing changed beyond it.",
            kimi: "Name the state a cell should produce before running it; when it returns, compare the returned evidence with that state, and for a cell that changed something also check that nothing changed beyond it.",
        },
        full_directive: DialectText {
            claude: "Name the state a cell should produce before running it; when it returns, COMPARE the returned evidence with that state, and for a cell that changed something also check that nothing changed beyond it. A result that hides a failed item or a truncated tail is not evidence.",
            kimi: "Name the state a cell should produce before running it; when it returns, compare the returned evidence with that state, and for a cell that changed something also check that nothing changed beyond it. A result that hides a failed item or a truncated tail is not evidence.",
        },
    },
    ExecutionToolingRule {
        id: "perceived-state-loop",
        concern: "code-cell-routing",
        directive: DialectText {
            claude: "Result must be SEEN (page, component, image, 3D scene, layout): one change -> render/screenshot -> look -> next. 3D: several angles. Page: desktop + mobile widths. Compare to reference or stated intent; ask only where two readings of intent diverge.",
            kimi: "When the result must be seen rather than read - a page, a component, an image, a 3D scene, a layout - make one change, render or screenshot it, look, then make the next; check a 3D scene from several angles and a page at desktop and mobile widths. Compare what you see with the reference or the stated intent, and ask only where two readings of that intent diverge.",
        },
        full_directive: DialectText {
            claude: "When the result must be SEEN rather than read - a page, a component, an image, a 3D scene, a layout - make one change, render or screenshot it, look, then make the next; check a 3D scene from several angles and a page at desktop and mobile widths. Compare what you see with the reference or the stated intent, and ask only where two readings of that intent diverge.",
            kimi: "When the result must be seen rather than read - a page, a component, an image, a 3D scene, a layout - make one change, render or screenshot it, look, then make the next; check a 3D scene from several angles and a page at desktop and mobile widths. Compare what you see with the reference or the stated intent, and ask only where two readings of that intent diverge.",
        },
    },
    ExecutionToolingRule {
        id: "eval-stay-direct",
        concern: "code-cell-routing",
        directive: DialectText {
            claude: "Direct tool call only when: one call suffices, result decides next call, semantic judgment sits between calls, or action needs approval.",
            kimi: "Use a direct tool call when one call is enough, when each result decides the next call, or when the action needs approval - then stop deliberating and make it.",
        },
        full_directive: DialectText {
            claude: "Call a tool directly only when one call is enough, the result decides the next call, semantic judgment sits between calls, or the action needs approval.",
            kimi: "Use a direct tool call when one call is enough, when each result decides the next call, or when the action needs approval - then stop deliberating and make it.",
        },
    },
];

/// `CONCERN_TOOL`: every rule's concern is code-cell routing, which needs `eval`.
const CONCERN_TOOL: &str = "eval";

/// Directives for the selected tools, or "" when eval is not available.
pub fn build_execution_tooling_section(
    tool_names: &[String],
    dialect: ExecutionToolingDialect,
    shared_sections: SharedSections,
) -> String {
    if !tool_names.iter().any(|name| name == CONCERN_TOOL) {
        return String::new();
    }
    let body = EXECUTION_TOOLING_RULES
        .iter()
        .map(|rule| {
            let text = match shared_sections {
                SharedSections::Full => &rule.full_directive,
                SharedSections::Diet => &rule.directive,
            };
            match dialect {
                ExecutionToolingDialect::Claude => text.claude,
                ExecutionToolingDialect::Kimi => text.kimi,
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    match dialect {
        ExecutionToolingDialect::Claude => {
            format!("<execution_tooling>\n{body}\n</execution_tooling>")
        }
        ExecutionToolingDialect::Kimi => body,
    }
}

/// Same as `build_execution_tooling_section` but followed by a paragraph gap, for inline placement.
pub fn build_execution_tooling_paragraph(
    tool_names: &[String],
    dialect: ExecutionToolingDialect,
    shared_sections: SharedSections,
) -> String {
    let section = build_execution_tooling_section(tool_names, dialect, shared_sections);
    if section.is_empty() {
        section
    } else {
        format!("{section}\n\n")
    }
}
