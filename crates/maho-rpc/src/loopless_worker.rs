//! Port of senpi `packages/coding-agent/src/modes/rpc/loopless-worker.ts`.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::loopless_runtime::{LooplessRuntime, LooplessRuntimeProfile};
use crate::loopless_worker_protocol::{HostToLooplessWorker, LooplessWorkerToHost};
use crate::runtime_factory_loopless::{
    LooplessRuntimeFactoryOptions, create_loopless_runtime_factory,
};

async fn send_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &LooplessWorkerToHost,
) -> Result<(), String> {
    let mut line = serde_json::to_string(message).map_err(|e| e.to_string())?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    writer.flush().await.map_err(|e| e.to_string())
}

/// Runs one loopless worker message loop over an input/output JSONL stream pair.
pub async fn run_loopless_worker_loop<R, W>(reader: R, mut writer: W) -> Result<(), String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = reader.lines();
    let mut runtime: Option<LooplessRuntime> = None;
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let message: HostToLooplessWorker = match serde_json::from_str(trimmed) {
            Ok(msg) => msg,
            Err(err) => {
                let _ = send_message(
                    &mut writer,
                    &LooplessWorkerToHost::Failure {
                        error: err.to_string(),
                    },
                )
                .await;
                return Err(err.to_string());
            }
        };
        match message {
            HostToLooplessWorker::Start { configuration, cwd } => {
                if runtime.is_some() {
                    let err = "Loopless worker already started".to_owned();
                    let _ = send_message(
                        &mut writer,
                        &LooplessWorkerToHost::Failure { error: err.clone() },
                    )
                    .await;
                    return Err(err);
                }
                let factory = create_loopless_runtime_factory(
                    configuration,
                    LooplessRuntimeFactoryOptions::default(),
                );
                match factory(LooplessRuntimeProfile::new(cwd)).await {
                    Ok(rt) => {
                        let mut tools: Vec<String> = rt.definitions.keys().cloned().collect();
                        tools.sort();
                        runtime = Some(rt);
                        send_message(&mut writer, &LooplessWorkerToHost::Ready { tools }).await?;
                    }
                    Err(err) => {
                        let _ = send_message(
                            &mut writer,
                            &LooplessWorkerToHost::Failure { error: err.clone() },
                        )
                        .await;
                        return Err(err);
                    }
                }
            }
            HostToLooplessWorker::Close => {
                if let Some(rt) = runtime.take() {
                    rt.dispose().await?;
                }
                return Ok(());
            }
        }
    }
    if let Some(rt) = runtime.take() {
        rt.dispose().await?;
    }
    Ok(())
}

/// Runs the loopless worker loop bound to process stdio (`stdin`/`stdout`).
pub async fn run_loopless_worker_stdio() -> Result<(), String> {
    run_loopless_worker_loop(BufReader::new(tokio::io::stdin()), tokio::io::stdout()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopless_runtime_settings::LooplessCliConfiguration;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn starts_reports_ready_tools_and_closes_cleanly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent");

        let start = serde_json::to_string(&HostToLooplessWorker::Start {
            configuration: LooplessCliConfiguration {
                cwd: dir.path().to_string_lossy().into_owned(),
                agent_dir: agent_dir.to_string_lossy().into_owned(),
                no_extensions: true,
                no_skills: true,
                ..Default::default()
            },
            cwd: dir.path().to_string_lossy().into_owned(),
        })
        .expect("start");
        let close = serde_json::to_string(&HostToLooplessWorker::Close).expect("close");
        let input = format!("{start}\n{close}\n");

        let (writer, mut reader) = tokio::io::duplex(64 * 1024);
        run_loopless_worker_loop(BufReader::new(input.as_bytes()), writer)
            .await
            .expect("worker loop");

        let mut out = String::new();
        reader.read_to_string(&mut out).await.expect("read");
        let reply: LooplessWorkerToHost = serde_json::from_str(out.trim()).expect("parse reply");
        assert!(matches!(
            reply,
            LooplessWorkerToHost::Ready { ref tools } if tools.iter().any(|t| t == "read")
        ));
    }
}
