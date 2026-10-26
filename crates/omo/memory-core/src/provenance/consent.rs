pub const CONSENT_PHRASES_KO: [&str; 10] = [
    "응",
    "네",
    "예",
    "좋아",
    "그래",
    "지워",
    "지워줘",
    "삭제해",
    "삭제해줘",
    "진행해",
];
pub const CONSENT_PHRASES_EN: [&str; 7] = [
    "yes",
    "ok",
    "okay",
    "do it",
    "go ahead",
    "delete it",
    "yes delete it",
];
pub fn is_consent_phrase(phrase: &str) -> bool {
    CONSENT_PHRASES_KO.contains(&phrase) || CONSENT_PHRASES_EN.contains(&phrase)
}
