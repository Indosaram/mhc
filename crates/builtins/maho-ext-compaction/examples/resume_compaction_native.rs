include!("../tests/registration.rs");

#[tokio::main(flavor = "current_thread")]
async fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("policy") => {
            run_policy_scenario().await;
            println!("PASS policy: registered context consumes disabled then live enabled admission gate; cleanup: no background resources");
        }
        Some("threshold") => run_native_scenario(false,true).await,
        Some("cancellation") => run_native_scenario(true,false).await,
        Some("overflow") => run_native_variant(false,true,"overflow").await,
        Some("fallback") => run_native_variant(false,false,"fallback").await,
        _ => panic!("expected policy, threshold, or cancellation"),
    }
}
