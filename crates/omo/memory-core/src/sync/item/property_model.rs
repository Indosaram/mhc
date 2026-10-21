//! Independent executable model of the pinned item contract for the seeded property.
//! Shares only YAML codecs, tag identifiers and Loro primitives, not item capture,
//! receipts, field writers, history, state combination, transplant or rendering.
use super::{ItemError, ItemFrontier, FmValue, crdt, split_item, yaml};
use loro::{ExportMode, JsonMapOp, JsonOpContent, LoroDoc, LoroValue, ToJson, UpdateOptions, VersionVector};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

type Maps = BTreeMap<String, BTreeMap<String, serde_json::Value>>;
type Stamp = (u64, u64);

struct WriteView {
    history: History,
    values: Vec<(String, yaml::Value)>,
}

#[derive(Default)]
struct History {
    keys: BTreeMap<String, (Stamp, String)>,
    additions: BTreeMap<String, BTreeMap<String, Stamp>>,
}

fn fields(doc: &LoroDoc) -> Maps {
    doc.get_value().to_json_value().as_object().into_iter().flatten()
        .filter(|(name, _)| name.starts_with("fm\u{1f}"))
        .map(|(name, _)| (name.clone(), doc.get_map(name.as_str()).get_deep_value().to_json_value()
            .as_object().expect("model map").iter().map(|(k, v)| (k.clone(), v.clone())).collect()))
        .collect()
}

fn tags(doc: &LoroDoc) -> BTreeSet<String> {
    let roots = doc.get_value().to_json_value();
    roots.as_object().into_iter().flat_map(|roots| roots.keys())
        .filter(|root| root.starts_with("fm\u{1f}"))
        .flat_map(|root| doc.get_map(root.as_str()).keys().map(|tag| tag.to_string()).collect::<Vec<_>>())
        .collect()
}

fn name(key: &str, kind: &str) -> String { format!("fm\u{1f}{key}\u{1f}{kind}") }

fn history(doc: &LoroDoc) -> History {
    let mut out = History::default();
    // Pinned analyzeHistory visits changes by their starting stamp, then ops
    // by counter; first-write means the first visit in that walk, not a global
    // minimum across interleaved operations of coalesced changes.
    let mut changes = doc.export_json_updates_without_peer_compression(&VersionVector::new(), &doc.oplog_vv()).changes;
    changes.sort_by_key(|change| (change.lamport, change.id.peer));
    for change in changes {
        for op in change.ops {
            let loro::ContainerID::Root { name, .. } = op.container else { continue };
            if !name.starts_with("fm\u{1f}") { continue; }
            let stamp = (u64::from(change.lamport) + u64::try_from(op.counter - change.id.counter).expect("op offset"), change.id.peer);
            let key = name.split('\u{1f}').nth(1).expect("model key").to_owned();
            out.keys.entry(key.clone()).or_insert((stamp, name.to_string()));
            if let JsonOpContent::Map(JsonMapOp::Insert { key: tag, .. } | JsonMapOp::Delete { key: tag }) = op.content {
                out.additions.entry(key).or_default().entry(tag).or_insert(stamp);
            }
        }
    }
    out
}

fn active(map: &BTreeMap<String, serde_json::Value>) -> Vec<(String, String)> {
    let replaced: BTreeSet<_> = map.values().flat_map(|v| v["s"].as_array().expect("supersedes"))
        .map(|v| v.as_str().expect("tag")).collect();
    map.iter().filter(|(tag, _)| !replaced.contains(tag.as_str()))
        .filter_map(|(tag, v)| v["v"].as_str().map(|s| (tag.clone(), s.to_owned()))).collect()
}

fn reg_text(source: &str) -> String {
    match yaml::parse(source).expect("model source").value {
        yaml::Value::String(text) => text,
        _ => source.into(),
    }
}

fn values(doc: &LoroDoc) -> Vec<(String, yaml::Value)> {
    values_with_history(doc, &history(doc))
}

fn values_with_history(doc: &LoroDoc, history: &History) -> Vec<(String, yaml::Value)> {
    let maps = fields(doc);
    let mut keys: Vec<_> = history.keys.iter().collect();
    keys.sort_by_key(|(_, (stamp, _))| *stamp);
    keys.into_iter().filter_map(|(key, (_, first))| {
        let empty = BTreeMap::new();
        let set = maps.get(&name(key, "set")).unwrap_or(&empty);
        let reg = maps.get(&name(key, "reg")).unwrap_or(&empty);
        let regs: BTreeSet<_> = active(reg).into_iter().map(|(_, v)| v).collect();
        if first.ends_with("reg") {
            let sole = regs.len() == 1 && set.is_empty();
            let parts: BTreeSet<_> = regs.into_iter().chain(set.values().map(|v| v.as_str().expect("element").to_owned())).collect();
            if parts.is_empty() { return None; }
            let text = parts.into_iter().collect::<Vec<_>>().join(" / ");
            let value = if sole {
                let value = yaml::parse(&text).expect("register source").value;
                match value {
                    yaml::Value::Seq(_) | yaml::Value::Map(_) | yaml::Value::TypedMap(_) => yaml::Value::Atom(yaml::source(&value)),
                    value => value,
                }
            } else { yaml::Value::Atom(yaml::double_quoted(&text)) };
            Some((key.clone(), value))
        } else {
            let additions = &history.additions[key];
            let mut elements: Vec<_> = set.iter().map(|(tag, v)| (additions[tag], v.as_str().expect("element").to_owned())).collect();
            for (tag, source) in active(reg) {
                let sources = match yaml::parse(&source).expect("register source").value {
                    yaml::Value::Seq(items) => items.iter().map(yaml::source).collect(),
                    _ => vec![source],
                };
                elements.extend(sources.into_iter().map(|source| (additions[&tag], source)));
            }
            elements.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.encode_utf16().cmp(b.1.encode_utf16())));
            let mut seen = BTreeSet::new();
            let elements: Vec<_> = elements.into_iter().filter(|(_, v)| seen.insert(v.clone()))
                .map(|(_, v)| yaml::parse(&v).expect("element source").value).collect();
            if elements.is_empty() { None } else { Some((key.clone(), yaml::Value::Seq(elements))) }
        }
    }).collect()
}

fn render(doc: &LoroDoc) -> String {
    let entries = values(doc);
    let body = doc.get_text("body").to_string();
    if entries.is_empty() { body } else { format!("---\n{}\n---\n{body}", super::frontmatter::emit_frontmatter(&entries)) }
}

fn insert(doc: &LoroDoc, root: &str, value: serde_json::Value, salt: &str, slot: usize) -> Result<(), ItemError> {
    doc.get_map(root).insert(&super::tag::new_tag(doc, salt, slot), LoroValue::from(value)).map_err(crdt)
}

fn write(doc: &LoroDoc, key: &str, value: Option<&FmValue>, before: Option<&FmValue>, seen: &BTreeSet<String>, delta: bool, snapshot: &WriteView) -> Result<(), ItemError> {
    let field = |kind| doc.get_map(name(key, kind)).get_deep_value().to_json_value()
        .as_object().expect("model map").iter().map(|(k, v)| (k.clone(), v.clone())).collect::<BTreeMap<_, _>>();
    let set = field("set");
    let reg = field("reg");
    let first = snapshot.history.keys.get(key).map(|(_, root)| root.clone());
    let value = value.filter(|v| **v != FmValue::Remove);
    let Some(first) = first else {
        match value {
            Some(FmValue::Set { elements, .. }) => for (slot, source) in elements.iter().enumerate() {
                insert(doc, &name(key, "set"), serde_json::json!(source), source, slot)?;
            },
            Some(FmValue::Reg { value }) => insert(doc, &name(key, "reg"), serde_json::json!({"v":value,"s":[]}), value, 0)?,
            Some(FmValue::Remove) | None => {},
        }
        return Ok(());
    };
    let active = active(&reg);
    if let Some(value) = value {
        if first.ends_with("reg") {
            let target = match value { FmValue::Set { flow, .. } => flow, FmValue::Reg { value } => value, FmValue::Remove => unreachable!() };
            let parts: BTreeSet<_> = active.iter().map(|(_, v)| v.clone()).chain(set.values().map(|v| v.as_str().expect("element").to_owned())).collect();
            let combined = parts.iter().cloned().collect::<Vec<_>>().join(" / ");
            let sole = active.iter().map(|(_, v)| v).collect::<BTreeSet<_>>().len() == 1 && set.is_empty();
            if !parts.is_empty() && (combined == *target || (!sole && combined == reg_text(target))) { return Ok(()); }
            if !active.is_empty() && active.iter().all(|(_, v)| v == target) { return Ok(()); }
            let replaced: Vec<_> = active.iter().filter(|(tag, _)| seen.contains(tag)).map(|(tag, _)| tag.clone()).collect();
            insert(doc, &name(key, "reg"), serde_json::json!({"v":target,"s":replaced}), target, 0)?;
        } else {
            let target = match value { FmValue::Set { elements, .. } => elements.clone(), FmValue::Reg { value } => vec![value.clone()], FmValue::Remove => unreachable!() };
            let element_delta = delta && matches!((before, value), (Some(FmValue::Set { .. }), FmValue::Set { .. }));
            if !element_delta {
                let current = snapshot.values.iter().find(|(k, _)| k == key).map(|(_, v)| match v { yaml::Value::Seq(items) => items.iter().map(yaml::source).collect::<Vec<_>>(), _ => unreachable!() }).unwrap_or_default();
                if current == target { return Ok(()); }
            }
            let old = match before { Some(FmValue::Set { elements, .. }) => elements.clone(), _ => vec![] };
            let removed: BTreeSet<_> = if element_delta { old.iter().filter(|v| !target.contains(v)).cloned().collect() } else { set.values().map(|v| v.as_str().expect("element").to_owned()).filter(|v| !target.contains(v)).collect() };
            for (tag, source) in &set {
                if seen.contains(tag) && removed.contains(source.as_str().expect("element")) { doc.get_map(name(key, "set")).delete(tag).map_err(crdt)?; }
            }
            let added: Vec<_> = target.iter().filter(|v| !element_delta || !old.contains(v)).collect();
            for (slot, source) in added.iter().enumerate() {
                if !set.values().any(|v| v.as_str() == Some(source.as_str()) && (!element_delta || !removed.contains(source.as_str()))) {
                    insert(doc, &name(key, "set"), serde_json::json!(source), source, slot)?;
                }
            }
            if element_delta { return Ok(()); }
        }
    }
    if value.is_none() || first.ends_with("reg") {
        for tag in set.keys().filter(|tag| seen.contains(*tag)) { doc.get_map(name(key, "set")).delete(tag).map_err(crdt)?; }
    }
    if value.is_none() || first.ends_with("set") {
        let replaced: Vec<_> = active.iter().filter(|(tag, _)| seen.contains(tag)).map(|(tag, _)| tag.clone()).collect();
        if !replaced.is_empty() { insert(doc, &name(key, "reg"), serde_json::json!({"v":null,"s":replaced}), "null", 0)?; }
    }
    Ok(())
}

fn apply(doc: &LoroDoc, before: &str, after: &str, seen: &BTreeSet<String>, delta: bool) -> Result<(), ItemError> {
    let b = split_item(before);
    let a = split_item(after);
    // History exports are Loro transaction barriers. Snapshot the view before
    // emitting any fields, as production does, rather than committing between
    // fields and losing the capture's commit message on the earlier operations.
    let history = history(doc);
    let values = values_with_history(doc, &history);
    let snapshot = WriteView { history, values };
    let old_keys: Vec<_> = b.fm.as_deref().unwrap_or(&[]).iter().map(|e| e.key.clone()).collect();
    let old: BTreeMap<_, _> = b.fm.unwrap_or_default().into_iter().map(|e| (e.key, e.value)).collect();
    let entries = a.fm.unwrap_or_default();
    let new: BTreeMap<_, _> = entries.iter().map(|e| (e.key.clone(), e.value.clone())).collect();
    let mut keys: Vec<_> = if delta { entries.iter().map(|e| e.key.clone()).collect() } else { snapshot.history.keys.keys().filter(|k| new.get(*k).is_none_or(|v| *v == FmValue::Remove)).cloned().collect() };
    if delta { keys.extend(old_keys.into_iter().filter(|k| !new.contains_key(k))); }
    else { keys.extend(entries.iter().filter(|e| e.value != FmValue::Remove).map(|e| e.key.clone())); }
    for key in keys {
        if !delta || old.get(&key) != new.get(&key) { write(doc, &key, new.get(&key), old.get(&key), seen, delta, &snapshot)?; }
    }
    let current = doc.get_text("body").to_string();
    let body = if !delta || current == b.body { a.body } else if a.body == b.body || a.body == current { current.clone() } else {
        let text = LoroDoc::new();
        text.set_peer_id(1).map_err(crdt)?;
        text.set_record_timestamp(false);
        text.get_text("t").insert(0, &b.body).map_err(crdt)?;
        text.commit();
        let left = text.fork();
        left.set_peer_id(2).map_err(crdt)?;
        left.get_text("t").update(&a.body, UpdateOptions::default()).map_err(crdt)?;
        left.commit();
        let right = text.fork();
        right.set_peer_id(3).map_err(crdt)?;
        right.get_text("t").update(&current, UpdateOptions::default()).map_err(crdt)?;
        right.commit();
        left.import(&right.export(ExportMode::updates(&text.oplog_vv())).map_err(crdt)?).map_err(crdt)?;
        left.get_text("t").to_string()
    };
    if body != current { doc.get_text("body").update(&body, UpdateOptions::default()).map_err(crdt)?; }
    Ok(())
}

#[derive(Clone)]
struct Receipt { base: ItemFrontier, text: String, seen: BTreeSet<String> }

fn same_base(a: &ItemFrontier, b: &ItemFrontier) -> bool {
    a.len() == b.len() && a.iter().all(|id| b.contains(id))
}

pub(super) struct Model {
    doc: LoroDoc,
    rendered: RefCell<Option<(VersionVector, String)>>,
    saved: loro::Frontiers,
    text: String,
    receipts: Vec<Receipt>,
}

impl Model {
    pub(super) fn new(peer: u64) -> Result<Self, ItemError> {
        let doc = LoroDoc::new();
        doc.set_peer_id(peer).map_err(crdt)?;
        doc.set_record_timestamp(false);
        Ok(Self { doc, rendered: RefCell::new(None), saved: loro::Frontiers::new(), text: String::new(), receipts: vec![] })
    }

    pub(super) fn render(&self) -> String {
        let version = self.doc.oplog_vv();
        if let Some((at, text)) = self.rendered.borrow().as_ref()
            && *at == version
        {
            return text.clone();
        }
        let text = render(&self.doc);
        *self.rendered.borrow_mut() = Some((version, text.clone()));
        text
    }

    pub(super) fn sync(&self, other: &Self) -> Result<(), ItemError> {
        let a = self.doc.export(ExportMode::updates(&other.doc.oplog_vv())).map_err(crdt)?;
        let b = other.doc.export(ExportMode::updates(&self.doc.oplog_vv())).map_err(crdt)?;
        self.doc.import(&b).map_err(crdt)?;
        other.doc.import(&a).map_err(crdt)?;
        Ok(())
    }

    pub(super) fn materialize(&mut self, base: &ItemFrontier, save: bool) {
        let text = self.render();
        self.receipts.retain(|r| !same_base(&r.base, base));
        self.receipts.push(Receipt { base: base.clone(), text: text.clone(), seen: tags(&self.doc) });
        if save { self.saved = self.doc.state_frontiers(); self.text = text; }
    }

    pub(super) fn capture(&mut self, base: &ItemFrontier, working: &str) -> Result<(), ItemError> {
        if !self.doc.oplog_vv().iter().all(|(_, n)| *n == 0) && working == self.render() {
            self.materialize(base, false);
            return Ok(());
        }
        let prior = self.receipts.iter().find(|r| same_base(&r.base, base)).cloned();
        let target = self.doc.fork();
        target.set_peer_id(self.doc.peer_id()).map_err(crdt)?;
        target.set_record_timestamp(false);
        let seen = if let Some(prior) = &prior {
            apply(&target, &prior.text, working, &prior.seen, true)?;
            prior.seen.clone()
        } else if self.doc.state_frontiers() == self.saved {
            let seen = tags(&self.doc);
            apply(&target, &self.text, working, &seen, false)?;
            seen
        } else {
            let scratch = self.doc.fork_at(&self.saved).map_err(crdt)?;
            let version = self.doc.oplog_vv();
            let mut unused = 0;
            while version.get(&unused).is_some() { unused += 1; }
            scratch.set_peer_id(unused).map_err(crdt)?;
            scratch.set_record_timestamp(false);
            let seen = tags(&scratch);
            apply(&scratch, &self.text, working, &seen, false)?;
            scratch.commit();
            let from = scratch.frontiers_to_vv(&self.saved).expect("saved frontier");
            if scratch.oplog_vv() == from { return Ok(()); }
            {
                let merged = self.doc.fork();
                merged.set_record_timestamp(false);
                merged.import(&scratch.export(ExportMode::updates(&from)).map_err(crdt)?).map_err(crdt)?;
                let have = fields(&target);
                let body = merged.get_text("body").to_string();
                if target.get_text("body").to_string() != body { target.get_text("body").update(&body, UpdateOptions::default()).map_err(crdt)?; }
                for (root, want) in fields(&merged) {
                    let old = have.get(&root).cloned().unwrap_or_default();
                    let added: Vec<_> = want.keys().filter(|tag| !old.contains_key(*tag)).collect();
                    let mut renames = BTreeMap::new();
                    for (slot, tag) in added.iter().enumerate() {
                        let value = &want[*tag];
                        let salt = match value.as_object() {
                            Some(object) => serde_json::to_string(&object.iter().collect::<BTreeMap<_, _>>()).map_err(crdt)?,
                            None => value.to_string(),
                        };
                        renames.insert((*tag).clone(), super::tag::new_tag(&target, &salt, slot + 1));
                    }
                    for tag in old.keys().filter(|tag| !want.contains_key(*tag)) { target.get_map(root.as_str()).delete(tag).map_err(crdt)?; }
                    for tag in added {
                        let mut value = want[tag].clone();
                        if let Some(s) = value.get_mut("s").and_then(|v| v.as_array_mut()) {
                            for tag in s { if let Some(new) = tag.as_str().and_then(|tag| renames.get(tag)) { *tag = serde_json::json!(new); } }
                        }
                        target.get_map(root.as_str()).insert(&renames[tag], LoroValue::from(value)).map_err(crdt)?;
                    }
                }
            }
            seen
        };
        // Device capture gives each update a distinct commit message. Loro only
        // coalesces adjacent changes with equal messages; losing that boundary
        // changes the pinned change-ordered history walk, even with equal ops.
        target.set_next_commit_message(&format!("model-capture:{}:{}", self.doc.peer_id(), self.doc.len_ops()));
        target.commit();
        if target.oplog_vv() == self.doc.oplog_vv() { return Ok(()); }
        let mut seen = seen;
        seen.extend(tags(&target).difference(&tags(&self.doc)).cloned());
        self.doc.import(&target.export(ExportMode::updates(&self.doc.oplog_vv())).map_err(crdt)?).map_err(crdt)?;
        self.receipts.retain(|r| !same_base(&r.base, base));
        self.receipts.push(Receipt { base: base.clone(), text: working.into(), seen });
        Ok(())
    }
}
