use maho_ext_permission_system::{service::PermissionService,events::PermissionEventEmitter,types::*};

fn request(id:&str,session:&str)->Request { Request{id:id.into(),session_id:session.into(),permission:"bash".into(),patterns:vec!["git commit".into()],always:vec!["git *".into()],metadata:Default::default(),tool:None} }
fn rule(action:Action)->Rule { Rule{permission:"bash".into(),pattern:"*".into(),action} }
#[tokio::test]
async fn static_allow(){ let mut s=PermissionService::new(vec![rule(Action::Allow)],vec![],PermissionEventEmitter::default()); assert!(s.ask(request("a","s")).await.is_ok()); assert!(s.list().is_empty()); }
#[tokio::test]
async fn approved_allow(){ let mut s=PermissionService::new(vec![],vec![rule(Action::Allow)],PermissionEventEmitter::default()); assert!(s.ask(request("a","s")).await.is_ok()); }
#[tokio::test]
async fn static_deny(){ let mut s=PermissionService::new(vec![rule(Action::Deny)],vec![],PermissionEventEmitter::default()); assert!(matches!(s.ask(request("a","s")).await,Err(PermissionError::Denied(_)))); }
#[tokio::test]
async fn approved_deny(){ let mut s=PermissionService::new(vec![],vec![rule(Action::Deny)],PermissionEventEmitter::default()); assert!(matches!(s.ask(request("a","s")).await,Err(PermissionError::Denied(_)))); }
#[tokio::test]
async fn once_resolves(){ let mut s=PermissionService::new(vec![],vec![],PermissionEventEmitter::default()); let future=s.ask(request("a","s")); assert_eq!(s.list().len(),1); s.reply(ReplyInput{request_id:"a".into(),reply:Reply::Once,message:None}); assert!(future.await.is_ok()); assert!(s.get_approved().is_empty()); }
#[tokio::test]
async fn always_covers_same_session(){ let mut s=PermissionService::new(vec![],vec![],PermissionEventEmitter::default()); let first=s.ask(request("a","s")); let second=s.ask(request("b","s")); s.reply(ReplyInput{request_id:"a".into(),reply:Reply::Always,message:None}); assert!(first.await.is_ok()); assert!(second.await.is_ok()); assert_eq!(s.get_approved().len(),1); }
#[tokio::test]
async fn reject_cascades_same_session(){ let mut s=PermissionService::new(vec![],vec![],PermissionEventEmitter::default()); let first=s.ask(request("a","s")); let second=s.ask(request("b","s")); s.reply(ReplyInput{request_id:"a".into(),reply:Reply::Reject,message:None}); assert!(matches!(first.await,Err(PermissionError::Rejected))); assert!(matches!(second.await,Err(PermissionError::Rejected))); }
#[tokio::test]
async fn feedback_correction(){ let mut s=PermissionService::new(vec![],vec![],PermissionEventEmitter::default()); let future=s.ask(request("a","s")); s.reply(ReplyInput{request_id:"a".into(),reply:Reply::Reject,message:Some("use status".into())}); assert!(matches!(future.await,Err(PermissionError::Corrected(text)) if text == "use status")); }
#[tokio::test]
async fn other_session_unaffected(){ let mut s=PermissionService::new(vec![],vec![],PermissionEventEmitter::default()); let first=s.ask(request("a","s")); let second=s.ask(request("b","other")); s.reply(ReplyInput{request_id:"a".into(),reply:Reply::Reject,message:None}); assert_eq!(s.list()[0].session_id,"other"); s.reply(ReplyInput{request_id:"b".into(),reply:Reply::Once,message:None}); assert!(first.await.is_err()); assert!(second.await.is_ok()); }
#[tokio::test]
async fn generated_id(){ let mut s=PermissionService::new(vec![],vec![],PermissionEventEmitter::default()); let future=s.ask(request("","s")); assert_eq!(s.list()[0].id,"permission-1"); s.reply(ReplyInput{request_id:"permission-1".into(),reply:Reply::Once,message:None}); assert!(future.await.is_ok()); }
#[tokio::test]
async fn denied_subset(){let mut s=PermissionService::new(vec![Rule{permission:"bash".into(),pattern:"rm *".into(),action:Action::Deny}],vec![],PermissionEventEmitter::default());let mut r=request("a","s");r.patterns=vec!["git commit".into(),"rm -rf tmp".into(),"ls".into()];assert!(matches!(s.ask(r).await,Err(PermissionError::Denied(patterns)) if patterns==vec!["rm -rf tmp"]));assert!(s.list().is_empty());}
#[tokio::test]
async fn generated_ids_increment(){let mut s=PermissionService::new(vec![],vec![],PermissionEventEmitter::default());for index in 1..=2{let future=s.ask(request("","s"));let id=format!("permission-{index}");assert_eq!(s.list()[0].id,id);s.reply(ReplyInput{request_id:id,reply:Reply::Once,message:None});assert!(future.await.is_ok());}}
#[tokio::test]
async fn pending_insertion_order(){let mut s=PermissionService::new(vec![],vec![],PermissionEventEmitter::default());let first=s.ask(request("first","s"));let second=s.ask(request("second","s"));assert_eq!(s.list().iter().map(|r|r.id.as_str()).collect::<Vec<_>>(),vec!["first","second"]);for id in ["first","second"]{s.reply(ReplyInput{request_id:id.into(),reply:Reply::Once,message:None});}assert!(first.await.is_ok());assert!(second.await.is_ok());}
#[test]
fn approved_defensive_copy(){let s=PermissionService::new(vec![],vec![rule(Action::Allow)],PermissionEventEmitter::default());let mut copy=s.get_approved();copy.clear();assert_eq!(s.get_approved().len(),1);}
#[test]
fn unknown_reply_ignored(){let mut s=PermissionService::new(vec![],vec![],PermissionEventEmitter::default());s.reply(ReplyInput{request_id:"unknown".into(),reply:Reply::Always,message:None});assert!(s.get_approved().is_empty());assert!(s.list().is_empty());}
#[tokio::test]
async fn uncovered_pending_remains(){let mut s=PermissionService::new(vec![],vec![],PermissionEventEmitter::default());let first=s.ask(request("a","s"));let mut r=request("b","s");r.patterns=vec!["docker build".into()];let second=s.ask(r);s.reply(ReplyInput{request_id:"a".into(),reply:Reply::Always,message:None});assert_eq!(s.list()[0].id,"b");s.reply(ReplyInput{request_id:"b".into(),reply:Reply::Once,message:None});assert!(first.await.is_ok());assert!(second.await.is_ok());}
#[tokio::test]
async fn always_other_session_remains(){let mut s=PermissionService::new(vec![],vec![],PermissionEventEmitter::default());let first=s.ask(request("a","s"));let second=s.ask(request("b","other"));s.reply(ReplyInput{request_id:"a".into(),reply:Reply::Always,message:None});assert_eq!(s.list()[0].session_id,"other");s.reply(ReplyInput{request_id:"b".into(),reply:Reply::Once,message:None});assert!(first.await.is_ok());assert!(second.await.is_ok());}
