//! Ultrawork skill pointer generation.

use std::fs;
use std::path::{Path, PathBuf};

pub const ULTRAWORK_SKILL_POINTER_TEMPLATE: &str = "<ultrawork-mode>\n\
ULTRAWORK MODE IS ACTIVE FOR THIS TASK.\n\
\n\
MANDATORY BOOTSTRAP: do all three steps, in order, before anything else.\n\
\n\
1. First user-visible line this turn MUST be exactly:\n\
`ULTRAWORK MODE ENABLED!`\n\
\n\
2. Call `create_goal` NOW with `objective` set to the user's request.\n\
Send `objective` only: no `status`, no budget fields. If the\n\
`create_goal` tool is unavailable, open your reply with a binding\n\
`# Goal` block instead. Never skip this step.\n\
\n\
3. Read the FULL ultrawork directive NOW, before any other tool call,\n\
plan, or edit. It is the `ultrawork` skill, stored at:\n\
\n\
{{ULTRAWORK_SKILL_PATH}}\n\
\n\
Read the whole file. If a read result comes back truncated, keep\n\
reading the remaining line ranges until you have seen every line.\n\
Every rule in that file is binding for this entire task: no\n\
compromise, no summarizing from memory, no skipping. If the file does\n\
not exist, tell the user the omo ultrawork skill is missing and\n\
continue with steps 1 and 2 plus evidence-bound execution.\n\
\n\
Do not start the requested work until all three steps are complete.\n\
</ultrawork-mode>\n";

pub const ULTRAWORK_DIRECTIVE_FALLBACK: &str = include_str!("../directive.md");

#[derive(Clone, Debug, Default)]
pub struct UltraworkAdditionalContextOptions {
    pub skill_file_path: Option<Option<PathBuf>>,
}

pub fn build_ultrawork_skill_pointer(skill_file_path: &Path) -> String {
    ULTRAWORK_SKILL_POINTER_TEMPLATE.replace("{{ULTRAWORK_SKILL_PATH}}", &skill_file_path.to_string_lossy())
}

pub fn build_ultrawork_additional_context(options: &UltraworkAdditionalContextOptions) -> String {
    if let Some(opt_path) = &options.skill_file_path {
        if let Some(path) = opt_path {
            if path.exists() {
                return build_ultrawork_skill_pointer(path);
            }
        }
        return ULTRAWORK_DIRECTIVE_FALLBACK.to_string();
    }
    // Default: fallback directive if no skill path provided or path does not exist
    ULTRAWORK_DIRECTIVE_FALLBACK.to_string()
}
