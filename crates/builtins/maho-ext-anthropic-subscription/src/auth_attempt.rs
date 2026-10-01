/// Owns the cleanup rule while a native attempt's stream is being consumed.
pub struct AttemptGuard<F:FnOnce()> {cleanup:Option<F>,retain_on_success:bool}
impl<F:FnOnce()> AttemptGuard<F> {
    pub fn retainable(discard:F)->Self {Self {cleanup:Some(discard),retain_on_success:true}}
    pub fn closing(close:F)->Self {Self {cleanup:Some(close),retain_on_success:false}}
    /// Call only after the stream reaches successful EOF, never on a result frame.
    pub fn complete(&mut self) {if self.retain_on_success {self.cleanup=None;}}
}
impl<F:FnOnce()> Drop for AttemptGuard<F> {fn drop(&mut self) {if let Some(cleanup)=self.cleanup.take() {cleanup();}}}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    #[test]
    fn only_successful_consumption_retains_attempt() {
        let calls=Arc::new(AtomicUsize::new(0));let callback=|| {let calls=calls.clone();move || {calls.fetch_add(1,Ordering::SeqCst);}};
        {let _abandoned=AttemptGuard::retainable(callback());}assert_eq!(calls.load(Ordering::SeqCst),1);
        {let mut retained=AttemptGuard::retainable(callback());retained.complete();}assert_eq!(calls.load(Ordering::SeqCst),1);
        {let mut closing=AttemptGuard::closing(callback());closing.complete();}assert_eq!(calls.load(Ordering::SeqCst),2);
    }
}
