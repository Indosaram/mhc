use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::locks::process_identity::ProcessLiveness;

use crate::reflection::stale_holder::{
    LockReadError, ReflectionLockProbe, ReflectionLockVerdict, classify_reflection_lock_holder,
};

const REFLECTION_DIR: &str = "/memory/agents/agent-a/runtime/reflection";
const RUN: &str = "reflection-run-64";
const HOST: &str = "this-host";
const LAUNCHER: u32 = 11096;
const CLAIMANT: u32 = 4200;
const RECORDED_START: &str = "linux-proc-start-ticks:100";

enum FileState {
    Text(String),
    Error(String),
}

struct Facts {
    files: BTreeMap<PathBuf, FileState>,
    pids: BTreeMap<u32, ProcessLiveness>,
    starts: BTreeMap<u32, Option<String>>,
    throw_start_identity: bool,
}

struct Scenario {
    name: &'static str,
    facts: Facts,
    expected: ReflectionLockVerdict,
}

impl Facts {
    fn with_file(mut self, path: PathBuf, state: Option<FileState>) -> Self {
        match state {
            Some(state) => {
                self.files.insert(path, state);
            }
            None => {
                self.files.remove(&path);
            }
        }
        self
    }

    fn with_pid(mut self, pid: u32, liveness: ProcessLiveness) -> Self {
        self.pids.insert(pid, liveness);
        self
    }

    fn with_start(mut self, pid: u32, start: Option<String>) -> Self {
        self.starts.insert(pid, start);
        self
    }

    fn throwing_start_identity(mut self) -> Self {
        self.throw_start_identity = true;
        self
    }
}

struct ScenarioProbe {
    facts: Facts,
}

impl ReflectionLockProbe for ScenarioProbe {
    fn read_text(&self, path: &Path) -> Result<String, LockReadError> {
        match self.facts.files.get(path) {
            None => Err(LockReadError::new("ENOENT")),
            Some(FileState::Error(code)) => Err(LockReadError::new(code.clone())),
            Some(FileState::Text(text)) => Ok(text.clone()),
        }
    }

    fn hostname(&self) -> String {
        HOST.to_string()
    }

    fn pid_liveness(&self, pid: u32) -> ProcessLiveness {
        self.facts
            .pids
            .get(&pid)
            .copied()
            .unwrap_or(ProcessLiveness::Dead)
    }

    fn process_start_identity(&self, pid: u32) -> Result<Option<String>, String> {
        if self.facts.throw_start_identity {
            return Err("ps failed".to_string());
        }
        Ok(self.facts.starts.get(&pid).cloned().flatten())
    }
}

fn reflection_dir() -> PathBuf {
    PathBuf::from(REFLECTION_DIR)
}

fn lock_path() -> PathBuf {
    reflection_dir().join("active.lock")
}

fn outcome_path() -> PathBuf {
    reflection_dir().join("runs").join(RUN).join("outcome.json")
}

fn claim_path() -> PathBuf {
    reflection_dir()
        .join("runs")
        .join(RUN)
        .join("terminal-claim.json")
}

fn valid_outcome() -> String {
    json!({
        "version": 1,
        "runId": RUN,
        "attempt": 1,
        "finishedAt": "2026-10-17T00:00:00.000Z",
        "childExit": { "code": 0, "signal": null },
        "timedOut": false,
    })
    .to_string()
}

fn lock(fields: Value) -> String {
    let mut object = serde_json::Map::new();
    object.insert("runId".to_string(), Value::String(RUN.to_string()));
    object.insert("launcherPid".to_string(), json!(LAUNCHER));
    object.insert("launcherHostname".to_string(), Value::String(HOST.to_string()));
    if let Some(extra) = fields.as_object() {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    Value::Object(object).to_string()
}

/// A provably dead holder: every rule passes. Each scenario changes exactly one fact of it.
fn dead_holder() -> Facts {
    let mut files = BTreeMap::new();
    files.insert(lock_path(), FileState::Text(lock(json!({}))));
    files.insert(outcome_path(), FileState::Text(valid_outcome()));
    files.insert(
        claim_path(),
        FileState::Text(json!({ "claimantPid": CLAIMANT }).to_string()),
    );
    Facts {
        files,
        pids: BTreeMap::new(),
        starts: BTreeMap::new(),
        throw_start_identity: false,
    }
}

fn live(run_id: &str) -> ReflectionLockVerdict {
    ReflectionLockVerdict::Live {
        run_id: run_id.to_string(),
    }
}

fn dead(run_id: &str) -> ReflectionLockVerdict {
    ReflectionLockVerdict::Dead {
        run_id: run_id.to_string(),
    }
}

fn stuck(run_id: &str, reason: impl Into<String>) -> ReflectionLockVerdict {
    ReflectionLockVerdict::Stuck {
        run_id: run_id.to_string(),
        reason: reason.into(),
    }
}

/// Each fail-closed rule has its own scenario that changes exactly one fact of a provably dead
/// holder, so removing that rule changes that scenario's verdict.
fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "every fact proves the holder gone",
            facts: dead_holder(),
            expected: dead(RUN),
        },
        Scenario {
            name: "no active.lock",
            facts: dead_holder().with_file(lock_path(), None),
            expected: ReflectionLockVerdict::None,
        },
        Scenario {
            name: "active.lock cannot be read [rule:lock-unreadable]",
            facts: dead_holder().with_file(
                lock_path(),
                Some(FileState::Error("EACCES".to_string())),
            ),
            expected: stuck("unknown", "active.lock could not be read (EACCES)"),
        },
        Scenario {
            name: "active.lock is not JSON [rule:lock-invalid]",
            facts: dead_holder().with_file(
                lock_path(),
                Some(FileState::Text("{not json".to_string())),
            ),
            expected: stuck("unknown", "active.lock is not valid JSON"),
        },
        Scenario {
            name: "active.lock is the JSON literal null",
            facts: dead_holder().with_file(lock_path(), Some(FileState::Text("null".to_string()))),
            expected: stuck("unknown", "its launcher identity is missing"),
        },
        Scenario {
            name: "the launcher pid is missing [rule:launcher-identity]",
            facts: dead_holder().with_file(
                lock_path(),
                Some(FileState::Text(
                    json!({ "runId": RUN, "launcherHostname": HOST }).to_string(),
                )),
            ),
            expected: stuck(RUN, "its launcher identity is missing"),
        },
        Scenario {
            name: "another host owns the lock [rule:other-host]",
            facts: dead_holder().with_file(
                lock_path(),
                Some(FileState::Text(lock(json!({ "launcherHostname": "other-host" })))),
            ),
            expected: stuck(
                RUN,
                "it is owned by host other-host, which cannot be checked from here",
            ),
        },
        Scenario {
            name: "the launcher is alive [rule:launcher-alive]",
            facts: dead_holder().with_pid(LAUNCHER, ProcessLiveness::Alive),
            expected: live(RUN),
        },
        Scenario {
            name: "the launcher cannot be checked [rule:launcher-unknown]",
            facts: dead_holder().with_pid(LAUNCHER, ProcessLiveness::Unknown),
            expected: stuck(RUN, format!("launcher pid {LAUNCHER} could not be checked")),
        },
        Scenario {
            name: "the launcher pid was reused by another process [rule:pid-reused]",
            facts: dead_holder()
                .with_file(
                    lock_path(),
                    Some(FileState::Text(lock(
                        json!({ "launcherProcessStart": RECORDED_START }),
                    ))),
                )
                .with_pid(LAUNCHER, ProcessLiveness::Alive)
                .with_start(LAUNCHER, Some("linux-proc-start-ticks:999".to_string())),
            expected: dead(RUN),
        },
        Scenario {
            name: "the launcher pid still has its recorded start",
            facts: dead_holder()
                .with_file(
                    lock_path(),
                    Some(FileState::Text(lock(
                        json!({ "launcherProcessStart": RECORDED_START }),
                    ))),
                )
                .with_pid(LAUNCHER, ProcessLiveness::Alive)
                .with_start(LAUNCHER, Some(RECORDED_START.to_string())),
            expected: live(RUN),
        },
        Scenario {
            name: "the launcher start cannot be read",
            facts: dead_holder()
                .with_file(
                    lock_path(),
                    Some(FileState::Text(lock(
                        json!({ "launcherProcessStart": RECORDED_START }),
                    ))),
                )
                .with_pid(LAUNCHER, ProcessLiveness::Alive)
                .with_start(LAUNCHER, None),
            expected: live(RUN),
        },
        Scenario {
            name: "the launcher start uses another scheme",
            facts: dead_holder()
                .with_file(
                    lock_path(),
                    Some(FileState::Text(lock(
                        json!({ "launcherProcessStart": "ps-lstart:Fri Oct 17 10:00:00 2026" }),
                    ))),
                )
                .with_pid(LAUNCHER, ProcessLiveness::Alive)
                .with_start(LAUNCHER, Some("proc-start-epoch:1792200000".to_string())),
            expected: live(RUN),
        },
        Scenario {
            name: "the run recorded no outcome [rule:outcome-missing]",
            facts: dead_holder().with_file(outcome_path(), None),
            expected: stuck(
                RUN,
                format!("launcher pid {LAUNCHER} is gone and the run recorded no outcome"),
            ),
        },
        Scenario {
            name: "the outcome cannot be read [rule:outcome-unreadable]",
            facts: dead_holder().with_file(
                outcome_path(),
                Some(FileState::Error("EIO".to_string())),
            ),
            expected: stuck(RUN, "its outcome could not be read (EIO)"),
        },
        Scenario {
            name: "the outcome is not JSON [rule:outcome-invalid]",
            facts: dead_holder().with_file(
                outcome_path(),
                Some(FileState::Text("{\"version\": 1, \"runId\": ".to_string())),
            ),
            expected: stuck(RUN, "its outcome is not valid JSON"),
        },
        Scenario {
            name: "the outcome is JSON but not a run outcome [rule:outcome-schema]",
            facts: dead_holder().with_file(
                outcome_path(),
                Some(FileState::Text(
                    json!({ "version": 1, "runId": RUN }).to_string(),
                )),
            ),
            expected: stuck(RUN, "its outcome is not a recorded run outcome"),
        },
        Scenario {
            name: "the outcome names another run",
            facts: dead_holder().with_file(
                outcome_path(),
                Some(FileState::Text(valid_outcome().replace(RUN, "reflection-run-63"))),
            ),
            expected: stuck(RUN, "its outcome is not a recorded run outcome"),
        },
        Scenario {
            name: "the outcome is the JSON literal null",
            facts: dead_holder().with_file(
                outcome_path(),
                Some(FileState::Text("null".to_string())),
            ),
            expected: stuck(RUN, "its outcome is not a recorded run outcome"),
        },
        Scenario {
            name: "the run failed but recorded a valid outcome",
            facts: dead_holder().with_file(
                outcome_path(),
                Some(FileState::Text(
                    json!({
                        "version": 1,
                        "runId": RUN,
                        "finishedAt": "2026-10-17T00:00:00.000Z",
                        "childExit": { "code": null, "signal": "SIGKILL" },
                        "timedOut": true,
                    })
                    .to_string(),
                )),
            ),
            expected: dead(RUN),
        },
        Scenario {
            name: "the terminal claim cannot be read [rule:claim-unreadable]",
            facts: dead_holder().with_file(
                claim_path(),
                Some(FileState::Error("EACCES".to_string())),
            ),
            expected: stuck(RUN, "its terminal claim could not be read (EACCES)"),
        },
        Scenario {
            name: "the terminal claim is not JSON [rule:claim-invalid]",
            facts: dead_holder().with_file(claim_path(), Some(FileState::Text("{".to_string()))),
            expected: stuck(RUN, "its terminal claim is not valid JSON"),
        },
        Scenario {
            name: "the terminal claim names no claimant [rule:claim-no-claimant]",
            facts: dead_holder().with_file(claim_path(), Some(FileState::Text("{}".to_string()))),
            expected: stuck(RUN, "its terminal claim names no claimant"),
        },
        Scenario {
            name: "the publish claimant is alive [rule:claimant-alive]",
            facts: dead_holder().with_pid(CLAIMANT, ProcessLiveness::Alive),
            expected: live(RUN),
        },
        Scenario {
            name: "the publish claimant cannot be checked [rule:claimant-unknown]",
            facts: dead_holder().with_pid(CLAIMANT, ProcessLiveness::Unknown),
            expected: stuck(
                RUN,
                format!("publish claimant pid {CLAIMANT} could not be checked"),
            ),
        },
        Scenario {
            name: "no publish claim was made",
            facts: dead_holder().with_file(claim_path(), None),
            expected: dead(RUN),
        },
    ]
}

#[test]
fn given_a_reflection_lock_scenario_when_the_holder_is_classified_then_the_verdict_is_exact() {
    for scenario in scenarios() {
        let verdict = classify_reflection_lock_holder(
            &reflection_dir(),
            &ScenarioProbe {
                facts: scenario.facts,
            },
        );
        assert_eq!(verdict, scenario.expected, "scenario: {}", scenario.name);
    }
}

#[test]
fn given_a_start_identity_reader_that_fails_when_the_launcher_is_alive_then_it_stays_live() {
    let facts = dead_holder()
        .with_file(
            lock_path(),
            Some(FileState::Text(lock(
                json!({ "launcherProcessStart": RECORDED_START }),
            ))),
        )
        .with_pid(LAUNCHER, ProcessLiveness::Alive)
        .with_start(LAUNCHER, Some("linux-proc-start-ticks:999".to_string()))
        .throwing_start_identity();

    let verdict = classify_reflection_lock_holder(&reflection_dir(), &ScenarioProbe { facts });

    assert_eq!(verdict, live(RUN));
}
