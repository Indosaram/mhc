use maho_ext_api::{QuestionAnswer, QuestionStatus};
use maho_ext_ask_user::{pending::PendingTimer, schema::{AskUserVariant, to_canonical}};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let request = to_canonical(AskUserVariant::Claude, &json!({"waitForAnswer":true,"questions":[{"header":"Choice","question":"Which?","multiSelect":false}]}), "native".into(), Some(0))?;
    let (outcome, mut received) = tokio::sync::watch::channel(None);
    let timer = PendingTimer::new(request.clone(), Arc::new(move |response| { outcome.send_replace(Some(response)); }));
    let answers = BTreeMap::from([("q1".into(), QuestionAnswer { selected: vec!["A".into()], text: None })]);
    timer.touch(Some((answers.clone(), Some("draft".into()))));
    received.changed().await?;
    let response = received.borrow().clone().ok_or("No timeout outcome")?;
    assert_eq!(response.status, QuestionStatus::TimedOut);
    assert_eq!(response.answers, answers);
    assert_eq!(timer.cancel(QuestionStatus::Cancelled), response);
    drop(timer);
    let (outcome, mut cancelled) = tokio::sync::watch::channel(None);
    let timer = PendingTimer::new(request, Arc::new(move |response| { outcome.send_replace(Some(response)); }));
    assert_eq!(timer.cancel(QuestionStatus::Cancelled).status, QuestionStatus::Cancelled);
    drop(timer);
    assert!(cancelled.changed().await.is_err());
    assert!(cancelled.borrow().is_none());
    println!("{}", json!({"timedOut":true,"draftPreserved":true,"cancelDisarmed":true,"pass":true}));
    println!("cleanup: both authoritative timer tasks retired; no UI or process resource created");
    Ok(())
}
