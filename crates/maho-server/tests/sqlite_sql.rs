use maho_server::sqlite::*;
use rusqlite::types::Value;
#[test]
fn query_iteration_is_lazy_and_stops_before_unrequested_row_decode() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    let query = SqlQuery::new("SELECT ? UNION ALL SELECT 'invalid'",vec![Value::Integer(7)]);
    let mut decoded = 0;
    let first = query.iterate(&db,|row| {decoded += 1;row.get::<_,i64>(0)},|rows| rows.next().transpose()).unwrap();
    assert_eq!(first, Some(7));
    assert_eq!(decoded, 1);
    assert!(query.iterate(&db,|row|row.get::<_,i64>(0),|rows|rows.collect::<rusqlite::Result<Vec<_>>>()).is_err());
    assert_eq!(db.query_row("SELECT 1",[],|row|row.get::<_,i64>(0)).unwrap(),1);
}

#[test]
fn nested_queries_preserve_parameter_order() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    SqlQuery::new(
        "CREATE TABLE entries (id TEXT PRIMARY KEY,kind TEXT NOT NULL,active INTEGER NOT NULL)",
        vec![],
    )
    .exec(&db)
    .unwrap();
    for (id, active) in [("one", 1), ("two", 0)] {
        SqlQuery::compose(
            &["INSERT INTO entries VALUES (", ",", ",", ")"],
            vec![
                SqlValue::Parameter(Value::Text(id.into())),
                SqlValue::Parameter(Value::Text("message".into())),
                SqlValue::Parameter(Value::Integer(active)),
            ],
        )
        .run(&db)
        .unwrap();
    }
    let filters = join_sql_fragments(
        vec![
            SqlQuery::new("kind = ?", vec![Value::Text("message".into())]),
            SqlQuery::new("active = ?", vec![Value::Integer(1)]),
        ],
        " AND ",
    );
    let query = SqlQuery::compose(
        &["SELECT id FROM entries WHERE ", " LIMIT ", ""],
        vec![
            SqlValue::Query(filters),
            SqlValue::Parameter(Value::Integer(10)),
        ],
    );
    assert_eq!(
        query.all(&db, |row| row.get::<_, String>(0)).unwrap(),
        vec!["one"]
    );
}
#[test]
fn parameterized_queries_and_missing_rows() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE values_table (id INTEGER PRIMARY KEY,value TEXT NOT NULL)")
        .unwrap();
    SqlQuery::new(
        "INSERT INTO values_table VALUES (?,?)",
        vec![Value::Integer(1), Value::Text("one".into())],
    )
    .run(&db)
    .unwrap();
    assert_eq!(
        SqlQuery::new(
            "SELECT value FROM values_table WHERE id=?",
            vec![Value::Integer(1)]
        )
        .get(&db, |row| row.get::<_, String>(0))
        .unwrap(),
        Some("one".into())
    );
    assert_eq!(
        SqlQuery::new(
            "SELECT value FROM values_table WHERE id=?",
            vec![Value::Integer(2)]
        )
        .get(&db, |row| row.get::<_, String>(0))
        .unwrap(),
        None
    );
    assert!(
        SqlQuery::new("SELECT ?", vec![Value::Integer(1)])
            .exec(&db)
            .is_err()
    );
}
#[test]
fn opening_existing_does_not_create_missing_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.sqlite");
    assert!(open_existing(&path).is_err());
    assert!(!path.exists());
    let db = open(&path).unwrap();
    db.execute_batch("CREATE TABLE test (value INTEGER)")
        .unwrap();
    drop(db);
    assert!(open_existing(&path).is_ok());
    let readonly = open_read_only(&path).unwrap();
    assert!(readonly.execute("INSERT INTO test VALUES (1)", []).is_err());
}

#[test]
fn initial_schema_is_idempotent_and_enforces_shared_id_integrity() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    apply_initial_schema(&db).unwrap();
    apply_initial_schema(&db).unwrap();
    let insert = "INSERT INTO entries (session_id,id,parent_id,seq,type,timestamp,payload) VALUES (?1,?2,?3,1,'message',0,'{}')";
    db.execute(
        insert,
        rusqlite::params!["session", "parent", None::<String>],
    )
    .unwrap();
    assert!(
        db.execute(insert, rusqlite::params!["session", "bad", Some("missing")])
            .is_err()
    );
    db.execute(
        insert,
        rusqlite::params!["session", "child", Some("parent")],
    )
    .unwrap();
    assert!(db.execute("INSERT INTO usage_ledger (session_id,id,seq,adjustment,usage) VALUES ('session','parent',2,0,'{}')",[]).is_err());
    db.execute("INSERT INTO usage_ledger (session_id,id,seq,adjustment,usage) VALUES ('session','usage',2,0,'{}')",[]).unwrap();
    assert!(
        db.execute(
            insert,
            rusqlite::params!["session", "usage", None::<String>]
        )
        .is_err()
    );
    db.execute(insert, rusqlite::params!["other", "usage", None::<String>])
        .unwrap();
}

#[test]
fn immediate_transaction_rolls_back_failed_callback() {
    let mut db=rusqlite::Connection::open_in_memory().unwrap();db.execute_batch("CREATE TABLE test (value INTEGER)").unwrap();
    let failure=transaction(&mut db,|tx|{tx.execute("INSERT INTO test VALUES (1)",[])?;tx.execute("INSERT INTO missing VALUES (2)",[])?;Ok(())});
    assert!(failure.is_err());assert_eq!(db.query_row("SELECT COUNT(*) FROM test",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    transaction(&mut db,|tx|{tx.execute("INSERT INTO test VALUES (3)",[])?;Ok(())}).unwrap();
    assert_eq!(db.query_row("SELECT value FROM test",[],|row|row.get::<_,i64>(0)).unwrap(),3);
}
