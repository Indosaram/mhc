//! Port of senpi `packages/agent/src/execution/testing/prep-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ai::types::BoxFuture;
use serde_json::{Value, json};

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::ledger_types::{
    ExecutionEntry, ExecutionOutcome, ExecutionProgress, FrozenCall,
};

#[derive(Debug, Clone, Default)]
pub struct PrepFixtureOptions {
    pub rewrite: Option<Value>,
    pub block: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrepOutcome {
    Prepared { call: FrozenCall },
    Blocked { reason: String },
    Refused { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReleaseOutcome {
    Settled {
        text: String,
        #[serde(rename = "isError")]
        is_error: bool,
    },
    Replayed,
    Refused {
        reason: String,
    },
}

pub trait PrepFixture: Send + Sync {
    fn handler_steps(&self) -> &[String];
    fn prepare(&self, id: &str, args: Value) -> BoxFuture<'_, Result<PrepOutcome, String>>;
    fn release(&self, id: &str) -> BoxFuture<'_, Result<ReleaseOutcome, String>>;
    fn trace(&self) -> Vec<String>;
    fn restart(&self) -> BoxFuture<'_, Result<(), String>>;
    fn row(&self, id: &str) -> BoxFuture<'_, Result<Option<ExecutionEntry>, String>>;
}

pub trait PrepFixtureAdapter: Send + Sync {
    fn open(
        &self,
        options: PrepFixtureOptions,
    ) -> BoxFuture<'_, Result<Arc<dyn PrepFixture>, String>>;
}

#[derive(Clone)]
pub struct PrepCaseAdapters {
    pub executor: Arc<dyn PrepFixtureAdapter>,
}

type PrepCase = CaseImplementation<PrepCaseAdapters>;

fn legacy_args() -> Value {
    json!({ "legacy": "42" })
}

fn prepared_call(outcome: PrepOutcome) -> Result<FrozenCall, String> {
    match outcome {
        PrepOutcome::Prepared { call } => Ok(call),
        other => Err(format!(
            "expected a prepared call, got {}",
            serde_json::to_string(&other).unwrap_or_default()
        )),
    }
}

fn count_step(trace: &[String], step: &str) -> usize {
    trace.iter().filter(|entry| entry.as_str() == step).count()
}

async fn shim(adapters: PrepCaseAdapters) -> ConformanceResult {
    let fixture = adapters
        .executor
        .open(PrepFixtureOptions::default())
        .await?;
    let call = prepared_call(fixture.prepare("prep-01", legacy_args()).await?)?;
    expect_equal(
        &json!({ "name": call.name, "args": call.args }),
        &json!({ "name": "qa", "args": { "value": 42 } }),
        "shim output is frozen",
    )?;
    let trace = fixture.trace();
    expect_equal(&count_step(&trace, "shim"), &1usize, "the shim runs once")?;
    let effect_idx = trace.iter().position(|s| s == "effect");
    let shim_idx = trace.iter().position(|s| s == "shim");
    expect_equal(
        &(effect_idx.is_some() && shim_idx.is_some() && effect_idx < shim_idx),
        &true,
        "the shim runs after effect_possible",
    )?;

    let invalid = adapters
        .executor
        .open(PrepFixtureOptions::default())
        .await?;
    let bad_outcome = invalid
        .prepare("prep-01-invalid", json!({ "value": "bad" }))
        .await?;
    let bad_kind = match bad_outcome {
        PrepOutcome::Blocked { .. } => "blocked",
        PrepOutcome::Prepared { .. } => "prepared",
        PrepOutcome::Refused { .. } => "refused",
    };
    expect_equal(&bad_kind, &"blocked", "validation blocks")?;
    expect_equal(
        &invalid.trace(),
        &vec![
            "effect".to_string(),
            "activate".to_string(),
            "shim".to_string(),
        ],
        "no handler runs after a failed validation",
    )?;
    let row = invalid.row("prep-01-invalid").await?;
    let outcome = row.as_ref().map(|r| match &r.progress {
        ExecutionProgress::Settled {
            outcome: ExecutionOutcome::Blocked { .. },
        } => "blocked",
        ExecutionProgress::Settled { outcome } => outcome.reason_str(),
        other => other.state(),
    });
    expect_equal(
        &json!({
            "outcome": outcome,
            "effectPossible": row.as_ref().map(|r| r.effect_possible),
        }),
        &json!({ "outcome": "blocked", "effectPossible": true }),
        "an invalid call settles blocked, with nothing frozen",
    )?;
    Ok(())
}

async fn activation_order(adapters: PrepCaseAdapters) -> ConformanceResult {
    let fixture = adapters
        .executor
        .open(PrepFixtureOptions::default())
        .await?;
    prepared_call(fixture.prepare("prep-02", legacy_args()).await?)?;
    let mut prepared = vec![
        "effect".to_string(),
        "activate".to_string(),
        "shim".to_string(),
    ];
    prepared.extend(fixture.handler_steps().iter().cloned());
    expect_equal(
        &fixture.trace(),
        &prepared,
        "preparation runs in today's order after effect_possible",
    )?;
    let rel = fixture.release("prep-02").await?;
    let rel_kind = match rel {
        ReleaseOutcome::Settled { .. } => "settled",
        ReleaseOutcome::Replayed => "replayed",
        ReleaseOutcome::Refused { .. } => "refused",
    };
    expect_equal(&rel_kind, &"settled", "release runs the body")?;
    let mut full = prepared;
    full.push("body(42)".to_string());
    expect_equal(
        &fixture.trace(),
        &full,
        "the body follows the frozen preparation",
    )?;
    Ok(())
}

async fn handlers_once(adapters: PrepCaseAdapters) -> ConformanceResult {
    let fixture = adapters
        .executor
        .open(PrepFixtureOptions::default())
        .await?;
    let (first_res, concurrent_res) = tokio::join!(
        fixture.prepare("prep-03", legacy_args()),
        fixture.prepare("prep-03", legacy_args())
    );
    let first = first_res?;
    let concurrent = concurrent_res?;
    let resent = fixture.prepare("prep-03", legacy_args()).await?;
    expect_equal(
        &vec![concurrent, resent],
        &vec![first.clone(), first],
        "a re-sent prepare returns the stored reply",
    )?;
    let released = fixture.release("prep-03").await?;
    expect_equal(
        &matches!(released, ReleaseOutcome::Settled { .. }),
        &true,
        "first release settles",
    )?;
    let second_release = fixture.release("prep-03").await?;
    expect_equal(
        &matches!(second_release, ReleaseOutcome::Settled { .. }),
        &false,
        "a re-sent release never reruns the body",
    )?;
    let late_prepare = fixture.prepare("prep-03", legacy_args()).await?;
    expect_equal(
        &matches!(late_prepare, PrepOutcome::Blocked { .. }),
        &false,
        "a prepare after release never reruns preparation",
    )?;
    let trace = fixture.trace();
    let mut expected_steps = vec![
        "effect".to_string(),
        "activate".to_string(),
        "shim".to_string(),
    ];
    expected_steps.extend(fixture.handler_steps().iter().cloned());
    expected_steps.push("body(42)".to_string());
    for step in expected_steps {
        expect_equal(
            &count_step(&trace, &step),
            &1usize,
            &format!("{step} runs exactly once"),
        )?;
    }
    Ok(())
}

async fn frozen_call_case(adapters: PrepCaseAdapters) -> ConformanceResult {
    let plain = prepared_call(
        adapters
            .executor
            .open(PrepFixtureOptions::default())
            .await?
            .prepare("prep-04", legacy_args())
            .await?,
    )?;
    let fixture = adapters
        .executor
        .open(PrepFixtureOptions {
            rewrite: Some(json!({ "value": 43 })),
            block: None,
        })
        .await?;
    let raw = legacy_args();
    let call = prepared_call(fixture.prepare("prep-04", raw).await?)?;
    expect_equal(
        &call.args,
        &json!({ "value": 43 }),
        "the frozen call is what the tool_call chain left",
    )?;
    expect_equal(
        &(call.digest == plain.digest),
        &false,
        "the digest binds the frozen arguments",
    )?;
    expect_equal(
        &fixture.row("prep-04").await?.map(|r| r.progress),
        &Some(ExecutionProgress::Prepared { call }),
        "the frozen call is durable",
    )?;
    expect_equal(
        &fixture.release("prep-04").await?,
        &ReleaseOutcome::Settled {
            text: "qa:43".to_string(),
            is_error: false,
        },
        "body result",
    )?;
    expect_equal(
        &fixture.trace().last().map(String::as_str),
        &Some("body(43)"),
        "release runs the body with the frozen arguments",
    )?;

    let blocked = adapters
        .executor
        .open(PrepFixtureOptions {
            rewrite: None,
            block: Some("held by policy".to_string()),
        })
        .await?;
    expect_equal(
        &blocked.prepare("prep-04-blocked", legacy_args()).await?,
        &PrepOutcome::Blocked {
            reason: "held by policy".to_string(),
        },
        "block",
    )?;
    let rel_blocked = blocked.release("prep-04-blocked").await?;
    expect_equal(
        &matches!(rel_blocked, ReleaseOutcome::Refused { .. }),
        &true,
        "a blocked call cannot be released",
    )?;
    expect_equal(
        &blocked.trace().iter().any(|step| step.starts_with("body(")),
        &false,
        "a blocked call never reaches the body",
    )?;
    Ok(())
}

async fn stored_replies(adapters: PrepCaseAdapters) -> ConformanceResult {
    let cases = [
        (PrepFixtureOptions::default(), legacy_args()),
        (
            PrepFixtureOptions {
                rewrite: None,
                block: Some("held by policy".to_string()),
            },
            legacy_args(),
        ),
        (PrepFixtureOptions::default(), json!({ "value": "bad" })),
    ];
    for (options, args) in cases {
        let fixture = adapters.executor.open(options).await?;
        let first = fixture.prepare("prep-05", args.clone()).await?;
        let steps = fixture.trace();
        fixture.restart().await?;
        let resent = fixture.prepare("prep-05", args).await?;
        let first_kind = match &first {
            PrepOutcome::Prepared { .. } => "prepared",
            PrepOutcome::Blocked { .. } => "blocked",
            PrepOutcome::Refused { .. } => "refused",
        };
        expect_equal(
            &resent,
            &first,
            &format!("re-sent {first_kind} reply after restart"),
        )?;
        expect_equal(
            &fixture.trace(),
            &steps,
            "a re-sent prepare after restart reruns no preparation step",
        )?;
        let row = fixture.row("prep-05").await?;
        expect_equal(
            &row.as_ref()
                .is_some_and(|r| r.progress.state() == "preparing"),
            &false,
            &format!("a {first_kind} preparation leaves no row in preparing for restart recovery"),
        )?;
    }
    Ok(())
}

pub fn prep_cases() -> BTreeMap<&'static str, PrepCase> {
    let mut map: BTreeMap<&'static str, PrepCase> = BTreeMap::new();
    map.insert("PREP-01", Arc::new(|a| Box::pin(shim(a))));
    map.insert("PREP-02", Arc::new(|a| Box::pin(activation_order(a))));
    map.insert("PREP-03", Arc::new(|a| Box::pin(handlers_once(a))));
    map.insert("PREP-04", Arc::new(|a| Box::pin(frozen_call_case(a))));
    map.insert("PREP-05", Arc::new(|a| Box::pin(stored_replies(a))));
    map
}

pub const PREP_CASES: fn() -> BTreeMap<&'static str, PrepCase> = prep_cases;
