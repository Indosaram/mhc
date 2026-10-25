use super::*;
use crate::provenance::consent::*;
use serde_json::json;
fn source(said: &str, quote: &str) -> ClaimReceipt {
    resolve_claim_receipt(
        &ClaimInput {
            source: Some(json!("user")),
            user_quote: Some(json!(quote)),
        },
        &[ConsentExchange {
            proposal: "I suggest removing plugins X and Y.".into(),
            reply: said.into(),
        }],
    )
}
fn user(receipt: ClaimReceipt) -> bool {
    matches!(receipt, ClaimReceipt::User { .. })
}
#[test]
fn closed_consent_corpus() {
    for phrase in CONSENT_PHRASES_KO.iter().chain(CONSENT_PHRASES_EN.iter()) {
        for said in [
            phrase.to_string(),
            format!("{phrase}."),
            format!("{phrase}!"),
            format!(" {} ", phrase.to_uppercase()),
        ] {
            assert!(user(source(&said, phrase)), "{said:?}");
        }
        for said in [
            format!("{phrase} now"),
            format!("hmm {phrase}"),
            format!("{phrase}?"),
        ] {
            assert!(!user(source(&said, phrase)), "{said:?}");
        }
    }
    let rejected: Vec<String> = serde_json::from_str(include_str!("rejected.json")).unwrap();
    for said in rejected {
        assert!(!user(source(&said, &said)), "{said:?}");
    }
    let rows: Vec<Value> = serde_json::from_str(include_str!("review_rows.json")).unwrap();
    for row in rows {
        let phrase = row["phrase"].as_str().unwrap();
        assert_eq!(
            user(source(phrase, phrase)),
            is_consent_phrase(&bare_message(phrase)),
            "{phrase:?}"
        );
    }
}
#[test]
fn resolves_and_round_trips_claims() {
    let receipt = source("Yes delete it!", "  YES   delete IT ");
    assert_eq!(
        receipt,
        ClaimReceipt::User {
            user_quote: "Yes delete it!".into(),
            approves: proposal_id("I suggest removing plugins X and Y.")
        }
    );
    for quote in ["remove plugins X and Y", "go ahead", ""] {
        assert_eq!(
            source("Yes delete it!", quote),
            ClaimReceipt::Agent {
                downgraded: Some(true)
            }
        );
    }
    assert!(!user(source(
        "Please look at the WordPress plugins and tell me which ones look unused.",
        "Please look at the WordPress plugins and tell me which ones look unused."
    )));
    for (input, expected) in [
        (json!({}), ClaimReceipt::Inferred),
        (json!({"source":"boss"}), ClaimReceipt::Inferred),
        (
            json!({"source":"agent","userQuote":"yes"}),
            ClaimReceipt::Agent { downgraded: None },
        ),
    ] {
        assert_eq!(
            resolve_claim_receipt(&serde_json::from_value(input).unwrap(), &[]),
            expected
        );
    }
    assert_eq!(
        parse_claim_receipt(&serde_json::to_value(&receipt).unwrap()),
        Some(receipt)
    );
    for invalid in [
        json!({"source":"user","userQuote":"ok"}),
        json!({"source":"user","userQuote":"ok","approves":"not-a-hash"}),
        json!({"source":"agent","downgraded":false}),
        json!({"source":"inferred","extra":true}),
    ] {
        assert!(parse_claim_receipt(&invalid).is_none());
    }
    assert_eq!(
        proposal_id("  I suggest\nremoving plugins X and Y. "),
        proposal_id("I suggest removing plugins X and Y.")
    );
    assert_eq!(
        claim_receipt_trailers(&ClaimReceipt::User {
            user_quote: "a\nb".into(),
            approves: "0123456789abcdef".into()
        }),
        vec![
            "Omo-Claim-Source: user",
            "Omo-User-Quote: a b",
            "Omo-Claim-Approves: 0123456789abcdef"
        ]
    );
    assert_eq!(
        claim_receipt_trailers(&ClaimReceipt::Agent {
            downgraded: Some(true)
        }),
        vec![
            "Omo-Claim-Source: agent",
            "Omo-Claim-Downgraded: unverified-user-quote"
        ]
    );
}
