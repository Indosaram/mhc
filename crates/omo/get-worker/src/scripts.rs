//! Port of `src/scripts.ts`: install-script serving, root detection, docs redirect.

use crate::datapoint::{
    is_qa_install, record_download, request_country, DownloadEvent, RequestKind, ServedFrom,
};
use crate::env::RequestContext;
use crate::http::{Body, Method, Request, Response};

pub const SCRIPT_TTL_SECONDS: u64 = 300;
pub const DOCS_URL: &str = "https://omo.dev/docs/install";
pub const INSTALL_SH: &str = include_str!("../assets/install.sh");
pub const INSTALL_PS1: &str = include_str!("../assets/install.ps1");

/// The two install scripts the worker serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptName {
    InstallSh,
    InstallPs1,
}

impl ScriptName {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ScriptName::InstallSh => "install.sh",
            ScriptName::InstallPs1 => "install.ps1",
        }
    }

    #[must_use]
    pub fn source(self) -> &'static str {
        match self {
            ScriptName::InstallSh => INSTALL_SH,
            ScriptName::InstallPs1 => INSTALL_PS1,
        }
    }
}

#[must_use]
pub fn is_script_name(value: &str) -> bool {
    script_named(value).is_some()
}

#[must_use]
pub fn script_named(value: &str) -> Option<ScriptName> {
    match value {
        "install.sh" => Some(ScriptName::InstallSh),
        "install.ps1" => Some(ScriptName::InstallPs1),
        _ => None,
    }
}

/// `serveScript`: a GET is counted, a HEAD returns the same headers with no body.
pub fn serve_script(request: &Request, ctx: &RequestContext<'_>, name: ScriptName) -> Response {
    if request.method == Method::Get {
        record_download(
            ctx.env.downloads.as_ref(),
            &DownloadEvent {
                kind: RequestKind::Script,
                source: ServedFrom::Worker,
                version: String::new(),
                asset: name.as_str().to_string(),
                country: request_country(request),
                qa: is_qa_install(request),
            },
        );
    }
    let body = match request.method {
        Method::Head => None,
        _ => Some(Body::Text(name.source().to_string())),
    };
    Response::new(200, body)
        .with_header("Content-Type", "text/plain; charset=utf-8")
        .with_header(
            "Cache-Control",
            format!("public, max-age={SCRIPT_TTL_SECONDS}"),
        )
        .with_header("X-Content-Type-Options", "nosniff")
}

/// `scriptForRoot`: PowerShell gets `install.ps1`, a download agent gets `install.sh`,
/// and anything else (a browser) gets `None` so the caller redirects to the docs.
#[must_use]
pub fn script_for_root(request: &Request) -> Option<ScriptName> {
    let agent = request.header("User-Agent").unwrap_or("");
    if agent.to_ascii_lowercase().contains("powershell") {
        return Some(ScriptName::InstallPs1);
    }
    if is_download_agent(agent) {
        return Some(ScriptName::InstallSh);
    }
    None
}

fn is_download_agent(agent: &str) -> bool {
    let Some((head, _)) = agent.split_once('/') else {
        return false;
    };
    matches!(
        head.to_ascii_lowercase().as_str(),
        "curl" | "wget" | "fetch" | "httpie"
    )
}

#[must_use]
pub fn docs_redirect() -> Response {
    Response::new(302, None)
        .with_header("Location", DOCS_URL)
        .with_header("Cache-Control", "public, max-age=3600")
}
