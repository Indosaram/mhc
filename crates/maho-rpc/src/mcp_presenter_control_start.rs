//! Port of senpi `packages/coding-agent/src/modes/rpc/mcp-presenter-control-start.ts`.
//!
//! Starts the management endpoint of an enforced presenter host (senpi#4249).

use crate::mcp_presenter_account::PresenterAccountAuthority;
use crate::mcp_presenter_control::{McpPresenterControl, McpPresenterWriter};
use crate::mcp_presenter_wire::BoxedPrivateStream;
use std::path::PathBuf;
use std::sync::Arc;

pub fn presenter_pipe_path() -> String {
    format!(r"\\.\pipe\senpi-presenter-{}", uuid::Uuid::new_v4())
}

pub struct McpPresenterControlStart {
    pub control: McpPresenterControl,
    pub dir: Option<PathBuf>,
}

pub async fn start_mcp_presenter_control(
    writer: Arc<dyn McpPresenterWriter>,
    authority: crate::mcp_presenter_authority::McpPresenterAuthority,
    enforced: bool,
    bootstrap: Option<BoxedPrivateStream>,
    account: Option<Arc<dyn PresenterAccountAuthority>>,
) -> Option<McpPresenterControlStart> {
    if !enforced || (bootstrap.is_none() && account.is_none()) {
        drop(bootstrap);
        return None;
    }
    #[cfg(unix)]
    {
        let mut nonce = [0u8; 8];
        let _ = getrandom::fill(&mut nonce);
        let dir_path = std::env::temp_dir().join(format!(
            "senpi-presenter-{}-{}",
            std::process::id(),
            hex::encode(nonce)
        ));
        let mut dir_builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            dir_builder.mode(0o700);
        }
        dir_builder.create(&dir_path).ok()?;
        let socket_path = dir_path.join("control.sock");
        let control = McpPresenterControl::new(writer, authority, &socket_path.to_string_lossy());
        if control.listen().await.is_err() {
            let _ = std::fs::remove_dir_all(&dir_path);
            return None;
        }
        if let Some(stream) = bootstrap {
            control.bootstrap(stream);
        }
        Some(McpPresenterControlStart {
            control,
            dir: Some(dir_path),
        })
    }
    #[cfg(not(unix))]
    {
        let pipe = presenter_pipe_path();
        let control = McpPresenterControl::new(writer, authority, &pipe);
        if let Some(stream) = bootstrap {
            control.bootstrap(stream);
        }
        Some(McpPresenterControlStart { control, dir: None })
    }
}
