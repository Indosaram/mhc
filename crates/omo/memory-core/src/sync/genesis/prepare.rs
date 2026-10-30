use super::types::{
    GenesisBody, GenesisBootstrap, GenesisBuildError, GenesisCapabilities,
    GenesisCountersignPayload, GenesisPrepared,
};
use super::validate::{to_value, validate_bootstrap, validate_prepared};
use crate::sync::crypto::{entry::WrapEntry, signatures::*};
use crate::sync::recovery::key_material::WrapAddress;
use serde_json::json;

/// G1 preparation, without custody or entropy policy: the injected native capabilities own all
/// secret material. Device then recovery is the ASCII order of the two epoch-1 recipients.
pub fn prepare_genesis(
    input: &GenesisBootstrap,
    capabilities: &dyn GenesisCapabilities,
) -> Result<GenesisPrepared, GenesisBuildError> {
    let bootstrap = input.clone();
    validate_bootstrap(&bootstrap)?;
    let account = bootstrap.account.clone();
    let custody_root = tagged_hash(
        "omo-sync/v1/custody-root",
        &to_value(&bootstrap, GenesisBuildError::BadBootstrap)?,
    )
    .ok_or(GenesisBuildError::BadCustodyRoot)?;
    let device_id = format!("device:{}", bootstrap.first_device.id);
    let recovery_id = format!("recovery:{}", bootstrap.recovery_key.credential_id);
    let device = WrapAddress {
        account: &account,
        epoch: 1,
        id: &device_id,
    };
    let recovery = WrapAddress {
        account: &account,
        epoch: 1,
        id: &recovery_id,
    };
    let wraps = [
        WrapEntry {
            epoch: 1,
            id: device_id.clone(),
            wrap: capabilities.seal(&device, &bootstrap.first_device.sealing_public_key)?,
        },
        WrapEntry {
            epoch: 1,
            id: recovery_id.clone(),
            wrap: capabilities.seal(&recovery, &bootstrap.recovery_key.sealing_public_key)?,
        },
    ];
    // Genesis countersigns ALL wraps, not merely the recovery recipient's entry.
    let wraps_digest =
        sha256_b64(&canonical(&json!(wraps)).map_err(|_| GenesisBuildError::BadWrapSet)?);
    let countersign = capabilities.countersign(&GenesisCountersignPayload {
        t: "omo-sync/v1/recovery-countersign",
        account: account.clone(),
        custody_root: custody_root.clone(),
        predecessor: "",
        wraps_digest: wraps_digest.clone(),
    })?;
    let prepared = GenesisPrepared {
        body: GenesisBody {
            kind: "genesis".into(),
            account,
            epoch: 1,
            bootstrap,
            custody_root,
            countersign,
            wraps_digest,
        },
        wraps,
    };
    validate_prepared(&prepared)?;
    Ok(prepared)
}
