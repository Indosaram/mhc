//! Native git-master attribution directive and settings view.
use maho_ext_api::JsonValue;
pub const DEFAULT_COMMIT_FOOTER: &str =
    "Ultraworked with [omo](https://github.com/code-yeongyu/oh-my-openagent)";
pub const COMMIT_ATTRIBUTION_OPEN_TAG: &str = "<commit_attribution>";
pub const DEFAULT_GIT_ENV_PREFIX: &str = "GIT_MASTER=1";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitMasterSettings {
    pub commit_footer: GitMasterCommitFooter,
    pub include_co_authored_by: bool,
    pub git_env_prefix: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum GitMasterCommitFooter {
    #[default]
    Disabled,
    Builtin,
    Custom(String),
}
impl GitMasterSettings {
    pub fn from_resolved(resolved: &JsonValue) -> Self {
        let commit_footer = match resolved.get("commit_footer") {
            Some(JsonValue::Bool(true)) => GitMasterCommitFooter::Builtin,
            Some(JsonValue::String(text)) => GitMasterCommitFooter::Custom(text.clone()),
            _ => GitMasterCommitFooter::Disabled,
        };
        let include_co_authored_by = resolved
            .get("include_co_authored_by")
            .and_then(JsonValue::as_bool)
            .unwrap_or(true);
        let git_env_prefix = match resolved.get("git_env_prefix") {
            Some(JsonValue::String(text)) => text.clone(),
            _ => DEFAULT_GIT_ENV_PREFIX.to_owned(),
        };
        Self {
            commit_footer,
            include_co_authored_by,
            git_env_prefix,
        }
    }
}
/// Build the attribution directive, independently honoring the optional body footer and default-on trailer.
pub fn build_git_master_attribution_directive(
    settings: &GitMasterSettings,
    git_command_prefix: &str,
) -> Option<String> {
    let footer_text = resolve_footer_text(&settings.commit_footer);
    if footer_text.is_none() && !settings.include_co_authored_by {
        return None;
    }
    let trailer = "Co-authored-by: sisyphus-dev-ai <sisyphus-dev-ai@users.noreply.github.com>";
    let prefix = if git_command_prefix.is_empty() {
        String::new()
    } else {
        format!("{git_command_prefix} ")
    };
    let command = format!(
        "{prefix}git {}commit -m \"{{Commit Message}}\"{}{}",
        if settings.include_co_authored_by {
            "-c trailer.ifexists=addIfDifferent "
        } else {
            ""
        },
        footer_text
            .as_ref()
            .map(|text| format!(" -m \"{text}\""))
            .unwrap_or_default(),
        if settings.include_co_authored_by {
            format!(" --trailer \"{trailer}\"")
        } else {
            String::new()
        }
    );
    let footer_line = footer_text
        .as_ref()
        .map(|text| format!("1. **Footer in the commit body:** {text}"))
        .unwrap_or_default();
    Some([COMMIT_ATTRIBUTION_OPEN_TAG, "## Commit Attribution", "", "Add attribution to EVERY commit:", "", &footer_line,
        if settings.include_co_authored_by { "Co-author trailer, exactly once. Pass it with --trailer, never inside -m; trailer.ifexists=addIfDifferent deduplicates existing trailers. After squash, amend the message to remove duplicates." } else { "Do NOT add a Co-authored-by trailer." },
        "", "**Example:**", "```bash", &command, "```", "</commit_attribution>"].join("\n"))
}
fn resolve_footer_text(footer: &GitMasterCommitFooter) -> Option<String> {
    match footer {
        GitMasterCommitFooter::Disabled => None,
        GitMasterCommitFooter::Builtin => Some(DEFAULT_COMMIT_FOOTER.to_owned()),
        GitMasterCommitFooter::Custom(text) => Some(text.clone()),
    }
}
