//! Model picker searchable fields from pinned senpi model-search.ts.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ModelSearchItem {
    pub id: String,
    pub provider: String,
    pub name: Option<String>,
}

pub fn get_model_search_text(item: &ModelSearchItem) -> String {
    let name = item.name.as_deref().filter(|name| !name.is_empty())
        .map(|name| format!(" {name}")).unwrap_or_default();
    let ModelSearchItem { id, provider, .. } = item;
    format!("{id} {provider} {provider}/{id} {provider} {id}{name}")
}
