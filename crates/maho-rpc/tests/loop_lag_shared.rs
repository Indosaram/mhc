#[tokio::test(start_paused=true)]
async fn installed_watchdog_publishes_drift_to_the_socket_actor_ledger(){
    use maho_rpc::{loop_lag_watchdog::LoopLagWatchdog,loop_blocked_time::LoopBlockedTime,session_attribution::SessionActivityRegistry};
    let blocked=std::sync::Mutex::new(LoopBlockedTime::default());
    let registry=SessionActivityRegistry::default();
    let mut watchdog=LoopLagWatchdog::new(&Default::default());
    let(stop,stopped)=tokio::sync::watch::channel(false);
    let mut now=0.;let mut records=Vec::new();
    watchdog.run_shared(&registry,&blocked,||{let result=now;now+=6000.;result},|sample|{records.push(sample.record.unwrap());stop.send(true).unwrap();},stopped).await;
    assert_eq!(records.len(),1);assert_eq!(records[0]["driftMs"],5800.);
    assert_eq!(blocked.lock().unwrap().loop_blocked_mark(),5800.);
}

#[tokio::test(start_paused=true)]
async fn unchanged_stop_signal_does_not_restart_the_watchdog_interval(){
    use maho_rpc::{loop_lag_watchdog::LoopLagWatchdog,loop_blocked_time::LoopBlockedTime,session_attribution::SessionActivityRegistry};
    let blocked=std::sync::Mutex::new(LoopBlockedTime::default());
    let registry=SessionActivityRegistry::default();
    let mut watchdog=LoopLagWatchdog::new(&Default::default());
    let(stop,stopped)=tokio::sync::watch::channel(false);
    let started=tokio::time::Instant::now();let mut ticks=0;
    let monitor=watchdog.run_shared(&registry,&blocked,||started.elapsed().as_secs_f64()*1000.,|sample|{ticks+=1;assert!(sample.record.is_none());stop.send(true).unwrap();},stopped);
    let signal=async{
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        stop.send(false).unwrap();
    };
    tokio::join!(monitor,signal);
    assert_eq!(ticks,1);
    assert_eq!(started.elapsed(),std::time::Duration::from_millis(200));
    assert_eq!(blocked.lock().unwrap().loop_blocked_mark(),0.);
}
