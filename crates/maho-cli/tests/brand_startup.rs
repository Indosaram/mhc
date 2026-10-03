use std::{fs, io::Write, process::{Command, Stdio}};

#[test]
fn flat_brand_startup_copies_engine_state_once() {
    let home = tempfile::tempdir().expect("isolated home");
    let legacy = home.path().join(".senpi/agent");
    fs::create_dir_all(&legacy).expect("legacy directory");
    fs::write(legacy.join("models.json"), "{\"providers\":{}}\n").expect("models fixture");
    let target = home.path().join(".qa");
    for iteration in 0..2 {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mhc"))
            .env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home.path()).env("MAHO_BRAND", r#"{"name":"qa","configDir":".qa","flatLayout":true}"#)
            .current_dir(home.path())
            .args(["--mode", "rpc", "--offline", "--no-session", "--model", "openai/gpt-4o"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().expect("real mhc");
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(b"{\"id\":\"brand-probe\",\"type\":\"get_messages\"}\n").expect("query");
        drop(stdin);
        let output = child.wait_with_output().expect("process exits");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).expect("RPC response");
        assert_eq!(response["id"], "brand-probe");
        assert_eq!(response["success"], true);
        assert_eq!(fs::read_to_string(target.join("models.json")).expect("copied models"), "{\"providers\":{}}\n");
        assert!(target.join(".migrated-from-senpi").exists());
        assert_eq!(fs::read_to_string(legacy.join("models.json")).expect("original models"),
            if iteration == 0 { "{\"providers\":{}}\n" } else { "changed source" });
        if iteration == 0 { fs::write(legacy.join("models.json"), "changed source").expect("change source"); }
    }
}
