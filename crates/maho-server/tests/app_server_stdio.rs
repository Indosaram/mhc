use maho_server::app_server::{server_core::ServerCore, stdio::run_stdio};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn duplex_transport_orders_parse_error_initialize_and_final_unterminated_request() {
    let mut core = ServerCore::new("/tmp/home".into(), "1".into(), "Linux".into(), "test".into(), "x64".into(), "linux".into());
    let (mut client, server) = tokio::io::duplex(8192);
    let (input, output) = tokio::io::split(server);
    let client_work = async move {
        client.write_all(b"invalid\n{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"c\",\"version\":\"1\"}}}\n{\"id\":2,\"method\":\"unknown\"}").await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        response
    };
    let (result, response) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(run_stdio(&mut core, "stdio", input, output), client_work)
    }).await.unwrap();
    result.unwrap();
    let messages: Vec<serde_json::Value> = response.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["error"]["code"], -32700);
    assert_eq!(messages[1]["id"], 1);
    assert_eq!(messages[1]["result"]["codexHome"], "/tmp/home");
    assert_eq!(messages[2]["id"], 2);
    assert_eq!(messages[2]["error"]["code"], -32601);
    assert!(core.get_connection("stdio").is_none());
}
