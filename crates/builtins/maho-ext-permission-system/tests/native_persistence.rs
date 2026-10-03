#[path = "../examples/permission_native/support.rs"]
mod support;

#[tokio::test]
async fn registered_patch_persists_approval_and_denies_without_mutation() {
    support::run().await.expect("registered permission persistence scenario");
}
