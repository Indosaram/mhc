use std::{collections::BTreeSet, future::Future};
#[derive(Debug, Clone)]
pub struct McpListPage<T> {
    pub items: Option<Vec<T>>, pub tools: Option<Vec<T>>, pub resources: Option<Vec<T>>, pub prompts: Option<Vec<T>>, pub next_cursor: Option<String>,
}
impl<T> Default for McpListPage<T> {
    fn default() -> Self { Self { items: None, tools: None, resources: None, prompts: None, next_cursor: None } }
}
#[derive(Debug, PartialEq)]
pub struct McpPaginationResult<T> { pub items: Vec<T>, pub warnings: Vec<String>, pub pages: usize }
pub async fn collect_all_pages<T, E, F, Fut>(mut list: F) -> Result<McpPaginationResult<T>, E>
where F: FnMut(Option<String>) -> Fut, Fut: Future<Output = Result<McpListPage<T>, E>> {
    let mut result = McpPaginationResult { items: Vec::new(), warnings: Vec::new(), pages: 0 };
    let mut seen = BTreeSet::new();
    let mut cursor = None;
    while result.pages < 1000 {
        let page = list(cursor).await?;
        result.pages += 1;
        result.items.extend(page.items.or(page.tools).or(page.resources).or(page.prompts).unwrap_or_default());
        let Some(next) = page.next_cursor.filter(|s| !s.is_empty()) else { return Ok(result); };
        if !seen.insert(next.clone()) { result.warnings.push(format!("Stopped MCP pagination after duplicate cursor '{next}'.")); return Ok(result); }
        cursor = Some(next);
    }
    result.warnings.push("Stopped MCP pagination after 1000 pages.".into());
    Ok(result)
}
