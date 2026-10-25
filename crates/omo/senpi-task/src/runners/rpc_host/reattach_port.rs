//! Bounded reattachment refuses late reopen (`rpc-host/reattach-port.ts`).
use super::handle_port::{HostSessionIdentity, HostSessionPort};
use std::sync::Arc;
pub struct ReattachPortInput {
    pub delays: Vec<std::time::Duration>,
    pub sleep: Arc<dyn Fn(std::time::Duration) + Send + Sync>,
    pub admit: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    pub ensure: Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>,
    pub open: Arc<
        dyn Fn(&str, &str) -> Result<(Arc<dyn HostSessionPort>, HostSessionIdentity), String>
            + Send
            + Sync,
    >,
    pub notice: Arc<dyn Fn(&str) + Send + Sync>,
    pub attach_only: bool,
}
pub fn create_reattach_port(
    input: &ReattachPortInput,
    socket: &str,
    path: &str,
    abandoned: &dyn Fn() -> bool,
) -> Result<Option<(Arc<dyn HostSessionPort>, HostSessionIdentity)>, String> {
    for delay in &input.delays {
        (input.sleep)(*delay);
        if abandoned() {
            return Ok(None);
        }
        let result = (|| {
            (input.admit)()?;
            let endpoint = (input.ensure)(socket)?;
            if abandoned() {
                return Ok(None);
            }
            (input.open)(&endpoint, path).map(Some)
        })();
        match result {
            Ok(result) => return Ok(result),
            Err(error) if error == "store_index_unavailable" => return Err(error),
            Err(error) if error == "host_incompatible" => {
                if abandoned() {
                    return Ok(None);
                }
                (input.notice)("host_incompatible");
                return Err(error);
            }
            Err(error) => eprintln!("host session reattach attempt failed: {error}"),
        }
    }
    if input.attach_only && !abandoned() {
        (input.notice)("own_host_unreachable");
        return Err("own_host_unreachable".into());
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[test]
    fn abandoned_after_ensure_never_opens_or_notifies() {
        let abandoned = Arc::new(AtomicBool::new(false));
        let mark = abandoned.clone();
        let input = ReattachPortInput {
            delays: vec![std::time::Duration::ZERO],
            sleep: Arc::new(|_| {}),
            admit: Arc::new(|| Ok(())),
            ensure: Arc::new(move |_| {
                mark.store(true, Ordering::Release);
                Ok("endpoint".into())
            }),
            open: Arc::new(|_, _| panic!("late reopen")),
            notice: Arc::new(|_| panic!("late notice")),
            attach_only: true,
        };
        assert!(
            create_reattach_port(&input, "endpoint", "path", &|| abandoned
                .load(Ordering::Acquire))
            .unwrap()
            .is_none()
        );
    }
}
