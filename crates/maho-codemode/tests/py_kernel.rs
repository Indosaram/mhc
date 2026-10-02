use maho_codemode::{bridge::protocol::BridgeConnectionConfig,kernels::py::{kernel::PythonKernel,kernel_contract::*}};
use std::time::Duration;

fn options()->PythonKernelStartOptions {
    PythonKernelStartOptions{interpreter_path:"python3".into(),session_id:"fifo".into(),cwd:env!("CARGO_MANIFEST_DIR").into(),connection:BridgeConnectionConfig{port:1,token:"test".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:None},env:None,session_env:None,startup_timeout:None,on_message:None}
}
fn input(id:&str,code:&str)->PythonKernelRunOptions {
    PythonKernelRunOptions{cell_id:id.into(),code:code.into(),timeout_ms:Some(5000),on_started:None,on_message:None}
}

#[tokio::test]
async fn real_python_kernel_preserves_state_and_resets() {
    let kernel=PythonKernel::start(options()).await.unwrap();
    assert_eq!(kernel.run(input("first","value = 41\nvalue")).await.unwrap()["valueRepr"],"41");
    assert_eq!(kernel.run(input("second","value + 1")).await.unwrap()["valueRepr"],"42");
    kernel.reset().await.unwrap();
    assert_eq!(kernel.run(input("reset","value")).await.unwrap()["ok"],false);
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn queued_cancellation_does_not_interrupt_active_cell() {
    use std::{future::Future,sync::Arc,task::Poll};
    let kernel=PythonKernel::start(options()).await.unwrap();
    let (started_tx,started_rx)=tokio::sync::oneshot::channel();
    let started_tx=std::sync::Mutex::new(Some(started_tx));
    let mut busy=input("active","import sys,json\nsys.__stdout__.write(json.dumps({'type':'text','stream':'stdout','data':'started'})+'\\n')\nsys.__stdout__.flush()\nwhile True: pass");
    busy.timeout_ms=None;
    busy.on_message=Some(Arc::new(move |message|{if message["type"]=="text" && message["data"]=="started" && let Some(sender)=started_tx.lock().unwrap().take(){sender.send(()).unwrap();}}));
    let active=kernel.run(busy);tokio::pin!(active);
    assert!(matches!(std::future::poll_fn(|cx|Poll::Ready(active.as_mut().poll(cx))).await,Poll::Pending));
    tokio::time::timeout(Duration::from_secs(5),started_rx).await.unwrap().unwrap();
    let queued=kernel.run(input("queued","99"));tokio::pin!(queued);
    assert!(matches!(std::future::poll_fn(|cx|Poll::Ready(queued.as_mut().poll(cx))).await,Poll::Pending));
    assert!(tokio::time::timeout(Duration::from_secs(5),kernel.cancel_queued("queued","cancelled queued")).await.unwrap());
    assert_eq!(queued.await.unwrap()["ok"],false);
    assert!(tokio::time::timeout(Duration::from_secs(7),kernel.interrupt("test",Some("active"))).await.unwrap().unwrap());
    assert_eq!(active.await.unwrap()["ok"],false);
    assert_eq!(kernel.run(input("after","1+1")).await.unwrap()["valueRepr"],"2");
    kernel.close().await.unwrap();
}
