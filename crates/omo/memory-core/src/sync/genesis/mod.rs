//! Genesis keyring builder: native-prepared bootstrap, validation and the signed first append.

pub mod build;
pub mod prepare;
pub mod types;
pub mod validate;

#[cfg(test)]
mod vector_tests;

pub use build::build_genesis;
pub use prepare::prepare_genesis;
pub use types::{
    GenesisAppend, GenesisBody, GenesisBootstrap, GenesisBuildError, GenesisCapabilities,
    GenesisCountersignPayload, GenesisPrepared,
};
pub use validate::{validate_bootstrap, validate_prepared};
