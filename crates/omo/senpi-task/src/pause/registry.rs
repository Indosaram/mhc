//! Durable parent pause custody (`pause/registry.ts`).
use super::{
    admission_barrier::{AdmissionBarrier, AdmissionState, ResumeCredential},
    ports::RunRef,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantKind {
    DagNode,
    Task,
    WorkpoolWorker,
    TeamMember,
    HeldAdmission,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantState {
    Pending,
    Held,
    Resuming,
    Resumed,
    HeldByUser,
    Restorable,
    Lost,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionPhase {
    PreGrant,
    PostGrant,
    Parked,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipantAdmission {
    pub model: String,
    pub lane_key: String,
    pub sequence: u64,
    pub phase: AdmissionPhase,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PauseParticipant {
    pub key: String,
    pub kind: ParticipantKind,
    pub run_ref: RunRef,
    pub task_id: Option<String>,
    pub node_id: Option<String>,
    pub run_epoch: Option<u64>,
    pub admission: Option<ParticipantAdmission>,
    pub state: ParticipantState,
    pub ack_seq: Option<u64>,
    pub observed_child_seq: Option<u64>,
    pub resume_turn_id: Option<String>,
    pub lost_reason: Option<String>,
    pub at: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PausePhase {
    Propagating,
    Applied,
    Resuming,
    Resumed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeIntent {
    pub resume_id: String,
    pub child_seq_cutoffs: BTreeMap<String, u64>,
    #[serde(default)]
    pub installed: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PauseRegistryRecord {
    pub parent_session_id: String,
    pub pause_seq: u64,
    pub phase: PausePhase,
    pub barrier: AdmissionState,
    pub participants: BTreeMap<String, PauseParticipant>,
    pub resume: Option<ResumeIntent>,
}
#[derive(Debug, thiserror::Error)]
pub enum PauseError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Barrier(#[from] super::admission_barrier::AdmissionBarrierError),
}
impl PauseError {
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}
pub struct RestoreLedger {
    pub pause_seq: u64,
    pub superseded: bool,
    pub requested_resume_id: Option<String>,
}
#[derive(Clone)]
pub struct PauseRegistry {
    pub state_dir: PathBuf,
    pub barrier: AdmissionBarrier,
}
impl PauseRegistry {
    pub fn new(state_dir: impl Into<PathBuf>, barrier: AdmissionBarrier) -> Self {
        Self {
            state_dir: state_dir.into(),
            barrier,
        }
    }
    fn path(&self, id: &str) -> Result<PathBuf, PauseError> {
        if id.is_empty() || id == "." || id == ".." || id.contains(['\\', '/', '\0', ':']) {
            return Err(PauseError::message("invalid pause registry session id"));
        }
        Ok(self.state_dir.join("pause").join(format!("{id}.json")))
    }
    pub fn load(&self, id: &str) -> Result<Option<PauseRegistryRecord>, PauseError> {
        let text = match fs::read_to_string(self.path(id)?) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let record: PauseRegistryRecord = serde_json::from_str(&text)?;
        if record.parent_session_id != id
            || record.pause_seq == 0
            || record.pause_seq > 9_007_199_254_740_991
        {
            return Err(PauseError::message("invalid pause registry"));
        }
        Ok(Some(record))
    }
    fn write(&self, record: &PauseRegistryRecord) -> Result<(), PauseError> {
        use std::io::Write;
        let path = self.path(&record.parent_session_id)?;
        fs::create_dir_all(self.state_dir.join("pause"))?;
        let temporary = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec(record)?)?;
        fs::rename(temporary, path)?;
        Ok(())
    }
    pub fn begin(
        &self,
        id: &str,
        seq: u64,
        participants: Vec<PauseParticipant>,
    ) -> Result<PauseRegistryRecord, PauseError> {
        self.barrier.close(id, seq);
        if let Some(prior) = self.load(id)?
            && prior.pause_seq >= seq {
                return Ok(prior);
            }
        let record = PauseRegistryRecord {
            parent_session_id: id.into(),
            pause_seq: seq,
            phase: PausePhase::Propagating,
            barrier: AdmissionState::Closed { pause_seq: seq },
            participants: participants
                .into_iter()
                .map(|p| (p.key.clone(), p))
                .collect(),
            resume: None,
        };
        self.write(&record)?;
        Ok(record)
    }
    pub fn restore(
        &self,
        id: &str,
        ledger: RestoreLedger,
        rebuild: impl FnOnce() -> Vec<PauseParticipant>,
    ) -> Result<PauseRegistryRecord, PauseError> {
        let mut record = match self.load(id)? {
            Some(record) if record.pause_seq >= ledger.pause_seq => record,
            _ => {
                self.barrier.close(id, ledger.pause_seq);
                self.begin(id, ledger.pause_seq, rebuild())?
            }
        };
        self.barrier.restore(
            id,
            record.barrier.clone(),
            ledger.pause_seq,
            ledger.superseded,
        );
        if let Some(resume_id) = ledger.requested_resume_id.filter(|_| !ledger.superseded) {
            self.request_resume(id, ledger.pause_seq, &resume_id)?;
            record = self.require(id)?;
        }
        let state = self.barrier.state(id);
        if record.barrier != state {
            record.barrier = state;
            self.write(&record)?;
        }
        Ok(record)
    }
    pub fn require(&self, id: &str) -> Result<PauseRegistryRecord, PauseError> {
        self.load(id)?
            .ok_or_else(|| PauseError::message("parent has no pause registry"))
    }
    pub fn update(
        &self,
        id: &str,
        seq: u64,
        key: &str,
        patch: impl FnOnce(&mut PauseParticipant),
    ) -> Result<bool, PauseError> {
        let mut record = self.require(id)?;
        if record.pause_seq != seq {
            return Ok(false);
        }
        let Some(participant) = record.participants.get_mut(key) else {
            return Ok(false);
        };
        patch(participant);
        participant.key = key.into();
        self.write(&record)?;
        Ok(true)
    }
    pub fn add(
        &self,
        id: &str,
        seq: u64,
        participant: PauseParticipant,
    ) -> Result<bool, PauseError> {
        let mut record = self.require(id)?;
        if record.pause_seq != seq || record.phase == PausePhase::Resumed {
            return Ok(false);
        }
        record
            .participants
            .insert(participant.key.clone(), participant);
        self.write(&record)?;
        Ok(true)
    }
    pub fn phase(&self, id: &str, seq: u64, phase: PausePhase) -> Result<bool, PauseError> {
        let mut record = self.require(id)?;
        if record.pause_seq != seq {
            return Ok(false);
        }
        record.phase = phase;
        record.barrier = self.barrier.state(id);
        self.write(&record)?;
        Ok(true)
    }
    pub fn request_resume(
        &self,
        id: &str,
        seq: u64,
        resume_id: &str,
    ) -> Result<ResumeIntent, PauseError> {
        let mut record = self.require(id)?;
        if record.pause_seq != seq {
            return Err(PauseError::message("stale resume sequence"));
        }
        if let Some(resume) = &record.resume
            && resume.resume_id == resume_id {
                return Ok(resume.clone());
            }
        let resume = ResumeIntent {
            resume_id: resume_id.into(),
            child_seq_cutoffs: record
                .participants
                .values()
                .filter_map(|p| p.observed_child_seq.map(|seq| (p.key.clone(), seq)))
                .collect(),
            installed: false,
        };
        record.phase = PausePhase::Resuming;
        record.resume = Some(resume.clone());
        self.write(&record)?;
        Ok(resume)
    }
    pub fn mark_installed(&self, id: &str, seq: u64, resume_id: &str) -> Result<bool, PauseError> {
        let mut record = self.require(id)?;
        if record.pause_seq != seq
            || record
                .resume
                .as_ref()
                .is_none_or(|r| r.resume_id != resume_id)
        {
            return Ok(false);
        }
        if let Some(resume) = &mut record.resume {
            resume.installed = true;
        }
        self.write(&record)?;
        Ok(true)
    }
    pub fn authorize_resume(
        &self,
        id: &str,
        seq: u64,
        resume_id: &str,
    ) -> Result<ResumeCredential, PauseError> {
        let credential = self.barrier.resume(id, seq, resume_id)?;
        self.phase(id, seq, PausePhase::Resuming)?;
        Ok(credential)
    }
    pub fn drop(&self, id: &str, seq: u64, key: &str) -> Result<bool, PauseError> {
        let mut record = self.require(id)?;
        if record.pause_seq != seq {
            return Ok(false);
        }
        record.participants.remove(key);
        self.write(&record)?;
        Ok(true)
    }
    pub fn ordered_admissions(&self, id: &str) -> Result<Vec<PauseParticipant>, PauseError> {
        let mut entries: Vec<_> = self.require(id)?.participants.into_values().collect();
        entries.sort_by_key(|p| {
            (
                p.admission
                    .as_ref()
                    .is_none_or(|a| a.phase != AdmissionPhase::Parked),
                p.admission.as_ref().map_or(0, |a| a.sequence),
                p.key.clone(),
            )
        });
        Ok(entries)
    }
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }
}
