//! Port of senpi `packages/agent/src/transport/testing/`.

pub mod fault_cases;
pub mod fault_transport;
pub mod multi_cases;
pub mod transport_cases;
pub mod transport_harness;

pub use fault_cases::{FAULT_TRANSPORT_CASES, fault_transport_cases};
pub use fault_transport::{
    FAULT_PROFILE_NAMES, FaultProfileName, FaultStats, create_carrier_fault,
};
pub use multi_cases::{MULTI_CASES, multi_cases};
pub use transport_cases::{TRANSPORT_CASES, transport_cases};
pub use transport_harness::{
    CarrierFault, TestLink, TransportCaseAdapters, TransportConformanceAdapter, WireView,
    all_delivered, expect_exact_delivery, open_link, range,
};
