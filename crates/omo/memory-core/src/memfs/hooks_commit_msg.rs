//! Commit-msg hook script and checker integration for claim receipt validation.

/// Configuration for the external memory hook checker runtime and entry script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryHookChecker {
    pub exec_path: String,
    pub script: String,
}

/// Fallback commit-msg hook script when no checker is provided.
pub const COMMIT_MSG_HOOK_SCRIPT: &str = r#"#!/bin/sh
# Refuse a hand-written claim receipt header in memory notes. Installed by omo memory-core; regenerated on startup.
grep -q '^Omo-Writer: memory-tool$' "$1" && exit 0
if git diff --cached -U0 --no-color --no-ext-diff | grep -q -i -E '^\+[[:space:]]*["'"'"']?provenance["'"'"']?[[:space:]]*:'; then
  echo "Claim receipt validation failed:"
  echo "  the provenance header is a claim receipt only the memory tool writes; remove that line (or save the note with the memory tool)"
  exit 1
fi
exit 0
"#;

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Augment a hook script with the optional memory hook checker invocation.
pub fn with_memory_hook_checker(
    script: &str,
    stage: &str,
    checker: Option<&MemoryHookChecker>,
) -> String {
    let Some(checker) = checker else {
        return script.to_string();
    };

    let anchor = if stage == "commit-msg" {
        if let Some(pos) = script.find("Omo-Writer: memory-tool") {
            script[pos..].find('\n').map(|p| pos + p)
        } else {
            script.find('\n')
        }
    } else {
        script.find('\n')
    };

    let anchor = match anchor {
        Some(a) => a,
        None => return script.to_string(),
    };

    let exec_quoted = shell_quote(&checker.exec_path);
    let script_quoted = shell_quote(&checker.script);
    if checker.script.is_empty() {
        let extra_arg = if stage == "commit-msg" { " \"$1\"" } else { "" };
        let block = format!("omo_hook_runtime={exec_quoted}\nif [ -f \"$omo_hook_runtime\" ]; then\n  \"$omo_hook_runtime\" --memory-hook {stage}{extra_arg} || exit 1\n{}fi", if stage == "commit-msg" { "  exit 0\n" } else { "" });
        return format!("{}{}\n{}", &script[..=anchor], block, &script[anchor + 1..]);
    }
    let extra_arg = if stage == "commit-msg" { " \"$1\"" } else { "" };
    let exit_zero = if stage == "commit-msg" { "\n  exit 0" } else { "" };

    let block = format!(
        "omo_hook_runtime={exec_quoted}\n\
         omo_hook_script={script_quoted}\n\
         if [ -f \"$omo_hook_runtime\" ] && [ -f \"$omo_hook_script\" ]; then\n\
         \x20 BUN_BE_BUN=1 \"$omo_hook_runtime\" \"$omo_hook_script\" --memory-hook {stage}{extra_arg} || {{ echo \"  (no reason above? restart omo so it reinstalls its memory hooks)\"; exit 1; }}{exit_zero}\n\
         fi"
    );

    format!(
        "{}{}\n{}",
        &script[..=anchor],
        block,
        &script[anchor + 1..]
    )
}
