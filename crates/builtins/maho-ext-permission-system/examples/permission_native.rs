#[path = "permission_native/support.rs"]
mod support;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    support::run().await
}
