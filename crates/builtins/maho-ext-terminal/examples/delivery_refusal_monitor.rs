//! F3-b/F3-b2 live QA: a monitor wake refused below the retry cap is delivered; one refused past the cap is dropped.
use maho_ext_api::MessageDeliveryReceipt;
use maho_ext_terminal::monitor_notify::{
    MonitorAdmission, MonitorInjection, MonitorNotifier, MonitorSendResult,
};
use maho_ext_terminal::monitor_registry::MonitorEvent;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const MONITOR_DELIVERY_ATTEMPTS: u32 = 3;

fn line(id: &str, text: &str) -> MonitorEvent {
    MonitorEvent::Line {
        id: id.to_owned(),
        description: "qa".to_owned(),
        line: text.to_owned(),
        human_input: false,
    }
}

type QaSend = Arc<dyn Fn(MonitorInjection) -> MonitorSendResult + Send + Sync + 'static>;

fn sender(refusals: usize) -> (QaSend, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let attempts = Arc::new(AtomicUsize::new(0));
    let accepted = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&attempts);
    let ok = Arc::clone(&accepted);
    let send: QaSend = Arc::new(move |injection: MonitorInjection| -> MonitorSendResult {
        let n = seen.fetch_add(1, Ordering::SeqCst);
        if n < refusals {
            return Err("injected refusal".to_owned());
        }
        ok.fetch_add(1, Ordering::SeqCst);
        Ok(MonitorAdmission {
            receipt: Some(MessageDeliveryReceipt::delivered()),
            pause_ids: injection.pause_ids,
        })
    });
    (send, attempts, accepted)
}

async fn wait_until(done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if done() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for the notifier");
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let settings = maho_ext_terminal::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
    let below_cap = (MONITOR_DELIVERY_ATTEMPTS - 1) as usize;
    let (send, attempts, accepted) = sender(below_cap);
    let notifier = MonitorNotifier::with_pacing(settings.clone(), || true, None, None, send, None);
    notifier
        .notify_event(line("qa-b", "survives refusals"))
        .expect("queue the wake");
    wait_until(|| accepted.load(Ordering::SeqCst) > 0).await;
    println!(
        "F3-b attempts={} accepted={}",
        attempts.load(Ordering::SeqCst),
        accepted.load(Ordering::SeqCst)
    );
    assert_eq!(attempts.load(Ordering::SeqCst), below_cap + 1);
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    println!("PASS F3-b: refusals below the cap still end in delivery");

    let (send, attempts, accepted) = sender(usize::MAX);
    let notifier = MonitorNotifier::with_pacing(settings, || true, None, None, send, None);
    notifier
        .notify_event(line("qa-b2", "dropped after the cap"))
        .expect("queue the wake");
    wait_until(|| attempts.load(Ordering::SeqCst) >= MONITOR_DELIVERY_ATTEMPTS as usize).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    println!(
        "F3-b2 attempts={} accepted={}",
        attempts.load(Ordering::SeqCst),
        accepted.load(Ordering::SeqCst)
    );
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        MONITOR_DELIVERY_ATTEMPTS as usize
    );
    assert_eq!(accepted.load(Ordering::SeqCst), 0);
    println!("PASS F3-b2: retries stop at the cap and the wake is not delivered later");
}
