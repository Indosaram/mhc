//! Computer skill markdown generator and materializer matching upstream `src/skill.ts`.

use std::fs;
use std::path::{Path, PathBuf};
use senpi_desktop_prelude::ComputerPreludeAssets;
use sha2::{Digest, Sha256};

pub const COMPUTER_SKILL_NAME: &str = "computer-use";

pub const COMPUTER_SKILL_DESCRIPTION: &str =
    "Experimental computer use. MUST read before driving the desktop with the computer tool: every helper, coordinate frames, accessibility refs, background vs foreground delivery, the stop and resume rules, and the safety rules for consequential actions.";

pub fn computer_skill_markdown() -> String {
    format!(
        r#"---
name: {COMPUTER_SKILL_NAME}
description: "{COMPUTER_SKILL_DESCRIPTION}"
---

# Computer use

Computer use is experimental: its behavior, platform coverage and settings may change between releases.

Find the `computer` tool with `tool_search` (query `computer`). Once it is active, eval cells get the `computer` global in the next cell.

Availability is unknown until `computer.capabilities()` has returned in this session: never tell the user computer use is available or unavailable before that; on `native-unavailable` repeat the host and reason the message names. A `capturePermission`, `inputPermission` or `axPermission` the task needs that is not `granted` is a setup step only the user can do: name the missing permission (on macOS, Screen Recording or Accessibility for the named app that launched OmO, in System Settings > Privacy & Security, then fully quit and relaunch that app) and stop instead of working around it. When the target is ambiguous, or an action still fails after a fresh observation, ask the user instead of guessing.

## Reference

{}

## Safety

{}
"#,
        ComputerPreludeAssets::documentation().trim(),
        ComputerPreludeAssets::safety().trim(),
    )
}

pub fn materialize_computer_skill(root: Option<&Path>) -> Result<PathBuf, std::io::Error> {
    let markdown = computer_skill_markdown();
    let mut hasher = Sha256::new();
    hasher.update(markdown.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    let prefix = &digest[..16];

    let base_root = match root {
        Some(r) => r.to_path_buf(),
        None => std::env::temp_dir(),
    };

    let dir = base_root
        .join(format!("senpi-computer-skill-{prefix}"))
        .join(COMPUTER_SKILL_NAME);
    let path = dir.join("SKILL.md");

    let needs_write = match fs::read_to_string(&path) {
        Ok(curr) => curr != markdown,
        Err(_) => true,
    };

    if needs_write {
        fs::create_dir_all(&dir)?;
        fs::write(&path, markdown)?;
    }

    Ok(path)
}
