//! Port of senpi packages/ai/src/auth/oauth/device-code.ts.

use crate::utils::abort::{AbortReason, AbortSignal};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::time::Instant;

const CANCEL_MESSAGE: &str = "Login cancelled";
const TIMEOUT_MESSAGE: &str = "Device flow timed out";
const SLOW_DOWN_TIMEOUT_MESSAGE: &str = "Device flow timed out after one or more slow_down responses. This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM clock and try again.";
const MINIMUM_INTERVAL_MS: u64 = 1000;
const DEFAULT_POLL_INTERVAL_SECONDS: f64 = 5.0;
const SLOW_DOWN_INTERVAL_INCREMENT_MS: u64 = 5000;

#[derive(Debug, Clone, PartialEq)]
pub enum OAuthDeviceCodePollResult<T> {
    Pending,
    SlowDown { interval_seconds: Option<f64> },
    Failed { message: String },
    Complete(T),
}

pub type DeviceCodePollFuture<T> = Pin<Box<dyn Future<Output = anyhow::Result<OAuthDeviceCodePollResult<T>>> + Send>>;

pub struct OAuthDeviceCodePollOptions<T> {
    pub interval_seconds: Option<f64>,
    pub expires_in_seconds: Option<f64>,
    pub wait_before_first_poll: bool,
    pub poll: Box<dyn FnMut() -> DeviceCodePollFuture<T> + Send>,
    pub signal: AbortSignal,
}

pub async fn abortable_sleep(ms: u64, signal: &AbortSignal, cancel_message: &str) -> anyhow::Result<()> {
    if signal.aborted() {
        anyhow::bail!("{cancel_message}");
    }
    tokio::select! {
        biased;
        () = signal.cancelled() => anyhow::bail!("{cancel_message}"),
        () = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
    }
}

fn milliseconds(seconds: f64) -> u64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return MINIMUM_INTERVAL_MS;
    }
    let ms = (seconds * 1000.0).floor();
    if ms >= u64::MAX as f64 { u64::MAX } else { ms as u64 }
}

fn remaining_ms(deadline: Option<Instant>) -> Option<u64> {
    let deadline = deadline?;
    let now = Instant::now();
    if now >= deadline {
        return Some(0);
    }
    Some((deadline - now).as_millis().min(u64::MAX as u128) as u64)
}

pub async fn poll_oauth_device_code_flow<T>(options: OAuthDeviceCodePollOptions<T>) -> anyhow::Result<T> {
    let OAuthDeviceCodePollOptions {
        interval_seconds,
        expires_in_seconds,
        wait_before_first_poll,
        mut poll,
        signal,
    } = options;

    let deadline = expires_in_seconds.map(|seconds| Instant::now() + Duration::from_millis(milliseconds(seconds)));
    let mut interval_ms = std::cmp::max(
        MINIMUM_INTERVAL_MS,
        ((interval_seconds.unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS)) * 1000.0).floor() as u64,
    );

    let mut slow_down_responses = 0usize;
    if wait_before_first_poll
        && let Some(remaining) = remaining_ms(deadline)
            && remaining > 0 {
                abortable_sleep(std::cmp::min(interval_ms, remaining), &signal, CANCEL_MESSAGE).await?;
            }

    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }
        if signal.aborted() {
            anyhow::bail!("{CANCEL_MESSAGE}");
        }

        let result = poll().await?;
        match result {
            OAuthDeviceCodePollResult::Complete(value) => return Ok(value),
            OAuthDeviceCodePollResult::Failed { message } => anyhow::bail!("{message}"),
            OAuthDeviceCodePollResult::SlowDown { interval_seconds } => {
                slow_down_responses += 1;
                interval_ms = match interval_seconds {
                    Some(seconds) if seconds.is_finite() && seconds > 0.0 => {
                        std::cmp::max(MINIMUM_INTERVAL_MS, (seconds * 1000.0).floor() as u64)
                    }
                    _ => std::cmp::max(MINIMUM_INTERVAL_MS, interval_ms + SLOW_DOWN_INTERVAL_INCREMENT_MS),
                };
            }
            OAuthDeviceCodePollResult::Pending => {}
        }

        let Some(remaining) = remaining_ms(deadline) else {
            abortable_sleep(interval_ms, &signal, CANCEL_MESSAGE).await?;
            continue;
        };
        if remaining == 0 {
            break;
        }
        abortable_sleep(std::cmp::min(interval_ms, remaining), &signal, CANCEL_MESSAGE).await?;
    }

    if slow_down_responses > 0 {
        anyhow::bail!("{SLOW_DOWN_TIMEOUT_MESSAGE}");
    }
    anyhow::bail!("{TIMEOUT_MESSAGE}");
}

#[allow(dead_code)]
fn abort_reason(signal: &AbortSignal) -> AbortReason {
    signal.reason().unwrap_or_else(AbortReason::dom_default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::abort::AbortController;
    use std::sync::{Arc, Mutex};
    use tokio::time::advance;

    fn never_aborted() -> AbortSignal {
        AbortController::new().signal()
    }

    async fn settle() {
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    fn recorder() -> (Arc<Mutex<Vec<Instant>>>, impl FnMut() -> DeviceCodePollFuture<String> + Send) {
        let times = Arc::new(Mutex::new(Vec::new()));
        let sink = times.clone();
        let poll = move || -> DeviceCodePollFuture<String> {
            sink.lock().unwrap_or_else(|p| p.into_inner()).push(Instant::now());
            Box::pin(async { Ok(OAuthDeviceCodePollResult::Complete("token".to_string())) })
        };
        (times, poll)
    }

    #[tokio::test(start_paused = true)]
    async fn polls_immediately_and_returns_the_completed_value() {
        let start = Instant::now();
        let times = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let sink = times.clone();
        let mut polls = 0usize;
        let handle = tokio::spawn(async move {
            poll_oauth_device_code_flow::<String>(OAuthDeviceCodePollOptions {
                interval_seconds: Some(2.0),
                expires_in_seconds: Some(30.0),
                wait_before_first_poll: false,
                signal: never_aborted(),
                poll: Box::new(move || {
                    polls += 1;
                    if polls == 1 {
                        sink.lock().unwrap_or_else(|p| p.into_inner()).push(Instant::now());
                        return Box::pin(async { Ok(OAuthDeviceCodePollResult::Pending) });
                    }
                    sink.lock().unwrap_or_else(|p| p.into_inner()).push(Instant::now());
                    Box::pin(async { Ok(OAuthDeviceCodePollResult::Complete("token".to_string())) })
                }),
            })
            .await
        });

        settle().await;
        assert_eq!(times.lock().unwrap_or_else(|p| p.into_inner()).len(), 1);
        advance(Duration::from_millis(1999)).await;
        settle().await;
        assert_eq!(times.lock().unwrap_or_else(|p| p.into_inner()).len(), 1);
        advance(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(handle.await.unwrap().unwrap(), "token");
        let times = times.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(times, vec![start, start + Duration::from_millis(2000)]);
    }

    #[tokio::test(start_paused = true)]
    async fn can_wait_before_the_first_poll() {
        let start = Instant::now();
        let (times, mut poll) = recorder();
        let handle = tokio::spawn(async move {
            poll_oauth_device_code_flow::<String>(OAuthDeviceCodePollOptions {
                interval_seconds: Some(2.0),
                expires_in_seconds: Some(30.0),
                wait_before_first_poll: true,
                signal: never_aborted(),
                poll: Box::new(move || poll()),
            })
            .await
        });

        settle().await;
        advance(Duration::from_millis(1999)).await;
        settle().await;
        assert!(times.lock().unwrap_or_else(|p| p.into_inner()).is_empty());
        advance(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(handle.await.unwrap().unwrap(), "token");
        assert_eq!(*times.lock().unwrap_or_else(|p| p.into_inner()), vec![start + Duration::from_millis(2000)]);
    }

    #[tokio::test(start_paused = true)]
    async fn increases_the_interval_by_five_seconds_after_slow_down_without_a_server_interval() {
        let start = Instant::now();
        let times = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let sink = times.clone();
        let mut polls = 0usize;
        let handle = tokio::spawn(async move {
            poll_oauth_device_code_flow::<String>(OAuthDeviceCodePollOptions {
                interval_seconds: Some(2.0),
                expires_in_seconds: Some(900.0),
                wait_before_first_poll: false,
                signal: never_aborted(),
                poll: Box::new(move || {
                    sink.lock().unwrap_or_else(|p| p.into_inner()).push(Instant::now());
                    polls += 1;
                    if polls == 1 {
                        Box::pin(async { Ok(OAuthDeviceCodePollResult::SlowDown { interval_seconds: None }) })
                    } else {
                        Box::pin(async { Ok(OAuthDeviceCodePollResult::Complete("token".to_string())) })
                    }
                }),
            })
            .await
        });

        settle().await;
        assert_eq!(*times.lock().unwrap_or_else(|p| p.into_inner()), vec![start]);
        advance(Duration::from_millis(6999)).await;
        settle().await;
        assert_eq!(*times.lock().unwrap_or_else(|p| p.into_inner()), vec![start]);
        advance(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(handle.await.unwrap().unwrap(), "token");
        assert_eq!(*times.lock().unwrap_or_else(|p| p.into_inner()), vec![start, start + Duration::from_millis(7000)]);
    }

    #[tokio::test(start_paused = true)]
    async fn honors_a_server_provided_slow_down_interval() {
        let start = Instant::now();
        let times = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let sink = times.clone();
        let mut polls = 0usize;
        let handle = tokio::spawn(async move {
            poll_oauth_device_code_flow::<String>(OAuthDeviceCodePollOptions {
                interval_seconds: Some(2.0),
                expires_in_seconds: Some(900.0),
                wait_before_first_poll: false,
                signal: never_aborted(),
                poll: Box::new(move || {
                    sink.lock().unwrap_or_else(|p| p.into_inner()).push(Instant::now());
                    polls += 1;
                    if polls == 1 {
                        Box::pin(async { Ok(OAuthDeviceCodePollResult::SlowDown { interval_seconds: Some(30.0) }) })
                    } else {
                        Box::pin(async { Ok(OAuthDeviceCodePollResult::Complete("token".to_string())) })
                    }
                }),
            })
            .await
        });

        settle().await;
        assert_eq!(*times.lock().unwrap_or_else(|p| p.into_inner()), vec![start]);
        advance(Duration::from_millis(29999)).await;
        settle().await;
        assert_eq!(*times.lock().unwrap_or_else(|p| p.into_inner()), vec![start]);
        advance(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(handle.await.unwrap().unwrap(), "token");
        assert_eq!(*times.lock().unwrap_or_else(|p| p.into_inner()), vec![start, start + Duration::from_millis(30000)]);
    }

    #[tokio::test(start_paused = true)]
    async fn reports_the_slow_down_timeout_message_when_the_deadline_passes_after_slow_down() {
        let error = poll_oauth_device_code_flow::<()>(OAuthDeviceCodePollOptions {
            interval_seconds: Some(1.0),
            expires_in_seconds: Some(2.0),
            wait_before_first_poll: false,
            poll: Box::new(|| Box::pin(async { Ok(OAuthDeviceCodePollResult::SlowDown { interval_seconds: None }) })),
            signal: never_aborted(),
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), SLOW_DOWN_TIMEOUT_MESSAGE);
    }

    #[tokio::test(start_paused = true)]
    async fn reports_the_plain_timeout_message_without_slow_down() {
        let error = poll_oauth_device_code_flow::<()>(OAuthDeviceCodePollOptions {
            interval_seconds: Some(1.0),
            expires_in_seconds: Some(2.0),
            wait_before_first_poll: false,
            poll: Box::new(|| Box::pin(async { Ok(OAuthDeviceCodePollResult::Pending) })),
            signal: never_aborted(),
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), TIMEOUT_MESSAGE);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_poll_result_is_raised_verbatim() {
        let error = poll_oauth_device_code_flow::<()>(OAuthDeviceCodePollOptions {
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(30.0),
            wait_before_first_poll: false,
            poll: Box::new(|| Box::pin(async { Ok(OAuthDeviceCodePollResult::<()>::Failed { message: "nope".into() }) })),
            signal: never_aborted(),
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "nope");
    }

    #[tokio::test(start_paused = true)]
    async fn cancels_an_in_flight_wait() {
        let controller = AbortController::new();
        let signal = controller.signal();
        let result = tokio::spawn(async move {
            poll_oauth_device_code_flow::<()>(OAuthDeviceCodePollOptions {
                interval_seconds: Some(5.0),
                expires_in_seconds: Some(30.0),
                wait_before_first_poll: false,
                poll: Box::new(|| Box::pin(async { Ok(OAuthDeviceCodePollResult::Pending) })),
                signal,
            })
            .await
        });
        tokio::task::yield_now().await;
        controller.abort(None);
        let error = result.await.unwrap().unwrap_err();
        assert_eq!(error.to_string(), CANCEL_MESSAGE);
    }
}
