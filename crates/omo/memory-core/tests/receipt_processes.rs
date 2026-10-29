use memory_core::receipts::{append_memory_receipt, append_memory_receipt_once, read_memory_receipts, MemoryReceiptInput, ReadMemoryReceiptsOptions};
use std::io::{BufRead, Write};

#[test]
fn receipt_process_worker() {
    let Some(root) = std::env::var_os("RECEIPT_PROCESS_ROOT") else { return; };
    println!("READY");
    std::io::stdout().flush().unwrap();
    let mut signal = String::new();
    std::io::stdin().read_line(&mut signal).unwrap();
    assert_eq!(signal.trim(), "GO");
    let input = MemoryReceiptInput::Facts { kind: "facts".into(), batch_id: "same-batch".into(), trigger: "settle".into(), event: "committed".into(), sha: None, reason: None, detail: None };
    if std::env::var_os("RECEIPT_PROCESS_ONCE").is_some() {
        append_memory_receipt_once(std::path::Path::new(&root), &input).unwrap();
    } else {
        append_memory_receipt(std::path::Path::new(&root), &input).unwrap();
    }
}

#[test]
fn competing_processes_append_whole_lines_and_deduplicate_under_one_lock() {
    for once in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut children = Vec::new();
        for _ in 0..3 {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "receipt_processes::receipt_process_worker", "--nocapture"])
                .env("RECEIPT_PROCESS_ROOT", root.path())
                .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped());
            if once { command.env("RECEIPT_PROCESS_ONCE", "1"); }
            let mut child = command.spawn().unwrap();
            let stdout = child.stdout.take().unwrap();
            let (sender, receiver) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    if line.unwrap() == "READY" { sender.send(()).unwrap(); }
                }
            });
            receiver.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            children.push((child, reader));
        }
        for (child, _) in &mut children { child.stdin.take().unwrap().write_all(b"GO\n").unwrap(); }
        for (mut child, reader) in children { assert!(child.wait().unwrap().success()); reader.join().unwrap(); }
        let read = read_memory_receipts(root.path(), &ReadMemoryReceiptsOptions::default()).unwrap();
        assert_eq!(read.receipts.len(), if once { 1 } else { 3 });
        assert_eq!(read.skipped_partial_lines, 0);
    }
}
