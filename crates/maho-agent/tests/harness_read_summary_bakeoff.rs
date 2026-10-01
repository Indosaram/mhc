use sha2::{Digest, Sha256};
#[path = "fixtures/read_prototype.rs"]
mod prototype;
#[derive(Clone)]
struct Fold {
    start: usize,
    end: usize,
}
#[derive(Clone)]
struct Sample {
    path: String,
    source: String,
    sha: String,
    folds: Vec<Fold>,
    allowed: Vec<Fold>,
    raw: f64,
    reference: f64,
    candidate: f64,
    retained: bool,
}
#[derive(Clone)]
struct Measurement {
    samples: Vec<Sample>,
    reference_available: bool,
    tokenizer_exact: bool,
    embedded: usize,
    budget: usize,
    engine: &'static str,
}
#[derive(Debug, PartialEq)]
struct Selection {
    engine: &'static str,
    status: &'static str,
    reason: &'static str,
}
fn sha(source: &str) -> String {
    format!("{:x}", Sha256::digest(source.as_bytes()))
}
fn measurement() -> Measurement {
    let source = (0..120)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    Measurement {
        samples: (0..5)
            .map(|i| Sample {
                path: format!("repo/file-{i}.ts"),
                source: source.clone(),
                sha: sha(&source),
                folds: vec![Fold { start: 3, end: 8 }],
                allowed: vec![Fold { start: 3, end: 8 }],
                raw: 100.0,
                reference: 50.0,
                candidate: 54.0,
                retained: true,
            })
            .collect(),
        reference_available: true,
        tokenizer_exact: true,
        embedded: 0,
        budget: 12582912,
        engine: "heuristic",
    }
}
fn valid(sample: &Sample) -> bool {
    let mut previous = 0;
    for fold in &sample.folds {
        if fold.start <= previous
            || fold.end < fold.start
            || fold.end > sample.source.split('\n').count()
            || !sample
                .allowed
                .iter()
                .any(|r| r.start == fold.start && r.end == fold.end)
        {
            return false;
        }
        previous = fold.end;
    }
    sample.retained
}
fn select(m: &Measurement) -> Selection {
    let raw = |reason, status| Selection {
        engine: "raw",
        reason,
        status,
    };
    if !m.reference_available {
        return raw("reference_unavailable", "inconclusive");
    }
    if !m.tokenizer_exact {
        return raw("exact_tokenizer_required", "inconclusive");
    }
    if m.embedded > m.budget {
        return raw("embedded_budget_exceeded", "inconclusive");
    }
    if m.samples.len() != 5 {
        return raw("measurement_blocked_insufficient_corpus", "pending_owner");
    }
    if m.samples
        .iter()
        .map(|s| &s.path)
        .collect::<std::collections::HashSet<_>>()
        .len()
        != m.samples.len()
    {
        return raw("duplicate_corpus_path", "inconclusive");
    }
    if m.samples.iter().any(|s| sha(&s.source) != s.sha) {
        return raw("stale_corpus_hash", "inconclusive");
    }
    if m.samples.iter().any(|s| {
        let lines = s.source.split('\n').count() - usize::from(s.source.ends_with('\n'));
        !(100..=2000).contains(&lines) || s.source.len() > 51200
    }) {
        return raw("ineligible_corpus", "inconclusive");
    }
    if m.samples.iter().any(|s| !valid(s)) {
        return raw("invalid_boundaries", "pending_owner");
    }
    if m.samples.iter().any(|s| {
        [s.raw, s.reference, s.candidate]
            .iter()
            .any(|n| !n.is_finite() || *n < 0.0 || n.fract() != 0.0 || *n > 9007199254740991.0)
            || s.raw == 0.0
    }) {
        return raw("invalid_token_counts", "inconclusive");
    }
    let mut savings: Vec<_> = m
        .samples
        .iter()
        .map(|s| (s.raw - s.candidate) / s.raw)
        .collect();
    savings.sort_by(f64::total_cmp);
    let mut reference: Vec<_> = m
        .samples
        .iter()
        .map(|s| (s.raw - s.reference) / s.raw)
        .collect();
    reference.sort_by(f64::total_cmp);
    let wins = m.samples.iter().map(|s| s.raw - s.candidate).sum::<f64>() > 0.0
        && (reference[2] <= 0.0 || savings[2] >= reference[2] * 0.9);
    Selection {
        engine: if wins { m.engine } else { "raw" },
        status: "conclusive",
        reason: if wins {
            "safe_quality_threshold_met"
        } else {
            "candidate_below_reference_threshold"
        },
    }
}
fn changed(change: impl Fn(&mut Sample)) -> Measurement {
    let mut m = measurement();
    for sample in &mut m.samples {
        change(sample);
    }
    m
}
#[test]
fn selects_safe_savings_at_ninety_percent() {
    let s = select(&measurement());
    assert_eq!(s.engine, "heuristic");
    assert_eq!(s.status, "conclusive");
}
#[test]
fn reference_required() {
    let mut m = measurement();
    m.reference_available = false;
    assert_eq!(select(&m).reason, "reference_unavailable");
}
#[test]
fn exact_tokenizer_required() {
    let mut m = measurement();
    m.tokenizer_exact = false;
    assert_eq!(select(&m).reason, "exact_tokenizer_required");
}
#[test]
fn embedded_budget_required() {
    let mut m = measurement();
    m.embedded = 12582913;
    assert_eq!(select(&m).reason, "embedded_budget_exceeded");
}
#[test]
fn short_corpus_pending_owner() {
    let mut m = measurement();
    m.samples.pop();
    assert_eq!(
        select(&m),
        Selection {
            engine: "raw",
            status: "pending_owner",
            reason: "measurement_blocked_insufficient_corpus"
        }
    );
}
#[test]
fn rejects_stale_bytes() {
    assert_eq!(
        select(&changed(|s| s.source.push_str("\nchanged"))).reason,
        "stale_corpus_hash"
    );
}
#[test]
fn rejects_duplicate_paths() {
    assert_eq!(
        select(&changed(|s| s.path = "same.ts".into())).reason,
        "duplicate_corpus_path"
    );
}
fn bad(folds: Vec<Fold>) {
    assert_eq!(
        select(&changed(|s| s.folds = folds.clone())).reason,
        "invalid_boundaries"
    );
}
#[test]
fn zero_coordinate() {
    bad(vec![Fold { start: 0, end: 8 }]);
}
#[test]
fn out_of_bounds_coordinate() {
    bad(vec![Fold { start: 3, end: 121 }]);
}
#[test]
fn unannotated_coordinate() {
    bad(vec![Fold { start: 3, end: 7 }]);
}
#[test]
fn reversed_coordinate() {
    bad(vec![Fold { start: 8, end: 3 }]);
}
#[test]
fn duplicate_coordinate() {
    bad(vec![Fold { start: 3, end: 8 }, Fold { start: 3, end: 8 }]);
}
#[test]
fn rejects_altered_retained_source() {
    assert_eq!(
        select(&changed(|s| s.retained = false)).reason,
        "invalid_boundaries"
    );
}
#[test]
fn rejects_nonfinite_token_counts() {
    assert_eq!(
        select(&changed(|s| s.reference = f64::NAN)).reason,
        "invalid_token_counts"
    );
}
#[test]
fn shortfall_is_decided_raw() {
    assert_eq!(
        select(&changed(|s| s.candidate = 56.0)),
        Selection {
            engine: "raw",
            status: "conclusive",
            reason: "candidate_below_reference_threshold"
        }
    );
}
#[test]
fn names_winning_grammar_engine() {
    let mut m = measurement();
    m.engine = "wasm";
    assert_eq!(
        select(&m),
        Selection {
            engine: "wasm",
            status: "conclusive",
            reason: "safe_quality_threshold_met"
        }
    );
    for sample in &mut m.samples {
        sample.candidate = 56.0;
    }
    assert_eq!(select(&m).reason, "candidate_below_reference_threshold");
}
#[test]
fn positive_saving_without_reference_saving() {
    assert_eq!(
        select(&changed(|s| {
            s.reference = 100.0;
            s.candidate = 99.0;
        }))
        .engine,
        "heuristic"
    );
}
#[test]
fn rejects_zero_total_saving() {
    assert_eq!(
        select(&changed(|s| s.candidate = 100.0)).reason,
        "candidate_below_reference_threshold"
    );
}
#[test]
fn raw_comparator_remains_verbatim() {
    use maho_agent::harness::utils::{
        read_folders::SELECTED_READ_FOLDER, segmented_read_view::create_default_read_summary,
    };
    let source = serde_json::to_string_pretty(&vec![vec!["raw comparator source bytes"; 12]; 20])
        .expect("fixture invariant");
    let path =
        std::env::temp_dir().join(format!("maho-raw-comparator-{}.json", std::process::id()));
    std::fs::write(&path, &source).expect("fixture invariant");
    let loaded = std::fs::read_to_string(&path).expect("fixture invariant");
    let summary = create_default_read_summary(
        "input.json",
        &loaded,
        None,
        None,
        Some(&SELECTED_READ_FOLDER),
        false,
    )
    .expect("fixture invariant");
    assert_ne!(summary.text, source);
    assert_eq!(
        std::fs::read_to_string(&path).expect("fixture invariant"),
        source
    );
    std::fs::remove_file(path).expect("fixture invariant");
}
fn lexical(language: &str) {
    let body = match language {
        "rust" => vec![
            "fn example<'a>(value: &'a str) -> &'a str {",
            "  /* outer /* nested } */ comment */",
            "  let character = '}';",
            "  let raw = r##\"{ raw }\"##;",
            "  let escaped = \"\\\\\\\"}\";",
            "  let ratio = 4 / 2;",
            "  let another = 3;",
            "  value",
            "}",
        ],
        "python" => vec![
            "def example(value):",
            "    \"\"\"A docstring with { braces }.",
            "    Another line.\"\"\"",
            "    text = f\"value {value}\"",
            "    ratio = 4 / 2",
            "    other = 3",
            "    value += other",
            "    return value",
            "",
        ],
        _ => vec![
            "function example(value) {",
            "  /* a } comment */",
            "  const ratio = 4 / 2;",
            "  const regex = /[{}\\/]+/g;",
            "  const text = `value ${(() => { return `nested ${1}`; })()}`;",
            "  const escaped = \"\\\\\\\"}\";",
            "  const other = 3;",
            "  return value;",
            "}",
        ],
    };
    let source = (0..20)
        .map(|i| {
            body.join("\n")
                .replacen("example", &format!("example{i}"), 1)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let result = prototype::heuristic(&source, language);
    assert!(!result.folds.is_empty(), "{result:?}");
    assert!(prototype::retained_exact(&source, &result));
    for fold in result.folds {
        assert_eq!(fold.start % 9, 2);
        assert_eq!(fold.end % 9, if language == "python" { 7 } else { 8 });
    }
}
#[test]
fn lexical_typescript() {
    lexical("ts");
}
#[test]
fn lexical_javascript() {
    lexical("js");
}
#[test]
fn lexical_rust() {
    lexical("rust");
}
#[test]
fn lexical_python() {
    lexical("python");
}
#[test]
fn malformed_python_falls_back() {
    let source = format!(
        "{}\n\"unterminated",
        (0..20)
            .map(|i| format!("def f{i}():\n{}", ["    x = 1"; 6].join("\n")))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let result = prototype::heuristic(&source, "python");
    assert!(result.folds.is_empty());
    assert_eq!(result.text, source);
    assert_eq!(result.reason, "python_unterminated_string");
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some("python_unterminated_string")
    );
}
#[test]
fn prototype_is_byte_exact_and_deterministic() {
    let source = (0..20)
        .map(|i| {
            format!(
                "function f{i}() {{\n{}\n}}",
                ["  let x = \"} {\";"; 6].join("\n")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let result = prototype::heuristic(&source, "js");
    assert!(!result.folds.is_empty());
    assert!(prototype::retained_exact(&source, &result));
    assert_eq!(prototype::heuristic(&source, "js"), result);
}
#[tokio::test]
async fn repeated_interrupts_do_not_affect_fresh_environment_read() {
    use maho_agent::harness::{
        context::{background_context, with_abort_signal},
        env::nodejs::NodeExecutionEnv,
        types::{FileErrorCode, FileSystem},
    };
    use maho_ai::utils::abort::AbortController;
    let root = std::env::temp_dir().join(format!("maho-read-interrupt-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("fixture invariant");
    std::fs::write(root.join("source.ts"), "before").expect("fixture invariant");
    let env = std::sync::Arc::new(NodeExecutionEnv::new(root.to_string_lossy().into_owned()));
    let controller = AbortController::new();
    let context = with_abort_signal(controller.signal(), &background_context());
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let read_env = env.clone();
    let pending = tokio::spawn(async move {
        started_tx.send(()).expect("fixture invariant");
        release_rx.await.expect("fixture invariant");
        read_env.read_text_file("source.ts", &context).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), started_rx)
        .await
        .expect("fixture invariant")
        .expect("fixture invariant");
    controller.abort(None);
    controller.abort(None);
    release_tx.send(()).expect("fixture invariant");
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), pending)
            .await
            .expect("fixture invariant")
            .expect("fixture invariant")
            .unwrap_err()
            .code,
        FileErrorCode::Aborted
    );
    std::fs::write(root.join("source.ts"), "after").expect("fixture invariant");
    assert_eq!(
        env.read_text_file("source.ts", &background_context())
            .await
            .expect("fixture invariant"),
        "after"
    );
    std::fs::remove_dir_all(root).expect("fixture invariant");
}
