use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Position { pub line: f64, pub column: f64 }
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ByteOffset { pub start: f64, pub end: f64 }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Range { pub start: Position, pub end: Position, pub byte_offset: ByteOffset }
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CharCount { pub leading: f64, pub trailing: f64 }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliMatch { pub text: String, pub range: Range, pub file: String, pub lines: String, pub char_count: CharCount, pub language: String }
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TruncationReason { MaxMatches, MaxOutputBytes, Timeout }
#[derive(Default, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SgResult {
    pub matches: Vec<CliMatch>, pub total_matches: usize, pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")] pub truncated_reason: Option<TruncationReason>,
    #[serde(skip_serializing_if = "Option::is_none")] pub error: Option<String>,
}
