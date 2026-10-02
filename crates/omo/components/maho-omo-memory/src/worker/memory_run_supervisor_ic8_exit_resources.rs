use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::Duration};
use tokio::{io::AsyncReadExt, sync::oneshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitResourceCounts { pub exit_servers: usize, pub accepted_sockets: usize, pub child_exit_timeouts: usize }
#[derive(Default)]
struct Counts { servers: Arc<AtomicUsize>, sockets: Arc<AtomicUsize>, timeouts: Arc<AtomicUsize> }
struct CounterGuard(Arc<AtomicUsize>);
impl Drop for CounterGuard { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::SeqCst); } }
pub struct ExitServer {
    pub port: u16,
    pub child_exited: oneshot::Receiver<Result<(), String>>,
    pub exit_socket_accepted: oneshot::Receiver<()>,
}
struct TrackedServer { task: tokio::task::JoinHandle<()>, cleanup: Option<oneshot::Sender<()>>, connected: Arc<std::sync::atomic::AtomicBool>, joined: bool }
pub struct ExitResources { wait_ms: u64, counts: Arc<Counts>, servers: Vec<TrackedServer> }
impl ExitResources {
    pub fn new(wait_ms: u64) -> Self { Self { wait_ms, counts: Arc::new(Counts::default()), servers: vec![] } }
    pub fn counts(&self) -> ExitResourceCounts {
        ExitResourceCounts { exit_servers: self.counts.servers.load(Ordering::SeqCst), accepted_sockets: self.counts.sockets.load(Ordering::SeqCst), child_exit_timeouts: self.counts.timeouts.load(Ordering::SeqCst) }
    }
    pub fn has_connected_sockets(&self) -> bool { self.counts().accepted_sockets > 0 }
    pub async fn wait_bounded<T>(&self, signal: impl std::future::Future<Output = T>, timeout_ms: u64, description: &str) -> Result<T, String> {
        self.counts.timeouts.fetch_add(1, Ordering::SeqCst);
        let _timeout = CounterGuard(self.counts.timeouts.clone());
        tokio::time::timeout(Duration::from_millis(timeout_ms), signal).await.map_err(|_| format!("waited {timeout_ms}ms for {description}"))
    }
    pub async fn open_server(&mut self) -> Result<ExitServer, String> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.map_err(|error| error.to_string())?;
        let port = listener.local_addr().map_err(|error| error.to_string())?.port();
        let (accepted, exit_socket_accepted) = oneshot::channel();
        let (exited, child_exited) = oneshot::channel();
        let (cleanup, mut cleaned) = oneshot::channel();
        let counts = self.counts.clone();
        let wait_ms = self.wait_ms;
        counts.servers.fetch_add(1, Ordering::SeqCst);
        counts.timeouts.fetch_add(1, Ordering::SeqCst);
        let server_guard = CounterGuard(counts.servers.clone());
        let timeout_guard = CounterGuard(counts.timeouts.clone());
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_connected = connected.clone();
        let task = tokio::spawn(async move {
            let server = server_guard;
            let timeout = timeout_guard;
            let mut work = Box::pin(async {
                    let (mut socket, _) = listener.accept().await.map_err(|error| error.to_string())?;
                    drop(listener);
                    counts.sockets.fetch_add(1, Ordering::SeqCst);
                    task_connected.store(true, Ordering::SeqCst);
                    let _socket = CounterGuard(counts.sockets.clone());
                    if accepted.send(()).is_err() { return Err("accepted model socket receiver closed".into()); }
                    let mut buffer = [0_u8; 1024];
                    while socket.read(&mut buffer).await.map_err(|error| error.to_string())? > 0 {}
                    Ok(())
            });
            let deadline = tokio::time::sleep(Duration::from_millis(wait_ms));
            tokio::pin!(deadline);
            let result = tokio::select! {
                result = &mut work => Some(result),
                _ = &mut deadline => None,
                _ = &mut cleaned => Some(Err("exit resource cleaned up before model child exit".into())),
            };
            drop(timeout);
            if let Some(result) = result {
                drop(work);
                task_connected.store(false, Ordering::SeqCst);
                drop(server);
                if exited.send(result).is_err() { eprintln!("model child exit receiver closed"); }
                return;
            }
            if exited.send(Err(format!("waited {wait_ms}ms for model child exit"))).is_err() { eprintln!("model child exit receiver closed"); }
            tokio::select! {
                result = &mut work => if let Err(error) = result { eprintln!("model socket close failed: {error}"); },
                _ = &mut cleaned => {},
            }
            drop(work);
            task_connected.store(false, Ordering::SeqCst);
            drop(server);
        });
        self.servers.push(TrackedServer { task, cleanup: Some(cleanup), connected, joined: false });
        Ok(ExitServer { port, child_exited, exit_socket_accepted })
    }
    pub async fn cleanup(&mut self) -> Result<usize, String> {
        let mut forced = 0;
        let mut first_error = None;
        for server in &mut self.servers {
            if !server.connected.load(Ordering::SeqCst)
                && let Some(cleanup) = server.cleanup.take() && cleanup.send(()).is_err() && !server.task.is_finished() {
                first_error.get_or_insert_with(|| "exit server cleanup receiver closed".into());
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(5_000);
        let mut timed_out = false;
        for server in &mut self.servers {
            if server.joined { continue; }
            match tokio::time::timeout_at(deadline, &mut server.task).await {
                Ok(result) => {
                    server.joined = true;
                    if let Err(error) = result { first_error.get_or_insert_with(|| error.to_string()); }
                }
                Err(_) => { timed_out = true; break; }
            }
        }
        if timed_out {
            for server in &mut self.servers {
                forced += usize::from(server.connected.load(Ordering::SeqCst));
                if let Some(cleanup) = server.cleanup.take() && cleanup.send(()).is_err() && !server.task.is_finished() {
                    first_error.get_or_insert_with(|| "exit server cleanup receiver closed".into());
                }
            }
            let deadline = tokio::time::Instant::now() + Duration::from_millis(5_000);
            for server in &mut self.servers {
                if server.joined { continue; }
                match tokio::time::timeout_at(deadline, &mut server.task).await {
                    Ok(result) => {
                        server.joined = true;
                        if let Err(error) = result { first_error.get_or_insert_with(|| error.to_string()); }
                    }
                    Err(_) => {
                        server.task.abort();
                        if let Err(error) = (&mut server.task).await && !error.is_cancelled() { eprintln!("{error}"); }
                        server.joined = true;
                        first_error.get_or_insert_with(|| "waited 5000ms for forced model socket close".into());
                    }
                }
            }
        }
        self.servers.clear();
        first_error.map_or(Ok(forced), Err)
    }
}
impl Drop for ExitResources {
    fn drop(&mut self) { for server in &self.servers { server.task.abort(); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn exact_socket_close_signal_releases_every_resource() {
        let mut resources = ExitResources::new(1000);
        let server = resources.open_server().await.unwrap();
        let socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, server.port)).await.unwrap();
        server.exit_socket_accepted.await.unwrap();
        assert!(resources.has_connected_sockets());
        drop(socket);
        server.child_exited.await.unwrap().unwrap();
        assert_eq!(resources.cleanup().await.unwrap(), 0);
        assert_eq!(resources.counts(), ExitResourceCounts { exit_servers: 0, accepted_sockets: 0, child_exit_timeouts: 0 });
    }
    #[tokio::test(start_paused = true)]
    async fn bounded_signal_timeout_removes_tracked_deadline() {
        let resources = ExitResources::new(1000);
        assert_eq!(resources.wait_bounded(std::future::pending::<()>(), 50, "test signal").await.unwrap_err(), "waited 50ms for test signal");
        assert_eq!(resources.counts().child_exit_timeouts, 0);
    }
    #[tokio::test]
    async fn cleanup_settles_unconnected_server_without_waiting_for_child_timeout() {
        let mut resources = ExitResources::new(60_000);
        let server = resources.open_server().await.unwrap();
        assert_eq!(resources.cleanup().await.unwrap(), 0);
        assert_eq!(server.child_exited.await.unwrap().unwrap_err(), "exit resource cleaned up before model child exit");
        assert!(server.exit_socket_accepted.await.is_err());
        assert_eq!(resources.counts(), ExitResourceCounts { exit_servers: 0, accepted_sockets: 0, child_exit_timeouts: 0 });
    }
    #[tokio::test]
    async fn forced_cleanup_reports_connected_socket_and_settles_exit_signal() {
        let mut resources = ExitResources::new(60_000);
        let server = resources.open_server().await.unwrap();
        let socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, server.port)).await.unwrap();
        server.exit_socket_accepted.await.unwrap();
        tokio::time::pause();
        assert_eq!(resources.cleanup().await.unwrap(), 1);
        assert_eq!(server.child_exited.await.unwrap().unwrap_err(), "exit resource cleaned up before model child exit");
        assert_eq!(resources.counts(), ExitResourceCounts { exit_servers: 0, accepted_sockets: 0, child_exit_timeouts: 0 });
        drop(socket);
    }
    #[tokio::test]
    async fn cleanup_forces_all_connected_sockets_after_one_shared_deadline() {
        let mut resources = ExitResources::new(60_000);
        let first = resources.open_server().await.unwrap();
        let second = resources.open_server().await.unwrap();
        let first_socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, first.port)).await.unwrap();
        let second_socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, second.port)).await.unwrap();
        first.exit_socket_accepted.await.unwrap();
        second.exit_socket_accepted.await.unwrap();
        tokio::time::pause();
        let started = tokio::time::Instant::now();
        assert_eq!(resources.cleanup().await.unwrap(), 2);
        // Tokio's timer wheel rounds a non-aligned deadline to the next millisecond.
        assert!((Duration::from_millis(5_000)..=Duration::from_millis(5_001)).contains(&started.elapsed()));
        assert_eq!(first.child_exited.await.unwrap().unwrap_err(), "exit resource cleaned up before model child exit");
        assert_eq!(second.child_exited.await.unwrap().unwrap_err(), "exit resource cleaned up before model child exit");
        assert_eq!(resources.counts(), ExitResourceCounts { exit_servers: 0, accepted_sockets: 0, child_exit_timeouts: 0 });
        drop((first_socket, second_socket));
    }
    #[tokio::test]
    async fn child_exit_timeout_does_not_release_a_still_connected_socket() {
        let mut resources = ExitResources::new(50);
        let server = resources.open_server().await.unwrap();
        let socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, server.port)).await.unwrap();
        server.exit_socket_accepted.await.unwrap();
        tokio::time::pause();
        assert_eq!(server.child_exited.await.unwrap().unwrap_err(), "waited 50ms for model child exit");
        assert!(resources.has_connected_sockets());
        assert_eq!(resources.counts().child_exit_timeouts, 0);
        assert_eq!(resources.cleanup().await.unwrap(), 1);
        assert_eq!(resources.counts(), ExitResourceCounts { exit_servers: 0, accepted_sockets: 0, child_exit_timeouts: 0 });
        drop(socket);
    }
}
