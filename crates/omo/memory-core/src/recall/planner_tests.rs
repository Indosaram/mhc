use super::{MAX_RECALL_QUERIES, plan_recall_queries};

fn texts(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn given_no_texts_when_planning_then_no_queries() {
    assert!(plan_recall_queries(&[], None).is_empty());
}

#[test]
fn given_only_stopwords_when_planning_then_no_queries() {
    assert!(plan_recall_queries(&texts(&["the and of"]), None).is_empty());
}

#[test]
fn given_korean_stopwords_only_when_planning_then_no_queries() {
    assert!(plan_recall_queries(&texts(&["그냥 그래서 진짜"]), None).is_empty());
}

#[test]
fn given_a_user_text_when_planning_then_terms_are_ranked_and_capped_with_phrases_appended() {
    let queries = plan_recall_queries(&texts(&["deploy pipeline rollback"]), None);
    assert_eq!(
        queries,
        texts(&[
            "pipeline",
            "rollback",
            "\"deploy pipeline\"",
            "\"pipeline rollback\""
        ])
    );
    assert!(queries.len() <= MAX_RECALL_QUERIES);
}

#[test]
fn given_tool_texts_when_planning_then_path_derived_terms_join_the_queries() {
    let queries = plan_recall_queries(&texts(&["deploy pipeline"]), Some(&texts(&["src/deploy.rs"])));
    assert_eq!(
        queries,
        texts(&["pipeline", "deploy", "src", "\"deploy pipeline\""])
    );
}

#[test]
fn given_a_single_kept_term_when_planning_then_the_cap_is_respected() {
    let queries = plan_recall_queries(&texts(&["rollback rollback rollback"]), None);
    assert!(queries.len() <= MAX_RECALL_QUERIES);
    assert_eq!(queries.first().map(String::as_str), Some("rollback"));
}

#[test]
fn given_a_query_with_a_two_character_ascii_token_when_planning_then_it_is_dropped() {
    let queries = plan_recall_queries(&texts(&["go rs rollback"]), None);
    assert!(!queries.iter().any(|query| query == "rs"));
    assert!(!queries.iter().any(|query| query == "go"));
}
