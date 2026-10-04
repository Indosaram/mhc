#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RealtimeVoice {
    #[serde(rename = "alloy")]
    Alloy,
    #[serde(rename = "arbor")]
    Arbor,
    #[serde(rename = "ash")]
    Ash,
    #[serde(rename = "ballad")]
    Ballad,
    #[serde(rename = "breeze")]
    Breeze,
    #[serde(rename = "cedar")]
    Cedar,
    #[serde(rename = "coral")]
    Coral,
    #[serde(rename = "cove")]
    Cove,
    #[serde(rename = "echo")]
    Echo,
    #[serde(rename = "ember")]
    Ember,
    #[serde(rename = "juniper")]
    Juniper,
    #[serde(rename = "maple")]
    Maple,
    #[serde(rename = "marin")]
    Marin,
    #[serde(rename = "sage")]
    Sage,
    #[serde(rename = "shimmer")]
    Shimmer,
    #[serde(rename = "sol")]
    Sol,
    #[serde(rename = "spruce")]
    Spruce,
    #[serde(rename = "vale")]
    Vale,
    #[serde(rename = "verse")]
    Verse,
}
