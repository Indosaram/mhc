use maho_rpc::host_memory_sampler::*;
use std::{collections::HashMap, time::Duration};

#[tokio::test(start_paused = true)]
async fn sampling_timer_publishes_pressure_after_one_period() {
    let mut sampler = HostMemorySampler::new(&HashMap::from([(HOST_RSS_WARN_MB_ENV.into(), "10".into())]));
    let (published, mut samples) = tokio::sync::mpsc::unbounded_channel();
    let start = tokio::time::Instant::now();
    let task = tokio::spawn(async move {
        sampler.run(|| (21 * 1048576, 2, HOST_MEMORY_SAMPLE_MS), |sample| { published.send(sample).unwrap(); }).await;
    });
    let sample = samples.recv().await.unwrap();
    assert_eq!(start.elapsed(), Duration::from_millis(HOST_MEMORY_SAMPLE_MS));
    assert_eq!(sample.pressure_change, Some(true));
    assert_eq!(sample.critical_change, Some((true, 21)));
    assert_eq!(sample.record.unwrap()["sessions"], 2);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}
