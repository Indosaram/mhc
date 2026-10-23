use super::*;
use crate::sync::engine::ports::EntryKind;

fn entry(seq: u64) -> PulledEntry {
    PulledEntry {
        seq,
        kind: EntryKind::Batch,
        batch_id: Some(format!("b{seq}")),
        digest: "d".into(),
        object_id: "o".into(),
        author: "device:x".into(),
    }
}

#[test]
fn state_defaults_then_round_trips_a_patch() -> Result<(), Box<dyn std::error::Error>> {
    let git = tempfile::tempdir()?;
    let journal = SyncJournal::new(git.path());
    assert_eq!(journal.state()?, EngineState::default());
    journal.update_state(|s| {
        s.applied = 4;
        s.paused = Some(Paused {
            seq: 5,
            code: RefusalCode::SequenceGap,
        });
    })?;
    let state = journal.state()?;
    assert_eq!(state.applied, 4);
    assert_eq!(
        state.paused,
        Some(Paused {
            seq: 5,
            code: RefusalCode::SequenceGap
        })
    );
    Ok(())
}

#[test]
fn incoming_lists_in_seq_order_and_drops() -> Result<(), Box<dyn std::error::Error>> {
    let git = tempfile::tempdir()?;
    let journal = SyncJournal::new(git.path());
    for seq in [3, 1, 2] {
        journal.put_incoming(&IncomingRecord {
            entry: entry(seq),
            stage: IncomingStage::Verified,
            items: vec![],
            snapshot: false,
        })?;
    }
    let seqs: Vec<u64> = journal.incoming()?.iter().map(|r| r.entry.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3]);
    journal.drop_incoming(2)?;
    let seqs: Vec<u64> = journal.incoming()?.iter().map(|r| r.entry.seq).collect();
    assert_eq!(seqs, vec![1, 3]);
    Ok(())
}

#[test]
fn render_landed_reads_the_staged_file_per_record_kind() -> Result<(), Box<dyn std::error::Error>> {
    let git = tempfile::tempdir()?;
    let journal = SyncJournal::new(git.path());
    let write = RenderRecord {
        seq: 1,
        key: "k".into(),
        frontier: "f".into(),
        hash: "h".into(),
        text: Some("t".into()),
    };
    let delete = RenderRecord {
        seq: 2,
        key: "k".into(),
        frontier: "f".into(),
        hash: "h".into(),
        text: None,
    };
    journal.append_render("c", &write)?;
    journal.append_render("c", &delete)?;
    assert_eq!(journal.next_render_seq("c")?, 3);
    super::super::atomic::write_atomic(&journal.staged_path("c", 1), b"t")?;
    assert!(!journal.render_landed("c", &write));
    assert!(!journal.render_landed("c", &delete));
    std::fs::remove_file(journal.staged_path("c", 1))?;
    super::super::atomic::write_atomic(&journal.staged_path("c", 2), b"")?;
    assert!(journal.render_landed("c", &write));
    assert!(journal.render_landed("c", &delete));
    journal.clear_renders(None)?;
    assert!(journal.renders("c")?.is_empty());
    Ok(())
}

#[test]
fn record_applied_skips_unnamed_entries_and_retire_moves_the_dir()
-> Result<(), Box<dyn std::error::Error>> {
    let git = tempfile::tempdir()?;
    let journal = SyncJournal::new(git.path());
    let mut unnamed = entry(2);
    unnamed.batch_id = None;
    journal.record_applied(&[entry(1), unnamed])?;
    assert_eq!(
        journal.applied()?.keys().cloned().collect::<Vec<_>>(),
        vec!["b1".to_string()]
    );
    journal.retire()?;
    assert!(!journal.dir.exists());
    journal.retire()?;
    Ok(())
}

#[test]
fn base64_round_trips() -> Result<(), EngineError> {
    assert_eq!(from_base64(&to_base64(&[0, 255, 7]))?, vec![0, 255, 7]);
    Ok(())
}
