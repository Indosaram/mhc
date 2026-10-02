use maho_core::model_runtime::ModelRuntime;
pub fn format_token_count(count: u64) -> String {
    let count = count as f64;
    let (value, suffix) = if count >= 1_000_000.0 { (count / 1_000_000.0, "M") } else if count >= 1_000.0 { (count / 1_000.0, "K") } else { return count.to_string(); };
    if value.fract() == 0.0 { format!("{value}{suffix}") } else { format!("{value:.1}{suffix}") }
}
pub fn list_models(runtime: &ModelRuntime, search: Option<&str>) -> String {
    let mut models = runtime.get_models(None);
    if let Some(search) = search.filter(|s| !s.is_empty()) { models.retain(|model| maho_tui::fuzzy::fuzzy_match(search, &format!("{} {}", model.provider, model.id)).matches); }
    if models.is_empty() { return search.filter(|s| !s.is_empty()).map_or_else(maho_core::auth_guidance::format_no_models_available_message, |s| format!("No models matching \"{s}\"")); }
    models.sort_by(|left, right| left.provider.cmp(&right.provider).then_with(|| left.id.cmp(&right.id)));
    let mut rows = vec![["provider".to_owned(), "model".to_owned(), "context".to_owned(), "max-out".to_owned(), "thinking".to_owned(), "images".to_owned()]];
    for model in models { rows.push([model.provider, model.id, format_token_count(model.context_window), format_token_count(model.max_tokens), if model.reasoning { "yes" } else { "no" }.to_owned(), if model.input.contains(&maho_ai::types::InputModality::Image) { "yes" } else { "no" }.to_owned()]); }
    let widths: [usize; 6] = std::array::from_fn(|i| rows.iter().map(|row| row[i].encode_utf16().count()).max().unwrap_or(0));
    rows.into_iter().map(|row| row.into_iter().enumerate().map(|(i, cell)| { let padding = widths[i].saturating_sub(cell.encode_utf16().count()); format!("{cell}{}", " ".repeat(padding)) }).collect::<Vec<_>>().join("  ")).collect::<Vec<_>>().join("\n")
}
