//! Notice inputs the memory prompt consumes (pin `wiring-notice-inputs.ts`): the soul delta and the
//! needs-human agreements delta, each bound to the identity of the session that asks.
use std::sync::Arc;

use memory_core::agreements::{
    AgreementItem, ConsumeAgreementsNoticeOptions, consume_agreements_notice_delta_with_warn,
};
use memory_core::git::GitMemoryRepo;
use memory_core::soul::watermark::{
    ConsumeSoulNoticeOptions, SoulNotice, consume_soul_notice_delta_with_warn,
};

use crate::agreement_notice::{AGREEMENT_FLAGGED_ENTRY_TYPE, AgreementFlaggedRecord};
use crate::context::MemoryIdentityContext;

pub type NoticeResolveContext = Arc<dyn Fn(&str) -> Option<MemoryIdentityContext> + Send + Sync>;
/// The resolved memory settings (`memory` of the omo config).
pub type NoticeLoadSettings = Arc<dyn Fn() -> serde_json::Value + Send + Sync>;
pub type NoticeAppendEntry = Arc<dyn Fn(&str, &AgreementFlaggedRecord) + Send + Sync>;

/// Receives memory-core's non-fatal watermark warnings; the host wires its component logger here.
pub type NoticeWarn = Arc<dyn Fn(&str, Option<&str>) + Send + Sync>;

pub struct MemoryNoticeInputsOptions {
    pub resolve_context: NoticeResolveContext,
    pub load_settings: NoticeLoadSettings,
    pub append_entry: Option<NoticeAppendEntry>,
    pub on_warn: Option<NoticeWarn>,
}

pub struct MemoryNoticeInputs {
    options: MemoryNoticeInputsOptions,
}

pub fn create_memory_notice_inputs(options: MemoryNoticeInputsOptions) -> MemoryNoticeInputs {
    MemoryNoticeInputs { options }
}

impl MemoryNoticeInputs {
    fn warn(&self, message: &str, detail: Option<&str>) {
        if let Some(on_warn) = &self.options.on_warn {
            on_warn(message, detail);
        }
    }

    fn bound(&self, session_id: &str, identity: &str) -> Option<MemoryIdentityContext> {
        (self.options.resolve_context)(session_id).filter(|context| context.identity == identity)
    }

    pub fn resolve_soul_notice(
        &self,
        repo: &GitMemoryRepo,
        session_id: &str,
        identity: &str,
    ) -> Result<Option<SoulNotice>, String> {
        let Some(context) = self.bound(session_id, identity) else {
            return Ok(None);
        };
        consume_soul_notice_delta_with_warn(
            repo,
            &ConsumeSoulNoticeOptions {
                notices_dir: context.identity_paths.notices.clone(),
                locks_dir: context.identity_paths.locks.clone(),
                wait_timeout_ms: None,
            },
            &|message, detail| self.warn(message, detail),
        )
        .map_err(|error| error.to_string())
    }

    /// The needs-human items not announced yet, or `None` when there are none or the feature is off.
    pub fn resolve_agreements_notice(
        &self,
        repo: &GitMemoryRepo,
        session_id: &str,
        identity: &str,
    ) -> Result<Option<Vec<AgreementItem>>, String> {
        let Some(context) = self.bound(session_id, identity) else {
            return Ok(None);
        };
        if !agreements_enabled(&(self.options.load_settings)(), identity) {
            return Ok(None);
        }
        let flagged = consume_agreements_notice_delta_with_warn(
            repo,
            &ConsumeAgreementsNoticeOptions {
                notices_dir: context.identity_paths.notices.clone(),
                locks_dir: context.identity_paths.locks.clone(),
                wait_timeout_ms: None,
            },
            &|message, detail| self.warn(message, detail),
        )
        .map_err(|error| error.to_string())?;
        if let Some(append) = &self.options.append_entry {
            for item in &flagged {
                append(
                    AGREEMENT_FLAGGED_ENTRY_TYPE,
                    &AgreementFlaggedRecord {
                        id: item.id.clone(),
                        text: item.text.clone(),
                        n: item.n,
                        by: item.by.as_str().to_owned(),
                    },
                );
            }
        }
        Ok((!flagged.is_empty()).then_some(flagged))
    }
}

fn agreements_enabled(settings: &serde_json::Value, identity: &str) -> bool {
    settings["agents"][identity]["agreements"]["enabled"]
        .as_bool()
        .or_else(|| settings["agreements"]["enabled"].as_bool())
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_override_beats_the_global_flag() {
        let settings = serde_json::json!({"agreements": {"enabled": true}, "agents": {"a": {"agreements": {"enabled": false}}}});
        assert!(!agreements_enabled(&settings, "a"));
        assert!(agreements_enabled(&settings, "b"));
        assert!(!agreements_enabled(
            &serde_json::json!({"agreements": {"enabled": false}}),
            "b"
        ));
    }

    #[test]
    fn a_context_for_another_identity_resolves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(dir.path(), "agent");
        let context = MemoryIdentityContext::new(
            "agent".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "h".into(),
                bound_at: 0.0,
            },
        );
        let inputs = create_memory_notice_inputs(MemoryNoticeInputsOptions {
            resolve_context: Arc::new(move |_| Some(context.clone())),
            load_settings: Arc::new(|| serde_json::json!({})),
            append_entry: None,
            on_warn: None,
        });
        assert!(inputs.bound("s", "other").is_none());
        assert!(inputs.bound("s", "agent").is_some());
    }
}
