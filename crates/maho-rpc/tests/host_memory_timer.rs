#[tokio::test(start_paused=true)]
async fn memory_timer_samples_at_the_pinned_interval_and_stops_on_signal(){
    let mut sampler=maho_rpc::host_memory_sampler::HostMemorySampler::new(&std::collections::HashMap::from([("SENPI_RPC_HOST_RSS_WARN_MB".into(),"10".into())]));
    let(stop,stopped)=tokio::sync::watch::channel(false);let start=tokio::time::Instant::now();let mut records=Vec::new();
    sampler.run_until_stopped(||(21*1048576,1,start.elapsed().as_millis() as u64),|sample|{records.push(sample.record.unwrap());stop.send(true).unwrap();},stopped).await;
    assert_eq!(start.elapsed(),std::time::Duration::from_millis(30000));
    assert_eq!(records,vec![serde_json::json!({"type":"host_memory_pressure","rssMb":21,"sessions":1})]);
}
