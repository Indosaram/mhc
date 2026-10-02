use maho_core::{model_runtime::{CreateModelRuntimeOptions, ModelRuntime}, sdk::{CreateAgentSessionOptions, NoToolsMode, create_agent_session}, session_manager::SessionManager, settings_manager::{InMemorySettingsStorage, SettingsManager}};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = std::env::args().nth(1).ok_or("usage: native_bad_recording <output.jsonl>")?;
    let temp = tempfile::tempdir()?;
    let cwd = temp.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let model = provider.get_model(Some("faux-1")).ok_or("missing faux model")?;
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        models_path: Some(temp.path().join("models.json")), auth_path: Some(temp.path().join("auth.json")),
        providers: Some(vec![provider.provider]), ..Default::default()
    });
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(cwd.clone()), model_runtime: Some(runtime), model: Some(model),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        settings_manager: Some(SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), false)),
        no_tools: Some(NoToolsMode::All), auto_title_sessions: Some(false), ..Default::default()
    }).await? .session;
    let (mut client, host) = tokio::net::UnixStream::pair()?;
    let (input, output) = host.into_split();
    let run = maho_rpc::rpc_mode::run_command_stream(&session, input, output);
    let drive = async {
        client.write_all(b"{invalid\n").await?;
        client.shutdown().await?;
        let mut output = String::new();
        client.read_to_string(&mut output).await?;
        Ok::<_, std::io::Error>(output)
    };
    let (run, output) = tokio::time::timeout(std::time::Duration::from_secs(10), async { tokio::join!(run, drive) }).await?;
    run?;
    tokio::fs::write(path, output?).await?;
    session.dispose().await;
    Ok(())
}
