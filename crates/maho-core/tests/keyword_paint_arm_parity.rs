//! The editor paints the ultrawork keyword exactly where the ultrawork engine arms.

use maho_core::reserved_keywords::{ReservedKeyword, find_reserved_keyword_spans};
use maho_ultrawork::has_visible_keyword;

const ALIASES: [&str; 11] = [
    "ulw",
    "ultrawork",
    "mass ulw",
    "mass-ulw",
    "mass_ulw",
    "massulw",
    "ulw mass",
    "mulw",
    "meth",
    "ULW",
    "Meth",
];
const LEFT: [&str; 16] = [
    "", " ", "\n", "(", "**", "#", "~", "x=", "\u{e9}", "\u{c6b8}", "/", "`", "\"", "-", "_", "a",
];
const RIGHT: [&str; 16] = [
    "",
    " ",
    ".",
    ",",
    ")",
    "**",
    "=1",
    "\u{2014}fix",
    "\u{2026}",
    "\u{d574}\u{c918}",
    "-loop",
    "-research",
    "_x",
    "a",
    "`",
    "/",
];

/// Painted as the ultrawork or mass keyword; the skill keywords (`ulw-loop`, ...) do not count.
fn painted(text: &str) -> bool {
    find_reserved_keyword_spans(text).iter().any(|span| {
        matches!(
            span.keyword,
            ReservedKeyword::Ultrawork | ReservedKeyword::MassUlw
        )
    })
}

#[test]
fn painted_equals_armed_for_every_alias_and_neighbour() {
    for alias in ALIASES {
        for left in LEFT {
            for right in RIGHT {
                let text = format!("{left}{alias}{right}");
                assert_eq!(painted(&text), has_visible_keyword(&text), "{text:?}");
            }
        }
    }
}

#[test]
fn plain_word_boundaries_arm_and_paint() {
    for alias in ALIASES {
        for left in ["", " ", "\n", "("] {
            for right in ["", " ", ".", ",", ")"] {
                let text = format!("{left}{alias}{right}");
                assert!(has_visible_keyword(&text), "not armed: {text:?}");
                assert!(painted(&text), "not painted: {text:?}");
            }
        }
    }
}

#[test]
fn glued_and_marked_up_mentions_neither_arm_nor_paint() {
    for text in [
        "**ulw**",
        "*meth*",
        "#ulw",
        "~ulw~",
        "x=ulw",
        "ulw=1",
        "ulw\u{2014}fix",
        "ulw\u{2026}",
        "ulw\u{d574}\u{c918}",
        "mass ulw\u{d574}\u{c918}",
        "\u{e9}ulw",
        "\u{c6b8}ulw",
        "mass-ulw-loop",
        "meth-loop",
        "mulw-mass",
    ] {
        assert!(!has_visible_keyword(text), "armed: {text:?}");
        assert!(!painted(text), "painted: {text:?}");
    }
}

#[test]
fn skill_keywords_paint_without_arming_ultrawork() {
    for text in [
        "ulw-loop until green",
        "ulw-research the market",
        "mass ulw-loop the queue",
    ] {
        assert!(!has_visible_keyword(text), "armed: {text:?}");
        assert!(!painted(text), "painted as ultrawork: {text:?}");
        assert!(
            !find_reserved_keyword_spans(text).is_empty(),
            "skill keyword not painted: {text:?}"
        );
    }
}

#[test]
fn explicit_negatives_neither_arm_nor_paint() {
    for text in [
        "methane",
        "ulwfoo",
        "ulw_helper",
        "mass",
        "mass u",
        "`meth`",
        "`mass_ulw`",
        "a/meth/b",
        "senpi-ulw-loop",
        "\"meth\"",
        "do not use mass_ulw",
        "something",
        "mulwx",
    ] {
        let text = format!("please {text} the migration");
        assert!(!has_visible_keyword(&text), "armed: {text:?}");
        assert!(
            find_reserved_keyword_spans(&text).is_empty(),
            "painted: {text:?}"
        );
    }
}
