use std::{
    fs,
    io::{self, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use telemetry_core::{otlp_spool_cap::*, otlp_spool_dir::*, otlp_spool_file::*, otlp_wal::*};
fn input(dir: &std::path::Path) -> SpoolWriterInput {
    SpoolWriterInput {
        dir: dir.into(),
        signal: Signal::Logs,
        pid: 4242,
        boot_id: "f".repeat(32),
        header: encode_header(Signal::Logs, &[], &[]),
        consent: Arc::new(|| true),
        on_error: Arc::new(|e, n| panic!("unexpected loss {n}: {e}")),
        write: None,
        truncate: None,
    }
}
fn context(dir: &std::path::Path, now: u64) -> SpoolContext {
    SpoolContext {
        dir: dir.into(),
        boot_id: "f".repeat(32),
        pid: 4242,
        now_ms: Arc::new(move || now),
        is_pid_alive: Arc::new(|_| false),
        cap_bytes: SPOOL_CAP_BYTES,
        max_age_ms: SPOOL_MAX_AGE_MS,
        on_drop: Arc::new(|_, _| {}),
        on_lock_left: None,
    }
}
#[test]
fn emitted_records_are_already_durable_and_off_consent_is_lazy() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let dir = root.path().join("spool");
    let mut off = input(&dir);
    off.consent = Arc::new(|| false);
    let mut writer = SpoolWriter::new(off);
    for _ in 0..25 {
        assert!(!writer.append(&[]));
    }
    assert!(!dir.exists());
    let mut processor = WalLogRecordProcessor::new(SpoolWriter::new(input(&dir)), Box::new(|_| {}));
    for _ in 0..3 {
        processor.on_emit(&WalLogRecord {
            event_name: Some("omo.feature_used".into()),
            ..Default::default()
        });
    }
    let bytes = fs::read(dir.join(&processor.writer.name))?;
    assert_eq!(read_frames(&bytes).len(), 4);
    assert!(matches!(
        spool_request(&bytes),
        SpoolRequest::Ok { records: 3, .. }
    ));
    processor.on_emit(&WalLogRecord {
        event_name: Some("free text".into()),
        ..Default::default()
    });
    assert_eq!(processor.writer.records, 3);
    Ok(())
}
#[test]
fn short_writes_are_completed_and_torn_append_rolls_back() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let mut config = input(root.path());
    let fail = Arc::new(AtomicBool::new(false));
    let injected = fail.clone();
    config.write = Some(Arc::new(move |file, bytes| {
        if injected.load(Ordering::SeqCst) {
            return Ok(0);
        }
        file.write(&bytes[..bytes.len().min(2)])
    }));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let recorded = errors.clone();
    config.on_error = Arc::new(move |_, n| recorded.lock().expect("errors").push(n));
    let mut writer = SpoolWriter::new(config);
    assert!(writer.append(&[8, 1]));
    let before = fs::read(root.path().join(&writer.name))?;
    fail.store(true, Ordering::SeqCst);
    assert!(!writer.append(&[8, 2]));
    fail.store(false, Ordering::SeqCst);
    assert!(writer.append(&[8, 3]));
    let after = fs::read(root.path().join(&writer.name))?;
    assert!(after.starts_with(&before));
    assert_eq!(read_frames(&after).len(), 3);
    assert_eq!(*errors.lock().expect("errors"), vec![1]);
    Ok(())
}
#[tokio::test]
async fn one_claim_wins_then_expired_lease_is_recovered() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let id = create_batch_id(1000);
    let ctx = context(root.path(), 2000);
    let mut writer = SpoolWriter::new(input(root.path()));
    assert!(writer.append(&[]));
    let sealed = writer.seal(&id)?.expect("sealed");
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            claim_batch(&ctx, Signal::Logs, &id),
            claim_batch(&ctx, Signal::Logs, &id)
        )
    })
    .await
    .expect("claim deadline");
    let a = a?;
    let b = b?;
    assert_eq!(usize::from(a.is_some()) + usize::from(b.is_some()), 1);
    let claimed = a.or(b).expect("one claim");
    assert!(!root.path().join(&sealed).exists());
    let later = context(root.path(), 2000 + CLAIM_LEASE_MS);
    sweep_spool(&later)?;
    assert!(root.path().join(&sealed).exists());
    assert!(!root.path().join(claimed).exists());
    Ok(())
}
#[test]
fn cap_keeps_active_and_sending_but_evicts_quarantine_then_old_batches() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let mut ctx = context(root.path(), 2000);
    ctx.cap_bytes = 1;
    let drops = Arc::new(Mutex::new(Vec::new()));
    let recorded = drops.clone();
    ctx.on_drop = Arc::new(move |r, n| recorded.lock().expect("drops").push((r, n)));
    let active = active_name(Signal::Logs, 4242, &ctx.boot_id);
    fs::write(root.path().join(&active), frame(&[]))?;
    let id = create_batch_id(1000);
    let batch = batch_name(Signal::Logs, &id);
    let mut bytes = frame(&encode_header(Signal::Logs, &[], &[]));
    bytes.extend(frame(&[]));
    fs::write(root.path().join(&batch), bytes)?;
    fs::create_dir(root.path().join(QUARANTINE_DIR))?;
    fs::write(root.path().join(QUARANTINE_DIR).join(&batch), [0; 20])?;
    let total = enforce_spool_cap(&ctx, &mut Default::default())?;
    assert_eq!(total, 4);
    assert!(root.path().join(active).exists());
    assert!(!root.path().join(batch).exists());
    assert_eq!(
        *drops.lock().expect("drops"),
        vec![(DropReason::Evicted, 1)]
    );
    Ok(())
}
#[test]
fn counter_overflow_advances_once_and_preserves_sort_order() {
    let last = BatchIdCounter {
        ms: 100,
        counter: BATCH_ID_COUNTER_LIMIT - 1,
        overflowed: false,
    };
    let next = next_batch_id_counter(last, 100, 0.5);
    assert_eq!(
        next,
        BatchIdCounter {
            ms: 101,
            counter: 0,
            overflowed: true
        }
    );
    assert_eq!(next_batch_id_counter(next, 100, 0.5).counter, 1);
    let a = create_batch_id(999);
    assert_eq!(batch_created_ms(&a), 999);
}

#[test]
fn wal_filters_log_and_span_attributes_before_encoding() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let mut logs =
        WalLogRecordProcessor::new(SpoolWriter::new(input(root.path())), Box::new(|_| {}));
    logs.on_emit(&WalLogRecord {
        event_name: Some("omo.feature_used".into()),
        attributes:
            serde_json::json!({"omo.feature":"KEEP", "$session_id":"SECRET", "platform":"SECRET"})
                .as_object()
                .expect("map")
                .clone(),
        ..Default::default()
    });
    let log_bytes = fs::read(root.path().join(&logs.writer.name))?;
    assert!(log_bytes.windows(4).any(|s| s == b"KEEP"));
    assert!(!log_bytes.windows(6).any(|s| s == b"SECRET"));
    let mut config = input(root.path());
    config.signal = Signal::Traces;
    config.header = encode_header(Signal::Traces, &[], &[]);
    let mut spans = WalSpanProcessor::new(
        SpoolWriter::new(config),
        Box::new(|_| {}),
        [("omo.turn".into(), vec!["omo.model".into()])].into(),
    );
    spans.on_end(&WalSpan {
        name: "unlisted".into(),
        ..Default::default()
    });
    assert_eq!(spans.writer.records, 0);
    spans.on_end(&WalSpan {
        name: "omo.turn".into(),
        attributes:
            serde_json::json!({"omo.model":"KEEP", "prompt":"SECRET", "$session_id":"SECRET"})
                .as_object()
                .expect("map")
                .clone(),
        ..Default::default()
    });
    let span_bytes = fs::read(root.path().join(&spans.writer.name))?;
    assert!(span_bytes.windows(4).any(|s| s == b"KEEP"));
    assert!(!span_bytes.windows(6).any(|s| s == b"SECRET"));
    assert!(matches!(
        spool_request(&span_bytes),
        SpoolRequest::Ok {
            signal: Signal::Traces,
            records: 1,
            ..
        }
    ));
    Ok(())
}
#[test]
fn failed_rollback_abandons_file_and_reports_previous_records() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let mut config = input(root.path());
    let fail = Arc::new(AtomicBool::new(false));
    let injected = fail.clone();
    config.write = Some(Arc::new(move |file, bytes| {
        if injected.load(Ordering::SeqCst) {
            Ok(0)
        } else {
            file.write(bytes)
        }
    }));
    config.truncate = Some(Arc::new(|_, _| Err(io::Error::other("truncate denied"))));
    let losses = Arc::new(Mutex::new(Vec::new()));
    let sink = losses.clone();
    config.on_error = Arc::new(move |_, n| sink.lock().expect("losses").push(n));
    let mut writer = SpoolWriter::new(config);
    assert!(writer.append(&[]));
    assert!(writer.append(&[]));
    fail.store(true, Ordering::SeqCst);
    assert!(!writer.append(&[]));
    assert!(!root.path().join(&writer.name).exists());
    assert_eq!(*losses.lock().expect("losses"), vec![2, 1]);
    fail.store(false, Ordering::SeqCst);
    assert!(writer.append(&[]));
    assert_eq!(writer.records, 1);
    Ok(())
}

#[test]
fn dead_owner_idle_recovery_and_duplicate_links_do_not_double_replay() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let active = active_name(Signal::Traces, 999, &"b".repeat(32));
    fs::write(root.path().join(&active), [])?;
    let stat = file_stat(root.path(), &active)?.expect("file");
    let now = stat.mtime_ms;
    sweep_spool(&context(root.path(), now))?;
    assert!(root.path().join(&active).exists());
    sweep_spool(&context(root.path(), now + DEAD_OWNER_IDLE_MS))?;
    assert!(!root.path().join(&active).exists());
    assert_eq!(oldest_sealed_batches(root.path())?.len(), 1);
    let linked = active_name(Signal::Logs, 998, &"b".repeat(32));
    let batch = batch_name(Signal::Logs, &create_batch_id(now));
    fs::write(root.path().join(&linked), [])?;
    fs::hard_link(root.path().join(&linked), root.path().join(&batch))?;
    sweep_spool(&context(root.path(), now + DEAD_OWNER_IDLE_MS))?;
    assert!(!root.path().join(linked).exists());
    assert!(root.path().join(batch).exists());
    assert_eq!(oldest_sealed_batches(root.path())?.len(), 2);
    Ok(())
}
#[test]
fn old_batch_expires_by_id_and_corrupt_records_are_counted_once() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let mut ctx = context(root.path(), SPOOL_MAX_AGE_MS + 2000);
    let drops = Arc::new(Mutex::new(Vec::new()));
    let sink = drops.clone();
    ctx.on_drop = Arc::new(move |r, n| sink.lock().expect("drops").push((r, n)));
    let old = batch_name(Signal::Logs, &create_batch_id(1000));
    let mut bytes = frame(&encode_header(Signal::Logs, &[], &[]));
    bytes.extend(frame(&[]));
    fs::write(root.path().join(&old), bytes)?;
    sweep_spool(&ctx)?;
    assert!(!root.path().join(old).exists());
    let corrupt = batch_name(Signal::Logs, &create_batch_id((ctx.now_ms)()));
    let mut bytes = frame(&[9, 9, 9]);
    bytes.extend(frame(&[1, 2]));
    fs::write(root.path().join(&corrupt), bytes)?;
    quarantine_file(&ctx, &corrupt)?;
    assert!(root.path().join(QUARANTINE_DIR).join(corrupt).exists());
    ctx.cap_bytes = 0;
    enforce_spool_cap(&ctx, &mut Default::default())?;
    assert_eq!(
        *drops.lock().expect("drops"),
        vec![(DropReason::Expired, 1), (DropReason::Corrupt, 2)]
    );
    Ok(())
}
