//! Drain-time device grant admission gate (`intake-admission.ts`).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use super::types::DeliveryRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntakeAdmission {
    Admitted,
    Refused,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntakeVerdict {
    Admit,
    Refuse,
    Skip,
}

pub type AdmitIntakeFn =
    Box<dyn Fn(&str) -> Pin<Box<dyn Future<Output = IntakeAdmission> + Send>> + Send + Sync>;

/// Per-drain-pass cache of sender-device grant verdicts (`#13037` 5.6, A2.2).
/// Local rows (`ledger_id` is `None`) are always admitted; intake rows require a positive answer.
pub struct IntakeAdmissionPass {
    admit: Option<AdmitIntakeFn>,
    answers: HashMap<String, IntakeAdmission>,
}

impl IntakeAdmissionPass {
    pub fn new(admit: Option<AdmitIntakeFn>) -> Self {
        Self {
            admit,
            answers: HashMap::new(),
        }
    }

    pub async fn check(&mut self, row: &DeliveryRow) -> IntakeVerdict {
        if row.ledger_id.is_none() {
            return IntakeVerdict::Admit;
        }
        let Some(device) = row
            .envelope
            .provenance
            .as_ref()
            .and_then(|p| p.device.as_deref())
        else {
            return IntakeVerdict::Skip;
        };
        let Some(admit) = &self.admit else {
            return IntakeVerdict::Skip;
        };
        let answer = if let Some(cached) = self.answers.get(device) {
            *cached
        } else {
            let verdict = admit(device).await;
            self.answers.insert(device.to_owned(), verdict);
            verdict
        };
        match answer {
            IntakeAdmission::Admitted => IntakeVerdict::Admit,
            IntakeAdmission::Refused => IntakeVerdict::Refuse,
            IntakeAdmission::Unknown => IntakeVerdict::Skip,
        }
    }
}
