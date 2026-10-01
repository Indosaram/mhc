use maho_agent::harness::utils::read_folders::*;

#[test]
fn qualifies_every_generated_program_against_independent_oracle() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/harness_read_oracle.json"))
            .expect("fixture invariant");
    let cases = fixture["cases"].as_array().expect("fixture invariant");
    assert_eq!(cases.len(), 1440);
    let mut emitted = 0;
    let mut protected_programs = 0;
    for case in cases {
        let text = case["source"].as_str().expect("fixture invariant");
        let path = format!(
            "input.{}",
            case["language"].as_str().expect("fixture invariant")
        );
        let parsed = SELECTED_READ_FOLDER.fold(ReadFolderInput {
            path: &path,
            text,
            settings: READ_FOLD_SETTINGS,
        });
        let protected = case["oracle"]["protected"]
            .as_array()
            .expect("fixture invariant");
        if !protected.is_empty() {
            protected_programs += 1;
        }
        match parsed {
            ReadFolderResult::Parsed { ranges, .. } => {
                assert_eq!(case["expected"]["status"], "parsed", "{}", case["name"]);
                let mut queue: std::collections::VecDeque<_> = ranges.iter().collect();
                while let Some(range) = queue.pop_front() {
                    emitted += 1;
                    for header in protected {
                        let start = header["start"].as_u64().expect("fixture invariant") as usize;
                        let end = header["end"].as_u64().expect("fixture invariant") as usize;
                        assert!(
                            !(range.start_line <= end && range.end_line >= start),
                            "{}: {range:?}",
                            case["name"]
                        );
                    }
                    assert!(
                        case["oracle"]["allowed"]
                            .as_array()
                            .expect("fixture invariant")
                            .iter()
                            .any(|allowed| allowed["start"] == range.start_line
                                && allowed["end"] == range.end_line),
                        "{}: {range:?}",
                        case["name"]
                    );
                    queue.extend(&range.children);
                }
            }
            ReadFolderResult::ParseFailure { reason } => {
                assert_eq!(
                    case["expected"]["status"], "parse_failure",
                    "{}",
                    case["name"]
                );
                assert_eq!(case["expected"]["reason"], reason, "{}", case["name"]);
            }
            ReadFolderResult::Unsupported { .. } => panic!("unsupported {}", case["name"]),
        }
    }
    assert_eq!(emitted, 244);
    assert_eq!(protected_programs, 1432);
}
