//! Port of upstream `store-extension/index.test.ts` with real SQLite transactional test receipts.

use rusqlite::{params, Connection, OptionalExtension, Transaction};

use maho_omo_gateway::{apply_gateway_rules_migrations_with_now, 
    apply_gateway_rules_migrations, block_for_session, rules_committed, sessions_with_rules,
    DeliverySummary, EnqueueDeliveryRequest, GatewayDeliveryEnqueueSeam, RulesCommittedArgs,
    RulesCommittedTarget,
};

/// Deterministic test-only enqueue seam that persists delivery receipts to an actual SQLite
/// table inside the supplied `tx`. This guarantees genuine transactional integration:
/// if a rules commit rolls back, the delivery receipts roll back with it.
struct SqliteTestReceiptsSeam;

impl SqliteTestReceiptsSeam {
    fn ensure_table(conn: &Connection) -> Result<(), rusqlite::Error> {
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS test_delivery_receipts (
                event_id TEXT PRIMARY KEY,
                delivery_id TEXT NOT NULL,
                binding_id TEXT NOT NULL,
                text TEXT NOT NULL,
                mode TEXT NOT NULL,
                created_at INTEGER NOT NULL
            )"#,
            [],
        )?;
        Ok(())
    }

    fn count_receipts(conn: &Connection) -> Result<usize, rusqlite::Error> {
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM test_delivery_receipts")?;
        let count: i64 = stmt.query_row([], |r| r.get(0))?;
        Ok(count as usize)
    }

    fn get_receipt(
        conn: &Connection,
        event_id: &str,
    ) -> Result<Option<(String, String, String, String)>, rusqlite::Error> {
        let mut stmt = conn.prepare(
            "SELECT delivery_id, binding_id, text, mode FROM test_delivery_receipts WHERE event_id = ?",
        )?;
        stmt.query_row(params![event_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()
    }
}

impl GatewayDeliveryEnqueueSeam for SqliteTestReceiptsSeam {
    fn enqueue(
        &self,
        tx: &Transaction<'_>,
        request: &EnqueueDeliveryRequest,
        now: f64,
    ) -> Result<DeliverySummary, String> {
        let mut check_stmt = tx
            .prepare("SELECT delivery_id FROM test_delivery_receipts WHERE event_id = ?")
            .map_err(|e| e.to_string())?;

        let existing: Option<String> = check_stmt
            .query_row(params![request.event_id], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;

        if let Some(delivery_id) = existing {
            return Ok(DeliverySummary::Ok {
                delivery_id,
                deduplicated: true,
            });
        }

        let delivery_id = format!("deliv_{}", request.event_id);
        tx.execute(
            r#"INSERT INTO test_delivery_receipts (event_id, delivery_id, binding_id, text, mode, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)"#,
            params![
                request.event_id,
                delivery_id,
                request.binding_id,
                request.text,
                request.mode,
                now as i64
            ],
        )
        .map_err(|e| e.to_string())?;

        Ok(DeliverySummary::Ok {
            delivery_id,
            deduplicated: false,
        })
    }
}

#[test]
fn given_a_bound_session_when_its_chat_rules_commit_then_block_for_session_returns_rendered_block_and_receipt_is_persisted() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_gateway_rules_migrations(&mut conn).unwrap();
    SqliteTestReceiptsSeam::ensure_table(&conn).unwrap();
    let seam = SqliteTestReceiptsSeam;

    let committed = rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![RulesCommittedTarget {
                session_durable_id: "sess-1".to_string(),
                binding_id: Some("b-1".to_string()),
                behavioral: Some(vec![
                    "answer in bullet points".to_string(),
                    "tag the owner on decisions".to_string(),
                ]),
            }],
        },
        &seam,
    )
    .unwrap();

    assert_eq!(committed.outcomes.len(), 1);
    assert_eq!(committed.outcomes[0].outcome, "updated");

    let block = block_for_session(&conn, "sess-1").unwrap().unwrap();
    assert_eq!(block.version, "v1");
    assert_eq!(
        block.block,
        [
            r#"<operating-rules version="v1">"#,
            "- answer in bullet points",
            "- tag the owner on decisions",
            "</operating-rules>",
        ]
        .join("
")
    );

    // Verify exact delivery receipt contents in SQLite
    let expected_event = "rules_changed:team:v1:sess-1";
    let receipt = SqliteTestReceiptsSeam::get_receipt(&conn, expected_event)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.0, format!("deliv_{expected_event}"));
    assert_eq!(receipt.1, "b-1");
    assert_eq!(receipt.3, "follow_up");
    assert!(receipt.2.contains(r#"<operating-rules-changed scope="team" version="v1">"#));
}

#[test]
fn given_a_rules_commit_when_it_repeats_and_new_version_commits_then_each_session_gains_exactly_one_delivery_per_version() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_gateway_rules_migrations(&mut conn).unwrap();
    SqliteTestReceiptsSeam::ensure_table(&conn).unwrap();
    let seam = SqliteTestReceiptsSeam;

    let targets = vec![
        RulesCommittedTarget {
            session_durable_id: "sess-1".to_string(),
            binding_id: Some("b-1".to_string()),
            behavioral: Some(vec!["rule one".to_string()]),
        },
        RulesCommittedTarget {
            session_durable_id: "sess-2".to_string(),
            binding_id: Some("b-2".to_string()),
            behavioral: Some(vec!["rule one".to_string()]),
        },
    ];

    // First commit: v1
    let first = rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: targets.clone(),
        },
        &seam,
    )
    .unwrap();

    assert_eq!(first.outcomes[0].outcome, "updated");
    assert_eq!(first.outcomes[1].outcome, "updated");
    assert!(matches!(
        first.outcomes[0].delivery,
        Some(DeliverySummary::Ok { deduplicated: false, .. })
    ));
    assert!(matches!(
        first.outcomes[1].delivery,
        Some(DeliverySummary::Ok { deduplicated: false, .. })
    ));
    assert_eq!(SqliteTestReceiptsSeam::count_receipts(&conn).unwrap(), 2);

    // Repeated commit: v1 (unchanged)
    let repeated = rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: targets.clone(),
        },
        &seam,
    )
    .unwrap();

    assert_eq!(repeated.outcomes[0].outcome, "unchanged");
    assert_eq!(repeated.outcomes[1].outcome, "unchanged");
    assert!(matches!(
        repeated.outcomes[0].delivery,
        Some(DeliverySummary::Ok { deduplicated: true, .. })
    ));
    assert!(matches!(
        repeated.outcomes[1].delivery,
        Some(DeliverySummary::Ok { deduplicated: true, .. })
    ));
    assert_eq!(SqliteTestReceiptsSeam::count_receipts(&conn).unwrap(), 2);

    // Duplicated targets within one call
    let duplicated = rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![targets[0].clone(), targets[0].clone()],
        },
        &seam,
    )
    .unwrap();

    assert_eq!(duplicated.outcomes.len(), 2);
    assert!(matches!(
        duplicated.outcomes[1].delivery,
        Some(DeliverySummary::Ok { deduplicated: true, .. })
    ));

    // Next version: v2
    let next = rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v2".to_string(),
            now: 2000.0,
            targets: targets.clone(),
        },
        &seam,
    )
    .unwrap();

    assert_eq!(next.outcomes[0].outcome, "updated");
    assert_eq!(next.outcomes[1].outcome, "updated");
    for outcome in &next.outcomes {
        assert!(matches!(
            outcome.delivery,
            Some(DeliverySummary::Ok { deduplicated: false, .. })
        ));
    }
    assert_eq!(SqliteTestReceiptsSeam::count_receipts(&conn).unwrap(), 4);
}

#[test]
fn given_valid_first_target_and_invalid_second_when_committed_then_transaction_rolls_back_both_rule_and_receipt() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_gateway_rules_migrations(&mut conn).unwrap();
    SqliteTestReceiptsSeam::ensure_table(&conn).unwrap();
    let seam = SqliteTestReceiptsSeam;

    let res = rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![
                // Valid first target
                RulesCommittedTarget {
                    session_durable_id: "valid-sess".to_string(),
                    binding_id: Some("b-valid".to_string()),
                    behavioral: Some(vec!["rule".to_string()]),
                },
                // Malformed second target (empty session durable id)
                RulesCommittedTarget {
                    session_durable_id: "".to_string(),
                    binding_id: Some("b-invalid".to_string()),
                    behavioral: Some(vec!["rule".to_string()]),
                },
            ],
        },
        &seam,
    );

    // Call must fail
    assert!(res.is_err());

    // Both rule block and delivery receipt for valid-sess MUST be rolled back (absent)
    let block = block_for_session(&conn, "valid-sess").unwrap();
    assert!(block.is_none(), "Rule block must be rolled back on commit failure");

    assert_eq!(
        SqliteTestReceiptsSeam::count_receipts(&conn).unwrap(),
        0,
        "Delivery receipts must be rolled back on commit failure"
    );
}

#[test]
fn given_a_committed_block_when_a_later_commit_clears_the_session_then_the_block_row_is_removed_without_delivery() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_gateway_rules_migrations(&mut conn).unwrap();
    SqliteTestReceiptsSeam::ensure_table(&conn).unwrap();
    let seam = SqliteTestReceiptsSeam;

    rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![RulesCommittedTarget {
                session_durable_id: "sess-1".to_string(),
                binding_id: Some("b-1".to_string()),
                behavioral: Some(vec!["rule one".to_string()]),
            }],
        },
        &seam,
    )
    .unwrap();

    // Clear session
    let cleared = rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v2".to_string(),
            now: 2000.0,
            targets: vec![RulesCommittedTarget {
                session_durable_id: "sess-1".to_string(),
                binding_id: Some("b-1".to_string()),
                behavioral: None,
            }],
        },
        &seam,
    )
    .unwrap();

    assert_eq!(cleared.outcomes[0].outcome, "removed");
    assert!(cleared.outcomes[0].delivery.is_none());

    let block = block_for_session(&conn, "sess-1").unwrap();
    assert!(block.is_none());
}

#[test]
fn given_malformed_targets_when_rules_committed_runs_then_the_call_is_refused() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_gateway_rules_migrations(&mut conn).unwrap();
    SqliteTestReceiptsSeam::ensure_table(&conn).unwrap();
    let seam = SqliteTestReceiptsSeam;

    let err = rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![],
        },
        &seam,
    );
    assert!(err.is_err());

    let err2 = rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![RulesCommittedTarget {
                session_durable_id: "".to_string(),
                binding_id: None,
                behavioral: Some(vec![]),
            }],
        },
        &seam,
    );
    assert!(err2.is_err());

    // Non-finite now validation
    let err3 = rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: f64::NAN,
            targets: vec![],
        },
        &seam,
    );
    assert!(err3.is_err());

    let err4 = rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: f64::INFINITY,
            targets: vec![],
        },
        &seam,
    );
    assert!(err4.is_err());
}

#[test]
fn given_two_sessions_in_one_scope_and_one_in_another_when_the_scope_is_listed_then_exactly_its_two_sessions_are_returned() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_gateway_rules_migrations(&mut conn).unwrap();
    SqliteTestReceiptsSeam::ensure_table(&conn).unwrap();
    let seam = SqliteTestReceiptsSeam;

    rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![
                RulesCommittedTarget {
                    session_durable_id: "a1".to_string(),
                    binding_id: None,
                    behavioral: Some(vec!["rule one".to_string()]),
                },
                RulesCommittedTarget {
                    session_durable_id: "a2".to_string(),
                    binding_id: None,
                    behavioral: Some(vec!["rule one".to_string()]),
                },
            ],
        },
        &seam,
    )
    .unwrap();

    rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "other".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![RulesCommittedTarget {
                session_durable_id: "b1".to_string(),
                binding_id: None,
                behavioral: Some(vec!["rule one".to_string()]),
            }],
        },
        &seam,
    )
    .unwrap();

    let listed = sessions_with_rules(&conn, "team").unwrap();
    assert_eq!(listed.sessions.len(), 2);
    assert_eq!(listed.sessions[0].session_durable_id, "a1");
    assert_eq!(listed.sessions[0].version, "v1");
    assert_eq!(listed.sessions[1].session_durable_id, "a2");
    assert_eq!(listed.sessions[1].version, "v1");
}

#[test]
fn given_a_missing_or_empty_scope_when_sessions_with_rules_runs_then_the_call_is_refused() {
    let conn = Connection::open_in_memory().unwrap();
    let err = sessions_with_rules(&conn, "");
    assert!(err.is_err());
}

#[test]
fn given_empty_session_id_when_block_for_session_runs_then_it_returns_an_error() {
    let conn = Connection::open_in_memory().unwrap();
    let err = block_for_session(&conn, "");
    assert!(err.is_err());
}

#[test]
fn given_failing_seam_when_enqueue_runs_then_error_outcome_is_preserved() {
    struct FailingSeam;
    impl GatewayDeliveryEnqueueSeam for FailingSeam {
        fn enqueue(
            &self,
            _tx: &Transaction<'_>,
            _request: &EnqueueDeliveryRequest,
            _now: f64,
        ) -> Result<DeliverySummary, String> {
            Err("connector offline".to_string())
        }
    }

    let mut conn = Connection::open_in_memory().unwrap();
    apply_gateway_rules_migrations(&mut conn).unwrap();

    let res = rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![RulesCommittedTarget {
                session_durable_id: "sess-fail".to_string(),
                binding_id: Some("b-fail".to_string()),
                behavioral: Some(vec!["rule".to_string()]),
            }],
        },
        &FailingSeam,
    )
    .unwrap();

    assert_eq!(res.outcomes[0].outcome, "updated");
    assert_eq!(
        res.outcomes[0].delivery,
        Some(DeliverySummary::Error {
            error: "connector offline".to_string()
        })
    );
}

#[test]
fn given_existing_populated_database_when_migrations_reapply_then_it_succeeds_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("gateway.sqlite");

    // First open & migration apply
    {
        let mut conn = Connection::open(&db_path).unwrap();
        apply_gateway_rules_migrations(&mut conn).unwrap();
    }

    // Second open & migration apply (simulating restart on populated DB)
    {
        let mut conn = Connection::open(&db_path).unwrap();
        let res = apply_gateway_rules_migrations(&mut conn);
        assert!(res.is_ok(), "Restart on existing database must succeed idempotently");
    }
}

#[test]
fn given_future_database_version_when_migrations_run_then_it_returns_an_error() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute(
        r#"CREATE TABLE extension_schema (
            name TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )"#,
        [],
    )
    .unwrap();

    // Insert a future version greater than total migrations
    conn.execute(
        "INSERT INTO extension_schema (name, version, updated_at) VALUES ('gateway_rules', 99, 1000)",
        [],
    )
    .unwrap();

    let res = apply_gateway_rules_migrations(&mut conn);
    assert!(res.is_err(), "Future schema version must be rejected");
}

#[test]
fn given_unregistered_database_with_actual_extension_schema_when_migrated_then_inserts_updated_at_correctly() {
    let mut conn = Connection::open_in_memory().unwrap();
    // Pinned exact 3-column extension_schema from upstream schema.ts
    conn.execute(
        r#"CREATE TABLE extension_schema (
            name TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )"#,
        [],
    )
    .unwrap();

    // Migrate with deterministic timestamp 123456789.0
    apply_gateway_rules_migrations_with_now(&mut conn, 123456789.0).unwrap();

    let mut stmt = conn
        .prepare("SELECT name, version, updated_at FROM extension_schema WHERE name = 'gateway_rules'")
        .unwrap();
    let row: (String, i64, i64) = stmt
        .query_row([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();

    assert_eq!(row.0, "gateway_rules");
    assert_eq!(row.1, 1);
    assert_eq!(row.2, 123456789);

    // Verify rules table exists and works
    let block = block_for_session(&conn, "any-session").unwrap();
    assert!(block.is_none());
}

#[test]
fn given_existing_database_with_pinned_extension_schema_and_current_marker_when_migrated_then_retains_version_and_reads_rules() {
    let mut conn = Connection::open_in_memory().unwrap();
    // Upstream actual schema: extension_schema + gateway_rules_blocks
    conn.execute(
        r#"CREATE TABLE extension_schema (
            name TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )"#,
        [],
    )
    .unwrap();
    conn.execute(
        r#"CREATE TABLE gateway_rules_blocks (
            session_durable_id TEXT PRIMARY KEY,
            scope TEXT NOT NULL,
            version TEXT NOT NULL,
            block TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        )"#,
        [],
    )
    .unwrap();

    // Current version marker 1 already recorded
    conn.execute(
        "INSERT INTO extension_schema (name, version, updated_at) VALUES ('gateway_rules', 1, 987654321)",
        [],
    )
    .unwrap();
    // Existing committed rule row
    conn.execute(
        r#"INSERT INTO gateway_rules_blocks (session_durable_id, scope, version, block, updated_at)
           VALUES ('sess-pinned', 'team', 'v1', '<operating-rules version="v1">- rule</operating-rules>', 987654321)"#,
        [],
    )
    .unwrap();

    // Re-running migration must succeed idempotently without rewriting updated_at
    apply_gateway_rules_migrations_with_now(&mut conn, 999999999.0).unwrap();

    let mut stmt = conn
        .prepare("SELECT version, updated_at FROM extension_schema WHERE name = 'gateway_rules'")
        .unwrap();
    let row: (i64, i64) = stmt.query_row([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(row.0, 1);
    assert_eq!(row.1, 987654321, "Existing updated_at timestamp must be retained");

    // Component/store reads populated DB successfully
    let block = block_for_session(&conn, "sess-pinned").unwrap().unwrap();
    assert_eq!(block.version, "v1");
    assert_eq!(block.block, r#"<operating-rules version="v1">- rule</operating-rules>"#);
}
