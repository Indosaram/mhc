#[path = "../examples/native_lifecycle/support.rs"]
mod support;

#[tokio::test]
async fn registered_first_party_hooks() { support::run(true,true).await.expect("native hooks"); }
#[tokio::test]
async fn registered_compatible_endpoint_hooks() { support::run(false,true).await.expect("compatible hooks"); }
#[tokio::test]
async fn registered_headless_hooks() { support::run(true,false).await.expect("headless hooks"); }
