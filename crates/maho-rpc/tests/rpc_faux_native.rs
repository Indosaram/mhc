use maho_test_support::{faux::{FauxResponse,FauxScript},faux_session::FauxSession};
use maho_rpc::{json_event::to_json_event,jsonl::{serialize_json_line,JsonlLineReader,LineRecord},print_mode::format_print_result};

#[tokio::test]
async fn native_prompt_events_survive_rpc_jsonl_and_print_formatting(){
    let session=FauxSession::new(FauxScript{name:"rpc-native".into(),prompt:"hello".into(),responses:vec![FauxResponse{content:"native response".into(),stop_reason:"stop".into()}]});
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.expect("native turn settles").expect("native session runs");
    let events=result["events"].as_array().expect("events");
    let wire=events.iter().map(|event|serialize_json_line(to_json_event(event).expect("event transformation").as_ref()).expect("JSONL serialization")).collect::<String>();
    let mut reader=JsonlLineReader::new(16*1024*1024).expect("line limit");
    let lines=reader.push(wire.as_bytes());assert_eq!(lines.len(),events.len());
    let parsed=lines.iter().map(|line|match line{LineRecord::Line(line)=>serde_json::from_str::<serde_json::Value>(line).expect("parse event"),LineRecord::Oversized=>panic!("native event exceeded bound")}).collect::<Vec<_>>();
    assert_eq!(parsed.first().expect("start")["type"],"agent_start");assert_eq!(parsed.last().expect("end")["type"],"agent_end");
    assert!(parsed.iter().any(|event|event["type"]=="message_update"));
    for event in parsed.iter().filter(|event|event["type"]=="message_update"){assert!(event.get("message").is_none());assert!(event["assistantMessageEvent"].get("partial").is_none());}
    let assistant=serde_json::from_value::<maho_ai::types::AssistantMessage>(result["messages"].as_array().expect("messages").last().expect("assistant").clone()).expect("typed assistant");
    let output=format_print_result(Some(&assistant)).expect("format print result");assert_eq!(output.stdout,"native response\n");assert_eq!(output.exit_code,0);
    assert_eq!(result["entries"].as_array().expect("entries").iter().filter(|entry|entry["type"]=="message").count(),2);
}
