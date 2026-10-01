use maho_ai::{auth::{credential_store::InMemoryCredentialStore,types::{Credential,CredentialStore,OAuthCredential}},utils::abort::AbortController};
use maho_ext_anthropic_subscription::accounts::{AccountSlot,AccountSource,empty_credential,add_account,refresh_slot};
use std::{sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
use tokio::sync::oneshot;

#[tokio::test]
async fn concurrent_refresh_reads_current_credential_under_store_lock() {
    let store=Arc::new(InMemoryCredentialStore::new());
    let slot=AccountSlot { name:"default".into(),display_name:None,refresh:"rA".into(),access:"aA".into(),expires:0.0,
        source:AccountSource::Login,blocked_until:None,block_reason:None };
    let credential=add_account(&empty_credential(),slot).unwrap();
    store.modify("anthropic-subscription",Box::new(move |_| Box::pin(async { Ok(Some(Credential::OAuth(credential))) })),None).await.unwrap();
    let calls=Arc::new(AtomicUsize::new(0));
    let (entered_tx,entered_rx)=oneshot::channel();let (release_tx,release_rx)=oneshot::channel();
    let first_store=store.clone();let first_calls=calls.clone();
    let first=tokio::spawn(async move {
        refresh_slot(first_store.as_ref(),"anthropic-subscription","default",move |_,_| async move {
            first_calls.fetch_add(1,Ordering::SeqCst); entered_tx.send(()).unwrap(); release_rx.await.unwrap();
            Ok(OAuthCredential::new("new","new",60000.0))
        },AbortController::new().signal(),|expires| expires<=1000.0).await
    });
    tokio::time::timeout(Duration::from_secs(5),entered_rx).await.unwrap().unwrap();
    let second_calls=calls.clone();
    let mut second=Box::pin(refresh_slot(store.as_ref(),"anthropic-subscription","default",move |_,_| async move {
        second_calls.fetch_add(1,Ordering::SeqCst);Ok(OAuthCredential::new("newer","newer",60000.0))
    },AbortController::new().signal(),|expires| expires<=1000.0));
    assert!(matches!(std::future::poll_fn(|cx| std::task::Poll::Ready(second.as_mut().poll(cx))).await,std::task::Poll::Pending));
    release_tx.send(()).unwrap(); first.await.unwrap().unwrap(); second.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst),1,"second serialized mutation must see the already-refreshed slot");
}
