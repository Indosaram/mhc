//! Static assets embedded for facts extraction persona.

/// Default persona instructions prompt for facts extractor subagents.
pub const FACTS_PERSONA: &str = "# Facts extractor\n\n\
You are a background task that extracts durable facts about the user from recent conversation turns.\n\n\
Rules:\n\
- Capture only enduring facts, preferences, constraints, project details, and relationship cues that will matter across sessions.\n\
- Do NOT capture transient requests, one-off questions, code snippets under active development, or tool noise.\n\
- Every fact must be attributable to the conversation evidence.\n\
- Write facts in clear, concise markdown bullet points.\n";

/// Load facts persona instructions prompt.
pub fn load_facts_persona() -> &'static str {
    FACTS_PERSONA
}

#[cfg(test)]
#[path = "assets_tests.rs"]
mod tests;
