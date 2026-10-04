#[tokio::test(start_paused=true)]
async fn installed_idle_policy_timer_exits_once_after_continuous_empty_window(){
    let mut policy=maho_rpc::session_command_router::SessionIdlePolicy::new(f64::INFINITY,100.);
    let(_stop,stopped)=tokio::sync::watch::channel(false);
    let started=tokio::time::Instant::now();let mut exits=0;
    policy.run(||(started.elapsed().as_secs_f64()*1000.,vec![],0,true),|sweep,_|{if sweep.exit{exits+=1;}},stopped).await;
    assert_eq!(exits,1);assert_eq!(started.elapsed(),std::time::Duration::from_millis(125));
}
