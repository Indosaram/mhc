#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadFoldSettings {
    pub min_body_lines: usize,
    pub min_comment_lines: usize,
    pub min_total_lines: usize,
    pub unfold_until: usize,
    pub unfold_limit: usize,
}
pub const READ_FOLD_SETTINGS: ReadFoldSettings = ReadFoldSettings {
    min_body_lines: 4,
    min_comment_lines: 6,
    min_total_lines: 100,
    unfold_until: 50,
    unfold_limit: 100,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLineRange {
    pub start_line: usize,
    pub end_line: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadBraceScan {
    Parsed { ranges: Vec<ReadLineRange> },
    ParseFailure { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFoldRange {
    pub start_line: usize,
    pub end_line: usize,
    pub children: Vec<ReadFoldRange>,
}
#[derive(Debug, Clone, Copy)]
pub struct ReadFolderInput<'a> {
    pub path: &'a str,
    pub text: &'a str,
    pub settings: ReadFoldSettings,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadFolderResult {
    Parsed {
        text: String,
        ranges: Vec<ReadFoldRange>,
    },
    Unsupported {
        reason: String,
    },
    ParseFailure {
        reason: String,
    },
}
pub trait ReadFolder: Send + Sync {
    fn id(&self) -> &str;
    fn version(&self) -> &str;
    fn fold(&self, input: ReadFolderInput<'_>) -> ReadFolderResult;
}
