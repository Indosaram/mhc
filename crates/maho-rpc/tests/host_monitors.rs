#[tokio::test(start_paused=true)]
async fn host_monitors_run_together_and_stop_without_leaving_a_timer(){
    let env=std::collections::HashMap::from([("SENPI_RPC_HOST_RSS_WARN_MB".into(),"10".into())]);
    let activity=maho_rpc::session_attribution::SessionActivityRegistry::default();
    let blocked=std::sync::Mutex::new(maho_rpc::loop_blocked_time::LoopBlockedTime::default());
    let(stop,stopped)=tokio::sync::watch::channel(false);let started=tokio::time::Instant::now();let mut records=Vec::new();
    maho_rpc::multi_session_host::run_host_monitors(&env,&activity,&blocked,||(21*1048576,1,started.elapsed().as_millis() as u64),||started.elapsed().as_secs_f64()*1000.,|sample|{
        match sample{
            maho_rpc::multi_session_host::HostMonitorSample::Lag(sample)=>assert!(sample.record.is_none()),
            maho_rpc::multi_session_host::HostMonitorSample::Memory(sample)=>{
                assert_eq!(sample.pressure_change,Some(true));
                assert_eq!(sample.critical_change,Some((true,21)));
                assert_eq!(sample.idle_pressure,None);
                records.push(sample.record.unwrap());stop.send(true).unwrap();
            }
        }
    },stopped).await;
    assert_eq!(records,vec![serde_json::json!({"type":"host_memory_pressure","rssMb":21,"sessions":1})]);
    assert_eq!(started.elapsed(),std::time::Duration::from_millis(30000));assert_eq!(blocked.lock().unwrap().loop_blocked_mark(),0.);
}

#[tokio::test(start_paused=true)]
async fn host_monitors_publish_pressure_recovery_even_without_a_wire_record(){
    use maho_rpc::multi_session_host::HostMonitorSample;
    let env=std::collections::HashMap::from([("SENPI_RPC_HOST_RSS_WARN_MB".into(),"10".into())]);
    let activity=maho_rpc::session_attribution::SessionActivityRegistry::default();
    let blocked=std::sync::Mutex::new(maho_rpc::loop_blocked_time::LoopBlockedTime::default());
    let(stop,stopped)=tokio::sync::watch::channel(false);
    let started=tokio::time::Instant::now();let mut reads=0;let mut samples=Vec::new();
    maho_rpc::multi_session_host::run_host_monitors(&env,&activity,&blocked,||{
        reads+=1;
        (if reads==1{21*1048576}else{0},0,started.elapsed().as_millis() as u64)
    },||started.elapsed().as_secs_f64()*1000.,|sample|{
        if let HostMonitorSample::Memory(sample)=sample{
            samples.push(sample);
            if samples.len()==2{stop.send(true).unwrap();}
        }
    },stopped).await;
    assert_eq!(samples.len(),2);
    assert_eq!(samples[0].idle_pressure,Some(21));
    assert_eq!(samples[1].pressure_change,Some(false));
    assert_eq!(samples[1].critical_change,Some((false,0)));
    assert!(samples[1].record.is_none());
    assert_eq!(started.elapsed(),std::time::Duration::from_millis(60000));
}
