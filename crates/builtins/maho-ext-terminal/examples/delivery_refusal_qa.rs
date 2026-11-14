//! F3-a live QA: a refused terminal exit notice is re-offered, then settles once delivered.
use maho_ext_api::MessageDeliveryReceipt;
use maho_ext_terminal::notify::{NotificationDelivery, TerminalNotifier};
use maho_ext_terminal::tools::context::TerminalExitView;

fn main() {
    let mut notifier = TerminalNotifier::default();
    let view = TerminalExitView::new(None, "done".to_owned());
    let mut offers: Vec<String> = Vec::new();
    let mut call = 0;
    for _ in 0..3 {
        call += 1;
        let mut sent: Option<String> = None;
        notifier.notify_completion(
            "qa-exit",
            Some(NotificationDelivery::Steer),
            &view,
            |text, _| {
                sent = Some(text);
                Ok(if call == 1 {
                    MessageDeliveryReceipt::failed("injected refusal")
                } else {
                    MessageDeliveryReceipt::delivered()
                })
            },
        );
        match sent {
            Some(text) => {
                offers.push(text);
                println!("call {call}: offered");
            }
            None => println!("call {call}: skipped (already settled)"),
        }
    }
    println!("offers={}", offers.len());
    assert_eq!(
        offers.len(),
        2,
        "the refused exit is offered again exactly once"
    );
    assert!(
        offers[0].contains("qa-exit"),
        "the re-offer is the same notice: {}",
        offers[0]
    );
    println!("PASS F3-a: refused notice re-offered, then settled once delivered");
}
