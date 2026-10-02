#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ModelVerification {
    #[serde(rename = "trustedAccessForCyber")]
    Value,
}
