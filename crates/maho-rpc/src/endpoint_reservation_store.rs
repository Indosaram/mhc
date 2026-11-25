//! The endpoint reservation store of sisyphuslabs/omo-desktop-app#4199 (design 4.2a "Reservation
//! through startup", "Wrapper-safe discovery and one-time claim", "Crash, cancellation and commit"):
//! a private SQLite database beside the endpoint lock, `<endpoint-lock-target>.reservation.db`, with
//! one short `BEGIN IMMEDIATE` transaction per transition.
//!
//! Only an endpoint-lock holder reserves or retires a generation; verified participants claim their
//! role with a one-time 256-bit nonce through a compare-and-set on endpoint, generation, role, nonce
//! and state. Rows are never deleted: cancellation is a durable `cancelling` state that revokes
//! offers and keeps every claimed identity.

use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;
use std::path::Path;
use subtle::ConstantTimeEq;

pub const ENDPOINT_RESERVATION_SCHEMA: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReservationRole {
    Host,
    Supervisor,
}

impl ReservationRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Supervisor => "supervisor",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "host" => Some(Self::Host),
            "supervisor" => Some(Self::Supervisor),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationState {
    Pending,
    Cancelling,
    Serving,
}

impl ReservationState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Cancelling => "cancelling",
            Self::Serving => "serving",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "cancelling" => Some(Self::Cancelling),
            "serving" => Some(Self::Serving),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleStatus {
    Offered,
    Claimed,
    Granted,
    Released,
    Revoked,
}

impl RoleStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Offered => "offered",
            Self::Claimed => "claimed",
            Self::Granted => "granted",
            Self::Released => "released",
            Self::Revoked => "revoked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "offered" => Some(Self::Offered),
            "claimed" => Some(Self::Claimed),
            "granted" => Some(Self::Granted),
            "released" => Some(Self::Released),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationRejection {
    StoreInsecure,
    StoreBusy,
    SchemaUnsupported,
    GenerationUnknown,
    GenerationCancelled,
    GenerationNotPending,
    EndpointMismatch,
    RoleNotOffered,
    RoleAlreadyOffered,
    RoleNotExpected,
    RoleAlreadyClaimed,
    RoleNotClaimed,
    RoleNotGranted,
    NonceMismatch,
    ParticipantMismatch,
    IssuerMismatch,
    CommitmentInvalid,
    RolesNotGranted,
}

impl ReservationRejection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StoreInsecure => "store_insecure",
            Self::StoreBusy => "store_busy",
            Self::SchemaUnsupported => "schema_unsupported",
            Self::GenerationUnknown => "generation_unknown",
            Self::GenerationCancelled => "generation_cancelled",
            Self::GenerationNotPending => "generation_not_pending",
            Self::EndpointMismatch => "endpoint_mismatch",
            Self::RoleNotOffered => "role_not_offered",
            Self::RoleAlreadyOffered => "role_already_offered",
            Self::RoleNotExpected => "role_not_expected",
            Self::RoleAlreadyClaimed => "role_already_claimed",
            Self::RoleNotClaimed => "role_not_claimed",
            Self::RoleNotGranted => "role_not_granted",
            Self::NonceMismatch => "nonce_mismatch",
            Self::ParticipantMismatch => "participant_mismatch",
            Self::IssuerMismatch => "issuer_mismatch",
            Self::CommitmentInvalid => "commitment_invalid",
            Self::RolesNotGranted => "roles_not_granted",
        }
    }
}

/// A typed refusal; `reason` is machine-readable and the message never carries a nonce.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("endpoint reservation {}: {detail}", reason.as_str())]
pub struct EndpointReservationError {
    pub reason: ReservationRejection,
    pub detail: String,
}

impl EndpointReservationError {
    pub const CODE: &'static str = "endpoint-reservation-rejected";

    pub fn new(reason: ReservationRejection, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }
}

/// What a wrapper may forward: no nonce, no credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationLocator {
    pub endpoint: String,
    pub generation: String,
    pub role: ReservationRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationRow {
    pub endpoint: String,
    pub generation: String,
    pub state: ReservationState,
    pub revision: i64,
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRow {
    pub role: ReservationRole,
    pub status: RoleStatus,
    pub issuer: String,
    pub participant: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed,
    AlreadyClaimedByCaller,
}

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS reservations (
	generation TEXT PRIMARY KEY,
	endpoint TEXT NOT NULL,
	state TEXT NOT NULL,
	revision INTEGER NOT NULL,
	owner TEXT NOT NULL,
	expected_roles TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS roles (
	generation TEXT NOT NULL REFERENCES reservations(generation),
	role TEXT NOT NULL,
	nonce TEXT,
	issuer TEXT NOT NULL,
	status TEXT NOT NULL,
	participant TEXT,
	commitment TEXT,
	PRIMARY KEY (generation, role)
);";

fn is_sqlite_busy(error: &rusqlite::Error) -> bool {
    error.sqlite_error().is_some_and(|e| {
        matches!(
            e.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        )
    }) || {
        let lower = error.to_string().to_lowercase();
        lower.contains("database is locked")
            || lower.contains("database is busy")
            || lower.contains("database table is locked")
    }
}

fn map_sqlite_error(error: rusqlite::Error) -> EndpointReservationError {
    if is_sqlite_busy(&error) {
        EndpointReservationError::new(
            ReservationRejection::StoreBusy,
            "another process holds the reservation store",
        )
    } else {
        EndpointReservationError::new(ReservationRejection::StoreInsecure, error.to_string())
    }
}

fn precreate_private(path: &Path) -> Result<(u64, u64), EndpointReservationError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|err| {
        EndpointReservationError::new(
            ReservationRejection::StoreInsecure,
            format!("cannot open {}: {err}", path.display()),
        )
    })?;
    let info = file.metadata().map_err(|err| {
        EndpointReservationError::new(
            ReservationRejection::StoreInsecure,
            format!("cannot stat {}: {err}", path.display()),
        )
    })?;
    if !info.is_file() {
        return Err(EndpointReservationError::new(
            ReservationRejection::StoreInsecure,
            format!("not a regular file: {}", path.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if info.uid() != rustix::process::getuid().as_raw() {
            return Err(EndpointReservationError::new(
                ReservationRejection::StoreInsecure,
                format!("owned by another user: {}", path.display()),
            ));
        }
        if (info.mode() & 0o077) != 0 {
            return Err(EndpointReservationError::new(
                ReservationRejection::StoreInsecure,
                format!("readable or writable by others: {}", path.display()),
            ));
        }
        Ok((info.dev(), info.ino()))
    }
    #[cfg(not(unix))]
    {
        Ok((0, 0))
    }
}

pub fn open_endpoint_reservation_store(
    path: &Path,
) -> Result<EndpointReservationStore, EndpointReservationError> {
    let validated = precreate_private(path)?;
    let database = Connection::open(path).map_err(map_sqlite_error)?;
    let init = (|| -> Result<(), EndpointReservationError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = std::fs::symlink_metadata(path).map_err(|err| {
                EndpointReservationError::new(
                    ReservationRejection::StoreInsecure,
                    format!("cannot stat {}: {err}", path.display()),
                )
            })?;
            if !opened.is_file() || (opened.dev(), opened.ino()) != validated {
                return Err(EndpointReservationError::new(
                    ReservationRejection::StoreInsecure,
                    format!("replaced while opening: {}", path.display()),
                ));
            }
        }
        #[cfg(not(unix))]
        let _ = validated;

        database
            .execute_batch("PRAGMA journal_mode = DELETE; PRAGMA busy_timeout = 100;")
            .map_err(map_sqlite_error)?;
        let version: u32 = database
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(map_sqlite_error)?;
        if version == 0 {
            database
                .execute_batch(&format!(
                    "BEGIN IMMEDIATE; {SCHEMA_SQL} PRAGMA user_version = {ENDPOINT_RESERVATION_SCHEMA}; COMMIT;"
                ))
                .map_err(map_sqlite_error)?;
        } else if version != ENDPOINT_RESERVATION_SCHEMA {
            return Err(EndpointReservationError::new(
                ReservationRejection::SchemaUnsupported,
                format!("schema {version} in {}", path.display()),
            ));
        }
        Ok(())
    })();
    match init {
        Ok(()) => Ok(EndpointReservationStore { database }),
        Err(err) => Err(err),
    }
}

pub struct EndpointReservationStore {
    database: Connection,
}

impl EndpointReservationStore {
    pub fn close(self) {
        drop(self.database);
    }

    fn random_hex(bytes_len: usize) -> Result<String, EndpointReservationError> {
        let mut buf = vec![0u8; bytes_len];
        getrandom::fill(&mut buf).map_err(|err| {
            EndpointReservationError::new(
                ReservationRejection::StoreInsecure,
                format!("random generation failed: {err}"),
            )
        })?;
        Ok(hex::encode(buf))
    }

    /// Creates a pending generation that expects exactly `expected_roles`.
    pub fn reserve(
        &mut self,
        endpoint: &str,
        owner: &str,
        expected_roles: &[ReservationRole],
    ) -> Result<String, EndpointReservationError> {
        let mut roles: Vec<ReservationRole> = expected_roles.to_vec();
        roles.sort();
        roles.dedup();
        if roles.is_empty() {
            return Err(EndpointReservationError::new(
                ReservationRejection::RoleNotExpected,
                "a reservation expects no role",
            ));
        }
        let roles_csv: Vec<&'static str> = roles.iter().map(|r| r.as_str()).collect();
        let roles_str = roles_csv.join(",");
        let generation = Self::random_hex(16)?;
        self.transaction(|db| {
            db.execute(
                "INSERT INTO reservations (generation, endpoint, state, revision, owner, expected_roles) VALUES (?1, ?2, 'pending', 1, ?3, ?4)",
                params![generation, endpoint, owner, roles_str],
            )
            .map_err(map_sqlite_error)?;
            Ok(generation.clone())
        })
    }

    /// Offers `role` with a fresh nonce, issued by `issuer`, before the launcher spawns it.
    pub fn offer(
        &mut self,
        generation: &str,
        role: ReservationRole,
        issuer: &str,
    ) -> Result<(), EndpointReservationError> {
        let nonce = Self::random_hex(32)?;
        self.transaction(|db| {
            Self::pending(db, generation)?;
            let expected = Self::expected_roles(db, generation)?;
            if !expected.is_some_and(|roles| roles.contains(&role)) {
                return Err(EndpointReservationError::new(
                    ReservationRejection::RoleNotExpected,
                    format!("{} in {generation}", role.as_str()),
                ));
            }
            if Self::role_raw(db, generation, role)?.is_some() {
                return Err(EndpointReservationError::new(
                    ReservationRejection::RoleAlreadyOffered,
                    format!("{} in {generation}", role.as_str()),
                ));
            }
            db.execute(
                "INSERT INTO roles (generation, role, nonce, issuer, status) VALUES (?1, ?2, ?3, ?4, 'offered')",
                params![generation, role.as_str(), nonce, issuer],
            )
            .map_err(map_sqlite_error)?;
            Self::bump(db, generation)
        })
    }

    /// The claimant reads its role's nonce from the store itself, never from a wrapper or an inherited channel.
    pub fn read_offer(
        &self,
        locator: &ReservationLocator,
    ) -> Result<Vec<u8>, EndpointReservationError> {
        let reservation = Self::locate(&self.database, locator)?;
        if reservation.state == ReservationState::Cancelling {
            return Err(EndpointReservationError::new(
                ReservationRejection::GenerationCancelled,
                locator.generation.clone(),
            ));
        }
        let row = Self::role_raw(&self.database, &locator.generation, locator.role)?;
        let Some(row) = row else {
            return Err(EndpointReservationError::new(
                ReservationRejection::RoleNotOffered,
                format!("{} in {}", locator.role.as_str(), locator.generation),
            ));
        };
        if row.status != RoleStatus::Offered {
            return Err(EndpointReservationError::new(
                ReservationRejection::RoleNotOffered,
                format!("{} in {}", locator.role.as_str(), locator.generation),
            ));
        }
        let Some(nonce_hex) = row.nonce else {
            return Err(EndpointReservationError::new(
                ReservationRejection::RoleNotOffered,
                format!("{} in {}", locator.role.as_str(), locator.generation),
            ));
        };
        hex::decode(&nonce_hex).map_err(|_| {
            EndpointReservationError::new(
                ReservationRejection::NonceMismatch,
                format!("{} in {}", locator.role.as_str(), locator.generation),
            )
        })
    }

    /// Consumes the role's nonce once, recording `participant`.
    pub fn claim(
        &mut self,
        locator: &ReservationLocator,
        nonce: &[u8],
        participant: &str,
    ) -> Result<ClaimOutcome, EndpointReservationError> {
        self.transaction(|db| {
            let reservation = Self::locate(db, locator)?;
            if reservation.state == ReservationState::Cancelling {
                return Err(EndpointReservationError::new(
                    ReservationRejection::GenerationCancelled,
                    locator.generation.clone(),
                ));
            }
            if reservation.state != ReservationState::Pending {
                return Err(EndpointReservationError::new(
                    ReservationRejection::GenerationNotPending,
                    locator.generation.clone(),
                ));
            }
            let row = Self::role_raw(db, &locator.generation, locator.role)?;
            let Some(row) = row else {
                return Err(EndpointReservationError::new(
                    ReservationRejection::RoleNotOffered,
                    format!("{} in {}", locator.role.as_str(), locator.generation),
                ));
            };
            if row.status == RoleStatus::Revoked {
                return Err(EndpointReservationError::new(
                    ReservationRejection::RoleNotOffered,
                    format!("{} in {}", locator.role.as_str(), locator.generation),
                ));
            }
            if row.status != RoleStatus::Offered {
                if row.participant.as_deref() == Some(participant) {
                    return Ok(ClaimOutcome::AlreadyClaimedByCaller);
                }
                return Err(EndpointReservationError::new(
                    ReservationRejection::RoleAlreadyClaimed,
                    format!("{} in {}", locator.role.as_str(), locator.generation),
                ));
            }
            let expected_hex = row.nonce.as_deref().unwrap_or("");
            let expected = hex::decode(expected_hex).unwrap_or_default();
            if expected.len() != 32 || nonce.len() != 32 || !bool::from(expected.ct_eq(nonce)) {
                return Err(EndpointReservationError::new(
                    ReservationRejection::NonceMismatch,
                    format!("{} in {}", locator.role.as_str(), locator.generation),
                ));
            }
            let changed = db
                .execute(
                    "UPDATE roles SET status = 'claimed', nonce = NULL, participant = ?1 WHERE generation = ?2 AND role = ?3 AND status = 'offered' AND nonce = ?4",
                    params![participant, locator.generation, locator.role.as_str(), expected_hex],
                )
                .map_err(map_sqlite_error)?;
            if changed != 1 {
                return Err(EndpointReservationError::new(
                    ReservationRejection::RoleAlreadyClaimed,
                    format!("{} in {}", locator.role.as_str(), locator.generation),
                ));
            }
            Self::bump(db, &locator.generation)?;
            Ok(ClaimOutcome::Claimed)
        })
    }

    /// The issuer grants the recorded participant, bound to the SHA-256 commitment of the credential it established.
    pub fn grant(
        &mut self,
        generation: &str,
        role: ReservationRole,
        issuer: &str,
        participant: &str,
        commitment: &[u8],
    ) -> Result<(), EndpointReservationError> {
        self.transaction(|db| {
            Self::pending(db, generation)?;
            let row = Self::role_raw(db, generation, role)?;
            let Some(row) = row else {
                return Err(EndpointReservationError::new(
                    ReservationRejection::RoleNotClaimed,
                    format!("{} in {generation}", role.as_str()),
                ));
            };
            if row.status != RoleStatus::Claimed {
                return Err(EndpointReservationError::new(
                    ReservationRejection::RoleNotClaimed,
                    format!("{} in {generation}", role.as_str()),
                ));
            }
            if row.issuer != issuer {
                return Err(EndpointReservationError::new(
                    ReservationRejection::IssuerMismatch,
                    format!("{} in {generation}", role.as_str()),
                ));
            }
            if row.participant.as_deref() != Some(participant) {
                return Err(EndpointReservationError::new(
                    ReservationRejection::ParticipantMismatch,
                    format!("{} in {generation}", role.as_str()),
                ));
            }
            if commitment.len() != 32 {
                return Err(EndpointReservationError::new(
                    ReservationRejection::CommitmentInvalid,
                    format!("{} in {generation}", role.as_str()),
                ));
            }
            db.execute(
                "UPDATE roles SET status = 'granted', commitment = ?1 WHERE generation = ?2 AND role = ?3",
                params![hex::encode(commitment), generation, role.as_str()],
            )
            .map_err(map_sqlite_error)?;
            Self::bump(db, generation)
        })
    }

    /// The durable cancellation fence: revokes every outstanding offer and keeps claimed and granted identities.
    pub fn cancel(&mut self, generation: &str) -> Result<(), EndpointReservationError> {
        self.transaction(|db| {
            Self::reservation_raw(db, generation)?;
            db.execute(
                "UPDATE roles SET status = 'revoked', nonce = NULL WHERE generation = ?1 AND status = 'offered'",
                params![generation],
            )
            .map_err(map_sqlite_error)?;
            db.execute(
                "UPDATE reservations SET state = 'cancelling' WHERE generation = ?1",
                params![generation],
            )
            .map_err(map_sqlite_error)?;
            Self::bump(db, generation)
        })
    }

    /// Commits `serving` only when the generation's expected roles are known and non-empty, each is
    /// granted, and every offered row is granted.
    pub fn mark_serving(&mut self, generation: &str) -> Result<(), EndpointReservationError> {
        self.transaction(|db| {
            Self::pending(db, generation)?;
            let mut stmt = db
                .prepare("SELECT role, status FROM roles WHERE generation = ?1")
                .map_err(map_sqlite_error)?;
            let rows = stmt
                .query_map(params![generation], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(map_sqlite_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_sqlite_error)?;

            let mut granted = HashSet::new();
            for (role_str, status_str) in &rows {
                if status_str != "granted" {
                    return Err(EndpointReservationError::new(
                        ReservationRejection::RolesNotGranted,
                        generation,
                    ));
                }
                if let Some(role) = ReservationRole::parse(role_str) {
                    granted.insert(role);
                }
            }
            let Some(expected) = Self::expected_roles(db, generation)? else {
                return Err(EndpointReservationError::new(
                    ReservationRejection::RolesNotGranted,
                    generation,
                ));
            };
            if expected.iter().any(|role| !granted.contains(role)) {
                return Err(EndpointReservationError::new(
                    ReservationRejection::RolesNotGranted,
                    generation,
                ));
            }
            db.execute(
                "UPDATE reservations SET state = 'serving' WHERE generation = ?1",
                params![generation],
            )
            .map_err(map_sqlite_error)?;
            Self::bump(db, generation)
        })
    }

    /// A granted participant's own `released` commit: its authority ended.
    pub fn release(
        &mut self,
        generation: &str,
        role: ReservationRole,
        participant: &str,
    ) -> Result<(), EndpointReservationError> {
        self.transaction(|db| {
            let row = Self::role_raw(db, generation, role)?;
            let Some(row) = row else {
                return Err(EndpointReservationError::new(
                    ReservationRejection::RoleNotGranted,
                    role.as_str(),
                ));
            };
            if row.status != RoleStatus::Granted {
                return Err(EndpointReservationError::new(
                    ReservationRejection::RoleNotGranted,
                    role.as_str(),
                ));
            }
            if row.participant.as_deref() != Some(participant) {
                return Err(EndpointReservationError::new(
                    ReservationRejection::ParticipantMismatch,
                    role.as_str(),
                ));
            }
            db.execute(
                "UPDATE roles SET status = 'released' WHERE generation = ?1 AND role = ?2",
                params![generation, role.as_str()],
            )
            .map_err(map_sqlite_error)?;
            Self::bump(db, generation)
        })
    }

    pub fn reservation(
        &self,
        generation: &str,
    ) -> Result<ReservationRow, EndpointReservationError> {
        Self::reservation_raw(&self.database, generation)
    }

    pub fn role_row(
        &self,
        locator: &ReservationLocator,
    ) -> Result<Option<RoleRow>, EndpointReservationError> {
        let Some(row) = Self::role_raw(&self.database, &locator.generation, locator.role)? else {
            return Ok(None);
        };
        Ok(Some(RoleRow {
            role: locator.role,
            status: row.status,
            issuer: row.issuer,
            participant: row.participant,
        }))
    }

    fn expected_roles(
        db: &Connection,
        generation: &str,
    ) -> Result<Option<Vec<ReservationRole>>, EndpointReservationError> {
        let raw: Option<String> = db
            .query_row(
                "SELECT expected_roles FROM reservations WHERE generation = ?1",
                params![generation],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite_error)?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        if raw.is_empty() {
            return Ok(None);
        }
        let tokens: Vec<&str> = raw.split(',').collect();
        let roles: Vec<ReservationRole> = tokens
            .iter()
            .filter_map(|t| ReservationRole::parse(t))
            .collect();
        if !roles.is_empty() && roles.len() == tokens.len() {
            Ok(Some(roles))
        } else {
            Ok(None)
        }
    }

    fn transaction<T>(
        &mut self,
        run: impl FnOnce(&Connection) -> Result<T, EndpointReservationError>,
    ) -> Result<T, EndpointReservationError> {
        self.database
            .execute_batch("BEGIN IMMEDIATE;")
            .map_err(map_sqlite_error)?;
        match run(&self.database) {
            Ok(val) => {
                if let Err(err) = self.database.execute_batch("COMMIT;") {
                    let _ = self.database.execute_batch("ROLLBACK;");
                    return Err(map_sqlite_error(err));
                }
                Ok(val)
            }
            Err(err) => {
                let _ = self.database.execute_batch("ROLLBACK;");
                Err(err)
            }
        }
    }

    fn reservation_raw(
        db: &Connection,
        generation: &str,
    ) -> Result<ReservationRow, EndpointReservationError> {
        let row: Option<(String, String, String, i64, String)> = db
            .query_row(
                "SELECT endpoint, generation, state, revision, owner FROM reservations WHERE generation = ?1",
                params![generation],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .map_err(map_sqlite_error)?;
        let Some((endpoint, generation, state_str, revision, owner)) = row else {
            return Err(EndpointReservationError::new(
                ReservationRejection::GenerationUnknown,
                generation,
            ));
        };
        let state = ReservationState::parse(&state_str).ok_or_else(|| {
            EndpointReservationError::new(
                ReservationRejection::SchemaUnsupported,
                format!("unknown state {state_str}"),
            )
        })?;
        Ok(ReservationRow {
            endpoint,
            generation,
            state,
            revision,
            owner,
        })
    }

    fn locate(
        db: &Connection,
        locator: &ReservationLocator,
    ) -> Result<ReservationRow, EndpointReservationError> {
        let reservation = Self::reservation_raw(db, &locator.generation)?;
        if reservation.endpoint != locator.endpoint {
            return Err(EndpointReservationError::new(
                ReservationRejection::EndpointMismatch,
                locator.generation.clone(),
            ));
        }
        Ok(reservation)
    }

    fn pending(db: &Connection, generation: &str) -> Result<(), EndpointReservationError> {
        let row = Self::reservation_raw(db, generation)?;
        match row.state {
            ReservationState::Cancelling => Err(EndpointReservationError::new(
                ReservationRejection::GenerationCancelled,
                generation,
            )),
            ReservationState::Pending => Ok(()),
            ReservationState::Serving => Err(EndpointReservationError::new(
                ReservationRejection::GenerationNotPending,
                generation,
            )),
        }
    }

    fn role_raw(
        db: &Connection,
        generation: &str,
        role: ReservationRole,
    ) -> Result<Option<RawRoleRow>, EndpointReservationError> {
        let row: Option<(String, Option<String>, String, Option<String>)> = db
            .query_row(
                "SELECT status, nonce, issuer, participant FROM roles WHERE generation = ?1 AND role = ?2",
                params![generation, role.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(map_sqlite_error)?;
        let Some((status_str, nonce, issuer, participant)) = row else {
            return Ok(None);
        };
        let status = RoleStatus::parse(&status_str).ok_or_else(|| {
            EndpointReservationError::new(
                ReservationRejection::SchemaUnsupported,
                format!("unknown role status {status_str}"),
            )
        })?;
        Ok(Some(RawRoleRow {
            status,
            nonce,
            issuer,
            participant,
        }))
    }

    fn bump(db: &Connection, generation: &str) -> Result<(), EndpointReservationError> {
        db.execute(
            "UPDATE reservations SET revision = revision + 1 WHERE generation = ?1",
            params![generation],
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }
}

struct RawRoleRow {
    status: RoleStatus,
    nonce: Option<String>,
    issuer: String,
    participant: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "{\"pid\":100}";
    const SUPERVISOR: &str = "{\"pid\":200}";
    const INTRUDER: &str = "{\"pid\":201}";
    const ENDPOINT: &str = "/tmp/rpc.sock";
    const COMMITMENT: [u8; 32] = [0xab; 32];

    fn open_store(dir: &Path) -> (std::path::PathBuf, EndpointReservationStore) {
        let path = dir.join("endpoint.reservation.db");
        let store = open_endpoint_reservation_store(&path).expect("open store");
        (path, store)
    }

    #[cfg(unix)]
    #[test]
    fn creates_owner_only_and_refuses_group_readable_or_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("tempdir");
        let (path, store) = open_store(temp.path());
        assert_eq!(
            std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
            0o600
        );
        store.close();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");
        assert_eq!(
            open_endpoint_reservation_store(&path)
                .err()
                .map(|e| e.reason),
            Some(ReservationRejection::StoreInsecure)
        );

        let target = temp.path().join("elsewhere.db");
        std::fs::write(&target, "").expect("write");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let link = temp.path().join("link.db");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert_eq!(
            open_endpoint_reservation_store(&link)
                .err()
                .map(|e| e.reason),
            Some(ReservationRejection::StoreInsecure)
        );
    }

    #[test]
    fn reservation_lifecycle_claim_grant_mark_serving_and_release() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (path, mut store) = open_store(temp.path());
        let generation = store
            .reserve(
                ENDPOINT,
                OWNER,
                &[ReservationRole::Supervisor, ReservationRole::Host],
            )
            .expect("reserve");
        store
            .offer(&generation, ReservationRole::Supervisor, OWNER)
            .expect("offer supervisor");
        let loc = ReservationLocator {
            endpoint: ENDPOINT.into(),
            generation: generation.clone(),
            role: ReservationRole::Supervisor,
        };
        let nonce = store.read_offer(&loc).expect("read offer");
        assert_eq!(nonce.len(), 32);

        // Two racing claimants: first wins, second gets RoleAlreadyClaimed.
        let other = open_endpoint_reservation_store(&path).expect("second open");
        let other_nonce = other.read_offer(&loc).expect("read");
        assert_eq!(
            store.claim(&loc, &nonce, SUPERVISOR),
            Ok(ClaimOutcome::Claimed)
        );
        other.close();
        let mut third = open_endpoint_reservation_store(&path).expect("third open");
        assert_eq!(
            third
                .claim(&loc, &other_nonce, INTRUDER)
                .unwrap_err()
                .reason,
            ReservationRejection::RoleAlreadyClaimed
        );
        third.close();

        // Retry by same participant succeeds idempotently.
        assert_eq!(
            store.claim(&loc, &nonce, SUPERVISOR),
            Ok(ClaimOutcome::AlreadyClaimedByCaller)
        );

        // Serving fails until every expected role is granted.
        store
            .grant(
                &generation,
                ReservationRole::Supervisor,
                OWNER,
                SUPERVISOR,
                &COMMITMENT,
            )
            .expect("grant sup");
        assert_eq!(
            store.mark_serving(&generation).unwrap_err().reason,
            ReservationRejection::RolesNotGranted
        );

        store
            .offer(&generation, ReservationRole::Host, SUPERVISOR)
            .expect("offer host");
        let host_loc = ReservationLocator {
            endpoint: ENDPOINT.into(),
            generation: generation.clone(),
            role: ReservationRole::Host,
        };
        let host_nonce = store.read_offer(&host_loc).expect("host nonce");
        store
            .claim(&host_loc, &host_nonce, "host-proc")
            .expect("claim host");
        store
            .grant(
                &generation,
                ReservationRole::Host,
                SUPERVISOR,
                "host-proc",
                &COMMITMENT,
            )
            .expect("grant host");
        store.mark_serving(&generation).expect("mark serving");
        assert_eq!(
            store.reservation(&generation).expect("res").state,
            ReservationState::Serving
        );

        store
            .release(&generation, ReservationRole::Host, "host-proc")
            .expect("release");
        assert_eq!(
            store
                .role_row(&host_loc)
                .expect("row")
                .expect("some")
                .status,
            RoleStatus::Released
        );
    }

    #[test]
    fn cancellation_revokes_offers_and_rejects_claims() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (_, mut store) = open_store(temp.path());
        let generation = store
            .reserve(ENDPOINT, OWNER, &[ReservationRole::Supervisor])
            .expect("reserve");
        store
            .offer(&generation, ReservationRole::Supervisor, OWNER)
            .expect("offer");
        let loc = ReservationLocator {
            endpoint: ENDPOINT.into(),
            generation: generation.clone(),
            role: ReservationRole::Supervisor,
        };
        let nonce = store.read_offer(&loc).expect("nonce");
        store.cancel(&generation).expect("cancel");
        assert_eq!(
            store.read_offer(&loc).unwrap_err().reason,
            ReservationRejection::GenerationCancelled
        );
        assert_eq!(
            store.claim(&loc, &nonce, SUPERVISOR).unwrap_err().reason,
            ReservationRejection::GenerationCancelled
        );
        assert_eq!(
            store.role_row(&loc).expect("row").expect("some").status,
            RoleStatus::Revoked
        );
    }

    #[test]
    fn busy_writer_and_unsupported_schema_are_typed_rejections() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (path, mut store) = open_store(temp.path());
        let generation = store
            .reserve(ENDPOINT, OWNER, &[ReservationRole::Host])
            .expect("reserve");
        let reader = Connection::open(&path).expect("reader");
        reader
            .execute_batch("BEGIN; SELECT * FROM reservations;")
            .expect("begin");
        assert_eq!(
            store
                .offer(&generation, ReservationRole::Host, OWNER)
                .unwrap_err()
                .reason,
            ReservationRejection::StoreBusy
        );
        reader.execute_batch("COMMIT;").expect("commit");
        reader.close().expect("close");
        store.close();

        let raw = Connection::open(&path).expect("raw");
        raw.execute_batch(&format!(
            "PRAGMA user_version = {};",
            ENDPOINT_RESERVATION_SCHEMA + 1
        ))
        .expect("bump");
        raw.close().expect("close");
        assert_eq!(
            open_endpoint_reservation_store(&path)
                .err()
                .map(|e| e.reason),
            Some(ReservationRejection::SchemaUnsupported)
        );
    }
}
