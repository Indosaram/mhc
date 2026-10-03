include!("../tests/registration.rs");

#[tokio::main(flavor = "current_thread")]
async fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("policy") => {
            run_policy_scenario().await;
            println!("PASS policy: registered context consumes disabled then live enabled admission gate; cleanup: no background resources");
        }
        Some("fractional") => run_native_variant(false,false,"fractional").await,
        Some("reminder") => run_native_variant(false,false,"reminder").await,
        Some("threshold") => run_native_scenario(false,true).await,
        Some("cancellation") => run_native_scenario(true,false).await,
        Some("overflow") => run_native_variant(false,true,"overflow").await,
        Some("fallback") => run_native_variant(false,false,"fallback").await,
        Some("remote-http") => run_native_variant(false,false,"remote-http").await,
        Some("remote-sse") => run_native_variant(false,false,"remote-sse").await,
        Some("remote-cancel") => run_native_variant(false,false,"remote-cancel").await,
        Some("session-abort") => run_native_variant(false,false,"session-abort").await,
        Some("remote-auth") => run_native_variant(false,false,"remote-auth").await,
        Some("remote-network") => run_native_variant(false,false,"remote-network").await,
        _ => panic!("expected policy, threshold, or cancellation"),
    }
}
