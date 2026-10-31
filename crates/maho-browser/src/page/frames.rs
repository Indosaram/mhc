//! Frames: the frame tree of a page, out-of-process iframe recovery and snapshots that stitch every
//! frame into one tree.
//!
//! Port of omowright `src/frames-snapshot.js` (`reconcileFrames`, `snapshotWithFrames`,
//! `collectMissingFrames`, `framePrefixes`) together with the parts of `src/core.js` it builds on:
//! `FrameManager` (frame map from `Page.getFrameTree`, `collectDescendantFrames` BFS, isolated world
//! per frame via `Page.createIsolatedWorld` + the page world bundle, `invalidateContext`) and the
//! multi-frame half of `takeSnapshot` (`f<N>` ref prefixes in BFS order, iframe refs resolved to
//! child frames with `DOM.describeNode`, `inlineChildTree`, `appendOrphanChildTree`,
//! `tagIframeLine` with the known password-manager origins).
//!
//! PORT-DIVERGENCE: core.js keeps the FrameManager live through CDP listeners. The Rust page has
//! no listener API, so [`FrameManager::refresh`] rebuilds the map from `Page.getFrameTree` and
//! [`reconcile_frames`] adopts out-of-process iframes on demand; refs stay page-level (`f1e3`), and
//! [`FrameManager::evaluate_in_frame`] runs code in the frame a prefix names.
use super::PAGE_WORLD;
use crate::cdp::CdpClient;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// omowright `ISOLATED_WORLD_NAME`, renamed like the vendored page world.
pub const ISOLATED_WORLD_NAME: &str = "__maho_utility";
const AUTO_ATTACH: &str = r#"{"autoAttach":true,"waitForDebuggerOnStart":false,"flatten":true}"#;

/// Known extension iframe origins and their labels (`KNOWN_IFRAME_ORIGINS`).
pub const KNOWN_IFRAME_ORIGINS: [(&str, &str); 5] = [
    (
        "chrome-extension://aeblfdkhhhdcdjpifhhbdiojplfjncoa/",
        "1Password",
    ),
    (
        "chrome-extension://nngceckbapebfimnlniiiahkandclblb/",
        "Bitwarden",
    ),
    (
        "chrome-extension://fdjamakpfbbddfjaooikfcpapjohcfmg/",
        "Dashlane",
    ),
    (
        "chrome-extension://hdokiejnpimakedhajhdlcegeplioahd/",
        "LastPass",
    ),
    (
        "chrome-extension://clcdgiameigmljcbkkcbjiljinmfkncl/",
        "OmO Password Manager",
    ),
];

/// One frame of the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    pub frame_id: String,
    pub parent_frame_id: Option<String>,
    pub url: String,
    pub name: String,
    pub session_id: String,
    pub isolated_context_id: Option<i64>,
}

/// A frame in `collectDescendantFrames` order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameVisit {
    pub frame_id: String,
    pub parent_frame_id: Option<String>,
    pub depth: usize,
}

/// A frame a snapshot could not enter (`missingFrames`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingFrame {
    pub frame_id: String,
    pub url: String,
}

/// A frame adopted by [`reconcile_frames`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdoptedFrame {
    pub frame_id: String,
    pub parent_frame_id: String,
    pub url: String,
    pub session_id: String,
    pub isolated_context_id: Option<i64>,
}

/// A stitched multi-frame snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct FramesSnapshot {
    pub tree: String,
    /// Every ref of every frame (`e3`, `f1e2`, ...) and its metadata.
    pub refs: serde_json::Map<String, Value>,
    pub missing_frames: Vec<MissingFrame>,
}

/// The frames of one page.
pub struct FrameManager {
    client: CdpClient,
    session_id: String,
    pub frames: BTreeMap<String, FrameInfo>,
    pub main_frame_id: Option<String>,
    prefixes: HashMap<String, String>,
}

/// `getKnownIframeOriginLabel`.
pub fn known_iframe_origin_label(url: &str) -> Option<&'static str> {
    KNOWN_IFRAME_ORIGINS
        .iter()
        .find(|(prefix, _)| url.starts_with(prefix))
        .map(|(_, label)| *label)
}

fn origin_label(label: Option<&str>) -> String {
    label.map_or_else(String::new, |label| {
        format!(" [origin=\"{}\"]", label.replace('"', "\\\""))
    })
}

/// Parses `^(\s*)- iframe(?:.*? )?\[ref=([^\]]*)\]` into (indent, ref).
fn iframe_line(line: &str) -> Option<(&str, &str)> {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);
    let rest = rest.strip_prefix("- iframe")?;
    if !(rest.starts_with(" [ref=") || rest.contains(" [ref=") || rest.starts_with("[ref=")) {
        return None;
    }
    let start = if rest.starts_with("[ref=") {
        0
    } else {
        rest.find(" [ref=")? + 1
    };
    let tail = &rest[start + "[ref=".len()..];
    let end = tail.find(']')?;
    Some((indent, &tail[..end]))
}

/// `tagIframeLine`: labels the iframe line of `iframe_ref` with its origin.
pub fn tag_iframe_line(tree: &str, iframe_ref: &str, label: &str) -> String {
    tree.split('\n')
        .map(|line| match iframe_line(line) {
            Some((indent, found)) if found == iframe_ref && !line.contains(" [origin=") => {
                let rest = &line[indent.len() + "- iframe".len()..];
                format!("{indent}- iframe{}{rest}", origin_label(Some(label)))
            }
            _ => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `inlineChildTree`: nests `child` under the iframe line of `iframe_ref`.
pub fn inline_child_tree(tree: &str, iframe_ref: &str, child: &str) -> String {
    tree.split('\n')
        .flat_map(|line| match iframe_line(line) {
            Some((indent, found)) if found == iframe_ref => {
                let mut lines = vec![if child.is_empty() {
                    line.to_owned()
                } else {
                    format!("{line}:")
                }];
                lines.extend(child.split('\n').map(|child| format!("{indent}  {child}")));
                lines
            }
            _ => vec![line.to_owned()],
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `appendOrphanChildTree`: appends `child` as an unattributed iframe.
pub fn append_orphan_child_tree(tree: &str, child: &str, label: Option<&str>) -> String {
    if child.is_empty() {
        return tree.to_owned();
    }
    let indented: Vec<String> = child.split('\n').map(|line| format!("  {line}")).collect();
    format!(
        "{tree}\n- iframe{}:\n{}",
        origin_label(label),
        indented.join("\n")
    )
}

/// `getRefPrefix`: the `f<N>` part of a ref, `""` for main-frame refs.
pub fn ref_prefix(reference: &str) -> &str {
    let digits = reference
        .strip_prefix('f')
        .map(|rest| rest.chars().take_while(char::is_ascii_digit).count())
        .unwrap_or(0);
    if digits > 0 && reference[1 + digits..].starts_with('e') {
        &reference[..1 + digits]
    } else {
        ""
    }
}

impl FrameManager {
    /// The frames of the page on `session_id`, read from `Page.getFrameTree`.
    pub fn new(client: CdpClient, session_id: &str) -> Result<Self, String> {
        let mut manager = Self {
            client,
            session_id: session_id.to_owned(),
            frames: BTreeMap::new(),
            main_frame_id: None,
            prefixes: HashMap::new(),
        };
        manager.refresh()?;
        Ok(manager)
    }

    pub fn client(&self) -> &CdpClient {
        &self.client
    }

    /// Re-reads the in-process frame tree; adopted out-of-process frames are kept.
    pub fn refresh(&mut self) -> Result<(), String> {
        let tree = self
            .client
            .send("Page.getFrameTree", json!({}), Some(&self.session_id))?;
        let session = self.session_id.clone();
        let mut seen = HashSet::new();
        if let Some(tree) = tree.get("frameTree") {
            self.walk(tree, None, &session, &mut seen);
        }
        // Frames of this session that left the tree are gone; OOPIF sessions stay.
        self.frames
            .retain(|id, frame| frame.session_id != session || seen.contains(id));
        Ok(())
    }

    fn walk(
        &mut self,
        node: &Value,
        parent: Option<String>,
        session: &str,
        seen: &mut HashSet<String>,
    ) {
        let frame = &node["frame"];
        let Some(id) = frame.get("id").and_then(Value::as_str) else {
            return;
        };
        let previous = self.frames.get(id);
        let url = frame
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let context = previous
            .filter(|previous| previous.url == url)
            .and_then(|previous| previous.isolated_context_id);
        let info = FrameInfo {
            frame_id: id.to_owned(),
            parent_frame_id: parent.clone(),
            url,
            name: frame
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            session_id: previous.map_or_else(|| session.to_owned(), |p| p.session_id.clone()),
            isolated_context_id: context,
        };
        if parent.is_none() {
            self.main_frame_id = Some(id.to_owned());
        }
        seen.insert(id.to_owned());
        self.frames.insert(id.to_owned(), info);
        for child in node
            .get("childFrames")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.walk(child, Some(id.to_owned()), session, seen);
        }
    }

    pub fn get_frame(&self, frame_id: &str) -> Option<&FrameInfo> {
        self.frames.get(frame_id)
    }

    /// `collectDescendantFrames`: `root` and its descendants, breadth first.
    pub fn collect_descendant_frames(&self, root: &str) -> Vec<FrameVisit> {
        if !self.frames.contains_key(root) {
            return Vec::new();
        }
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        for frame in self.frames.values() {
            if let Some(parent) = &frame.parent_frame_id {
                children
                    .entry(parent.as_str())
                    .or_default()
                    .push(&frame.frame_id);
            }
        }
        let mut out = Vec::new();
        let mut queue = VecDeque::from([FrameVisit {
            frame_id: root.to_owned(),
            parent_frame_id: self
                .frames
                .get(root)
                .and_then(|f| f.parent_frame_id.clone()),
            depth: 0,
        }]);
        let mut seen = HashSet::new();
        while let Some(visit) = queue.pop_front() {
            if !seen.insert(visit.frame_id.clone()) {
                continue;
            }
            for child in children.get(visit.frame_id.as_str()).into_iter().flatten() {
                queue.push_back(FrameVisit {
                    frame_id: (*child).to_owned(),
                    parent_frame_id: Some(visit.frame_id.clone()),
                    depth: visit.depth + 1,
                });
            }
            out.push(visit);
        }
        out
    }

    /// `framePrefixes`: main frame `""`, the others `f1..fN` in BFS order.
    pub fn frame_prefixes(&self) -> Vec<(String, String)> {
        let Some(main) = &self.main_frame_id else {
            return Vec::new();
        };
        let mut counter = 0;
        self.collect_descendant_frames(main)
            .into_iter()
            .map(|visit| {
                let prefix = if &visit.frame_id == main {
                    String::new()
                } else {
                    counter += 1;
                    format!("f{counter}")
                };
                (visit.frame_id, prefix)
            })
            .collect()
    }

    /// `isAttributed`: whether the parent chain of `frame_id` reaches the main frame.
    pub fn is_attributed(&self, frame_id: &str) -> bool {
        let mut seen = HashSet::new();
        let mut frame = self.frames.get(frame_id);
        while let Some(current) = frame {
            if !seen.insert(current.frame_id.clone()) {
                break;
            }
            if Some(&current.frame_id) == self.main_frame_id.as_ref() {
                return true;
            }
            frame = current
                .parent_frame_id
                .as_ref()
                .and_then(|parent| self.frames.get(parent));
        }
        false
    }

    /// `invalidateContext`.
    pub fn invalidate_context(&mut self, frame_id: &str) {
        if let Some(frame) = self.frames.get_mut(frame_id) {
            frame.isolated_context_id = None;
        }
    }

    fn world_alive(&self, frame: &FrameInfo) -> bool {
        let Some(context) = frame.isolated_context_id else {
            return false;
        };
        self.client
            .send(
                "Runtime.callFunctionOn",
                json!({"functionDeclaration": "function() { return typeof globalThis.__mahoPage; }", "executionContextId": context, "returnByValue": true}),
                Some(&frame.session_id),
            )
            .ok()
            .and_then(|result| result.pointer("/result/value").cloned())
            == Some(json!("object"))
    }

    /// `ensureInjected`: the frame's isolated world with the page world loaded, created on
    /// first use.
    pub fn ensure_injected(&mut self, frame_id: &str) -> Option<i64> {
        let frame = self.frames.get(frame_id)?.clone();
        if frame.isolated_context_id.is_some() {
            if self.world_alive(&frame) {
                return frame.isolated_context_id;
            }
            self.invalidate_context(frame_id);
        }
        let created = self
            .client
            .send(
                "Page.createIsolatedWorld",
                json!({"frameId": frame_id, "worldName": ISOLATED_WORLD_NAME}),
                Some(&frame.session_id),
            )
            .ok()?;
        let context = created.get("executionContextId").and_then(Value::as_i64)?;
        if let Some(stored) = self.frames.get_mut(frame_id) {
            stored.isolated_context_id = Some(context);
        }
        let _ = self.client.send(
            "Runtime.evaluate",
            json!({"expression": PAGE_WORLD, "contextId": context}),
            Some(&frame.session_id),
        );
        let frame = self.frames.get(frame_id)?.clone();
        if self.world_alive(&frame) {
            Some(context)
        } else {
            self.invalidate_context(frame_id);
            None
        }
    }

    /// The frame a ref belongs to, by the prefixes of the last [`snapshot_with_frames`].
    pub fn frame_for_ref(&self, reference: &str) -> Option<&FrameInfo> {
        let prefix = ref_prefix(reference);
        if prefix.is_empty() {
            return self
                .main_frame_id
                .as_ref()
                .and_then(|id| self.frames.get(id));
        }
        self.prefixes
            .get(prefix)
            .and_then(|frame_id| self.frames.get(frame_id))
    }

    /// `evaluateInFrame`: runs `function(arg) { ... }` source in the frame's isolated world.
    pub fn evaluate_in_frame(
        &mut self,
        frame_id: &str,
        function: &str,
        arg: Value,
    ) -> Result<Value, String> {
        let session = self
            .frames
            .get(frame_id)
            .map(|frame| frame.session_id.clone())
            .ok_or_else(|| format!("Frame is not available: {frame_id}"))?;
        let context = self
            .ensure_injected(frame_id)
            .ok_or_else(|| format!("Frame isolated world is not ready: {frame_id}"))?;
        let result = self.client.send(
            "Runtime.callFunctionOn",
            json!({"functionDeclaration": function, "arguments": [{"value": arg}], "executionContextId": context, "returnByValue": true, "awaitPromise": true}),
            Some(&session),
        )?;
        if let Some(details) = result.get("exceptionDetails") {
            return Err(details
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| details.get("text").and_then(Value::as_str))
                .unwrap_or("Evaluation failed")
                .to_owned());
        }
        Ok(result
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null))
    }
}

fn detach(client: &CdpClient, session: &str) {
    let _ = client.send(
        "Target.detachFromTarget",
        json!({"sessionId": session}),
        None,
    );
}

/// `iframeTargets`: iframe targets of this page's browser context.
fn iframe_targets(manager: &FrameManager, target_id: &str) -> Result<Vec<Value>, String> {
    let info = manager
        .client
        .send("Target.getTargetInfo", json!({"targetId": target_id}), None)?;
    let context = info
        .pointer("/targetInfo/browserContextId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let listed = manager.client.send(
        "Target.getTargets",
        json!({"filter": [{"type": "iframe"}]}),
        None,
    )?;
    Ok(listed
        .get("targetInfos")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|info| info.get("type").and_then(Value::as_str) == Some("iframe"))
        .filter(|info| {
            let theirs = info.get("browserContextId").and_then(Value::as_str);
            context.is_none() || theirs.is_none() || theirs == context.as_deref()
        })
        .cloned()
        .collect())
}

/// Attach, then ask the child itself who its parent is (`describeTarget`).
fn describe_target(client: &CdpClient, target_id: &str) -> Result<(String, Value), String> {
    let attached = client.send(
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        None,
    )?;
    let session = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or("Target.attachToTarget returned no sessionId")?
        .to_owned();
    let described = client
        .send("Page.enable", json!({}), Some(&session))
        .and_then(|_| client.send("Page.getFrameTree", json!({}), Some(&session)));
    match described {
        Ok(tree) => Ok((
            session,
            tree.pointer("/frameTree/frame")
                .cloned()
                .unwrap_or_default(),
        )),
        Err(error) => {
            detach(client, &session);
            Err(error)
        }
    }
}

/// `reconcileFrames`: adopts every iframe target of the page that the frame map does not
/// attribute yet; returns the frames added.
pub fn reconcile_frames(
    manager: &mut FrameManager,
    target_id: &str,
) -> Result<Vec<AdoptedFrame>, String> {
    let mut added = Vec::new();
    let mut pending: Vec<Value> = iframe_targets(manager, target_id)?
        .into_iter()
        .filter(|info| {
            !manager.is_attributed(info.get("targetId").and_then(Value::as_str).unwrap_or(""))
        })
        .collect();
    let mut progress = true;
    while progress && !pending.is_empty() {
        progress = false;
        let mut retry = Vec::new();
        for target in pending {
            let target_id = target
                .get("targetId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if manager.is_attributed(target_id) {
                continue;
            }
            let (session, frame) = describe_target(&manager.client, target_id)?;
            let parent = frame
                .get("parentId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let Some(parent) = parent.filter(|parent| manager.is_attributed(parent)) else {
                // Not ours (another tab), or its parent is still pending: never adopt a frame we
                // cannot attribute to this page.
                detach(&manager.client, &session);
                if frame.get("parentId").is_some_and(|p| !p.is_null()) {
                    retry.push(target.clone());
                }
                continue;
            };
            let _ = manager.client.send("DOM.enable", json!({}), Some(&session));
            let auto: Value = serde_json::from_str(AUTO_ATTACH).unwrap_or_default();
            let _ = manager
                .client
                .send("Target.setAutoAttach", auto, Some(&session));
            let frame_id = frame
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(target_id)
                .to_owned();
            let url = frame
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            manager.frames.insert(
                frame_id.clone(),
                FrameInfo {
                    frame_id: frame_id.clone(),
                    parent_frame_id: Some(parent.clone()),
                    url: url.clone(),
                    name: frame
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    session_id: session.clone(),
                    isolated_context_id: None,
                },
            );
            let isolated_context_id = manager.ensure_injected(&frame_id);
            added.push(AdoptedFrame {
                frame_id,
                parent_frame_id: parent,
                url,
                session_id: session,
                isolated_context_id,
            });
            progress = true;
        }
        pending = retry;
    }
    Ok(added)
}

/// `collectMissingFrames`: frames that contributed no ref and have no isolated world.
pub fn collect_missing_frames(
    manager: &FrameManager,
    refs: &serde_json::Map<String, Value>,
) -> Vec<MissingFrame> {
    let seen: HashSet<&str> = refs.keys().map(|reference| ref_prefix(reference)).collect();
    manager
        .frame_prefixes()
        .into_iter()
        .filter(|(_, prefix)| !prefix.is_empty() && !seen.contains(prefix.as_str()))
        .filter_map(|(frame_id, _)| {
            let frame = manager.get_frame(&frame_id);
            if frame.is_some_and(|frame| frame.isolated_context_id.is_some()) {
                return None;
            }
            Some(MissingFrame {
                url: frame.map(|frame| frame.url.clone()).unwrap_or_default(),
                frame_id,
            })
        })
        .collect()
}

struct FrameTree {
    tree: String,
    refs: serde_json::Map<String, Value>,
}

fn snapshot_frame(
    manager: &mut FrameManager,
    frame_id: &str,
    prefix: &str,
    options: &Value,
) -> Result<FrameTree, String> {
    let mut options = options.clone();
    options["refPrefix"] = json!(prefix);
    let value = manager.evaluate_in_frame(
        frame_id,
        "function(opts) { return globalThis.__mahoPage.takeSnapshot(opts); }",
        options,
    )?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(error.to_owned());
    }
    Ok(FrameTree {
        tree: value
            .get("tree")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        refs: value
            .get("refs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    })
}

/// For each iframe ref of a frame, the child frame it hosts and the origin label.
fn iframe_children(
    manager: &FrameManager,
    frame_id: &str,
    refs: &serde_json::Map<String, Value>,
) -> Vec<(String, Option<String>, Option<&'static str>)> {
    let Some(frame) = manager.get_frame(frame_id) else {
        return Vec::new();
    };
    let Some(context) = frame.isolated_context_id else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (reference, meta) in refs {
        let is_iframe = meta
            .get("tagName")
            .and_then(Value::as_str)
            .is_some_and(|tag| tag.eq_ignore_ascii_case("iframe"));
        if !is_iframe {
            continue;
        }
        let Ok(handle) = manager.client.send(
            "Runtime.callFunctionOn",
            json!({"functionDeclaration": "function(ref) { return globalThis.__mahoPage.elementRegistry.get(ref); }", "arguments": [{"value": reference}], "executionContextId": context, "returnByValue": false}),
            Some(&frame.session_id),
        ) else {
            continue;
        };
        let Some(object) = handle.pointer("/result/objectId").and_then(Value::as_str) else {
            continue;
        };
        let described = manager.client.send(
            "DOM.describeNode",
            json!({"objectId": object}),
            Some(&frame.session_id),
        );
        let _ = manager.client.send(
            "Runtime.releaseObject",
            json!({"objectId": object}),
            Some(&frame.session_id),
        );
        let Ok(described) = described else { continue };
        let node = &described["node"];
        let child = node
            .get("frameId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let child_url = child
            .as_ref()
            .and_then(|id| manager.get_frame(id))
            .map(|frame| frame.url.clone())
            .unwrap_or_default();
        let src = node
            .get("attributes")
            .and_then(Value::as_array)
            .and_then(|attributes| {
                attributes
                    .chunks(2)
                    .find(|pair| pair.first().and_then(Value::as_str) == Some("src"))
                    .and_then(|pair| pair.get(1).and_then(Value::as_str))
            })
            .unwrap_or_default();
        let label =
            known_iframe_origin_label(&child_url).or_else(|| known_iframe_origin_label(src));
        out.push((reference.clone(), child, label));
    }
    out
}

/// `snapshotWithFrames`: reconciles out-of-process iframes, snapshots every frame with its
/// `f<N>` prefix and stitches the child trees under their iframe lines. `options` are the page
/// world's `takeSnapshot` options (`interactive`, `maxDepth`, `maxChars`).
pub fn snapshot_with_frames(
    manager: &mut FrameManager,
    target_id: &str,
    options: &Value,
) -> Result<FramesSnapshot, String> {
    manager.refresh()?;
    reconcile_frames(manager, target_id)?;
    let main = manager
        .main_frame_id
        .clone()
        .ok_or("Main frame is not available")?;
    let prefixes = manager.frame_prefixes();
    manager.prefixes = prefixes
        .iter()
        .filter(|(_, prefix)| !prefix.is_empty())
        .map(|(frame, prefix)| (prefix.clone(), frame.clone()))
        .collect();
    let mut trees: HashMap<String, FrameTree> = HashMap::new();
    for (frame_id, prefix) in &prefixes {
        match snapshot_frame(manager, frame_id, prefix, options) {
            Ok(tree) => {
                trees.insert(frame_id.clone(), tree);
            }
            Err(error) if frame_id == &main => return Err(error),
            Err(_) => {}
        }
    }
    let mut parent_links: HashMap<String, HashMap<String, String>> = HashMap::new();
    for (frame_id, _) in &prefixes {
        let Some(tree) = trees.get(frame_id) else {
            continue;
        };
        let children = iframe_children(manager, frame_id, &tree.refs);
        let mut links = HashMap::new();
        let mut labels = Vec::new();
        for (reference, child, label) in children {
            if let Some(child) = child {
                links.insert(child, reference.clone());
            }
            if let Some(label) = label {
                labels.push((reference, label));
            }
        }
        if let Some(tree) = trees.get_mut(frame_id) {
            for (reference, label) in labels {
                tree.tree = tag_iframe_line(&tree.tree, &reference, label);
            }
        }
        parent_links.insert(frame_id.clone(), links);
    }
    let mut refs = serde_json::Map::new();
    for tree in trees.values() {
        refs.extend(tree.refs.clone());
    }
    let mut visits = manager.collect_descendant_frames(&main);
    visits.sort_by(|a, b| b.depth.cmp(&a.depth));
    for visit in visits {
        let Some(parent) = visit.parent_frame_id.clone() else {
            continue;
        };
        if visit.frame_id == main {
            continue;
        }
        let Some(child) = trees.get(&visit.frame_id).map(|tree| tree.tree.clone()) else {
            continue;
        };
        if child.is_empty() || !trees.contains_key(&parent) {
            continue;
        }
        let link = parent_links
            .get(&parent)
            .and_then(|links| links.get(&visit.frame_id))
            .cloned();
        let label = manager
            .get_frame(&visit.frame_id)
            .and_then(|frame| known_iframe_origin_label(&frame.url));
        if let Some(parent_tree) = trees.get_mut(&parent) {
            parent_tree.tree = match link {
                Some(reference) => inline_child_tree(&parent_tree.tree, &reference, &child),
                None => append_orphan_child_tree(&parent_tree.tree, &child, label),
            };
        }
    }
    let tree = trees
        .remove(&main)
        .map(|tree| tree.tree)
        .unwrap_or_default();
    let missing_frames = collect_missing_frames(manager, &refs);
    Ok(FramesSnapshot {
        tree,
        refs,
        missing_frames,
    })
}

#[cfg(test)]
mod tests {
    use super::super::fake_cdp::FakeCdp;
    use super::*;
    use std::sync::{Arc, Mutex, PoisonError};

    fn frame_tree() -> Value {
        json!({"frameTree": {"frame": {"id": "M", "url": "https://a.test/"}, "childFrames": [
            {"frame": {"id": "C1", "parentId": "M", "url": "https://a.test/c1"}, "childFrames": [
                {"frame": {"id": "G1", "parentId": "C1", "url": "https://a.test/g1"}}]},
            {"frame": {"id": "C2", "parentId": "M", "url": "https://a.test/c2"}}]}})
    }

    #[test]
    fn descendant_frames_are_breadth_first_and_prefixed_f1_to_fn() {
        let tree = frame_tree();
        let cdp = FakeCdp::new(Box::new(move |_: &str, _: &Value, _: Option<&str>| {
            Ok(tree.clone())
        }));
        let manager =
            FrameManager::new(cdp.client.clone(), "S").unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(manager.main_frame_id.as_deref(), Some("M"));
        let order: Vec<(String, usize)> = manager
            .collect_descendant_frames("M")
            .into_iter()
            .map(|visit| (visit.frame_id, visit.depth))
            .collect();
        assert_eq!(
            order,
            [
                ("M".to_owned(), 0usize),
                ("C1".to_owned(), 1),
                ("C2".to_owned(), 1),
                ("G1".to_owned(), 2)
            ]
        );
        let prefixes: Vec<String> = manager
            .frame_prefixes()
            .into_iter()
            .map(|(_, prefix)| prefix)
            .collect();
        assert_eq!(prefixes, ["", "f1", "f2", "f3"]);
        assert!(manager.is_attributed("G1"));
        assert!(!manager.is_attributed("nope"));
        assert!(manager.collect_descendant_frames("nope").is_empty());
    }

    #[test]
    fn stitching_helpers_match_upstream_output() {
        let parent = "- heading \"Top\" [ref=e1]\n- iframe [ref=e2]";
        assert_eq!(
            inline_child_tree(parent, "e2", "- button \"Go\" [ref=f1e1]"),
            "- heading \"Top\" [ref=e1]\n- iframe [ref=e2]:\n  - button \"Go\" [ref=f1e1]"
        );
        assert_eq!(inline_child_tree(parent, "e2", ""), format!("{parent}\n  "));
        assert_eq!(
            append_orphan_child_tree("- a", "- b\n- c", Some("1Password")),
            "- a\n- iframe [origin=\"1Password\"]:\n  - b\n  - c"
        );
        assert_eq!(append_orphan_child_tree("- a", "", None), "- a");
        assert_eq!(
            tag_iframe_line(parent, "e2", "Bitwarden"),
            "- heading \"Top\" [ref=e1]\n- iframe [origin=\"Bitwarden\"] [ref=e2]"
        );
        let tagged = tag_iframe_line(parent, "e2", "Bitwarden");
        assert_eq!(tag_iframe_line(&tagged, "e2", "Other"), tagged);
        assert_eq!(
            known_iframe_origin_label("chrome-extension://nngceckbapebfimnlniiiahkandclblb/x.html"),
            Some("Bitwarden")
        );
        assert_eq!(ref_prefix("f12e3"), "f12");
        assert_eq!(ref_prefix("e3"), "");
        assert_eq!(ref_prefix("f1"), "");
    }

    #[test]
    fn missing_frames_are_those_without_refs_or_a_world() {
        let tree = frame_tree();
        let cdp = FakeCdp::new(Box::new(move |_: &str, _: &Value, _: Option<&str>| {
            Ok(tree.clone())
        }));
        let mut manager =
            FrameManager::new(cdp.client.clone(), "S").unwrap_or_else(|error| panic!("{error}"));
        if let Some(frame) = manager.frames.get_mut("C2") {
            frame.isolated_context_id = Some(7);
        }
        let mut refs = serde_json::Map::new();
        refs.insert("e1".into(), json!({}));
        refs.insert("f1e1".into(), json!({}));
        let missing = collect_missing_frames(&manager, &refs);
        assert_eq!(
            missing,
            [MissingFrame {
                frame_id: "G1".into(),
                url: "https://a.test/g1".into()
            }]
        );
    }

    #[test]
    fn reconcile_adopts_an_existing_oopif_and_detaches_foreign_ones() {
        let state = Arc::new(Mutex::new(0u32));
        let counter = Arc::clone(&state);
        let cdp = FakeCdp::new(Box::new(
            move |method: &str, params: &Value, session: Option<&str>| {
                Ok(match (method, session) {
                    ("Page.getFrameTree", Some("S")) => {
                        json!({"frameTree": {"frame": {"id": "M", "url": "https://a.test/"}}})
                    }
                    ("Page.getFrameTree", Some("child-O")) => {
                        json!({"frameTree": {"frame": {"id": "O", "parentId": "M", "url": "https://b.test/"}}})
                    }
                    ("Page.getFrameTree", Some("child-X")) => {
                        json!({"frameTree": {"frame": {"id": "X", "parentId": "elsewhere", "url": "https://c.test/"}}})
                    }
                    ("Target.getTargetInfo", _) => {
                        json!({"targetInfo": {"targetId": "T", "browserContextId": "ctx"}})
                    }
                    ("Target.getTargets", _) => json!({"targetInfos": [
                    {"targetId": "O", "type": "iframe", "browserContextId": "ctx"},
                    {"targetId": "X", "type": "iframe", "browserContextId": "ctx"},
                    {"targetId": "Z", "type": "iframe", "browserContextId": "other"}]}),
                    ("Target.attachToTarget", _) => {
                        json!({"sessionId": format!("child-{}", params["targetId"].as_str().unwrap_or(""))})
                    }
                    ("Page.createIsolatedWorld", _) => {
                        *counter.lock().unwrap_or_else(PoisonError::into_inner) += 1;
                        json!({"executionContextId": 41})
                    }
                    ("Runtime.callFunctionOn", _) => json!({"result": {"value": "object"}}),
                    _ => json!({}),
                })
            },
        ));
        let mut manager =
            FrameManager::new(cdp.client.clone(), "S").unwrap_or_else(|error| panic!("{error}"));
        let added = reconcile_frames(&mut manager, "T").unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].frame_id, "O");
        assert_eq!(added[0].parent_frame_id, "M");
        assert_eq!(added[0].session_id, "child-O");
        assert_eq!(added[0].isolated_context_id, Some(41));
        assert!(manager.is_attributed("O"));
        let detached: Vec<Value> = cdp
            .calls_of("Target.detachFromTarget")
            .into_iter()
            .map(|call| call.params["sessionId"].clone())
            .collect();
        assert!(!detached.is_empty());
        assert!(
            detached.iter().all(|session| session == "child-X"),
            "only the unattributable frame is detached: {detached:?}"
        );
        assert!(
            cdp.calls_of("Target.attachToTarget")
                .iter()
                .all(|call| call.params["targetId"] != "Z"),
            "targets of other browser contexts are never attached"
        );
        assert_eq!(
            reconcile_frames(&mut manager, "T").map(|added| added.len()),
            Ok(0),
            "nothing left to recover"
        );
    }

    #[test]
    fn snapshot_with_frames_stitches_child_trees_under_their_iframe() {
        let cdp = FakeCdp::new(Box::new(
            |method: &str, params: &Value, session: Option<&str>| {
                Ok(match method {
                    "Page.getFrameTree" => {
                        json!({"frameTree": {"frame": {"id": "M", "url": "https://a.test/"}, "childFrames": [
                        {"frame": {"id": "C", "parentId": "M", "url": "https://a.test/c"}}]}})
                    }
                    "Target.getTargetInfo" => json!({"targetInfo": {"targetId": "T"}}),
                    "Target.getTargets" => json!({"targetInfos": []}),
                    "Page.createIsolatedWorld" => {
                        let context = if params["frameId"] == "M" { 1 } else { 2 };
                        json!({"executionContextId": context})
                    }
                    "Runtime.callFunctionOn" => {
                        let declaration = params["functionDeclaration"].as_str().unwrap_or("");
                        if declaration.contains("typeof globalThis.__mahoPage") {
                            json!({"result": {"value": "object"}})
                        } else if declaration.contains("takeSnapshot") {
                            let prefix = params["arguments"][0]["value"]["refPrefix"]
                                .as_str()
                                .unwrap_or("");
                            if prefix.is_empty() {
                                json!({"result": {"value": {"tree": "- heading \"Top\" [ref=e1]\n- iframe [ref=e2]", "refs": {"e1": {"tagName": "H1"}, "e2": {"tagName": "IFRAME"}}}}})
                            } else {
                                json!({"result": {"value": {"tree": "- button \"Go\" [ref=f1e1]", "refs": {"f1e1": {"tagName": "BUTTON"}}}}})
                            }
                        } else {
                            json!({"result": {"objectId": "obj-e2"}})
                        }
                    }
                    "DOM.describeNode" => {
                        json!({"node": {"frameId": "C", "attributes": ["src", "https://a.test/c"]}})
                    }
                    _ => {
                        let _ = session;
                        json!({})
                    }
                })
            },
        ));
        let mut manager =
            FrameManager::new(cdp.client.clone(), "S").unwrap_or_else(|error| panic!("{error}"));
        let snapshot = snapshot_with_frames(&mut manager, "T", &json!({"interactive": false}))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            snapshot.tree,
            "- heading \"Top\" [ref=e1]\n- iframe [ref=e2]:\n  - button \"Go\" [ref=f1e1]"
        );
        let mut keys: Vec<&String> = snapshot.refs.keys().collect();
        keys.sort();
        assert_eq!(keys, ["e1", "e2", "f1e1"]);
        assert!(snapshot.missing_frames.is_empty());
        assert_eq!(
            manager
                .frame_for_ref("f1e1")
                .map(|frame| frame.frame_id.as_str()),
            Some("C")
        );
        assert_eq!(
            manager
                .frame_for_ref("e2")
                .map(|frame| frame.frame_id.as_str()),
            Some("M")
        );
    }
}
