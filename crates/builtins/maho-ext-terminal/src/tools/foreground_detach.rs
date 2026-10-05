use maho_pty::PtyExit;
use maho_tools::definition::AbortSignal;
pub enum ForegroundOutcome {Exit(PtyExit),Aborted,TimedOut,Detached}
pub async fn foreground_outcome(mut exit:tokio::sync::watch::Receiver<Option<Result<PtyExit,String>>>,signal:&AbortSignal,delay:std::time::Duration,timeout_grace:Option<std::time::Duration>)->Result<ForegroundOutcome,String> {
    let deadline=async move {if delay==std::time::Duration::MAX {std::future::pending::<()>().await;} else {tokio::time::sleep(delay).await;}};tokio::pin!(deadline);
    let grace=async move {if let Some(grace)=timeout_grace {tokio::time::sleep(grace).await;} else {std::future::pending::<()>().await;}};tokio::pin!(grace);
    loop {
        if signal.is_aborted() {return Ok(ForegroundOutcome::Aborted);}
        if let Some(result)=exit.borrow_and_update().clone() {return result.map(ForegroundOutcome::Exit);}
        tokio::select! {biased;
            _=signal.cancelled()=>return Ok(ForegroundOutcome::Aborted),
            changed=exit.changed()=>{changed.map_err(|error|error.to_string())?;},
            _=&mut grace=>{
                tokio::task::yield_now().await;
                if signal.is_aborted() {return Ok(ForegroundOutcome::Aborted);}
                if let Some(result)=exit.borrow_and_update().clone() {return result.map(ForegroundOutcome::Exit);}
                return Ok(ForegroundOutcome::TimedOut);
            }
            _=&mut deadline=>{
                tokio::task::yield_now().await;
                if signal.is_aborted() {return Ok(ForegroundOutcome::Aborted);}
                if let Some(result)=exit.borrow_and_update().clone() {return result.map(ForegroundOutcome::Exit);}
                return Ok(ForegroundOutcome::Detached);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn already_settled_exit_and_abort_win_over_detach() {
        let (sender,exit)=tokio::sync::watch::channel(Some(Ok(PtyExit {exit_code:Some(0),cancelled:false,timed_out:false})));
        let signal=AbortSignal::default();assert!(matches!(foreground_outcome(exit.clone(),&signal,std::time::Duration::ZERO,None).await.unwrap(),ForegroundOutcome::Exit(_)));
        signal.abort();assert!(matches!(foreground_outcome(exit,&signal,std::time::Duration::ZERO,None).await.unwrap(),ForegroundOutcome::Aborted));drop(sender);
    }
    #[tokio::test]
    async fn live_session_commits_detach_at_deadline() {
        let (_sender,exit)=tokio::sync::watch::channel(None);
        assert!(matches!(foreground_outcome(exit,&AbortSignal::default(),std::time::Duration::ZERO,None).await.unwrap(),ForegroundOutcome::Detached));
    }
    #[tokio::test]
    async fn timeout_grace_releases_a_reader_that_never_settles() {
        let (_sender,exit)=tokio::sync::watch::channel(None);
        assert!(matches!(foreground_outcome(exit,&AbortSignal::default(),std::time::Duration::MAX,Some(std::time::Duration::ZERO)).await.unwrap(),ForegroundOutcome::TimedOut));
    }
    #[tokio::test]
    async fn settled_exit_and_abort_win_over_timeout_grace() {
        let (sender,exit)=tokio::sync::watch::channel(Some(Ok(PtyExit {exit_code:Some(0),cancelled:false,timed_out:false})));
        assert!(matches!(foreground_outcome(exit,&AbortSignal::default(),std::time::Duration::MAX,Some(std::time::Duration::ZERO)).await.unwrap(),ForegroundOutcome::Exit(_)));
        let (_sender,exit)=tokio::sync::watch::channel(None);let signal=AbortSignal::default();signal.abort();
        assert!(matches!(foreground_outcome(exit,&signal,std::time::Duration::MAX,Some(std::time::Duration::ZERO)).await.unwrap(),ForegroundOutcome::Aborted));drop(sender);
    }
}
