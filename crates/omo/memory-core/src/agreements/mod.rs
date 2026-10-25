//! Working-agreement ledger: per-person `system/agreements.md` with violation counts.

pub mod events;
pub mod format;
pub mod guard;
pub mod ledger_file;
pub mod mutate;
pub mod notice_watermark;
pub mod record;
pub mod render;
pub mod types;

#[cfg(test)]
mod replay_tests;

pub use events::{
    agreement_event_key, legacy_agreement_event_key, legacy_agreement_events,
    parse_agreement_event_line, serialize_agreement_events,
};
pub use format::{
    AgreementsLedgerParse, agreement_id, normalize_agreement_text, parse_agreements_ledger,
    serialize_agreements_ledger, sort_agreements,
};
pub use guard::{describe_agreements_tool_violation, read_agreements_body};
pub use ledger_file::{
    agreement_items_of, known_agreements_of, read_agreements_at, read_agreements_worktree,
    render_agreements_file, render_agreements_file_with_receipts,
};
pub use mutate::{
    AgreementEvent, AgreementsMutation, ack_agreement, add_human_agreement,
    apply_agreement_records, apply_agreement_records_with_receipts, drop_agreement,
};
pub use notice_watermark::{
    AGREEMENTS_NOTICE_WATERMARK_FILENAME, AgreementsNoticeError, ConsumeAgreementsNoticeOptions,
    consume_agreements_notice_delta,
};
pub use record::{
    AgreementRecordValidationError, agreement_text_problem, clean_agreement_text,
    parse_agreement_record, parse_agreement_records_jsonl, validate_agreement_source,
};
pub use render::render_agreements_section;
pub use types::{
    AGREEMENT_TEXT_MAX_CHARS, AGREEMENTS_DESCRIPTION, AGREEMENTS_PATH, AgreementAuthor,
    AgreementExtractionRecord, AgreementItem, AgreementScope, AgreementSourceEvent,
    AgreementStatus, AgreementsPolicy, DEFAULT_AGREEMENTS_POLICY, FactsKnownAgreement,
};
