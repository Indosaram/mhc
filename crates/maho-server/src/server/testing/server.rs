use crate::server::{Server,errors::ServerError,types::ServerHost};
use std::{sync::Arc,time::Duration};
#[derive(Default)]
pub struct TestServerOptions {pub host:Option<Arc<dyn ServerHost>>,pub server_id:Option<String>,pub max_frame_length:Option<u32>,pub handshake_timeout:Option<Duration>}
pub struct TestServer {pub server:Arc<Server>,pub host:Arc<dyn ServerHost>}
pub fn create_test_server(options:TestServerOptions)->Result<TestServer,ServerError> {
    let host=options.host.unwrap_or_else(||Arc::new(super::host::TestServerHost::default()));
    let server=Server::new(host.clone(),options.server_id.unwrap_or_else(||"00000000-0000-4000-8000-000000000001".into()),options.max_frame_length,options.handshake_timeout)?;
    Ok(TestServer {server,host})
}
