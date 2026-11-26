use super::types::{ModelCapabilitiesSnapshotEntry, SnapshotLimit, SnapshotModalities};

pub(super) fn gpt_6_2_entries() -> [(String, ModelCapabilitiesSnapshotEntry); 4] {
    [
        "gpt-6.2-astra",
        "gpt-6.2-astra-fast",
        "gpt-6.2-sol",
        "gpt-6.2-sol-fast",
    ]
    .map(|id| {
        (
            id.to_owned(),
            ModelCapabilitiesSnapshotEntry {
                id: id.to_owned(),
                family: Some("gpt".to_owned()),
                reasoning: Some(true),
                temperature: Some(false),
                tool_call: Some(true),
                modalities: Some(SnapshotModalities {
                    input: Some(vec!["text".to_owned(), "image".to_owned()]),
                    output: Some(vec!["text".to_owned()]),
                }),
                limit: Some(SnapshotLimit {
                    context: Some(1_050_000),
                    input: Some(922_000),
                    output: Some(128_000),
                }),
            },
        )
    })
}
