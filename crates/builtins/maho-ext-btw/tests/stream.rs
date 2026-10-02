use maho_ext_btw::side_query::collect_reply;
use maho_ai::{types::{AssistantMessage,AssistantMessageEvent,Usage,StopReason,DoneReason},utils::event_stream::AssistantMessageEventStream};
fn message()->AssistantMessage{AssistantMessage{content:vec![],api:"faux".into(),provider:"faux".into(),model:"m".into(),response_model:None,response_id:None,provider_thinking_level:None,diagnostics:None,usage:Usage::default(),stop_reason:StopReason::Stop,stop_details:None,deferred:None,error_message:None,abort_source:None,raw_stop_reason:None,end_turn:None,timestamp:0}}
#[tokio::test]
async fn deltas_accumulate_until_done(){let stream=AssistantMessageEventStream::assistant();stream.push(AssistantMessageEvent::Start{partial:message()});for delta in ["A","B"]{stream.push(AssistantMessageEvent::TextDelta{content_index:0,delta:delta.into(),partial:message()});}stream.push(AssistantMessageEvent::Done{reason:DoneReason::Stop,message:message()});let mut deltas=Vec::new();assert_eq!(collect_reply(&stream,30000,|delta|deltas.push(delta.to_owned())).await.expect("reply"),"AB");assert_eq!(deltas,["A","B"]);}
#[tokio::test(start_paused=true)]
async fn start_event_does_not_establish(){let stream=AssistantMessageEventStream::assistant();stream.push(AssistantMessageEvent::Start{partial:message()});assert!(collect_reply(&stream,30000,|_|{}).await.is_err());}
#[tokio::test(start_paused=true)]
async fn stream_failure_propagates(){let stream=AssistantMessageEventStream::assistant();stream.fail(maho_ai::utils::event_stream::StreamError::new("failed"));assert_eq!(collect_reply(&stream,30000,|_|{}).await.expect_err("stream failure"),"failed");}
