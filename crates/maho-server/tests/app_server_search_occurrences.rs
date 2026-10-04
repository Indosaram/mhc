use maho_server::app_server::{search_occurrences::{markdown_to_search_text,occurrences_response},turn_log::{LoggedTurn,TurnStatus}};
use serde_json::{Value,json};

#[test]
fn occurrence_search_uses_visible_messages_markdown_and_utf16_ranges() {
    let items = [json!({"id":"user","type":"userMessage","content":[{"type":"text","text":"😀Hello hello"}]}),json!({"id":"hidden","type":"agentMessage","text":"hello hidden"}),json!({"id":"final","type":"agentMessage","text":"**Hello** [world](https://example.test)"})].into_iter().map(|item|item.as_object().unwrap().clone()).collect();
    let turns = vec![LoggedTurn {turn_id:"turn".into(),started_at:"2020-01-01T00:00:00Z".into(),completed_at:None,duration_ms:None,error:None,status:TurnStatus::Completed,items}];
    let params = json!({"threadId":"thread","searchTerm":"hello","limit":1});
    let first = occurrences_response(&params,&turns).unwrap();
    assert_eq!(first["data"][0]["snippetMatchRange"],json!({"start":2,"end":7}));
    let mut next_params = params.clone();next_params["cursor"] = first["nextCursor"].clone();
    let second = occurrences_response(&next_params,&turns).unwrap();
    assert_eq!(second["data"][0]["itemId"],"user");
    next_params["cursor"] = second["nextCursor"].clone();
    let third = occurrences_response(&next_params,&turns).unwrap();
    assert_eq!(third["data"][0]["itemId"],"final");
    assert_eq!(third["nextCursor"],Value::Null);
    assert_eq!(markdown_to_search_text("**Hello** [world](https://example.test)"),"Hello world");
    next_params["searchTerm"] = json!("different");
    assert_eq!(occurrences_response(&next_params,&turns).unwrap_err().code,-32600);
}
