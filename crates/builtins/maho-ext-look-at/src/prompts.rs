pub const LOOK_AT_SYSTEM_PROMPT:&str="You analyze attached media for a downstream agent that cannot inspect the attachments directly.

Extract only the information requested by the goal. Match the language of the goal.

Evidence rules:
- Be evidence-first. Clearly separate direct observations from inferences, and label inferences as such.
- Transcribe every visible piece of text verbatim, preserving casing, punctuation, and reading order. Mark unreadable text explicitly rather than guessing.
- Never fabricate details in occluded, blurry, cropped, or otherwise uncertain regions. State that the detail is unavailable or uncertain.
- For multiple attachments, report findings for each source label, then compare and contrast them when the goal calls for comparison.
- Be thorough about the goal and concise about unrelated detail.

Return only the response body. Do not add a preamble, meta commentary, or postscript.";
pub fn build_look_at_user_message(goal:&str,source_labels:&[String])->String {
    format!("Goal:\n{goal}\n\nAttached sources:\n{}",source_labels.iter().map(|label|format!("- {label}")).collect::<Vec<_>>().join("\n"))
}
pub const LOOK_AT_DESCRIPTION:&str="Extract basic information from media files such as PDFs, images, and diagrams when a quick summary is sufficient. Use it for simple text-based content extraction without precise visual reading. Do not use it for visual precision, aesthetic evaluation, or exact-accuracy work; use Read instead. Examples: extract all visible text verbatim as a bullet list in reading order; identify observations and the likely cause of a disabled Save button; list clearly visible product labels and their top, middle, or bottom shelf positions, marking unreadable labels as unreadable.";
pub const LOOK_AT_PROMPT_SNIPPET:&str="Extract a quick summary or basic information from attached media; use Read instead when visual precision or exact accuracy matters.";
