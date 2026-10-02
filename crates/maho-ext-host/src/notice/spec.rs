#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeTone { Accent, Warning, Error, Success, Dim }
impl NoticeTone {
    pub const fn as_str(self) -> &'static str {
        match self { Self::Accent => "accent", Self::Warning => "warning", Self::Error => "error", Self::Success => "success", Self::Dim => "dim" }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeLine { pub text: String, pub tone: Option<NoticeTone> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeSpec {
    pub title: String,
    pub tone: Option<NoticeTone>,
    pub why: String,
    pub extra: Vec<NoticeLine>,
    pub expanded_line: Option<String>,
}
