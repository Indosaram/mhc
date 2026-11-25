use super::harness::Fixture;
use maho_machine_ops::{MessageRequest, ProcessInfoRequest};
use serde_json::json;

#[tokio::test]
async fn info_process_and_message_and_host() {
    let f = Fixture::create(true).await;

    let info = f
        .ops
        .info()
        .process_info(ProcessInfoRequest::default())
        .await
        .expect("own process info");
    assert_eq!(info.pid, std::process::id());
    assert!(info.rss_bytes > 0);

    let first = f
        .ops
        .info()
        .message(
            MessageRequest {
                topic: "alpha".to_owned(),
                body: json!({ "n": 1 }),
            },
            &f.ctx(),
        )
        .await
        .expect("msg 1");
    let second = f
        .ops
        .info()
        .message(
            MessageRequest {
                topic: "alpha".to_owned(),
                body: json!({ "n": 2 }),
            },
            &f.ctx(),
        )
        .await
        .expect("msg 2");
    let other = f
        .ops
        .info()
        .message(
            MessageRequest {
                topic: "beta".to_owned(),
                body: json!("x"),
            },
            &f.ctx(),
        )
        .await
        .expect("msg 3");
    assert_eq!([first.seq, second.seq, other.seq], [1, 2, 1]);
    assert_eq!(first.inbox_path, second.inbox_path);

    let text = tokio::fs::read_to_string(&first.inbox_path)
        .await
        .expect("read inbox");
    let lines: Vec<serde_json::Value> = text
        .trim_end()
        .split('\n')
        .map(|l| serde_json::from_str(l).expect("parse line"))
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["body"], json!({ "n": 1 }));
    assert_eq!(lines[1]["body"], json!({ "n": 2 }));

    let host = f.ops.info().host();
    assert!(host.cores > 0);
}
