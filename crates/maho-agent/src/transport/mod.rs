//! Port of senpi `packages/agent/src/transport/`.

pub mod in_process_carrier;
pub mod local_endpoint;
pub mod reliable_transport;
pub mod testing;
#[allow(clippy::module_inception)]
pub mod transport;

pub use in_process_carrier::{
    CarrierLink, InProcessNetwork, InProcessNetworkOptions, LinkUnlisten, create_in_process_network,
};
pub use local_endpoint::{LocalEndpoint, endpoint_key, local_endpoint};
pub use reliable_transport::{
    DecodedReferencePacket, ReferencePacket, ReliableTransport, ReliableTransportOptions,
    create_reliable_transport, decode_reference_packet,
};
pub use transport::{
    CarrierConnection, CarrierMessage, CarrierSide, FrameHandler, Transport, TransportClock,
    TransportClosedError, TransportFrame, TransportSide, TransportTimerCancel,
};
