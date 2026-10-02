use std::{future::Future,path::{Path,PathBuf},time::Duration};
use notify::Watcher;

pub async fn wait_for_filesystem_state<T,F,Fut>(directory:&Path,mut probe:F,timeout_ms:u64,description:&str)->Result<T,String>
where F:FnMut()->Fut,Fut:Future<Output=Result<Option<T>,String>> {
    if let Some(value)=probe().await? {return Ok(value);}
    let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();
    let mut watcher=notify::recommended_watcher(move |event:notify::Result<notify::Event>| {let _=sender.send(event);}).map_err(|error|error.to_string())?;
    watcher.watch(directory,notify::RecursiveMode::NonRecursive).map_err(|error|error.to_string())?;
    let deadline=tokio::time::sleep(Duration::from_millis(timeout_ms)); tokio::pin!(deadline);
    let mut recheck=tokio::time::interval(Duration::from_millis(25));
    loop {
        let result=tokio::select! {
            _=&mut deadline=>return Err(format!("waited {timeout_ms}ms for {description}")),
            result=probe()=>result?,
        };
        if let Some(value)=result {return Ok(value);}
        tokio::select! {
            _=&mut deadline=>return Err(format!("waited {timeout_ms}ms for {description}")),
            _=recheck.tick()=>{},
            event=events.recv()=>{event.ok_or_else(||"filesystem watcher closed".to_owned())?.map_err(|error|error.to_string())?;},
        }
    }
}
pub fn create_test_clock(run_dir:&Path,initial:f64)->std::io::Result<PathBuf> {
    let clock=run_dir.join("clock-events"); std::fs::create_dir_all(&clock)?;
    std::fs::write(clock.join(format!("000000-{initial}")),"")?; Ok(clock)
}
pub fn advance_test_clock(clock:&Path,value:f64)->std::io::Result<()> {
    let sequence=std::fs::read_dir(clock)?.count();
    std::fs::write(clock.join(format!("{sequence:06}-{value}")),"")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock_sequence_can_move_backwards_and_preserves_initial_file() {
        let root=tempfile::tempdir().unwrap();let clock=create_test_clock(root.path(),100.0).unwrap();
        advance_test_clock(&clock,20.5).unwrap();
        assert!(clock.join("000000-100").exists()); assert!(clock.join("000001-20.5").exists());
        assert_eq!(super::super::supervisor_process_identity::read_injected_clock(&clock),20.5);
    }
    #[tokio::test]
    async fn initial_state_does_not_require_directory_or_watcher() {
        assert_eq!(wait_for_filesystem_state(Path::new("/missing-memory-directory"),||async{Ok(Some(7))},10,"ready").await.unwrap(),7);
    }
    #[tokio::test]
    async fn registration_race_is_closed_by_authoritative_probe() {
        let root=tempfile::tempdir().unwrap();let mut calls=0;
        let value=wait_for_filesystem_state(root.path(),||{calls+=1;let result=(calls==2).then_some(9);async move{Ok(result)}},100,"ready").await.unwrap();
        assert_eq!(value,9);assert_eq!(calls,2);
    }
    #[tokio::test(start_paused=true)]
    async fn deadline_reports_exact_description_and_budget() {
        let root=tempfile::tempdir().unwrap();
        assert_eq!(wait_for_filesystem_state::<(),_,_>(root.path(),||async{Ok(None)},50,"child exit").await.unwrap_err(),"waited 50ms for child exit");
    }
}
