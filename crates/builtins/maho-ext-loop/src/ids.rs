use std::sync::{Arc,atomic::{AtomicU64,Ordering}};
pub type LoopIdFactory=Arc<dyn Fn(&str)->String+Send+Sync>;
pub fn default_ids(now:Arc<dyn Fn()->f64+Send+Sync>)->LoopIdFactory {
    let sequence=AtomicU64::new(0);
    Arc::new(move |prefix|format!("{prefix}-{}-{}",base36(now() as u64),base36(sequence.fetch_add(1,Ordering::Relaxed)+1)))
}
fn base36(mut value:u64)->String {
    let mut digits=Vec::new();
    loop { digits.push(char::from(b"0123456789abcdefghijklmnopqrstuvwxyz"[(value%36) as usize])); value/=36; if value==0 { break; } }
    digits.into_iter().rev().collect()
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn identity_prefixes_share_one_sequence_with_base36_clock_and_counter() {
        let ids=default_ids(Arc::new(||1295.0));
        assert_eq!(ids("loop"),"loop-zz-1"); assert_eq!(ids("delivery"),"delivery-zz-2"); assert_eq!(ids("wakeup"),"wakeup-zz-3");
        for _ in 4..36 { ids("loop"); }
        assert_eq!(ids("loop"),"loop-zz-10");
    }
}
