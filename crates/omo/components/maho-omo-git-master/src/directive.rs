//! Port of omo-senpi `components/git-master/directive.ts` at omo `455dee62`: the
//! commit-attribution directive text, the resolved `git_master` settings view it reads, and the
//! footer-text resolution.
//!
//! The directive prose is copied from the pinned source verbatim; it is model-facing text, so it
//! carries no prose-pinning test (only the machine-consumed footer value and the no-trailer
//! contract are asserted).

use maho_ext_api::JsonValue;

/// Upstream `DEFAULT_COMMIT_FOOTER`: the builtin footer text used when `commit_footer` is `true`.
pub const DEFAULT_COMMIT_FOOTER: &str =
    "Ultraworked with [omo](https://github.com/code-yeongyu/oh-my-openagent)";

/// The resolved `git_master` settings: the typed view of the object returned by
/// `omo_config_core::resolve_omo_git_master_settings`.
///
/// Commit-identity contract: commits our tooling causes in a user's repository carry the
/// operator's own author/committer and never a GitHub-resolvable automation identity. The
/// directive therefore only ever describes the opt-in body footer; `include_co_authored_by` is
/// accepted for backward compatibility but no longer emits a `Co-authored-by` trailer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitMasterSettings {
    pub commit_footer: GitMasterCommitFooter,
    /// Upstream `include_co_authored_by`: a deprecated no-op. Kept on the parsed view so a
    /// caller can observe the resolved value; the directive never reads it.
    pub include_co_authored_by: bool,
}

/// Upstream `OmoGitMasterSettings["commit_footer"]` (`boolean | string`, default `false`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum GitMasterCommitFooter {
    /// `false`: no attribution footer is emitted.
    #[default]
    Disabled,
    /// `true`: the builtin [`DEFAULT_COMMIT_FOOTER`] text.
    Builtin,
    /// A string replacing the builtin footer text.
    Custom(String),
}

impl GitMasterSettings {
    /// Parses the resolved `git_master` object returned by the config owner's
    /// `resolve_omo_git_master_settings`.
    ///
    /// That resolver materializes the schema defaults, so `commit_footer` is a boolean or a
    /// string; a missing key (never produced by the resolver) falls back to the disabled default,
    /// which is the schema's own `false`.
    pub fn from_resolved(resolved: &JsonValue) -> Self {
        let commit_footer = match resolved.get("commit_footer") {
            Some(JsonValue::Bool(true)) => GitMasterCommitFooter::Builtin,
            Some(JsonValue::String(text)) => GitMasterCommitFooter::Custom(text.clone()),
            _ => GitMasterCommitFooter::Disabled,
        };
        let include_co_authored_by = resolved
            .get("include_co_authored_by")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        Self {
            commit_footer,
            include_co_authored_by,
        }
    }
}

/// Upstream `buildGitMasterAttributionDirective`: the directive appended to the read result, or
/// `None` when the footer is disabled.
pub fn build_git_master_attribution_directive(settings: &GitMasterSettings) -> Option<String> {
    let footer_text = resolve_footer_text(&settings.commit_footer)?;
    let footer_line = format!("1. **Footer in the commit body:** {footer_text}");
    let example_line = format!("git commit -m \"{{Commit Message}}\" -m \"{footer_text}\"");

    Some(
        [
            "<commit_attribution>",
            "## Commit Footer (MANDATORY)",
            "",
            "Add omo attribution to EVERY commit you create:",
            "",
            footer_line.as_str(),
            "",
            "Do NOT add a Co-authored-by trailer.",
            "",
            "**Example:**",
            "```bash",
            example_line.as_str(),
            "```",
            "</commit_attribution>",
        ]
        .join("\n"),
    )
}

/// Upstream `resolveFooterText`: `false` disables the footer, a string replaces the builtin text,
/// `true` uses [`DEFAULT_COMMIT_FOOTER`].
fn resolve_footer_text(footer: &GitMasterCommitFooter) -> Option<String> {
    match footer {
        GitMasterCommitFooter::Disabled => None,
        GitMasterCommitFooter::Builtin => Some(DEFAULT_COMMIT_FOOTER.to_owned()),
        GitMasterCommitFooter::Custom(text) => Some(text.clone()),
    }
}
