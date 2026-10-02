#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ForcedChatgptWorkspaceIds {
    Variant0(Box<String>),
    Variant1(Box<Vec<String>>),
}
