//! Port of senpi `websearch/websearch/providers/public.ts`.
use crate::websearch::provider_endpoints::SearchProvider;
use crate::websearch::providers::shared::SearchResultItem;
use std::collections::HashMap;

pub const PUBLIC_ENGINES: [SearchProvider; 5] = [
    SearchProvider::Startpage,
    SearchProvider::GoogleHtml,
    SearchProvider::DuckduckgoHtml,
    SearchProvider::Ecosia,
    SearchProvider::Mojeek,
];

pub const PUBLIC_SOFT_DEADLINE_MS: u64 = 5_000;
pub const PUBLIC_HARD_DEADLINE_MS: u64 = 30_000;

pub fn public_dedupe_key(raw_url: &str) -> String {
    let Ok(url) = url::Url::parse(raw_url) else {
        return raw_url.to_owned();
    };
    let host = url.host_str().unwrap_or_default().to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let path = url.path();
    let path = if path.len() > 1 && path.ends_with('/') {
        &path[..path.len() - 1]
    } else {
        path
    };
    let query = url
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    format!("{host}{path}{query}")
}

struct MergedResult {
    item: SearchResultItem,
    engines: usize,
    best_rank: usize,
    order: usize,
}

fn merge_results(
    merged: &mut HashMap<String, MergedResult>,
    next_order: &mut usize,
    results: &[SearchResultItem],
) {
    for (rank, item) in results.iter().enumerate() {
        let key = public_dedupe_key(&item.url);
        match merged.get_mut(&key) {
            None => {
                merged.insert(
                    key,
                    MergedResult {
                        item: item.clone(),
                        engines: 1,
                        best_rank: rank,
                        order: *next_order,
                    },
                );
                *next_order += 1;
            }
            Some(existing) => {
                existing.engines += 1;
                if rank < existing.best_rank {
                    existing.best_rank = rank;
                    existing.item.title = item.title.clone();
                    existing.item.url = item.url.clone();
                }
                let longer = item.snippet.as_ref().map(String::len).unwrap_or(0)
                    > existing.item.snippet.as_ref().map(String::len).unwrap_or(0);
                if longer {
                    existing.item.snippet = item.snippet.clone();
                }
                if existing.item.published_at.is_none() {
                    existing.item.published_at = item.published_at.clone();
                }
            }
        }
    }
}

pub fn merge_engine_results(
    answers: &[Option<Vec<SearchResultItem>>],
    max_results: f64,
) -> (Vec<SearchResultItem>, bool) {
    let mut merged = HashMap::new();
    let mut next_order = 0;
    for results in answers.iter().flatten() {
        merge_results(&mut merged, &mut next_order, results);
    }
    let mut ranked = merged.into_values().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .engines
            .cmp(&left.engines)
            .then(left.best_rank.cmp(&right.best_rank))
            .then(left.order.cmp(&right.order))
    });
    let limit = max_results.max(0.0) as usize;
    let truncated = ranked.len() > limit;
    ranked.truncate(limit);
    (
        ranked.into_iter().map(|entry| entry.item).collect(),
        truncated,
    )
}

pub fn all_engines_failed_error(failures: &[Option<String>], duration_ms: f64) -> String {
    let reasons = PUBLIC_ENGINES
        .iter()
        .enumerate()
        .map(|(index, provider)| {
            let reason = failures
                .get(index)
                .and_then(Clone::clone)
                .unwrap_or_else(|| format!("no answer within {duration_ms}ms"));
            format!("{}: {reason}", provider.as_str())
        })
        .collect::<Vec<_>>();
    format!(
        "Search provider public failed: every engine failed ({})",
        reasons.join("; ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::websearch::providers::shared::result;

    fn item(title: &str, url: &str, snippet: Option<&str>) -> SearchResultItem {
        result(Some(title), Some(url), snippet, None, None).expect("result")
    }

    #[test]
    fn dedupe_key_normalizes_host_case_www_trailing_slash_and_fragment() {
        assert_eq!(
            public_dedupe_key("HTTPS://WWW.Example.COM/a/?q=1#frag"),
            "example.com/a?q=1"
        );
        assert_eq!(public_dedupe_key("https://a.test"), "a.test/");
        assert_eq!(public_dedupe_key("not a url"), "not a url");
    }

    #[test]
    fn ranks_by_engine_count_then_best_rank_then_engine_order() {
        let startpage = vec![
            item("A1", "https://a.test", None),
            item("B1", "https://b.test", None),
        ];
        let mojeek = vec![item("A2", "https://www.a.test/", Some("longer snippet"))];
        let (results, truncated) =
            merge_engine_results(&[Some(startpage), None, None, None, Some(mojeek)], 10.0);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].url, "https://a.test");
        assert_eq!(results[0].title, "A1");
        assert_eq!(results[0].snippet.as_deref(), Some("longer snippet"));
        assert_eq!(results[1].url, "https://b.test");
        assert!(!truncated);
    }

    #[test]
    fn breaks_ties_by_earlier_engine_order() {
        let startpage = vec![item("A", "https://a.test", None)];
        let ecosia = vec![item("B", "https://b.test", None)];
        let (results, _) =
            merge_engine_results(&[Some(startpage), None, None, Some(ecosia), None], 10.0);
        assert_eq!(results[0].url, "https://a.test");
        assert_eq!(results[1].url, "https://b.test");
    }

    #[test]
    fn truncates_to_the_result_limit() {
        let engine = (0..3)
            .map(|index| item("T", &format!("https://{index}.test"), None))
            .collect::<Vec<_>>();
        let (results, truncated) = merge_engine_results(&[Some(engine)], 2.0);
        assert_eq!(results.len(), 2);
        assert!(truncated);
    }

    #[test]
    fn all_failed_message_names_every_engine_in_order() {
        let failures = vec![
            Some("blocked".to_string()),
            None,
            None,
            Some("timeout".to_string()),
            None,
        ];
        let message = all_engines_failed_error(&failures, 1_234.0);
        assert_eq!(
            message,
            "Search provider public failed: every engine failed (\
             startpage: blocked; google-html: no answer within 1234ms; \
             duckduckgo-html: no answer within 1234ms; ecosia: timeout; \
             mojeek: no answer within 1234ms)"
        );
    }
}
