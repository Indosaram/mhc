//! Port of senpi `packages/agent/src/execution/testing/multi-epoch-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::future::join_all;
use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;

use super::epoch_cases::texts;
use super::epoch_harness::{EpochBrainSpec, OpenEpochRigOptions, open_epoch_rig};
use super::hand_harness::{HandCaseAdapters, call};

const SCOPES: &[&str] = &["a", "b", "c"];

async fn one_session_moves(adapters: HandCaseAdapters) -> ConformanceResult {
    let rig = open_epoch_rig(
        adapters,
        OpenEpochRigOptions {
            brains: vec![
                EpochBrainSpec {
                    name: "brain-old".to_string(),
                    epoch: 1,
                },
                EpochBrainSpec {
                    name: "brain-new".to_string(),
                    epoch: 2,
                },
            ],
            placement: None,
            kill: None,
        },
    )
    .await?;
    for scope in SCOPES {
        rig.set_owner(scope, "brain-old");
    }
    let init_futs: Vec<_> = SCOPES
        .iter()
        .map(|scope| rig.execute("brain-old", scope, vec![call(&format!("{scope}1"))]))
        .collect();
    for res in rig
        .settle(
            join_all(init_futs),
            "MULTI-EPOCH-01: every scope ran on the old owner",
        )
        .await?
    {
        res?;
    }
    rig.set_owner("a", "brain-new");
    let moved = rig
        .settle(
            rig.execute("brain-new", "a", vec![call("a2")]),
            "MULTI-EPOCH-01: scope a moved",
        )
        .await??;
    expect_equal(
        &texts(&moved),
        &vec!["ran:a2".to_string()],
        "MULTI-EPOCH-01: the moved scope runs on its new owner",
    )?;
    let after_futs: Vec<_> = SCOPES
        .iter()
        .map(|scope| rig.execute("brain-old", scope, vec![call(&format!("{scope}3"))]))
        .collect();
    let after_settled = rig
        .settle(
            join_all(after_futs),
            "MULTI-EPOCH-01: the old owner's next calls",
        )
        .await?;
    let mut after_texts = Vec::new();
    for res in after_settled {
        after_texts.push(texts(&res?));
    }
    expect_equal(
        &after_texts,
        &vec![
            vec!["The hand refused this tool call: epoch_fenced".to_string()],
            vec!["ran:b3".to_string()],
            vec!["ran:c3".to_string()],
        ],
        "MULTI-EPOCH-01: only the moved scope fences the old owner; the others run untouched",
    )?;
    for scope in ["b", "c"] {
        expect_equal(
            &rig.floor(scope).await?,
            &Some(json!({ "epoch": 1, "owner": "brain-old" })),
            &format!("MULTI-EPOCH-01: scope {scope}'s floor"),
        )?;
    }
    let mut actual_bodies = rig.bodies();
    actual_bodies.sort();
    let expected_bodies: Vec<String> = ["a1", "a2", "b1", "b3", "c1", "c3"]
        .into_iter()
        .map(|id| format!("{}:{id}", &id[..1]))
        .collect();
    expect_equal(
        &actual_bodies,
        &expected_bodies,
        "MULTI-EPOCH-01: every scope's calls ran once; the fenced one never ran",
    )?;
    Ok(())
}

pub fn multi_epoch_cases() -> BTreeMap<&'static str, CaseImplementation<HandCaseAdapters>> {
    let mut map: BTreeMap<&'static str, CaseImplementation<HandCaseAdapters>> = BTreeMap::new();
    map.insert(
        "MULTI-EPOCH-01",
        Arc::new(|a| Box::pin(one_session_moves(a))),
    );
    map
}

pub const MULTI_EPOCH_CASES: fn() -> BTreeMap<&'static str, CaseImplementation<HandCaseAdapters>> =
    multi_epoch_cases;
