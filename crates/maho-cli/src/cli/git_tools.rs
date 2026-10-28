//! MIT profile (`--no-default-features`): the read-only git tools from `maho-git` (ported from the
//! MIT oh-my-pi agentic-commit tools), mounted as `<builtin:git>`: `git_overview`,
//! `git_file_diff`, `git_hunk` and `recent_commits`. They work in the session's repository and
//! never write to it. The `omo` build does not mount them.

/// The git tools extension.
pub fn extension() -> maho_git::GitToolsExtension {
    maho_git::GitToolsExtension
}

#[cfg(test)]
mod tests {
    #[test]
    fn registers_the_four_read_only_tools() {
        let _ = super::extension();
        assert_eq!(
            maho_git::TOOL_NAMES,
            &[
                "git_overview",
                "git_file_diff",
                "git_hunk",
                "recent_commits"
            ]
        );
    }
}
