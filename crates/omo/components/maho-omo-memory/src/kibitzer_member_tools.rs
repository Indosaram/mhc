//! The two memory-owned member-scoped Kibitzer tools: `memory` and `nudge`.
//!
//! `omo_mount.rs` composes a child's tool list as the three CLI host tools (`read`, `grep`,
//! `session_entries`) extended by [`kibitzer_member_tools`], which returns exactly these two, in
//! this order. Upstream `kibitzer/tools/index.ts` fixes `KIBITZER_SIDECAR_TOOL_NAMES` to
//! `read, grep, session_entries, memory, nudge` with no aliases and no builtin passthrough; the
//! first three are the CLI host's (`crates/maho-cli/src/cli/kibitzer_tools.rs`) and this module is
//! the memory-owned tail.
//!
//! Both halves this module binds already exist: the pure `memory` executor
//! ([`crate::kibitzer_tools_memory::execute_memory`]) and the live `nudge` wrapper
//! ([`crate::kibitzer_tools_nudge::create_kibitzer_sidecar_nudge_tool`]). This module only binds
//! them to ONE bound session's shared [`KibitzerSessionResources`] as registered
//! `ToolDefinition`s.
//!
//! # Charge ownership (exactly one charge per INVOKED call)
//!
//! Upstream wraps every sidecar closure in `budgeted(input.budget, ...)`
//! (`kibitzer/tools/result.ts`), and the native host validates a call against the tool's schema
//! BEFORE `execute` runs (`maho-agent` `prepare_agent_tool_call` -> `validate_tool_arguments`), so a
//! schema-invalid call never reaches a closure in either implementation. The
//! `read`/`grep`/`session_entries` ports charge inside their registered `ToolDefinition`, and
//! `memory` does the same here: its pure executor charges nothing, so the registry charges the wake
//! budget ONCE on entry. The `nudge` wrapper keeps its charge INSIDE the wrapper (upstream's
//! `budgeted` sits in the factory), so the registry MUST NOT charge an admitted `nudge` - a second
//! charge would double-count one call. The single exception is the defensive decode branch in
//! [`nudge_tool`]: the wrapper is never reached from it, so it charges itself and every INVOKED call
//! still consumes exactly one charge, never zero and never two.
//!
//! # The actual native result ABI (metadata, not flattening)
//!
//! `ToolResult` carries only `content` + `details`, and `wrap_tool_definition` builds the `Err` arm
//! as a bare text block with `details = null`. Two facts follow, and [`tool_result_of`] is built on
//! them:
//!   * the child observation adapter (`crates/maho-cli/src/cli/kibitzer_child.rs`) reads the
//!     early-termination hint, the original error flag and the structured refusal from `details` and
//!     the content text;
//!   * an `Err` can carry NEITHER the termination hint NOR the original `is_error`, so a refusal -
//!     terminating or not - must ride the `Ok` arm with its full metadata in `details`.
//!
//! `details` always carries `message`, `terminate`, `is_error` and `refusal` (the `rejected` code
//! parsed from the result text, or `null`), so no consumer re-derives the discriminator and a capped
//! rejection is never presented as a plain success. Only a genuine repository/identity failure
//! becomes a typed `Err`.
//!
//! # State
//!
//! Both tools read the SAME [`KibitzerSessionResources`] handle the sidecar resolves for the
//! session (one budget slot, one corpus cache, one searched set), so a `memory` search's returned
//! paths become nudge-eligible through `resources.searched` without rebuilding either tool. There is
//! no process-global registry and no private state here: cloning the handle shares the slots.

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError};

use maho_ext_api::{ToolDefinition, ToolError, ToolResult};
use memory_core::git::GitMemoryRepo;

use crate::kibitzer_nudge_tool::KibitzerNudgeParams;
use crate::kibitzer_session_resources::KibitzerSessionResources;
use crate::kibitzer_tools_caps::KibitzerToolCaps;
use crate::kibitzer_tools_memory::{
    KIBITZER_MEMORY_TOOL_NAME, MEMORY_EXPANSION_BOUNDS, execute_memory,
};
use crate::kibitzer_tools_nudge::{
    KIBITZER_NUDGE_DESCRIPTION, KIBITZER_NUDGE_TOOL_NAME, create_kibitzer_sidecar_nudge_tool,
    kibitzer_nudge_parameters,
};
use crate::kibitzer_tools_result::{KibitzerRejectionCode, KibitzerToolResult, rejection};
use crate::prompt::PromptContextResolver;

/// The bound parent session's RAW entries after a cursor, exactly as `omo_mount.rs` supplies them.
///
/// Neither `memory` nor `nudge` reads session entries (the CLI host owns `session_entries`); the
/// field is retained so [`KibitzerMemberToolsInput`] matches the mount's actual construction
/// unchanged.
pub type KibitzerMemberSessionEntries = Arc<dyn Fn(i64) -> Vec<serde_json::Value> + Send + Sync>;

/// Everything the two member closures need, field for field as `omo_mount.rs` builds it.
pub struct KibitzerMemberToolsInput {
    /// The EXPLICIT parent session this child serves; the identity repository resolves through it.
    pub session_id: String,
    /// The mount's identity resolver: a session id to its bound memory identity context.
    pub resolve_context: PromptContextResolver,
    /// The mount's resolved environment. Retained for the mount's call shape; neither closure reads
    /// it today (the identity comes from `resolve_context`, not the environment).
    pub env: BTreeMap<String, String>,
    /// The bound parent's raw entries. Retained for the mount's call shape; see the type alias.
    pub session_entries: KibitzerMemberSessionEntries,
    /// The SAME per-session handle the sidecar and the three host tools share.
    pub resources: KibitzerSessionResources,
    /// `memory.recall.tool_caps`, resolved per spawn by the mount.
    pub caps: KibitzerToolCaps,
    /// `memory.recall.query_expansion` for this child, resolved by the mount from the SAME per-agent
    /// recall block it derives `caps` from. It selects the `memory` description and parameter schema
    /// and is passed straight to `execute_memory`, so the selected production configuration works.
    pub query_expansion: bool,
}

/// Upstream `SEARCH_ONLY_DESCRIPTION` (`kibitzer/tools/memory.ts`): the description when
/// `memory.recall.query_expansion` is off.
const MEMORY_TOOL_DESCRIPTION: &str =
    "Read-only access to committed memories: search by query or read one path. This tool cannot write.";

/// Upstream `EXPANSION_DESCRIPTION`: the description when `memory.recall.query_expansion` is on.
const MEMORY_TOOL_EXPANSION_DESCRIPTION: &str =
    "Read-only access to committed memories: search by query or read one path. This tool cannot write. Search matches words, not meaning: when a memory may be worded differently from the query, also pass synonyms / keywords / related / note_line. Each of their terms counts for less than a word of the query, and a memory that holds every word of the query still ranks first.";

/// Builds the `memory` and `nudge` tools, in registry order after the three host tools.
pub fn kibitzer_member_tools(input: KibitzerMemberToolsInput) -> Vec<ToolDefinition> {
    vec![memory_tool(&input), nudge_tool(&input)]
}

/// The `memory` tool: charge once, then `execute_memory` over the shared corpus cache and searched
/// set. An `Err(GitError)` from a corpus load is the port's only non-refusal failure.
fn memory_tool(input: &KibitzerMemberToolsInput) -> ToolDefinition {
    let session_id = input.session_id.clone();
    let resolve_context = Arc::clone(&input.resolve_context);
    let resources = input.resources.clone();
    let caps = input.caps;
    let query_expansion = input.query_expansion;
    ToolDefinition::new(
        KIBITZER_MEMORY_TOOL_NAME,
        kibitzer_memory_description(query_expansion),
        kibitzer_memory_parameters(query_expansion),
        Arc::new(move |call| {
            // Own everything the async block needs BEFORE the await: `call` borrows the caller frame.
            let params = call.params.clone();
            let session_id = session_id.clone();
            let resolve_context = Arc::clone(&resolve_context);
            let resources = resources.clone();
            Box::pin(async move {
                // ONE charge for this call; an exhausted budget is the shared structured refusal
                // (`tool_budget_exceeded`, `terminate = false`), returned before any repository effect.
                let budget = resources.budget_slot.current();
                if !budget.charge() {
                    return tool_result_of(budget_refusal(budget.limit()));
                }
                let Some(context) = resolve_context(&session_id) else {
                    return Err(ToolError::Message(
                        "memory: no memory identity is bound to this session; enable omo memory and restart the session so the kibitzer memory tool can initialize".into(),
                    ));
                };
                // Read-only: `open` never creates or seeds the repository (unlike the writer path).
                let repo = GitMemoryRepo::open(&context.identity_paths.repo, &context.identity)
                    .map_err(|error| ToolError::Message(error.to_string()))?;
                // Both guards are held only for the synchronous executor call below: no `await`, no
                // callback, and no other lock is taken while they are held.
                let mut cache = resources.corpus_cache.lock().unwrap_or_else(PoisonError::into_inner);
                let mut searched = resources.searched.lock().unwrap_or_else(PoisonError::into_inner);
                match execute_memory(&repo, &mut cache, caps, query_expansion, &mut searched, &params) {
                    Ok(result) => tool_result_of(result),
                    // Upstream awaits `corpus()` and lets the rejection propagate; a real repository
                    // failure is not a refusal code and must not be flattened into one.
                    Err(error) => Err(ToolError::Message(error.to_string())),
                }
            })
        }),
    )
}

/// The `nudge` tool: the live wrapper over the SAME resources handle, charged INSIDE the wrapper.
fn nudge_tool(input: &KibitzerMemberToolsInput) -> ToolDefinition {
    // Build ONE wrapper per child from the same handle; cloning the resources shares the slots.
    let nudge = Arc::new(create_kibitzer_sidecar_nudge_tool(input.resources.clone()));
    let resources = input.resources.clone();
    ToolDefinition::new(
        KIBITZER_NUDGE_TOOL_NAME,
        KIBITZER_NUDGE_DESCRIPTION,
        kibitzer_nudge_parameters(),
        Arc::new(move |call| {
            let params = call.params.clone();
            let nudge = Arc::clone(&nudge);
            let resources = resources.clone();
            Box::pin(async move {
                // The native host validates the call against this tool's schema BEFORE `execute`
                // runs, so a schema-invalid call never reaches here. This decode is therefore
                // defensive: if it ever fails, the call was still INVOKED, so it consumes exactly
                // one charge (the CLI host tools' charge-then-reject convention) and returns the
                // structured `invalid_argument` refusal. The wrapper is never reached on this path,
                // so its own charge cannot double-count.
                let params: KibitzerNudgeParams = match serde_json::from_value(params) {
                    Ok(params) => params,
                    Err(error) => {
                        let budget = resources.budget_slot.current();
                        if !budget.charge() {
                            return tool_result_of(budget_refusal(budget.limit()));
                        }
                        return tool_result_of(rejection(
                            KibitzerRejectionCode::InvalidArgument,
                            &format!("nudge: {error}"),
                            None,
                        ));
                    }
                };
                // The wrapper owns the ONE charge for an admitted call; the registry never charges
                // again.
                tool_result_of(nudge.execute(&params))
            })
        }),
    )
}

/// Maps a memory-owned result onto the port's native `ToolResult`/`ToolError` pair.
///
/// A refusal - terminating or not - is NOT flattened into `Err`: the native `Err` arm cannot carry
/// `details`, so flattening would drop both the `terminate` hint and the original `is_error`. Every
/// memory-owned result therefore rides the `Ok` arm with its full metadata in `details`
/// (`message`, `terminate`, `is_error`, `refusal`), and only a genuine repository or identity
/// failure becomes a typed `Err(ToolError::Message(..))`.
fn tool_result_of(result: KibitzerToolResult) -> Result<ToolResult, ToolError> {
    let mut details = serde_json::Map::new();
    details.insert("message".to_owned(), serde_json::Value::String(result.text.clone()));
    details.insert("terminate".to_owned(), serde_json::Value::Bool(result.terminate));
    details.insert("is_error".to_owned(), serde_json::Value::Bool(result.is_error));
    // The machine refusal code, taken from the SAME `rejection` producer's body
    // (`{"rejected": <code>, ...}`); a prose refusal (the base nudge rejection) has none.
    details.insert(
        "refusal".to_owned(),
        match refusal_code(&result.text) {
            Some(code) => serde_json::Value::String(code),
            None => serde_json::Value::Null,
        },
    );
    let mut out = ToolResult::text(result.text);
    out.details = Some(serde_json::Value::Object(details));
    Ok(out)
}

/// The `rejected` code of a structured refusal body, or `None` for a prose result.
fn refusal_code(text: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| value.get("rejected").and_then(serde_json::Value::as_str).map(str::to_owned))
}

/// The shared exhausted-budget refusal (`KibitzerRejectionCode::ToolBudgetExceeded`,
/// `terminate = false`), byte-identical to the CLI host tools' `charge_or_reject` message.
fn budget_refusal(limit: usize) -> KibitzerToolResult {
    rejection(
        KibitzerRejectionCode::ToolBudgetExceeded,
        &format!("The tool-call budget for this wake ({limit}) is exhausted; end the turn."),
        None,
    )
}

/// The `memory` description; upstream selects it from `memory.recall.query_expansion`.
fn kibitzer_memory_description(query_expansion: bool) -> &'static str {
    if query_expansion { MEMORY_TOOL_EXPANSION_DESCRIPTION } else { MEMORY_TOOL_DESCRIPTION }
}

/// The `memory` parameter schema (upstream `KibitzerMemoryParams`; the expansion variant adds the
/// four added-term fields). The operations are exactly `search` and `read`; there is no write
/// operation, by design.
fn kibitzer_memory_parameters(query_expansion: bool) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    properties.insert("operation".into(), serde_json::json!({
        "type": "string",
        "enum": ["search", "read"],
        "description": "search: find committed memories by query. read: return one committed memory body."
    }));
    properties.insert("query".into(), serde_json::json!({
        "type": "string",
        "description": "Search terms (double quotes group a phrase). Required by search."
    }));
    properties.insert("path".into(), serde_json::json!({
        "type": "string",
        "description": "Memory path relative to the memory repo. Required by read."
    }));
    if query_expansion {
        let terms = MEMORY_EXPANSION_BOUNDS.terms;
        let term_chars = MEMORY_EXPANSION_BOUNDS.term_chars;
        let tier = |description: &str| serde_json::json!({
            "type": "array",
            "maxItems": terms,
            "items": { "type": "string", "minLength": 1, "maxLength": term_chars },
            "description": description
        });
        properties.insert("synonyms".into(), tier(
            "search only: 4-8 close synonyms or other wordings of the query's key words, in the query's language; not words already in it."
        ));
        properties.insert("keywords".into(), tier(
            "search only: 4-8 keywords for the topic in the user's other working language(s), e.g. English for a Korean query."
        ));
        properties.insert("related".into(), tier(
            "search only: 4-8 looser terms a memory answering the question might contain (broader, narrower or associated)."
        ));
        properties.insert("note_line".into(), serde_json::json!({
            "type": "string",
            "maxLength": MEMORY_EXPANSION_BOUNDS.note_line_chars,
            "description": "search only: one short sentence written like a line of a memory that would answer the question."
        }));
    }
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["operation"],
        "properties": serde_json::Value::Object(properties)
    })
}

#[cfg(test)]
mod tests {
    //! Budget-charge regressions for the REGISTERED member tools.
    //!
    //! Every case drives the real `ToolDefinition::execute` closure returned by
    //! [`kibitzer_member_tools`] over one bound session's shared
    //! [`KibitzerSessionResources`] (built through the public registry getter, never the private
    //! constructor), and reads the charge count from the SAME `KibitzerToolBudget` the closure
    //! charged (`used()`), so no private executor stands in for the registered path.

    use super::*;
    use crate::kibitzer_contract::KibitzerToolBudget;
    use crate::kibitzer_session_resources::KibitzerSessionResourceRegistry;
    use crate::kibitzer_tools_caps::DEFAULT_KIBITZER_TOOL_CAPS;

    /// A factual observation hint (admissible: not imperative, no second person, no secret).
    const OBSERVATION: &str = "The rollout note records that nodes are drained before a rollout.";

    fn call(params: serde_json::Value) -> maho_ext_api::ToolCall<'static> {
        maho_ext_api::ToolCall { id: "call", params, signal: Default::default(), on_update: None, context: None }
    }

    /// One bound session's shared resources, resolved through the real registry getter.
    fn resources() -> KibitzerSessionResources {
        KibitzerSessionResourceRegistry::new().for_session("session")
    }

    /// The registered member tools for one session. `resolve_context` PANICS if consulted, so any
    /// case that refuses before the repository proves it performed no identity/repository work.
    fn tools_without_identity(resources: KibitzerSessionResources) -> Vec<ToolDefinition> {
        let resolve_context: PromptContextResolver = Arc::new(|_| panic!("resolve_context must not run"));
        kibitzer_member_tools(KibitzerMemberToolsInput {
            session_id: "session".into(),
            resolve_context,
            env: BTreeMap::new(),
            session_entries: Arc::new(|_| Vec::new()),
            resources,
            caps: DEFAULT_KIBITZER_TOOL_CAPS,
            query_expansion: false,
        })
    }

    #[tokio::test]
    async fn exhausted_budget_refuses_memory_without_repository_effect() {
        let resources = resources();
        // A zero-limit current-wake budget: every charge refuses.
        resources.budget_slot.reset(0);
        let tools = tools_without_identity(resources.clone());

        // The registered `memory` tool; the panicking resolver proves the refusal precedes any
        // identity resolution or repository open.
        let result = (tools[0].execute)(call(serde_json::json!({ "operation": "search", "query": "x" })))
            .await
            .expect("a refusal rides the Ok arm with metadata");
        let details = result.details.expect("refusal carries details");
        assert_eq!(details["refusal"], "tool_budget_exceeded");
        assert_eq!(details["is_error"], true);
        assert_eq!(details["terminate"], false);

        // No charge was spent, and the shared searched set is untouched (no repository effect).
        assert_eq!(resources.budget_slot.current().used(), 0);
        assert!(resources.searched.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
    }

    #[tokio::test]
    async fn admitted_nudge_consumes_exactly_one_charge_through_the_wrapper() {
        let resources = resources();
        resources.budget_slot.reset(8);
        resources.bind_max_items(5);
        resources.offered.lock().unwrap_or_else(PoisonError::into_inner).insert("reference/fact.md".into());
        let tools = tools_without_identity(resources.clone());

        // The registered `nudge` tool: the wrapper owns the single charge for an admitted call.
        let result = (tools[1].execute)(call(serde_json::json!({
            "path": "reference/fact.md",
            "hint": OBSERVATION,
        })))
        .await
        .expect("an admitted nudge rides the Ok arm");
        let details = result.details.expect("result carries details");
        assert_eq!(details["is_error"], false);
        assert_eq!(details["terminate"], false);

        // EXACTLY one charge (never zero, never two) and one recorded nudge.
        assert_eq!(resources.budget_slot.current().used(), 1);
        assert_eq!(resources.accepted.current().lock().unwrap_or_else(PoisonError::into_inner).len(), 1);
    }

    #[tokio::test]
    async fn retained_closure_follows_a_replaced_budget() {
        let resources = resources();
        resources.budget_slot.reset(0);
        resources.bind_max_items(5);
        resources.offered.lock().unwrap_or_else(PoisonError::into_inner).insert("reference/fact.md".into());
        let tools = tools_without_identity(resources.clone());

        // Built ONCE; the exhausted current budget refuses the first call.
        let refused = (tools[1].execute)(call(serde_json::json!({
            "path": "reference/fact.md",
            "hint": OBSERVATION,
        })))
        .await
        .expect("Ok arm");
        assert_eq!(refused.details.expect("details")["refusal"], "tool_budget_exceeded");

        // New-turn admission swaps the budget; the SAME retained closure resolves it at call time.
        resources.budget_slot.reset(8);
        let admitted = (tools[1].execute)(call(serde_json::json!({
            "path": "reference/fact.md",
            "hint": OBSERVATION,
        })))
        .await
        .expect("Ok arm");
        assert_eq!(admitted.details.expect("details")["is_error"], false);
        // The replacement budget was charged exactly once by this call.
        assert_eq!(resources.budget_slot.current().used(), 1);
    }

    #[tokio::test]
    async fn undecodable_invoked_nudge_consumes_exactly_one_charge() {
        let resources = resources();
        resources.budget_slot.reset(8);
        let tools = tools_without_identity(resources.clone());

        // An INVOKED call whose params cannot decode to `KibitzerNudgeParams` (`path` is not text).
        // The registry charges once on this branch and never reaches the wrapper, so the total is
        // one charge, not two.
        let result = (tools[1].execute)(call(serde_json::json!({ "path": 42 })))
            .await
            .expect("a refusal rides the Ok arm");
        let details = result.details.expect("refusal carries details");
        assert_eq!(details["refusal"], "invalid_argument");
        assert_eq!(details["is_error"], true);
        assert_eq!(resources.budget_slot.current().used(), 1);
    }

    fn tools_for_schema(query_expansion: bool) -> Vec<ToolDefinition> {
        let resolve_context: PromptContextResolver = Arc::new(|_| panic!("resolve_context must not run"));
        kibitzer_member_tools(KibitzerMemberToolsInput {
            session_id: "session".into(),
            resolve_context,
            env: BTreeMap::new(),
            session_entries: Arc::new(|_| Vec::new()),
            resources: resources(),
            caps: DEFAULT_KIBITZER_TOOL_CAPS,
            query_expansion,
        })
    }

    fn memory_properties(query_expansion: bool) -> serde_json::Map<String, serde_json::Value> {
        let tools = tools_for_schema(query_expansion);
        tools[0].parameters["properties"].as_object().expect("properties is an object").clone()
    }

    #[test]
    fn query_expansion_off_exposes_only_the_base_memory_keys() {
        let properties = memory_properties(false);
        let mut keys: Vec<&str> = properties.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["operation", "path", "query"]);
        assert!(properties.get("synonyms").is_none());
        assert!(properties.get("keywords").is_none());
        assert!(properties.get("related").is_none());
        assert!(properties.get("note_line").is_none());
    }

    #[test]
    fn query_expansion_on_adds_the_bounded_added_term_keys() {
        let properties = memory_properties(true);
        for key in ["operation", "query", "path", "synonyms", "keywords", "related", "note_line"] {
            assert!(properties.get(key).is_some(), "missing property {key}");
        }
        for tier in ["synonyms", "keywords", "related"] {
            let field = &properties[tier];
            assert_eq!(field["type"], "array", "{tier}.type");
            assert_eq!(field["maxItems"], MEMORY_EXPANSION_BOUNDS.terms, "{tier}.maxItems");
            assert_eq!(field["items"]["type"], "string", "{tier}.items.type");
            assert_eq!(field["items"]["minLength"], 1, "{tier}.items.minLength");
            assert_eq!(field["items"]["maxLength"], MEMORY_EXPANSION_BOUNDS.term_chars, "{tier}.items.maxLength");
        }
        assert_eq!(properties["note_line"]["type"], "string", "note_line.type");
        assert_eq!(properties["note_line"]["maxLength"], MEMORY_EXPANSION_BOUNDS.note_line_chars, "note_line.maxLength");
    }
}
