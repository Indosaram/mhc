use super::*;
use crate::compile::external_projection::{
    ExternalProjectionInput, ExternalProjectionLimits, render_external_projection,
    render_external_projection_stats,
};
use crate::compile::external_projection_names::NAME_LEGEND;

const AWS_KEY_SHAPED: &str = concat!("AKIA", "ABCDEFGHIJKLMNOP");
const IDS: [&str; 3] = ["Y2Gw9vTZ9pnH2K5a", "87U3F2ZseCw7bLMz", "2Fc9uTN3x7DPCCxs"];

fn labels(paths: &[String]) -> Vec<String> {
    group_directories(paths, None)
        .into_iter()
        .map(|group| group.label)
        .collect()
}

fn paths(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn directory_classes_have_exact_line_labels() {
    let id_paths: Vec<String> = IDS
        .iter()
        .map(|id| format!("jobs/{id}/MEMORY.md"))
        .collect();
    assert_eq!(labels(&id_paths), ["jobs/<id>/ (3 dirs)"]);

    assert_eq!(
        labels(&id_paths[..2]),
        {
            let mut expected = vec!["jobs/Y2Gw9vTZ9pnH2K5a/", "jobs/87U3F2ZseCw7bLMz/"];
            expected.sort_by(|a, b| locale_compare(a, b));
            expected
        },
        "two leaf siblings named by ids"
    );
    assert_eq!(
        labels(&paths(&[
            "people/cy/card.md",
            "people/ann/card.md",
            "people/bob/card.md"
        ])),
        ["people/{ann, bob, cy}/"]
    );
    assert_eq!(
        labels(&paths(&[
            "people/ann/card.md",
            "people/bob/card.md",
            "people/cy/notes.md"
        ])),
        ["people/ann/", "people/bob/", "people/cy/"]
    );
    let mut below = id_paths.clone();
    below.push(format!("jobs/{}/runs/log.md", IDS[0]));
    assert_eq!(
        labels(&below),
        [
            "jobs/2Fc9uTN3x7DPCCxs/",
            "jobs/87U3F2ZseCw7bLMz/",
            "jobs/Y2Gw9vTZ9pnH2K5a/",
            "jobs/Y2Gw9vTZ9pnH2K5a/runs/"
        ]
    );
    let masked = paths(&[
        "people/ann/card.md",
        "people/bob/card.md",
        &format!("people/{AWS_KEY_SHAPED}/card.md"),
    ]);
    assert_eq!(
        labels(&masked),
        ["people/***/", "people/ann/", "people/bob/"]
    );
    assert_eq!(
        labels(&paths(&["ARCHIVE.md", "notes/n.md"])),
        ["$MEMORY_DIR/", "notes/"]
    );
}

#[test]
fn id_named_directories_render_one_line_under_the_legend() {
    let id_paths: Vec<String> = IDS
        .iter()
        .map(|id| format!("reference/jobs/{id}/MEMORY.md"))
        .collect();
    let input = ExternalProjectionInput {
        limits: ExternalProjectionLimits {
            max_entries_per_directory: 0,
            max_bytes: 40_960,
        },
        ..Default::default()
    };
    let text = render_external_projection(&id_paths, &input);
    let stats = render_external_projection_stats(&id_paths, &input);
    assert_eq!(text.lines().nth(1), Some(NAME_LEGEND));
    assert!(
        text.lines()
            .any(|line| line == "reference/jobs/<id>/ (3 dirs): MEMORY")
    );
    assert_eq!((stats.shown, stats.omitted), (3, 0));
}
