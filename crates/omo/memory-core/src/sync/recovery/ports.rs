use super::RecoveryError;
use crate::sync::crypto::{
    entry::{KeyringBody, VerifiedWrap, WrapEntry},
    parse::{Principal, RecoveryKey},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, future::Future, pin::Pin};

pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, RecoveryError>> + 'a>>;

#[derive(Clone, Copy)]
pub struct PasskeyFlags {
    pub user_verified: bool,
    pub backup_eligible: bool,
    pub backup_state: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Attachment {
    Platform,
    CrossPlatform,
}
pub struct PasskeyCreation {
    pub credential_id: String,
    pub attachment: Attachment,
    pub flags: PasskeyFlags,
    pub prf_output: Option<zeroize::Zeroizing<Vec<u8>>>,
}
pub struct PasskeyAssertion {
    pub credential_id: String,
    pub user_handle: Vec<u8>,
    pub flags: PasskeyFlags,
    pub prf_output: Option<zeroize::Zeroizing<Vec<u8>>>,
}
pub struct DiscoveredCredential {
    pub credential_id: String,
    pub user_handle: Vec<u8>,
}
pub struct CreateRequest {
    pub rp_id: String,
    pub user_handle: Vec<u8>,
    pub user_name: String,
    pub challenge: Vec<u8>,
    pub prf_salt: Vec<u8>,
    pub exclude_credential_ids: Vec<String>,
    pub roaming: bool,
}
pub struct AssertRequest {
    pub rp_id: String,
    pub challenge: Vec<u8>,
    pub prf_salts: BTreeMap<String, Vec<u8>>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformSignal {
    Signalled,
    Unsupported,
}
pub trait PasskeyAuthenticator: Send + Sync {
    fn prf_supported(&self) -> bool;
    fn create(&self, request: CreateRequest) -> PortFuture<'_, Option<PasskeyCreation>>;
    fn assert(&self, request: AssertRequest) -> PortFuture<'_, Option<PasskeyAssertion>>;
    fn discover<'a>(&'a self, rp_id: &'a str) -> PortFuture<'a, Option<Vec<DiscoveredCredential>>>;
    fn signal_removed<'a>(
        &'a self,
        rp_id: &'a str,
        credential_id: &'a str,
    ) -> PortFuture<'a, PlatformSignal>;
}

pub struct EnrollBridgeRequest {
    pub account: String,
    pub rp_id: String,
    pub user_name: String,
    pub roaming: bool,
    pub used_credential_ids: Vec<String>,
    pub custody_root: String,
    pub predecessor: String,
    pub epoch: u64,
    pub device_wrap: VerifiedWrap,
    pub expected_history: BTreeMap<u64, String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnrollRefusal {
    Cancelled,
    NoPrf,
    NoUserVerification,
    NotSurvivable,
    CredentialReused,
    DeviceWrapRefused,
}
pub struct EnrolledKey {
    pub key: RecoveryKey,
    pub countersign: String,
    pub wrap: WrapEntry,
    pub wraps_digest: String,
}
pub type EnrollBridgeResult = Result<EnrolledKey, EnrollRefusal>;

#[derive(Clone)]
pub struct Countersign {
    pub custody_root: String,
    pub predecessor: String,
    pub wraps_digest: String,
    pub signature: String,
}
#[derive(Clone)]
pub struct Enrollment {
    pub wraps: Vec<WrapEntry>,
    pub expected_history: BTreeMap<u64, String>,
}
#[derive(Clone)]
pub struct RecoveryCandidate {
    pub key: RecoveryKey,
    pub countersign: Countersign,
    pub wrap: WrapEntry,
    pub expected_history: BTreeMap<u64, String>,
    pub enrollment: Enrollment,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverySaltInput {
    pub credential_id: String,
    pub enrollment_nonce: String,
}
pub struct RecoverBeginRequest {
    pub account: String,
    pub rp_id: String,
    pub salts: Vec<RecoverySaltInput>,
    pub known_credential_ids: Vec<String>,
    pub acknowledge_unknown: bool,
}
pub struct BegunRecovery {
    pub handle: String,
    pub credential_id: String,
    pub sealing_public_key: String,
    pub signing_public_key: String,
}
pub type RecoverBeginResult = Result<BegunRecovery, RecoverRefusal>;
pub struct JoinBase {
    pub epoch: u64,
    pub keyring_head: String,
    pub base_seq: u64,
    pub prior_link: String,
}
pub struct RecoverFinishRequest {
    pub handle: String,
    pub account: String,
    pub candidate: RecoveryCandidate,
    pub device: Principal,
    pub join: JoinBase,
}
pub const RECOVER_AGAIN: &str = "start recovery again";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoverRefusal {
    Cancelled,
    NoPrf,
    NoUserVerification,
    NotEnrolled,
    HistoryIncomplete,
    KeyMismatch,
    CountersignMismatch,
    WrapRefused,
    WrapUnbound,
    RecoveryExpired,
}
pub struct SignedJoin {
    pub body: KeyringBody,
    pub digest: String,
    pub link: String,
    pub keyring_link: String,
    pub record_sig: String,
}
pub struct RecoveredKey {
    pub credential_id: String,
    pub device_wrap: WrapEntry,
    pub join: SignedJoin,
}
pub type RecoverBridgeResult = Result<RecoveredKey, RecoverRefusal>;
pub trait RecoveryBridge: Send + Sync {
    fn prf_supported(&self) -> bool;
    fn enroll(&self, request: EnrollBridgeRequest) -> PortFuture<'_, EnrollBridgeResult>;
    fn recover_begin(&self, request: RecoverBeginRequest) -> PortFuture<'_, RecoverBeginResult>;
    fn recover_finish(&self, request: RecoverFinishRequest) -> PortFuture<'_, RecoverBridgeResult>;
    // Synchronous disposal permits an RAII abort guard, including dropped futures and thrown exits.
    fn recover_abort(&self, handle: &str);
    fn signal_removed<'a>(
        &'a self,
        rp_id: &'a str,
        credential_id: &'a str,
    ) -> PortFuture<'a, PlatformSignal>;
}
#[derive(Clone, Serialize, Deserialize)]
pub struct RecoveryWrapRecord {
    pub seq: u64,
    pub digest: String,
    pub keyring_link: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct RecoveryWrapRow {
    pub record: RecoveryWrapRecord,
    pub entry: WrapEntry,
}
#[derive(Clone)]
pub struct KeyringAppendRequest {
    pub record: KeyringBody,
    pub base_seq: u64,
    pub link: String,
    pub keyring_link: String,
    pub record_sig: String,
    pub wraps: Vec<WrapEntry>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportRefusal {
    StaleBase,
    RateLimited,
    NotFound,
    Refused,
}
pub type AppendResult = Result<u64, TransportRefusal>;
pub trait RecoveryTransport: Send + Sync {
    // Keep hostile responses untyped until the receiver validates their exact wire schema.
    fn list_keys(&self) -> PortFuture<'_, Value>;
    fn list_entries(&self) -> PortFuture<'_, Value>;
    fn list_recovery_wraps(&self) -> PortFuture<'_, Value>;
    fn append_keys(&self, request: KeyringAppendRequest) -> PortFuture<'_, AppendResult>;
    fn recovery_join(&self, request: KeyringAppendRequest) -> PortFuture<'_, AppendResult>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RecoveryEvent {
    #[serde(rename = "sync.recovery.enrolled")]
    Enrolled,
    #[serde(rename = "sync.recovery.enroll_refused")]
    EnrollRefused,
    #[serde(rename = "sync.recovery.recovered")]
    Recovered,
    #[serde(rename = "sync.recovery.joined_read_only")]
    JoinedReadOnly,
    #[serde(rename = "sync.recovery.recover_refused")]
    RecoverRefused,
    #[serde(rename = "sync.recovery.removed")]
    Removed,
}
pub type EventSink<'a> = Option<&'a (dyn Fn(RecoveryEvent) + Send + Sync)>;
