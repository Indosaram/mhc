use maho_interactive::model_search::{ModelSearchItem, get_model_search_text};
use maho_interactive::model_search_rank::rank_model_search_items;

#[derive(serde::Deserialize)]
struct Case {
    query: String,
    #[serde(rename = "favoritesFirst")]
    favorites_first: bool,
    favorites: Vec<String>,
    expected: Vec<String>,
}

#[derive(serde::Deserialize)]
struct Golden { models: Vec<ModelSearchItem>, cases: Vec<Case>, text: Vec<String> }

#[test]
fn search_ranking_matches_pinned_catalog_queries_and_favorite_partitions() {
    let golden: Golden = serde_json::from_str(include_str!("golden/task33-search.json")).unwrap();
    for case in golden.cases {
        let ranked = rank_model_search_items(&golden.models, &case.query, Clone::clone,
            case.favorites_first, |item| case.favorites.contains(&format!("{}/{}", item.provider, item.id)));
        let ids: Vec<_> = ranked.iter().map(|item| format!("{}/{}", item.provider, item.id)).collect();
        assert_eq!(ids, case.expected, "query {:?}, favorites {}", case.query, case.favorites_first);
    }
}

#[test]
fn search_fields_match_pinned_model_search_text() {
    let golden: Golden = serde_json::from_str(include_str!("golden/task33-search.json")).unwrap();
    assert_eq!(golden.models.iter().map(get_model_search_text).collect::<Vec<_>>(), golden.text);
}
