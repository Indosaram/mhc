//! File-backed team mailbox: send, inbox listing, reservation, poll-injection and ack.

pub mod ack;
pub mod consumed_ledger;
pub mod consumer_lease;
pub mod inbox;
pub mod pending_delivery_recovery;
pub mod poll;
pub mod reservation;
pub mod send;

pub use ack::ack_messages;
pub use consumed_ledger::is_message_consumed;
pub use consumer_lease::{InboxConsumerLeaseOptions, with_inbox_consumer_lease};
pub use inbox::list_unread_messages;
pub use pending_delivery_recovery::{find_delivered_message_ids, requeue_pending_live_deliveries};
pub use poll::{InjectionResult, build_envelope, poll_and_build_injection};
pub use reservation::{
    DeliveryReservation, commit_delivery_reservation, reclaim_stale_reservations,
    release_delivery_reservation, reserve_message_for_delivery,
};
pub use send::{SendContext, SendResult, send_message};
