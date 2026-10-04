#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Variant0(Box<String>),
    Variant1(Box<serde_json::Number>),
}
