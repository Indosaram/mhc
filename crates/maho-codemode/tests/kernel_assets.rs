use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn python_asset_executes_persistent_cells() {
    let mut child = tokio::process::Command::new("python3")
        .args(["-u", concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/py/prelude.py")])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true)
        .spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    input.write_all(b"{\"type\":\"init\",\"sessionId\":\"asset-test\",\"connection\":{\"port\":1,\"token\":\"test\"}}\n").await.unwrap();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(5), output.next_line()).await.unwrap().unwrap().unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&ready).unwrap()["type"], "ready");
    for (cell_id, code, expected) in [("one", "x = 7\nx", "7"), ("two", "x + 1", "8")] {
        let frame = serde_json::json!({"type":"run","cellId":cell_id,"code":code});
        input.write_all(format!("{frame}\n").as_bytes()).await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let line = output.next_line().await.unwrap().unwrap();
                let value: serde_json::Value = serde_json::from_str(&line).unwrap();
                if value["type"] == "result" { break value; }
            }
        }).await.unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["valueRepr"], expected);
    }
    input.write_all(b"{\"type\":\"close\"}\n").await.unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await.unwrap().unwrap().success());
}

#[test]
fn every_runtime_asset_is_shipped() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/kernels");
    for path in ["js/worker-entry.js", "js/inline-worker-entry.js", "js/worker-core.js", "js/worker-runtime.js", "js/worker-indirect-eval.js", "js/kernel-tools-pump.js", "js/workpool.js", "py/prelude.py", "rb/runner.rb", "rb/prelude.rb", "rb/workpool.rb", "jl/runner.jl", "jl/prelude.jl"] {
        assert!(root.join(path).is_file(), "missing asset {path}");
    }
}
