#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum FunctionCallOutputBody {
    Variant0(Box<String>),
    Variant1(Box<Vec<Box<crate::app_server::protocol::generated::function_call_output_content_item::FunctionCallOutputContentItem>>>),
}
