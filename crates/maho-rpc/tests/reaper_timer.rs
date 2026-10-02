use maho_rpc::{child_reaper::*, child_reaper_syscalls::ChildReaperSyscalls};

struct WaitableChild { reaped: bool }
impl ChildReaperSyscalls for WaitableChild {
    fn list_direct_children(&mut self) -> Vec<i32> { if self.reaped { vec![] } else { vec![4242] } }
    fn is_waitable(&mut self, _: i32) -> bool { true }
    fn reap_exited(&mut self, _: i32) -> bool { self.reaped = true; true }
    fn describe(&mut self, _: i32) -> String { "fixture".into() }
}

#[tokio::test(start_paused = true)]
async fn timer_reaps_only_after_the_full_waitable_window() {
    let mut reaper = ChildReaper::new(WaitableChild { reaped: false }, 5000);
    let start = tokio::time::Instant::now();
    let (published, mut logs) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        reaper.run(1000, || start.elapsed().as_millis() as u64, |message| { published.send(message).unwrap(); }).await;
    });
    let message = logs.recv().await.unwrap();
    assert_eq!(start.elapsed(), std::time::Duration::from_millis(6000));
    assert!(message.contains("reaped=1"));
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}
