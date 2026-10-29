use pretty_assertions::assert_eq;

use super::*;

struct FakeProvider {
    conversations: Vec<TranscriptConversation>,
}

impl TranscriptProvider for FakeProvider {
    fn list_conversations(&self) -> Vec<TranscriptConversation> {
        self.conversations.clone()
    }
}

fn fake_provider(conversations: Vec<TranscriptConversation>) -> FakeProvider {
    FakeProvider { conversations }
}

fn conversation(
    id: &str,
    messages: Vec<SearchDocument>,
    hidden: Option<bool>,
) -> TranscriptConversation {
    TranscriptConversation {
        id: id.to_string(),
        hidden,
        messages,
    }
}

fn message(
    id: &str,
    conversation_id: Option<&str>,
    content: &str,
    date: Option<&str>,
) -> SearchDocument {
    SearchDocument {
        id: id.to_string(),
        conversation_id: conversation_id.unwrap_or("c1").to_string(),
        date: date.map(str::to_string),
        message_type: Some("user_message".to_string()),
        content: Some(serde_json::Value::String(content.to_string())),
        reasoning: None,
        summary: None,
        tool_calls: None,
        tool_return: None,
        func_response: None,
    }
}

#[test]
fn test_search_transcripts_when_different_first_match_indices_then_results_sort_by_ascending_score()
{
    let provider = fake_provider(vec![conversation(
        "c1",
        vec![
            message("far", None, "alpha gamma delta beta", None),
            message("near", None, "beta later", None),
        ],
        None,
    )]);

    let results = search_transcripts(&provider, "beta", &SearchOptions::default());

    let ids: Vec<String> = results.iter().map(|r| r.message_id.clone()).collect();
    assert_eq!(ids, vec!["near", "far"]);
    assert_eq!(results[0].score, (13 + 46) as f64);
    assert_eq!(results[1].score, (31 + 46) as f64);
}

#[test]
fn test_search_transcripts_when_equal_scores_then_ties_break_on_descending_date() {
    let provider = fake_provider(vec![conversation(
        "c1",
        vec![
            message("older", None, "beta", Some("2026-01-01T00:00:00.000Z")),
            message("newer", None, "beta", Some("2026-05-01T00:00:00.000Z")),
        ],
        None,
    )]);

    let results = search_transcripts(&provider, "beta", &SearchOptions::default());

    assert_eq!(results[0].score, results[1].score);
    let ids: Vec<String> = results.iter().map(|r| r.message_id.clone()).collect();
    assert_eq!(ids, vec!["newer", "older"]);
}

#[test]
fn test_search_transcripts_when_more_matches_than_limit_then_default_limit_is_100_and_explicit_limits_truncate()
 {
    let messages: Vec<SearchDocument> = (0..120)
        .map(|index| {
            let date = format!("2026-01-01T00:00:{:02}.000Z", index % 60);
            message(&format!("m{index}"), None, "beta", Some(&date))
        })
        .collect();
    let provider = fake_provider(vec![conversation("c1", messages, None)]);

    let defaulted = search_transcripts(&provider, "beta", &SearchOptions::default());
    let limited = search_transcripts(
        &provider,
        "beta",
        &SearchOptions {
            limit: Some(5),
            ..Default::default()
        },
    );
    let zero = search_transcripts(
        &provider,
        "beta",
        &SearchOptions {
            limit: Some(0),
            ..Default::default()
        },
    );

    assert_eq!(defaulted.len(), 100);
    assert_eq!(limited.len(), 5);
    assert_eq!(zero.is_empty(), true);
}

#[test]
fn test_search_transcripts_when_empty_or_term_free_query_then_no_results_returned() {
    let provider = fake_provider(vec![conversation(
        "c1",
        vec![message("m1", None, "beta", None)],
        None,
    )]);

    let blank = search_transcripts(&provider, "   ", &SearchOptions::default());
    let empty_quotes = search_transcripts(&provider, "\"\"", &SearchOptions::default());

    assert_eq!(blank.is_empty(), true);
    assert_eq!(empty_quotes.is_empty(), true);
}

#[test]
fn test_search_transcripts_when_date_bounds_then_bounds_are_inclusive_and_undated_messages_stay_eligible()
 {
    let provider = fake_provider(vec![conversation(
        "c1",
        vec![
            message("before", None, "beta", Some("2025-12-31T23:59:59.000Z")),
            message("onStart", None, "beta", Some("2026-01-01T00:00:00.000Z")),
            message("after", None, "beta", Some("2026-03-01T00:00:00.000Z")),
            message("undated", None, "beta", None),
        ],
        None,
    )]);

    let results = search_transcripts(
        &provider,
        "beta",
        &SearchOptions {
            start_date: Some("2026-01-01T00:00:00.000Z".to_string()),
            end_date: Some("2026-02-01T00:00:00.000Z".to_string()),
            ..Default::default()
        },
    );

    let mut ids: Vec<String> = results.iter().map(|r| r.message_id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec!["onStart", "undated"]);
}

#[test]
fn test_search_transcripts_when_hidden_conversations_then_excluded_unless_include_hidden_set() {
    let provider = fake_provider(vec![
        conversation("c1", vec![message("visible", None, "beta", None)], None),
        conversation(
            "c2",
            vec![message("concealed", Some("c2"), "beta", None)],
            Some(true),
        ),
    ]);

    let defaulted = search_transcripts(&provider, "beta", &SearchOptions::default());
    let with_hidden = search_transcripts(
        &provider,
        "beta",
        &SearchOptions {
            include_hidden: Some(true),
            ..Default::default()
        },
    );

    let defaulted_ids: Vec<String> = defaulted.iter().map(|r| r.message_id.clone()).collect();
    assert_eq!(defaulted_ids, vec!["visible"]);

    let mut with_hidden_ids: Vec<String> =
        with_hidden.iter().map(|r| r.message_id.clone()).collect();
    with_hidden_ids.sort();
    assert_eq!(with_hidden_ids, vec!["concealed", "visible"]);
}

#[test]
fn test_search_transcripts_when_conversation_filter_then_only_that_conversation_contributes_results()
 {
    let provider = fake_provider(vec![
        conversation("c1", vec![message("m1", None, "beta", None)], None),
        conversation("c2", vec![message("m2", Some("c2"), "beta", None)], None),
    ]);

    let results = search_transcripts(
        &provider,
        "beta",
        &SearchOptions {
            conversation_id: Some("c2".to_string()),
            ..Default::default()
        },
    );

    let ids: Vec<String> = results.iter().map(|r| r.message_id.clone()).collect();
    assert_eq!(ids, vec!["m2"]);
    assert_eq!(results[0].conversation_id, "c2");
}

#[test]
fn test_search_transcripts_when_phrase_plus_term_query_then_only_documents_matching_every_term_and_phrase_returned()
 {
    let provider = fake_provider(vec![conversation(
        "c1",
        vec![
            message("both", None, "deploy the quick brown fox", None),
            message("phraseOnly", None, "the quick brown fox", None),
            message("termOnly", None, "deploy the brown quick fox", None),
        ],
        None,
    )]);

    let results = search_transcripts(
        &provider,
        "deploy \"quick brown\"",
        &SearchOptions::default(),
    );

    let ids: Vec<String> = results.iter().map(|r| r.message_id.clone()).collect();
    assert_eq!(ids, vec!["both"]);
}

#[test]
fn test_search_transcripts_when_matched_document_then_identity_fields_are_carried_through() {
    let provider = fake_provider(vec![conversation(
        "c1",
        vec![message(
            "m1",
            None,
            "beta",
            Some("2026-04-01T10:00:00.000Z"),
        )],
        None,
    )]);

    let results = search_transcripts(&provider, "beta", &SearchOptions::default());

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].message_id, "m1");
    assert_eq!(results[0].conversation_id, "c1");
    assert_eq!(results[0].created_at, "2026-04-01T10:00:00.000Z");
    assert_eq!(
        results[0].document.content,
        Some(serde_json::Value::String("beta".to_string()))
    );
}
