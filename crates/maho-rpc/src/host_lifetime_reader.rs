//! Watching a lifetime descriptor for EOF (senpi `host-lifetime-reader.ts`).
//!
//! A FIFO is read through the runtime's reactor. Any other descriptor (macOS reopened stdin) is read on its OWN
//! detached OS thread, never the runtime's blocking pool, so a host that exits on its own is never held in a pool
//! join (the failure senpi documents for a thread-pool read).
use crate::host_lifetime_binding::LifetimeEvent;

/// Owns `file`: EOF sends `Gone(reason)`, a read error sends `Unavailable`. Dropping the receiver disarms it.
#[cfg(unix)]
pub fn watch_host_lifetime(
    file: std::fs::File,
    reason: &'static str,
) -> tokio::sync::oneshot::Receiver<LifetimeEvent> {
    use std::os::unix::fs::FileTypeExt;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let is_fifo = file
        .metadata()
        .is_ok_and(|metadata| metadata.file_type().is_fifo());
    if is_fifo && tokio::runtime::Handle::try_current().is_ok() {
        let Ok(mut pipe) = tokio::net::unix::pipe::Receiver::from_file(file) else {
            let _ = sender.send(LifetimeEvent::Unavailable);
            return receiver;
        };
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buffer = [0u8; 256];
            let event = loop {
                match pipe.read(&mut buffer).await {
                    Ok(0) => break LifetimeEvent::Gone(reason.into()),
                    Ok(_) => {}
                    Err(_) => break LifetimeEvent::Unavailable,
                }
            };
            let _ = sender.send(event);
        });
        return receiver;
    }
    let mut file = file;
    // A thread that cannot be spawned drops the sender: the receiver resolves closed, read as unavailable.
    let _ = std::thread::Builder::new()
        .name("rpc-host-lifetime".into())
        .spawn(move || {
            use std::io::Read;
            let mut buffer = [0u8; 256];
            let event = loop {
                match file.read(&mut buffer) {
                    Ok(0) => break LifetimeEvent::Gone(reason.into()),
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break LifetimeEvent::Unavailable,
                }
            };
            let _ = sender.send(event);
        });
    receiver
}
