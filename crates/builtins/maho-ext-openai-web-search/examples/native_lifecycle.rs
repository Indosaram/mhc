#[path = "native_lifecycle/support.rs"]
mod support;

#[tokio::main(flavor="current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for (native, ui) in [(true,true),(false,true),(true,false)] { support::run(native,ui).await?; }
    println!("cleanup: all native runners shut down and invalidated; no runtime resources created");
    Ok(())
}
