use std::{fs, process::Command};

#[test]
fn print_prompt_sizes_emits_the_json_report_on_stdout() {
    let home = tempfile::tempdir().expect("isolated home");
    let agent_dir = home.path().join(".omo/agent");
    fs::create_dir_all(&agent_dir).expect("isolated agent state");
    fs::write(home.path().join(".omo/onboarding-completed"), "{}\n")
        .expect("onboarding already complete");
    fs::write(
        agent_dir.join("models.json"),
        serde_json::json!({
            "providers": { "offline": {
                "api": "openai-completions",
                "baseUrl": "http://127.0.0.1:9/v1",
                "apiKey": "offline-fixture",
                "models": [{
                    "id": "offline",
                    "reasoning": false,
                    "input": ["text"],
                    "contextWindow": 128000,
                    "maxTokens": 4096,
                }],
            } }
        })
        .to_string(),
    )
    .expect("model catalog fixture");

    let mut command = Command::new(env!("CARGO_BIN_EXE_mhc"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .current_dir(home.path())
        .args(["--print-prompt-sizes", "--offline"])
        .env("NO_COLOR", "1");
    if cfg!(windows) {
        command.env("USERPROFILE", home.path());
        if let Some(sysroot) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", sysroot);
        }
    }

    let output = command.output().expect("real mhc");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "mhc --print-prompt-sizes failed: status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );

    let report: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not a single JSON object: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    let object = report.as_object().expect("report is a JSON object");
    for key in ["model", "provider", "systemPromptBytes", "contextWindow"] {
        assert!(
            object.contains_key(key),
            "report is missing `{key}`: {stdout}"
        );
    }
    assert!(
        object["systemPromptBytes"].as_u64().unwrap_or(0) > 0,
        "systemPromptBytes must be a positive count: {stdout}"
    );
    assert!(
        object["contextWindow"].as_u64().unwrap_or(0) > 0,
        "contextWindow must be a positive count: {stdout}"
    );
}
