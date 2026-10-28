use maho_ext_api::JsonValue;
use maho_omo_git_master::directive::{
    DEFAULT_COMMIT_FOOTER, GitMasterCommitFooter, GitMasterSettings,
    build_git_master_attribution_directive,
};

const BUILTIN_FOOTER: &str =
    "Ultraworked with [omo](https://github.com/code-yeongyu/oh-my-openagent)";
const CO_AUTHOR_TRAILER: &str = "co-authored-by:";

fn footer(commit_footer: GitMasterCommitFooter) -> GitMasterSettings {
    GitMasterSettings {
        commit_footer,
        include_co_authored_by: false,
        git_env_prefix: String::new(),
    }
}

fn resolved(value: JsonValue) -> GitMasterSettings {
    GitMasterSettings::from_resolved(&value)
}

#[test]
fn a_disabled_footer_emits_no_directive() {
    assert_eq!(
        build_git_master_attribution_directive(&footer(GitMasterCommitFooter::Disabled), ""),
        None
    );
}

#[test]
fn the_builtin_footer_emits_the_shipped_footer_text_without_a_co_author_trailer() {
    let directive =
        build_git_master_attribution_directive(&footer(GitMasterCommitFooter::Builtin), "")
            .expect("directive");
    assert!(directive.contains(BUILTIN_FOOTER));
    assert_eq!(DEFAULT_COMMIT_FOOTER, BUILTIN_FOOTER);
    assert!(!directive.to_lowercase().contains(CO_AUTHOR_TRAILER));
    assert!(!directive.contains("sisyphus-dev-ai"));
    assert!(!directive.contains("users.noreply.github.com"));
}

#[test]
fn the_co_author_flag_emits_the_trailer_alongside_the_footer() {
    let settings = GitMasterSettings {
        commit_footer: GitMasterCommitFooter::Builtin,
        include_co_authored_by: true,
        git_env_prefix: String::new(),
    };
    let directive = build_git_master_attribution_directive(&settings, "").expect("directive");
    assert!(directive.contains(BUILTIN_FOOTER));
    assert!(directive.contains("--trailer \"Co-authored-by: sisyphus-dev-ai <sisyphus-dev-ai@users.noreply.github.com>\""));
}

#[test]
fn a_custom_footer_replaces_the_builtin_text() {
    let directive = build_git_master_attribution_directive(
        &footer(GitMasterCommitFooter::Custom("Shipped with omo".into())),
        "",
    )
    .expect("directive");
    assert!(directive.contains("Shipped with omo"));
    assert!(!directive.contains("Ultraworked with"));
    assert!(!directive.to_lowercase().contains(CO_AUTHOR_TRAILER));
}

#[test]
fn the_resolved_union_parses_the_false_true_and_string_arms() {
    assert_eq!(
        resolved(serde_json::json!({"commit_footer": false})).commit_footer,
        GitMasterCommitFooter::Disabled
    );
    assert_eq!(
        resolved(serde_json::json!({"commit_footer": true})).commit_footer,
        GitMasterCommitFooter::Builtin
    );
    assert_eq!(
        resolved(serde_json::json!({"commit_footer": "Shipped with omo"})).commit_footer,
        GitMasterCommitFooter::Custom("Shipped with omo".into())
    );
    assert_eq!(
        resolved(serde_json::json!({})).commit_footer,
        GitMasterCommitFooter::Disabled
    );
    assert!(resolved(serde_json::json!({"include_co_authored_by": true})).include_co_authored_by);
}

#[test]
fn a_git_command_prefix_leads_the_example_commit_line() {
    let directive = build_git_master_attribution_directive(
        &footer(GitMasterCommitFooter::Builtin),
        "GIT_MASTER=1",
    )
    .expect("directive");
    assert!(directive.contains("GIT_MASTER=1 git commit -m"));
}
