use super::{
    args::{argument_cost, segment_flags},
    argv::{argv_budget, platform, room_after},
    enumerate::Candidate,
    json_rows::{FileRows, collect_segment_rows},
    paths::path_order,
    prefix_pass::{MAX_FILE_BYTES, read_searchable_prefix},
    process::{check, run},
};
use crate::{definition::AbortSignal, grep::engine::*};
use std::{collections::BTreeMap, path::Path, time::Instant};

const CAPPED_BATCH_SIZE: u32 = 200;
const MAX_CAPPED_BATCH_SIZE: u32 = 1600;

// A capped segment may grow to the ordered prefix already planned: launches grow
// logarithmically while the work searched past the cap stays at most double.
fn capped_limit(head: &Candidate) -> usize {
    head.ordinal
        .saturating_sub(1)
        .clamp(CAPPED_BATCH_SIZE, MAX_CAPPED_BATCH_SIZE) as usize
}

fn joins(previous: &[&Candidate], candidate: &Candidate, request: &GrepEngineRequest) -> bool {
    let head = previous[0];
    let max = MAX_FILE_BYTES as u64;
    if head.size > max || candidate.size > max {
        return false;
    }
    let under_cap = request.max_count.is_none() || previous.len() < capped_limit(head);
    // Explicit files from any root share one rg launch; binary members stay out of its argv.
    if head.root.file && candidate.root.file {
        return under_cap;
    }
    !head.binary
        && !candidate.binary
        && std::sync::Arc::ptr_eq(&head.root, &candidate.root)
        && under_cap
}

pub struct SegmentPlan<'a> {
    pub walk: &'a [String],
    pub matcher: &'a [String],
    pub budget: Option<usize>,
    pub platform: Option<&'a str>,
}

/// Orders candidates into rg launches whose whole command line, fixed arguments included, fits the argv budget.
pub fn plan_segments<'a>(
    ordered: &'a [Candidate],
    request: &GrepEngineRequest,
    plan: &SegmentPlan<'_>,
) -> Vec<Vec<&'a Candidate>> {
    let platform = plan.platform.unwrap_or_else(platform);
    let budget = plan.budget.unwrap_or_else(|| argv_budget(platform));
    let mut segments: Vec<Vec<&Candidate>> = Vec::new();
    let mut room = 0usize;
    for candidate in ordered {
        let cost = if candidate.binary {
            0
        } else {
            argument_cost(candidate, platform)
        };
        let joined = segments
            .last()
            .is_some_and(|previous| joins(previous, candidate, request) && cost <= room);
        if let (true, Some(previous)) = (joined, segments.last_mut()) {
            previous.push(candidate);
            room -= cost;
        } else {
            segments.push(vec![candidate]);
            room = room_after(
                &segment_flags(&[candidate], &request.pattern, plan.walk, plan.matcher),
                budget,
                platform,
            );
        }
    }
    segments
}

fn admitted_count(mode: GrepMode, result: &GrepEngineResult) -> u32 {
    if mode == GrepMode::Files {
        result.counts.files
    } else {
        result.counts.matches.unwrap_or(0)
    }
}

fn admit_files(
    files: BTreeMap<String, FileRows<'_>>,
    request: &GrepEngineRequest,
    result: &mut GrepEngineResult,
) -> Option<u32> {
    let mode = request.mode.unwrap_or(GrepMode::Content);
    let mut last_committed = None;
    let mut segment_files: Vec<_> = files.into_values().collect();
    segment_files.sort_by(|a, b| path_order(&a.candidate.display, &b.candidate.display));
    for file in segment_files {
        // BTreeMap keys are line numbers; one row per line, so (line, column) order is line order.
        let rows: Vec<GrepEngineMatch> = file.rows.into_values().collect();
        let matching: Vec<&GrepEngineMatch> = rows.iter().filter(|row| !row.is_context).collect();
        if matching.is_empty() {
            continue;
        }
        let per_file_cap = request
            .max_count_per_file
            .map_or(usize::MAX, |cap| cap as usize);
        let per_file_limited = mode != GrepMode::Files && matching.len() > per_file_cap;
        result.per_file_limit_reached |= per_file_limited;
        let remaining = request.max_count.map_or(usize::MAX, |max| {
            max.saturating_sub(admitted_count(mode, result)) as usize
        });
        let admitted_len = if mode == GrepMode::Files {
            usize::from(remaining > 0)
        } else {
            per_file_cap.min(remaining)
        }
        .min(matching.len());
        let available = if mode == GrepMode::Files {
            1
        } else {
            matching.len().min(per_file_cap)
        };
        if admitted_len < available {
            result.limit_reached = true;
        }
        if admitted_len == 0 {
            continue;
        }
        last_committed = Some(file.candidate.ordinal);
        result.counts.files += 1;
        if let Some(total) = &mut result.counts.matches {
            *total += u32::try_from(admitted_len).unwrap_or(u32::MAX);
        }
        if mode == GrepMode::Count || mode == GrepMode::Files {
            result.file_counts.push(maho_grep::GrepFileCount {
                path: file.candidate.display.clone(),
                count: (mode != GrepMode::Files)
                    .then(|| u32::try_from(admitted_len).unwrap_or(u32::MAX)),
                limit_reached: per_file_limited,
            });
        } else {
            let admitted: Vec<u32> = matching[..admitted_len]
                .iter()
                .map(|row| row.line)
                .collect();
            let before = request.context_before.unwrap_or(0);
            let after = request.context_after.unwrap_or(0);
            for row in &rows {
                if admitted.contains(&row.line)
                    || (row.is_context
                        && admitted.iter().any(|line| {
                            row.line >= line.saturating_sub(before)
                                && row.line <= line.saturating_add(after)
                        }))
                {
                    result.matches.push(row.clone());
                }
            }
        }
    }
    last_committed
}

#[allow(clippy::too_many_arguments)]
pub async fn search_segments(
    candidates: &[Candidate],
    request: &GrepEngineRequest,
    walk: &[String],
    matcher: &[String],
    result: &mut GrepEngineResult,
    budget: Option<usize>,
    deadline: Instant,
    signal: &AbortSignal,
) -> Result<(), GrepEngineError> {
    let mode = request.mode.unwrap_or(GrepMode::Content);
    let segments = plan_segments(
        candidates,
        request,
        &SegmentPlan {
            walk,
            matcher,
            budget,
            platform: None,
        },
    );
    let mut matcher_ran = false;
    let pattern_args = || {
        let mut args = matcher.to_vec();
        args.extend(["--".into(), request.pattern.clone()]);
        args
    };
    for (segment_index, segment) in segments.iter().enumerate() {
        check(deadline, signal)?;
        let last_ordinal = segment.last().map_or(0, |candidate| candidate.ordinal);
        let searchable: Vec<&Candidate> = segment
            .iter()
            .copied()
            .filter(|candidate| !candidate.binary)
            .collect();
        let Some(&first) = searchable.first() else {
            result.files_searched = last_ordinal;
            continue;
        };
        let oversized = first.size > MAX_FILE_BYTES as u64;
        let prefix = if oversized {
            read_searchable_prefix(first, result, deadline, signal).await?
        } else {
            None
        };
        if oversized && prefix.is_none() {
            result.files_searched = first.ordinal;
            continue;
        }
        matcher_ran = true;
        let output = if oversized {
            run(
                &pattern_args(),
                Path::new(&request.cwd),
                prefix,
                deadline,
                signal,
            )
            .await?
        } else {
            run(
                &segment_flags(&searchable, &request.pattern, walk, matcher),
                &first.root.cwd,
                None,
                deadline,
                signal,
            )
            .await?
        };
        check(deadline, signal)?;
        let collected = collect_segment_rows(&output, &searchable, request, oversized)?;
        // Nothing from a partially completed segment is visible before this point.
        if oversized {
            result.prefix_searched += 1;
        }
        result.skipped_binary += u32::try_from(collected.segment_binary.len()).unwrap_or(u32::MAX);
        let last_committed = admit_files(collected.files, request, result);
        result.files_searched = last_ordinal;
        if let Some(max) = request.max_count
            && admitted_count(mode, result) >= max
        {
            // A segment may have searched beyond the cap; only the admitted prefix counts.
            if let Some(ordinal) = last_committed {
                result.files_searched = ordinal;
            }
            if segment_index + 1 < segments.len() {
                result.limit_reached = true;
            }
            break;
        }
    }
    // An empty/all-skipped corpus must still report a malformed matcher, not a false no-match.
    if !matcher_ran {
        run(
            &pattern_args(),
            Path::new(&request.cwd),
            Some(Vec::new()),
            deadline,
            signal,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! senpi test/suite/regressions/3516-grep-rg-argv-budget.test.ts and
    //! 3516-grep-rg-batch-equivalence.test.ts.
    use super::*;
    use crate::grep::rg::{
        args::{matcher_flags, walker_flags},
        argv::{EXECUTABLE_RESERVE, argv_cost, command_line_overflow},
        enumerate::Root,
    };
    use std::{path::PathBuf, sync::Arc};

    const BASE: &str = "/work/repo";

    fn file_candidate(index: usize, name: &str) -> Candidate {
        let path = PathBuf::from(BASE).join(format!("d{index:04}")).join(name);
        let root = Arc::new(Root {
            path: path.clone(),
            alias: path.clone(),
            cwd: path.parent().expect("parent").to_path_buf(),
            file: true,
        });
        Candidate {
            absolute: path.clone(),
            canonical: path.clone(),
            display: path.to_string_lossy().into_owned(),
            root,
            size: 10,
            ordinal: u32::try_from(index + 1).expect("ordinal"),
            binary: false,
        }
    }

    fn tree_candidates(count: usize, name_of: impl Fn(usize) -> String) -> Vec<Candidate> {
        let root = Arc::new(Root {
            path: BASE.into(),
            alias: BASE.into(),
            cwd: BASE.into(),
            file: false,
        });
        (0..count)
            .map(|index| {
                let absolute = PathBuf::from(BASE).join("src dir").join(name_of(index));
                Candidate {
                    absolute: absolute.clone(),
                    canonical: absolute.clone(),
                    display: absolute.to_string_lossy().into_owned(),
                    root: Arc::clone(&root),
                    size: 10,
                    ordinal: u32::try_from(index + 1).expect("ordinal"),
                    binary: false,
                }
            })
            .collect()
    }

    fn command_line(argv: &[String], platform: &str) -> usize {
        EXECUTABLE_RESERVE + argv.iter().map(|a| argv_cost(a, platform)).sum::<usize>()
    }

    fn request(pattern: &str) -> GrepEngineRequest {
        GrepEngineRequest {
            pattern: pattern.into(),
            paths: vec![BASE.into()],
            cwd: BASE.into(),
            ..Default::default()
        }
    }

    fn heavy_request() -> GrepEngineRequest {
        GrepEngineRequest {
            pattern: (0..700)
                .map(|i| format!("ident{i}"))
                .collect::<Vec<_>>()
                .join("|"),
            glob: Some(
                (0..60)
                    .map(|i| format!("!**/generated-{i}/**/*.snapshot.ts"))
                    .collect(),
            ),
            ..request("")
        }
    }

    fn awkward_name(seed: u64) -> String {
        let parts = ["a\\\"b", "c d", "\\\\", "\"", "한글", "", "e\\"];
        let mut state = seed.wrapping_mul(2_654_435_761);
        let mut name = String::new();
        for _ in 0..1 + seed % 5 {
            state = (state.wrapping_mul(1_103_515_245).wrapping_add(12_345)) % (1 << 31);
            let index = (state % parts.len() as u64) as usize;
            if index == 5 {
                name.push_str(&"x".repeat((seed % 37) as usize));
            } else {
                name.push_str(parts[index]);
            }
        }
        format!("{name}{seed}.ts")
    }

    fn ordinals(segments: &[Vec<&Candidate>]) -> Vec<Vec<u32>> {
        segments
            .iter()
            .map(|s| s.iter().map(|c| c.ordinal).collect())
            .collect()
    }

    fn flatten(segments: &[Vec<&Candidate>]) -> Vec<PathBuf> {
        segments
            .iter()
            .flatten()
            .map(|c| c.absolute.clone())
            .collect()
    }

    #[test]
    fn keeps_the_windows_budget_under_the_command_line_limit() {
        assert!(argv_budget("win32") <= 32_767);
        assert!(argv_budget("darwin") < 1024 * 1024);
    }

    #[test]
    fn counts_windows_quoting() {
        assert_eq!(argv_cost("ab", "darwin"), 3);
        assert_eq!(argv_cost("a b", "win32"), 6);
        assert_eq!(argv_cost("a\"b", "win32"), 7);
        assert_eq!(argv_cost("C:\\dir\\", "win32"), "C:\\dir\\".len() + 3 + 1);
        let heavy = "a\\\\\"b c\\";
        assert_eq!(argv_cost(heavy, "win32"), heavy.len() + 3 + 1 + 2 + 1);
    }

    #[test]
    fn packs_exactly_at_the_win32_limit_and_splits_at_limit_minus_one() {
        let request = request("needle");
        let files = [
            file_candidate(0, "a\\\\\"b c\\"),
            file_candidate(1, "\\\"q\\\" r\\\\"),
        ];
        let limit = command_line(
            &segment_flags(&[&files[0], &files[1]], "needle", &[], &[]),
            "win32",
        );
        let plan = |budget| {
            ordinals(&plan_segments(
                &files,
                &request,
                &SegmentPlan {
                    walk: &[],
                    matcher: &[],
                    budget: Some(budget),
                    platform: Some("win32"),
                },
            ))
        };
        assert_eq!(plan(limit), vec![vec![1, 2]]);
        assert_eq!(plan(limit - 1), vec![vec![1], vec![2]]);
    }

    #[test]
    fn splits_with_a_long_pattern_and_many_globs_under_every_budget() {
        let request = heavy_request();
        let walk = walker_flags(&request);
        let matcher = matcher_flags(&request);
        for platform in ["win32", "darwin", "linux"] {
            let budget = argv_budget(platform);
            let scale = if platform == "win32" { 1 } else { 10 };
            let files: Vec<_> = (0..1500 * scale)
                .map(|i| file_candidate(i, "f.ts"))
                .collect();
            let tree = tree_candidates(1000 * scale, |i| format!("module \"{i}\".ts"));
            for candidates in [&tree, &files] {
                let segments = plan_segments(
                    candidates,
                    &request,
                    &SegmentPlan {
                        walk: &walk,
                        matcher: &matcher,
                        budget: None,
                        platform: Some(platform),
                    },
                );
                assert!(segments.len() > 1, "{platform}");
                assert_eq!(
                    flatten(&segments),
                    candidates
                        .iter()
                        .map(|c| c.absolute.clone())
                        .collect::<Vec<_>>()
                );
                for segment in &segments {
                    let argv = segment_flags(segment, &request.pattern, &walk, &matcher);
                    assert!(argv.contains(&request.pattern));
                    assert!(command_line(&argv, platform) <= budget, "{platform}");
                }
                // Each launch is filled close to the limit, not split needlessly.
                assert!(
                    command_line(
                        &segment_flags(&segments[0], &request.pattern, &walk, &matcher),
                        platform
                    ) > budget - 400,
                    "{platform}"
                );
            }
        }
    }

    #[test]
    fn never_exceeds_the_budget_for_awkward_names_across_many_budgets() {
        let request = GrepEngineRequest {
            glob: Some(vec!["!*.md".into()]),
            ..request("need\"le\\")
        };
        let walk = walker_flags(&request);
        let matcher = matcher_flags(&request);
        let files: Vec<_> = (0..400)
            .map(|i| file_candidate(i, &awkward_name(i as u64)))
            .collect();
        let tree = tree_candidates(400, |i| awkward_name(i as u64));
        for platform in ["win32", "darwin", "linux"] {
            for candidates in [&files, &tree] {
                for budget in [5_000, 5_123, 7_777, 12_345, argv_budget(platform)] {
                    let segments = plan_segments(
                        candidates,
                        &request,
                        &SegmentPlan {
                            walk: &walk,
                            matcher: &matcher,
                            budget: Some(budget),
                            platform: Some(platform),
                        },
                    );
                    assert_eq!(flatten(&segments).len(), candidates.len());
                    for segment in &segments {
                        assert!(
                            command_line(
                                &segment_flags(segment, &request.pattern, &walk, &matcher),
                                platform
                            ) <= budget
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn names_the_platform_limit_an_argument_list_would_break() {
        let overflow = |len: usize, platform| command_line_overflow(&["x".repeat(len)], platform);
        assert!(
            overflow(30_000, "win32")
                .expect("win32")
                .contains("32000-character limit on win32")
        );
        assert!(
            overflow(131_072, "linux")
                .expect("linux")
                .contains("per-argument limit")
        );
        assert!(overflow(131_071, "linux").is_none());
        assert!(overflow(131_072, "darwin").is_none());
    }

    #[test]
    fn gives_an_argument_that_cannot_share_a_launch_its_own_launch() {
        let long = format!("{}.ts", "x".repeat(400));
        let files = [
            file_candidate(0, "f.ts"),
            file_candidate(1, &long),
            file_candidate(2, "f.ts"),
        ];
        let request = request("needle");
        let budget = command_line(&segment_flags(&[&files[0]], "needle", &[], &[]), "darwin") + 100;
        let segments = plan_segments(
            &files,
            &request,
            &SegmentPlan {
                walk: &[],
                matcher: &[],
                budget: Some(budget),
                platform: Some("darwin"),
            },
        );
        assert_eq!(ordinals(&segments), vec![vec![1], vec![2], vec![3]]);
        assert_eq!(
            segment_flags(&[&files[1]], "needle", &[], &[])
                .last()
                .cloned(),
            Some(files[1].root.path.to_string_lossy().into_owned())
        );
    }

    #[tokio::test]
    async fn rejects_a_pattern_no_launch_can_carry_before_spawning_anything() {
        let directory = tempfile::tempdir().expect("tempdir");
        let pattern = "a|".repeat(argv_budget(platform()) / 2 + 10);
        let result = crate::grep::rg_engine::search(
            GrepEngineRequest {
                pattern,
                paths: vec![
                    directory
                        .path()
                        .join("missing")
                        .to_string_lossy()
                        .into_owned(),
                ],
                cwd: directory.path().to_string_lossy().into_owned(),
                ..Default::default()
            },
            &AbortSignal::default(),
        )
        .await;
        // A missing root would fail PATH_NOT_FOUND if roots were resolved first.
        assert!(
            matches!(result, Err(GrepEngineError::InvalidPattern(message)) if message.contains("too long"))
        );
    }

    fn comparable(result: &GrepEngineResult) -> String {
        format!(
            "{:?}|{:?}|{:?}|{}|{}|{}|{}|{}|{}|{:?}|{:?}|{}",
            result.matches,
            result
                .file_counts
                .iter()
                .map(|f| (&f.path, f.count, f.limit_reached))
                .collect::<Vec<_>>(),
            (
                result.counts.matches,
                result.counts.files,
                result.counts.exact
            ),
            result.files_searched,
            result.limit_reached,
            result.per_file_limit_reached,
            result.skipped_oversized,
            result.prefix_searched,
            result.skipped_binary,
            result.missing_paths,
            result
                .warnings
                .iter()
                .map(|w| (&w.path, &w.code, &w.message))
                .collect::<Vec<_>>(),
            result.timed_out,
        )
    }

    #[tokio::test]
    async fn batched_rg_launches_return_exactly_the_per_file_results() {
        if crate::managed_tools::find_on_path(
            crate::managed_tools::ManagedTool::Rg,
            std::env::var_os("PATH").as_deref(),
        )
        .is_none()
        {
            return;
        }
        let directory = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(directory.path()).expect("canonical");
        let mut names = vec![
            "has space.ts",
            "quote\"d.ts",
            "-leading.ts",
            "glob[*]{a,b}?.ts",
            "한글 파일.ts",
            ".hidden.ts",
            "ignored.ts",
            "notes.md",
            "bin.dat",
        ];
        if cfg!(unix) {
            names.extend(["new\nline.ts", "back\\slash\\.ts", "C:\\Windows\\style.ts"]);
        }
        let mut explicit = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let dir = root.join(format!("n{index:02}"));
            std::fs::create_dir(&dir).expect("dir");
            let body: Vec<u8> = if *name == "bin.dat" {
                b"needle\0".to_vec()
            } else {
                format!("Needle {index}\nctx\nneedle again {index}\nneedle 3\n").into_bytes()
            };
            std::fs::write(dir.join(name), body).expect("write");
            explicit.push(dir.join(name).to_string_lossy().into_owned());
        }
        std::fs::create_dir_all(root.join("tree").join(".hide")).expect("tree");
        for index in 0..12 {
            std::fs::write(
                root.join("tree").join(format!("t{index}.ts")),
                format!("needle {index}\n"),
            )
            .expect("t");
        }
        std::fs::write(
            root.join("tree").join(".hide").join("h.ts"),
            "needle hidden\n",
        )
        .expect("h");
        std::fs::write(root.join(".gitignore"), "ignored.ts\ntree/t3.ts\n").expect("gitignore");
        explicit.push(explicit[0].clone());
        let base = GrepEngineRequest {
            pattern: "needle".into(),
            cwd: root.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let options: Vec<(&str, GrepEngineRequest)> = vec![
            ("content", base.clone()),
            (
                "glob excludes",
                GrepEngineRequest {
                    glob: Some(vec!["!*.md".into(), "!**/t1*".into()]),
                    ..base.clone()
                },
            ),
            (
                "glob includes",
                GrepEngineRequest {
                    glob: Some(vec!["*.ts".into()]),
                    ..base.clone()
                },
            ),
            (
                "no hidden",
                GrepEngineRequest {
                    hidden: Some(false),
                    ..base.clone()
                },
            ),
            (
                "no ignore files",
                GrepEngineRequest {
                    gitignore: Some(false),
                    ..base.clone()
                },
            ),
            (
                "type filter",
                GrepEngineRequest {
                    r#type: Some("ts".into()),
                    ..base.clone()
                },
            ),
            (
                "max count across batches",
                GrepEngineRequest {
                    max_count: Some(7),
                    ..base.clone()
                },
            ),
            (
                "files mode max count",
                GrepEngineRequest {
                    mode: Some(GrepMode::Files),
                    max_count: Some(9),
                    ..base.clone()
                },
            ),
            (
                "count mode",
                GrepEngineRequest {
                    mode: Some(GrepMode::Count),
                    ..base.clone()
                },
            ),
            (
                "per-file cap",
                GrepEngineRequest {
                    max_count_per_file: Some(1),
                    ..base.clone()
                },
            ),
            (
                "context",
                GrepEngineRequest {
                    context_before: Some(1),
                    context_after: Some(1),
                    ..base.clone()
                },
            ),
            (
                "ignore case literal",
                GrepEngineRequest {
                    ignore_case: Some(true),
                    literal: Some(true),
                    pattern: "needle 3".into(),
                    ..base.clone()
                },
            ),
        ];
        let tree_paths: Vec<String> =
            std::iter::once(root.join("tree").to_string_lossy().into_owned())
                .chain(explicit[..5].iter().cloned())
                .collect();
        for (label, option) in options {
            for paths in [explicit.clone(), tree_paths.clone()] {
                let request = GrepEngineRequest {
                    paths,
                    ..option.clone()
                };
                let mut results = Vec::new();
                for budget in [Some(1), None, Some(6_000)] {
                    let result = crate::grep::rg_engine::search_with_options(
                        request.clone(),
                        &AbortSignal::default(),
                        crate::grep::rg_engine::RgEngineOptions {
                            batch_budget: budget,
                        },
                    )
                    .await
                    .unwrap_or_else(|error| panic!("{label}: {error:?}"));
                    results.push(comparable(&result));
                }
                assert_eq!(results[1], results[0], "{label}: batched");
                assert_eq!(results[2], results[0], "{label}: mid budget");
            }
        }
    }
}
