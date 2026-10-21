use loro::{ContainerID, JsonMapOp, JsonOpContent, LoroDoc, VersionVector};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FmPart {
    Set,
    Reg,
}
impl FmPart {
    pub fn name(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Reg => "reg",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Stamp {
    pub lamport: u64,
    pub peer: u64,
}
#[derive(Clone, Debug)]
pub struct KeyHistory {
    pub kind: FmPart,
    pub first: Stamp,
    pub by_kind: BTreeMap<FmPart, Stamp>,
}
#[derive(Clone, Debug, Default)]
pub struct ItemHistory {
    pub keys: BTreeMap<String, KeyHistory>,
    pub set_adds: BTreeMap<String, BTreeMap<String, Stamp>>,
    pub reg_adds: BTreeMap<String, BTreeMap<String, Stamp>>,
}
pub fn analyze_history(doc: &LoroDoc) -> ItemHistory {
    let mut history = ItemHistory::default();
    let mut changes = doc
        .export_json_updates_without_peer_compression(&VersionVector::new(), &doc.oplog_vv())
        .changes;
    changes.sort_by_key(|c| (c.lamport, c.id.peer));
    for change in changes {
        for op in change.ops {
            let ContainerID::Root { name, .. } = op.container else {
                continue;
            };
            let Some((key, part)) = parse_fm_name(&name) else {
                continue;
            };
            let Ok(offset) = u64::try_from(op.counter - change.id.counter) else {
                continue;
            };
            let stamp = Stamp {
                lamport: u64::from(change.lamport) + offset,
                peer: change.id.peer,
            };
            let kh = history
                .keys
                .entry(key.clone())
                .or_insert_with(|| KeyHistory {
                    kind: part,
                    first: stamp,
                    by_kind: BTreeMap::new(),
                });
            kh.by_kind.entry(part).or_insert(stamp);
            if let JsonOpContent::Map(
                JsonMapOp::Insert { key: tag, .. } | JsonMapOp::Delete { key: tag },
            ) = op.content
            {
                let book = match part {
                    FmPart::Set => &mut history.set_adds,
                    FmPart::Reg => &mut history.reg_adds,
                };
                book.entry(key).or_default().entry(tag).or_insert(stamp);
            }
        }
    }
    history
}
pub fn parse_fm_name(name: &str) -> Option<(String, FmPart)> {
    let mut parts = name.split('\u{1f}');
    if parts.next()? != "fm" {
        return None;
    }
    let key = decode(parts.next()?)?;
    let part = match parts.next()? {
        "set" => FmPart::Set,
        "reg" => FmPart::Reg,
        _ => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((key, part))
}
fn decode(text: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut bytes = text.bytes();
    while let Some(b) = bytes.next() {
        out.push(if b == b'%' {
            let a = char::from(bytes.next()?).to_digit(16)?;
            let b = char::from(bytes.next()?).to_digit(16)?;
            u8::try_from(a * 16 + b).ok()?
        } else {
            b
        });
    }
    String::from_utf8(out).ok()
}
