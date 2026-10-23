//! Port of senpi packages/coding-agent/src/core/share/share-session.ts.
//!
//! `/share` end to end: nothing is uploaded until the user has seen a description of the exact redacted
//! bytes and chosen Upload. Tool outputs stay out unless the user includes them, which re-renders and
//! re-asks. A review copy is written only on request, into a private temporary directory that is removed
//! on every exit path.

use std::cell::RefCell;
use std::path::PathBuf;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::share_projection::{new_output_omissions, omit_tool_outputs};
use super::share_redaction::{ShareRedactionCategory, ShareRedactionContext, ShareRedactionTally};
use super::share_redaction_value::redact_share_value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareDecision {
    Upload,
    IncludeOutputs,
    ExcludeOutputs,
    Review,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareAskOptions {
    /// Tool outputs left out of this preview; above 0, offer to include them.
    pub omitted_outputs: usize,
    /// Tool outputs are in this preview; offer to leave them out again.
    pub includes_outputs: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareUploadResult {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub aborted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhStatus {
    Ok,
    Missing,
    LoggedOut,
}

/// The sanitize hook the renderer calls over session data (senpi `sanitize`).
pub type Sanitize<'a> = &'a dyn Fn(Value) -> Value;

/// What `run_share` needs from its host (senpi `ShareDependencies`).
#[allow(async_fn_in_trait)]
pub trait ShareHost {
    fn disabled_reason(&self) -> Option<String>;
    fn check_gh(&self) -> GhStatus;
    fn render(&self, sanitize: Sanitize<'_>) -> Result<String, String>;
    fn context(&self) -> ShareRedactionContext;
    async fn ask(&self, preview: String, options: ShareAskOptions) -> ShareDecision;
    fn show_review_copy(&self, path: &str);
    async fn upload(&self, html: String) -> ShareUploadResult;
    fn viewer_base(&self) -> Option<String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareOutcome {
    Disabled {
        reason: String,
    },
    GhMissing,
    GhLoggedOut,
    ExportFailed {
        message: String,
    },
    Cancelled,
    UploadFailed {
        message: String,
    },
    Shared {
        gist_url: String,
        viewer_url: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct PreparedShare {
    pub html: String,
    pub sha256: String,
    pub tally: ShareRedactionTally,
    pub includes_outputs: bool,
    pub omitted_outputs: usize,
}

/// Renders the session through projection (unless outputs are included) and then redaction.
pub fn prepare_share(
    render: &dyn Fn(Sanitize<'_>) -> Result<String, String>,
    context: &ShareRedactionContext,
    include_outputs: bool,
) -> Result<PreparedShare, String> {
    let tally = RefCell::new(ShareRedactionTally::new());
    let omitted = RefCell::new(0usize);
    let html = render(&|value: Value| {
        if include_outputs {
            return redact_share_value(&value, context, &mut tally.borrow_mut());
        }
        let mut omissions = new_output_omissions();
        let projected = omit_tool_outputs(&value, &mut omissions);
        let mut kept = omitted.borrow_mut();
        *kept = (*kept).max(omissions.count);
        drop(kept);
        redact_share_value(&projected, context, &mut tally.borrow_mut())
    })?;
    let sha256 = hex::encode(Sha256::digest(html.as_bytes()));
    Ok(PreparedShare {
        html,
        sha256,
        tally: tally.into_inner(),
        includes_outputs: include_outputs,
        omitted_outputs: omitted.into_inner(),
    })
}

const CATEGORY_ORDER: [ShareRedactionCategory; 8] = [
    ShareRedactionCategory::Credential,
    ShareRedactionCategory::PrivateKey,
    ShareRedactionCategory::HomePath,
    ShareRedactionCategory::UserName,
    ShareRedactionCategory::HostName,
    ShareRedactionCategory::Email,
    ShareRedactionCategory::IpAddress,
    ShareRedactionCategory::Image,
];

fn describe_tally(tally: &ShareRedactionTally) -> String {
    let parts: Vec<String> = CATEGORY_ORDER
        .iter()
        .filter_map(|category| {
            let total = tally.get(category).copied().unwrap_or(0);
            (total > 0).then(|| {
                format!(
                    "{total} {}{}",
                    category.as_str(),
                    if total == 1 { "" } else { "s" }
                )
            })
        })
        .collect();
    if parts.is_empty() {
        "nothing matched the redaction rules".to_owned()
    } else {
        parts.join(", ")
    }
}

/// The consent text shown before anything is uploaded.
pub fn describe_share(prepared: &PreparedShare, viewer_base: Option<&str>) -> String {
    let kilobytes = ((prepared.html.len() as f64 / 1024.0) + 0.5)
        .floor()
        .max(1.0) as u64;
    let short = &prepared.sha256[..12.min(prepared.sha256.len())];
    let viewer = match viewer_base {
        None => "No viewer is configured, so you get the gist link only.".to_owned(),
        Some(base) => format!(
            "The link opens in {base}, a viewer set by SHARE_VIEWER_URL that receives the gist id."
        ),
    };
    [
        "Share this session as a secret GitHub gist?".to_owned(),
        String::new(),
        if prepared.includes_outputs {
            format!("Uploads: the conversation, tool calls AND their outputs, system prompt and tool list, as one HTML file ({kilobytes} KB, sha256 {short}).")
        } else {
            format!("Uploads: your messages and the replies, tool names and arguments (including any file content the agent wrote or edited), system prompt and tool list, as one HTML file ({kilobytes} KB, sha256 {short}).")
        },
        if prepared.includes_outputs {
            "Tool outputs are included (file contents, command output, extension messages).".to_owned()
        } else {
            format!("Tool outputs are left out: {} replaced by \"[output omitted]\".", prepared.omitted_outputs)
        },
        format!("Removed first: {}. Images are always left out.", describe_tally(&prepared.tally)),
        "Who can read it: anyone with the link. A secret gist is unlisted, not private, and stays until you delete it on GitHub.".to_owned(),
        viewer,
        "Redaction is pattern-based and can miss things; review the exact file if the session holds anything sensitive.".to_owned(),
    ]
    .join("\n")
}

/// The first `https://` token of `gh gist create`'s output, when it ends in a gist id.
pub fn parse_gist_url(stdout: &str) -> Option<String> {
    let url = stdout
        .split_whitespace()
        .find(|token| token.starts_with("https://"))?;
    url.rsplit('/')
        .next()
        .filter(|id| !id.is_empty())
        .map(|_| url.to_owned())
}

/// A private temporary directory removed when dropped, on every exit path.
struct ReviewDir(PathBuf);

impl ReviewDir {
    fn create() -> std::io::Result<Self> {
        let base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        for attempt in 0..64u32 {
            let path = base.join(format!(
                "{}-share-{:x}{:x}{attempt:x}",
                crate::config::app_name(),
                std::process::id(),
                nanos
            ));
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not create a private review directory",
        ))
    }
}

impl Drop for ReviewDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_review_copy(dir: &ReviewDir, html: &str) -> std::io::Result<String> {
    use std::io::Write;
    let path = dir.0.join("session.html");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&path)?.write_all(html.as_bytes())?;
    Ok(path.to_string_lossy().into_owned())
}

pub async fn run_share<H: ShareHost>(host: &H) -> ShareOutcome {
    if let Some(reason) = host.disabled_reason() {
        return ShareOutcome::Disabled { reason };
    }
    match host.check_gh() {
        GhStatus::Missing => return ShareOutcome::GhMissing,
        GhStatus::LoggedOut => return ShareOutcome::GhLoggedOut,
        GhStatus::Ok => {}
    }
    let context = host.context();
    let render = |sanitize: Sanitize<'_>| host.render(sanitize);
    let mut prepared = match prepare_share(&render, &context, false) {
        Ok(prepared) => prepared,
        Err(message) => return ShareOutcome::ExportFailed { message },
    };
    let viewer_base = host.viewer_base();
    let mut review_dir: Option<ReviewDir> = None;
    loop {
        let omitted_outputs = if prepared.includes_outputs {
            0
        } else {
            prepared.omitted_outputs
        };
        let options = ShareAskOptions {
            omitted_outputs,
            includes_outputs: prepared.includes_outputs,
        };
        match host
            .ask(describe_share(&prepared, viewer_base.as_deref()), options)
            .await
        {
            ShareDecision::Cancel => return ShareOutcome::Cancelled,
            ShareDecision::Upload => break,
            decision @ (ShareDecision::IncludeOutputs | ShareDecision::ExcludeOutputs) => {
                let include = decision == ShareDecision::IncludeOutputs;
                if if include {
                    omitted_outputs == 0
                } else {
                    !prepared.includes_outputs
                } {
                    continue;
                }
                match prepare_share(&render, &context, include) {
                    Ok(next) => prepared = next,
                    Err(message) => return ShareOutcome::ExportFailed { message },
                }
            }
            ShareDecision::Review => {
                if review_dir.is_none() {
                    match ReviewDir::create() {
                        Ok(dir) => review_dir = Some(dir),
                        Err(error) => {
                            return ShareOutcome::ExportFailed {
                                message: error.to_string(),
                            };
                        }
                    }
                }
                let Some(dir) = review_dir.as_ref() else {
                    continue;
                };
                match write_review_copy(dir, &prepared.html) {
                    Ok(path) => host.show_review_copy(&path),
                    Err(error) => {
                        return ShareOutcome::ExportFailed {
                            message: error.to_string(),
                        };
                    }
                }
            }
        }
    }

    let result = host.upload(prepared.html.clone()).await;
    if result.aborted {
        return ShareOutcome::Cancelled;
    }
    if result.code != Some(0) {
        let message = result.stderr.trim();
        return ShareOutcome::UploadFailed {
            message: if message.is_empty() {
                "Unknown error".to_owned()
            } else {
                message.to_owned()
            },
        };
    }
    let Some(gist_url) = parse_gist_url(&result.stdout) else {
        return ShareOutcome::UploadFailed {
            message: "Failed to parse gist URL from gh output".to_owned(),
        };
    };
    let gist_id = gist_url.rsplit('/').next().unwrap_or("");
    let viewer_url = viewer_base.map(|base| format!("{base}#{gist_id}"));
    ShareOutcome::Shared {
        gist_url,
        viewer_url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::{Cell, RefCell};

    struct Host {
        decisions: RefCell<Vec<ShareDecision>>,
        asked: RefCell<Vec<ShareAskOptions>>,
        uploads: Cell<usize>,
        upload_result: ShareUploadResult,
        disabled: Option<String>,
        gh: GhStatus,
        viewer: Option<String>,
        reviewed: RefCell<Vec<String>>,
    }

    impl Host {
        fn new(decisions: Vec<ShareDecision>) -> Self {
            Self {
                decisions: RefCell::new(decisions),
                asked: RefCell::new(Vec::new()),
                uploads: Cell::new(0),
                upload_result: ShareUploadResult {
                    code: Some(0),
                    stdout: "https://gist.github.com/me/abc123\n".into(),
                    stderr: String::new(),
                    aborted: false,
                },
                disabled: None,
                gh: GhStatus::Ok,
                viewer: None,
                reviewed: RefCell::new(Vec::new()),
            }
        }
    }

    impl ShareHost for Host {
        fn disabled_reason(&self) -> Option<String> {
            self.disabled.clone()
        }
        fn check_gh(&self) -> GhStatus {
            self.gh
        }
        fn render(&self, sanitize: Sanitize<'_>) -> Result<String, String> {
            let entries = sanitize(
                json!([{"role": "toolResult", "toolName": "read", "content": [{"type": "text", "text": "file body"}]}, {"role": "user", "content": "hi"}]),
            );
            Ok(entries.to_string())
        }
        fn context(&self) -> ShareRedactionContext {
            ShareRedactionContext {
                home_directory: "/home/alice".into(),
                user_name: "alice".into(),
                host_name: "box".into(),
                extra_literals: Vec::new(),
            }
        }
        async fn ask(&self, _preview: String, options: ShareAskOptions) -> ShareDecision {
            self.asked.borrow_mut().push(options);
            self.decisions.borrow_mut().remove(0)
        }
        fn show_review_copy(&self, path: &str) {
            assert!(std::path::Path::new(path).exists());
            self.reviewed.borrow_mut().push(path.to_owned());
        }
        async fn upload(&self, _html: String) -> ShareUploadResult {
            self.uploads.set(self.uploads.get() + 1);
            self.upload_result.clone()
        }
        fn viewer_base(&self) -> Option<String> {
            self.viewer.clone()
        }
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    #[test]
    fn disabled_and_gh_states_short_circuit() {
        let mut host = Host::new(vec![]);
        host.disabled = Some("off".into());
        assert_eq!(
            block_on(run_share(&host)),
            ShareOutcome::Disabled {
                reason: "off".into()
            }
        );
        let mut host = Host::new(vec![]);
        host.gh = GhStatus::Missing;
        assert_eq!(block_on(run_share(&host)), ShareOutcome::GhMissing);
        host.gh = GhStatus::LoggedOut;
        assert_eq!(block_on(run_share(&host)), ShareOutcome::GhLoggedOut);
    }

    #[test]
    fn cancel_never_uploads() {
        let host = Host::new(vec![ShareDecision::Cancel]);
        assert_eq!(block_on(run_share(&host)), ShareOutcome::Cancelled);
        assert_eq!(host.uploads.get(), 0);
    }

    #[test]
    fn outputs_are_left_out_until_included_then_can_be_left_out_again() {
        let host = Host::new(vec![
            ShareDecision::IncludeOutputs,
            ShareDecision::ExcludeOutputs,
            ShareDecision::Upload,
        ]);
        let outcome = block_on(run_share(&host));
        assert_eq!(
            outcome,
            ShareOutcome::Shared {
                gist_url: "https://gist.github.com/me/abc123".into(),
                viewer_url: None
            }
        );
        let asked = host.asked.borrow();
        assert_eq!(
            asked[0],
            ShareAskOptions {
                omitted_outputs: 1,
                includes_outputs: false
            }
        );
        assert_eq!(
            asked[1],
            ShareAskOptions {
                omitted_outputs: 0,
                includes_outputs: true
            }
        );
        assert_eq!(
            asked[2],
            ShareAskOptions {
                omitted_outputs: 1,
                includes_outputs: false
            }
        );
    }

    #[test]
    fn review_writes_a_private_copy_removed_on_exit() {
        let host = Host::new(vec![ShareDecision::Review, ShareDecision::Cancel]);
        block_on(run_share(&host));
        let reviewed = host.reviewed.borrow();
        assert_eq!(reviewed.len(), 1);
        assert!(!std::path::Path::new(&reviewed[0]).exists());
    }

    #[test]
    fn viewer_url_is_built_only_when_a_viewer_is_configured() {
        let mut host = Host::new(vec![ShareDecision::Upload]);
        host.viewer = Some("https://viewer.example/".into());
        assert_eq!(
            block_on(run_share(&host)),
            ShareOutcome::Shared {
                gist_url: "https://gist.github.com/me/abc123".into(),
                viewer_url: Some("https://viewer.example/#abc123".into())
            }
        );
    }

    #[test]
    fn upload_failures_report_gh_stderr_or_a_fallback() {
        let mut host = Host::new(vec![ShareDecision::Upload]);
        host.upload_result = ShareUploadResult {
            code: Some(1),
            stdout: String::new(),
            stderr: " boom \n".into(),
            aborted: false,
        };
        assert_eq!(
            block_on(run_share(&host)),
            ShareOutcome::UploadFailed {
                message: "boom".into()
            }
        );
        let mut host = Host::new(vec![ShareDecision::Upload]);
        host.upload_result = ShareUploadResult {
            code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
            aborted: false,
        };
        assert_eq!(
            block_on(run_share(&host)),
            ShareOutcome::UploadFailed {
                message: "Unknown error".into()
            }
        );
        let mut host = Host::new(vec![ShareDecision::Upload]);
        host.upload_result = ShareUploadResult {
            code: Some(0),
            stdout: "no url".into(),
            stderr: String::new(),
            aborted: false,
        };
        assert_eq!(
            block_on(run_share(&host)),
            ShareOutcome::UploadFailed {
                message: "Failed to parse gist URL from gh output".into()
            }
        );
    }

    #[test]
    fn description_names_size_hash_and_removed_categories() {
        let prepared = prepare_share(
            &|sanitize| Ok(sanitize(json!({"a": "/home/alice/x"})).to_string()),
            &ShareRedactionContext {
                home_directory: "/home/alice".into(),
                ..Default::default()
            },
            false,
        )
        .expect("prepared");
        let text = describe_share(&prepared, None);
        assert!(text.starts_with("Share this session as a secret GitHub gist?"));
        assert!(text.contains("1 home path") && text.contains("No viewer is configured"));
        assert!(text.contains(&prepared.sha256[..12]));
    }
}
