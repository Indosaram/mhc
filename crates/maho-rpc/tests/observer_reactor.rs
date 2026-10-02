use maho_rpc::observer_link::ObserverLink;
use std::{cell::{Cell, RefCell}, collections::VecDeque, pin::Pin, time::Duration};

#[tokio::test(start_paused = true)]
async fn failed_retries_continue_until_replacement_socket_opens() {
    let start = tokio::time::Instant::now();
    let sockets = RefCell::new(VecDeque::from([Ok(1), Err("retry"), Err("retry"), Ok(2)]));
    let settled = Cell::new(false);
    let observed = RefCell::new(Vec::new());
    let mut link = ObserverLink::default();
    link.run(||std::future::ready(sockets.borrow_mut().pop_front().unwrap()), |socket| {
        observed.borrow_mut().push(socket);
        if socket == 2 { settled.set(true); }
        Box::pin(std::future::ready(())) as Pin<Box<dyn std::future::Future<Output=()>>>
    }, ||settled.get(), Duration::from_millis(100), ||start.elapsed().as_secs_f64()*1000.).await.unwrap();
    assert_eq!(*observed.borrow(), [1, 2]);
    assert_eq!(start.elapsed(), Duration::from_millis(300));
    assert!(sockets.borrow().is_empty());
}

#[tokio::test]
async fn initial_connection_failure_remains_the_callers_error() {
    let mut link = ObserverLink::default();
    let result = link.run(||std::future::ready(Err::<(),_>("initial")), |_|Box::pin(std::future::ready(())), ||false, Duration::from_secs(1), ||0.).await;
    assert_eq!(result, Err("initial"));
    assert!(!link.healthy());
}
