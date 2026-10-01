//! `team/messaging/delivery-journal.test.ts`

use pretty_assertions::assert_eq;
use team_core::types::{Message, MessageKind};

use crate::team::messaging::delivery_journal::{LeadDeliveryJournalOptions, create_lead_delivery_journal};

const TEAM: &str = "00000000-0000-4000-8000-000000000000";
const OTHER_TEAM: &str = "11111111-1111-4111-8111-111111111111";

fn message(id: &str, from: &str) -> Message {
    Message {
        version: 1,
        message_id: id.to_string(),
        from: from.to_string(),
        to: "lead".to_string(),
        kind: MessageKind::Message,
        body: format!("body-{id}"),
        summary: None,
        references: None,
        timestamp: 1,
        correlation_id: None,
        color: None,
    }
}

fn taken_id(message: Option<Message>) -> Option<String> {
    message.map(|message| message.message_id)
}

#[test]
fn given_recorded_messages_when_drained_then_it_returns_the_oldest_unreported_first() {
    let journal = create_lead_delivery_journal(LeadDeliveryJournalOptions::default());
    journal.record(TEAM, message("m1", "alpha"));
    journal.record(TEAM, message("m2", "alpha"));

    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, None)), Some("m1".to_string()));
    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, None)), Some("m2".to_string()));
    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, None)), None);
}

#[test]
fn given_mixed_senders_when_drained_with_a_from_filter_then_only_matching_messages_are_taken() {
    let journal = create_lead_delivery_journal(LeadDeliveryJournalOptions::default());
    journal.record(TEAM, message("m1", "alpha"));
    journal.record(TEAM, message("m2", "beta"));

    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, Some("beta"))), Some("m2".to_string()));
    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, Some("beta"))), None);
    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, Some("alpha"))), Some("m1".to_string()));
}

#[test]
fn given_two_teams_when_drained_then_deliveries_stay_isolated_per_team() {
    let journal = create_lead_delivery_journal(LeadDeliveryJournalOptions::default());
    journal.record(TEAM, message("m1", "alpha"));

    assert_eq!(taken_id(journal.take_oldest_unreported(OTHER_TEAM, None)), None);
    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, None)), Some("m1".to_string()));
}

#[test]
fn given_a_reported_message_that_is_re_recorded_when_drained_then_it_is_unreported_again() {
    let journal = create_lead_delivery_journal(LeadDeliveryJournalOptions::default());
    journal.record(TEAM, message("m1", "alpha"));
    journal.mark_reported(TEAM, "m1");
    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, None)), None);

    // the same message is reserved again (recovery redelivery)
    journal.record(TEAM, message("m1", "alpha"));

    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, None)), Some("m1".to_string()));
}

#[test]
fn given_a_dropped_team_when_drained_then_nothing_remains() {
    let journal = create_lead_delivery_journal(LeadDeliveryJournalOptions::default());
    journal.record(TEAM, message("m1", "alpha"));

    journal.drop_team(TEAM);

    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, None)), None);
}

#[test]
fn given_more_deliveries_than_the_cap_when_drained_then_the_oldest_are_evicted_and_order_is_preserved() {
    let journal = create_lead_delivery_journal(LeadDeliveryJournalOptions { max_per_team: Some(3) });
    for id in ["m1", "m2", "m3", "m4", "m5"] {
        journal.record(TEAM, message(id, "alpha"));
    }

    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, None)), Some("m3".to_string()));
    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, None)), Some("m4".to_string()));
    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, None)), Some("m5".to_string()));
    assert_eq!(taken_id(journal.take_oldest_unreported(TEAM, None)), None);
}
