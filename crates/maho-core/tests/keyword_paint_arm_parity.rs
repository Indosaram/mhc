//! The editor paints a reserved keyword exactly where the ultrawork engine arms.

use maho_core::reserved_keywords::find_reserved_keyword_spans;
use maho_ultrawork::has_visible_keyword;

fn painted(text: &str) -> bool {
    !find_reserved_keyword_spans(text).is_empty()
}

#[test]
fn painted_equals_armed_for_every_alias_and_negative_case() {
    let positives = [
        "ulw", "ultrawork", "ULW", "mass ulw", "mass-ulw", "mass_ulw", "massulw", "mass  ulw",
        "mass\nulw", "ulw mass", "ulw-mass", "ulw_mass", "ulwmass", "mulw", "MULW", "meth",
        "Meth",
    ];
    let negatives = [
        "methane", "ulwfoo", "ulw_helper", "mass", "mass u", "`meth`", "`mass_ulw`", "a/meth/b",
        "senpi-ulw-loop", "\"meth\"", "do not use mass_ulw", "something", "mulwx",
    ];
    for alias in positives {
        let text = format!("please {alias} the migration");
        assert!(painted(&text), "not painted: {alias:?}");
        assert!(has_visible_keyword(&text), "not armed: {alias:?}");
    }
    for text in negatives {
        let text = format!("please {text} the migration");
        assert_eq!(painted(&text), has_visible_keyword(&text), "{text:?}");
        assert!(!painted(&text), "painted: {text:?}");
    }
}
