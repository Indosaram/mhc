//! Deterministic upstream corpus generation and output-only value oracle.
use crate::sync::item::{
    frontmatter::{FmValue, split_header, split_item},
    yaml::{Value, parse},
};
use std::collections::BTreeMap;

pub(super) struct Random(u32);

impl Random {
    pub(super) fn new(seed: u32) -> Self {
        Self(seed)
    }

    // The same wrapping 32-bit operations as upstream item/test-support.ts mulberry32.
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x6d2b79f5);
        let mut t = (self.0 ^ (self.0 >> 15)).wrapping_mul(1 | self.0);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        t ^ (t >> 14)
    }

    pub(super) fn unit(&mut self) -> f64 {
        f64::from(self.next()) / 4_294_967_296.0
    }

    pub(super) fn chance(&mut self, threshold: f64) -> bool {
        self.unit() < threshold
    }

    pub(super) fn index(&mut self, len: usize) -> usize {
        let scaled = u64::from(self.next()) * u64::try_from(len).expect("pool length");
        usize::try_from(scaled >> 32).expect("pool index")
    }
}

pub(super) fn block(header: &str) -> String {
    hand_block(&header.replace("-->", "-- >"))
}

pub(super) fn hand_block(kept: &str) -> String {
    format!(
        "\n<!-- memory-sync 2026-10-15: the other machine had different frontmatter for n.md; kept here so no fact is lost:\n{kept}-->\n"
    )
}

pub(super) fn sole_registers(text: &str) -> BTreeMap<String, Vec<String>> {
    split_item(&text.replace("\r\n", "\n"))
        .fm
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| match entry.value {
            FmValue::Reg { value } => Some((entry.key, vec![value])),
            FmValue::Set { .. } | FmValue::Remove => None,
        })
        .collect()
}

pub(super) struct Facts(BTreeMap<String, (bool, Vec<Value>)>);

impl Facts {
    // Decode only the OUTPUT header, bypassing adoption and read-back classification.
    // Generated strings never contain " / ", so joined components are unambiguous.
    pub(super) fn read(output: &str) -> Self {
        let mut facts = BTreeMap::new();
        if let Some((header, _)) = split_header(output) {
            let Value::Map(entries) = parse(header).expect("output YAML").value else {
                panic!("output header must be a mapping");
            };
            for (key, value) in entries {
                let fact = match value {
                    Value::Seq(values) => (true, values),
                    Value::String(value) if value.contains(" / ") => (
                        false,
                        value
                            .split(" / ")
                            .map(|part| parse(part).expect("register component").value)
                            .collect(),
                    ),
                    value => (false, vec![value]),
                };
                facts.insert(key, fact);
            }
        }
        Self(facts)
    }

    pub(super) fn holds(&self, key: &str, wanted: &Value) -> bool {
        if matches!(wanted, Value::Seq(elements) if elements.is_empty()) {
            return true;
        }
        let Some((set, values)) = self.0.get(key) else {
            return false;
        };
        match wanted {
            Value::Seq(elements) if *set => elements.iter().all(|e| values.contains(e)),
            Value::Null | Value::String(_) | Value::Atom(_) | Value::Seq(_)
            | Value::Map(_) | Value::TypedMap(_) => values.contains(wanted),
        }
    }

    pub(super) fn assert_keys(&self, keys: &[(String, Value)], context: &str) {
        for (key, value) in keys {
            assert!(self.holds(key, value), "{context}: missing {key}={value:?}");
        }
    }
}
