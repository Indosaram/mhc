use super::*;
use crate::compile::external_projection_names::NAME_NOTE;

fn at(entries: &[(&str, i64)]) -> BTreeMap<String, i64> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_string(), *v))
        .collect()
}

fn paths(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_string()).collect()
}

fn input(times: BTreeMap<String, i64>, cap: usize, bytes: usize) -> ExternalProjectionInput {
    ExternalProjectionInput {
        times,
        limits: ExternalProjectionLimits {
            max_entries_per_directory: cap,
            max_bytes: bytes,
        },
        ..Default::default()
    }
}

#[test]
fn per_directory_cap_shows_the_newest_names_and_counts_the_rest() {
    let p = paths(&["dir/c1.md", "dir/c2.md", "dir/c3.md"]);
    let times = at(&[("dir/c1.md", 1), ("dir/c2.md", 2), ("dir/c3.md", 3)]);
    let text = render_external_projection(&p, &input(times, 2, 0));
    assert!(
        text.lines()
            .any(|line| line == "dir/: c3, c2 (+1 more; read $MEMORY_DIR/dir/ to list)")
    );
}

#[test]
fn disabled_limits_equal_the_render_with_no_input() {
    let p = paths(&[
        "ARCHIVE.md",
        "reference/zeta.md",
        "reference/Alpha.md",
        "reference/project/b.md",
        "reference/project/a.md",
        "people/한글/card.md",
        "notes/n.md",
        "reference/AKIAABCDEFGHIJKLMNOP.md",
    ]);
    let unbounded = format!(
        "<external_projection>\n{NAME_NOTE}\n$MEMORY_DIR/: ARCHIVE\nnotes/: n\npeople/한글/: card\nreference/: ***, Alpha, zeta\nreference/project/: a, b\n</external_projection>"
    );
    let disabled = render_external_projection(
        &p,
        &input(at(&[("reference/zeta.md", 9), ("notes/n.md", 1)]), 0, 0),
    );
    assert_eq!(disabled, unbounded);
    assert_eq!(render_external_projection_unbounded(&p), unbounded);
}

#[test]
fn the_largest_directory_shrinks_first_and_every_line_stays() {
    let mut p: Vec<String> = (0..8)
        .map(|i| format!("big/entry-with-a-long-enough-name-{i}.md"))
        .collect();
    p.extend(paths(&["small/s1.md", "small/s2.md"]));
    let times: BTreeMap<String, i64> = p.iter().cloned().zip(0..).collect();
    let full = render_external_projection(&p, &input(times.clone(), 0, 0));
    let limits = input(times, 0, full.len() - 40);
    let first = render_external_projection(&p, &limits);
    assert!(first.len() <= limits.limits.max_bytes);
    assert_eq!(first, render_external_projection(&p, &limits));
    assert!(first.lines().any(|line| line == "small/: s2, s1"));
}

#[test]
fn shrinking_stops_at_the_first_listing_that_fits() {
    let mut p = paths(&["aa/x1.md", "aa/x2.md"]);
    p.extend((0..30).map(|i| format!("zz/entry-{i:02}.md")));
    let times: BTreeMap<String, i64> = p.iter().cloned().zip(0..).collect();
    let ten = render_external_projection(&p, &input(times.clone(), 10, 0));
    let eleven = render_external_projection(&p, &input(times.clone(), 11, 0));
    let limits = input(times, 0, eleven.len() - 1);
    assert_eq!(render_external_projection(&p, &limits), ten);
    assert!(!render_external_projection_stats(&p, &limits).overflow);
}

#[test]
fn the_smallest_listing_is_found_when_widest_first_shrinking_misses_it() {
    let long = "a-name-long-enough-to-outweigh-its-omitted-names-marker";
    let mut p: Vec<String> = ["a", "b", "c", "d", "e", "f"]
        .iter()
        .map(|name| format!("A/{name}.md"))
        .collect();
    p.extend([format!("B/{long}-1.md"), format!("B/{long}-2.md")]);
    let smallest = [
        "<external_projection>",
        NAME_NOTE,
        "$MEMORY_DIR/",
        "A/: a, b, c, d, e, f",
        "B/: (+2 more; read $MEMORY_DIR/B/ to list)",
        "</external_projection>",
    ]
    .join("\n");
    let limits = input(BTreeMap::new(), 0, smallest.len());
    assert_eq!(render_external_projection(&p, &limits), smallest);
    assert!(!render_external_projection_stats(&p, &limits).overflow);
}

#[test]
fn a_budget_equal_to_the_full_render_omits_nothing() {
    let p = paths(&["a/1.md", "a/2.md", "b/1.md", "b/2.md", "c/1.md"]);
    let times: BTreeMap<String, i64> = p.iter().cloned().zip(0..).collect();
    let full = render_external_projection(&p, &input(times.clone(), 100, 0));
    assert_eq!(
        render_external_projection(&p, &input(times, 100, full.len())),
        full
    );
}

#[test]
fn a_masked_directory_points_at_the_nearest_readable_parent() {
    let p = paths(&[
        "reference/AKIAABCDEFGHIJKLMNOP/a.md",
        "reference/AKIAABCDEFGHIJKLMNOP/b.md",
    ]);
    let text = render_external_projection(&p, &input(BTreeMap::new(), 1, 0));
    assert!(text.contains("reference/***/: a (+1 more; read $MEMORY_DIR/reference/ to list)"));
}

#[test]
fn below_the_floor_the_floor_render_is_returned_and_the_overflow_reported() {
    let long = "a-name-long-enough-to-outweigh-its-omitted-names-marker";
    let mut p = vec![format!("{long}.md")];
    p.extend((0..8).map(|i| format!("a/{long}-{i}.md")));
    p.push(format!("b/{long}.md"));
    let limits = input(BTreeMap::new(), 0, 1);
    let text = render_external_projection(&p, &limits);
    let stats = render_external_projection_stats(&p, &limits);
    assert_eq!(
        text,
        [
            "<external_projection>",
            NAME_NOTE,
            "$MEMORY_DIR/: (+1 more; read $MEMORY_DIR/ to list)",
            "a/: (+8 more; read $MEMORY_DIR/a/ to list)",
            "b/: (+1 more; read $MEMORY_DIR/b/ to list)",
            "</external_projection>",
        ]
        .join("\n")
    );
    assert_eq!(
        (stats.shown, stats.omitted, stats.bytes),
        (0, 10, text.len())
    );
    assert!(stats.overflow && !stats.recency_unavailable);
}

#[test]
fn equal_times_break_ties_by_name_and_untimed_names_come_last() {
    let p = paths(&["d/b.md", "d/a.md", "d/untimed.md", "d/new.md"]);
    let times = at(&[("d/b.md", 5), ("d/a.md", 5), ("d/new.md", 9)]);
    let text = render_external_projection(&p, &input(times, 10, 0));
    assert!(text.lines().any(|line| line == "d/: new, a, b, untimed"));
}

#[test]
fn a_render_within_both_limits_omits_nothing() {
    let p = paths(&["d/a.md", "d/b.md"]);
    let i = input(at(&[("d/a.md", 1), ("d/b.md", 2)]), 40, 24_576);
    let stats = render_external_projection_stats(&p, &i);
    assert_eq!(
        (stats.shown, stats.omitted, stats.bytes, stats.max_bytes),
        (2, 0, render_external_projection(&p, &i).len(), 24_576)
    );
    assert!(!stats.overflow && !stats.recency_unavailable);
}
