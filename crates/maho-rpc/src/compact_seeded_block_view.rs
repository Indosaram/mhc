//! senpi `modes/rpc/compact-seeded-block-view.ts`: compatibility observation only; acceptance
//! neither parses raw spelling nor assembles public values.

use crate::compact_content_store::{CompactContentStore, utf16_slice};
use crate::compact_read_lease::CompactError;
use crate::compact_seeded_block::FrozenSeededBlock;
use crate::compact_seeded_draft::SeededFields;
use crate::compact_values::CompactValues;
use maho_ai::utils::json_parse::parse_streaming_json;
use serde_json::{Map, Value};

pub async fn read_seeded_fields(
    store: &CompactContentStore,
    fields: &SeededFields,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
) -> Result<Map<String, Value>, CompactError> {
    let values = CompactValues::new(store.clone());
    let mut result = Map::new();
    for (key, reference) in fields {
        let value = values.read(reference).await?;
        valid()?;
        result.insert(key.clone(), value);
    }
    Ok(result)
}

/// Compatibility observation only: acceptance neither parses raw spelling nor assembles public
/// values.
pub async fn read_seeded_block(
    store: &CompactContentStore,
    block: &FrozenSeededBlock,
    text: &str,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
) -> Result<Value, CompactError> {
    let mut fields = read_seeded_fields(store, &block.fields, valid).await?;
    let mut signature = String::new();
    for root in [block.signature.root, block.signature.tail_root] {
        for part in store.text().chunks(root, valid).await? {
            signature.push_str(&part);
        }
    }
    match block.block_type.as_str() {
        "text" => {
            fields.insert("text".into(), Value::String(text.to_owned()));
            if fields.contains_key("textSignature") {
                fields.insert("textSignature".into(), Value::String(signature));
            }
        }
        "thinking" => {
            fields.insert("thinking".into(), Value::String(text.to_owned()));
            if fields.contains_key("thinkingSignature") {
                fields.insert("thinkingSignature".into(), Value::String(signature));
            }
        }
        "toolCall"
            if block.phase == crate::compact_seeded_block::SeededBlockPhase::Open
                && block.parsed_through > 0 =>
        {
            let prefix = utf16_slice(text, 0, block.parsed_through);
            fields.insert("arguments".into(), parse_streaming_json(Some(prefix)));
        }
        "toolCall" => {}
        _ => {}
    }
    Ok(Value::Object(fields))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_seeded_block::{
        SeedBlockInput, SeedFrontier, SeededBlockEvent, fold_seeded_block, frozen_seeded_block,
    };
    use crate::compact_seeded_draft::CompactSeededDraft;
    use serde_json::json;

    #[tokio::test]
    async fn reads_open_toolcall_streaming_arguments_at_parse_frontier() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let ok = || -> Result<(), CompactError> { Ok(()) };
        let frozen = {
            let mut draft = CompactSeededDraft::new(store.clone());
            let seeded = fold_seeded_block(
                &mut draft,
                None,
                &SeededBlockEvent::ToolcallStart {
                    content_index: 0,
                    id: "tc1".into(),
                    tool_name: "read".into(),
                    seed: SeedBlockInput {
                        block: json!({"type":"toolCall","id":"tc1","name":"read","arguments":{}}),
                        frontier: SeedFrontier {
                            covered_code_units: 0,
                            next_offset: Some(9),
                            raw_argument_prefix: Some("{\"a\":1,\"b".into()),
                            parsed_through: Some(6),
                        },
                    },
                },
            )
            .expect("start");
            let frozen = frozen_seeded_block(&seeded);
            for owner in crate::compact_seeded_block::seeded_block_leaves(&frozen) {
                store.adjust(owner, 1, 0).expect("retain");
            }
            frozen
        };
        let value = read_seeded_block(&store, &frozen, "{\"a\":1,\"b", &ok)
            .await
            .expect("read");
        assert_eq!(value["arguments"], json!({"a": 1}));
        for owner in crate::compact_seeded_block::seeded_block_leaves(&frozen) {
            store.adjust(owner, -1, 0).expect("release");
        }
        store.retire().await.expect("retire");
    }
}
