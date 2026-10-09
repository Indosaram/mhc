use crate::grok_mermaid::graph::{
    empty_class_info, parse_dir, ClassInfo, Edge, Graph, Head, LineKind, Shape, MAX_EDGES,
    MAX_GROUP_DEPTH, MAX_GROUPS, MAX_MEMBERS, MAX_NODES,
};
use crate::grok_mermaid::labels::{
    ascii_lower, clean_label, decode_html_entities, is_id_char, src_lines,
};
use std::collections::HashMap;

fn first(s: &str) -> &str { s.split_whitespace().next().unwrap_or("") }
fn ws(s: &str) -> Vec<&str> { s.split_whitespace().collect() }
fn once<'a>(s: &'a str, p: &str) -> Option<(&'a str, &'a str)> {
    s.find(p).map(|i| (&s[..i], &s[i + p.len()..]))
}
fn ne(s: String) -> Option<String> { (!s.is_empty()).then_some(s) }
fn kind(ss: &[String]) -> Option<String> { ss.first().map(|s| ascii_lower(first(s))) }

fn flush(s: &mut String, out: &mut Vec<String>) {
    let x = s.trim();
    if !x.is_empty() { out.push(x.into()); }
    s.clear();
}

pub fn split_statements(line: &str, out: &mut Vec<String>) {
    let c: Vec<char> = line.chars().collect();
    let mut s = String::new();
    let mut q = false;
    let mut i = 0;
    while i < c.len() {
        if q {
            if c[i] == '"' { q = false; }
            s.push(c[i]);
        } else if c[i] == '"' {
            q = true; s.push(c[i]);
        } else if c[i] == '%' && c.get(i + 1) == Some(&'%') {
            break;
        } else if c[i] == ';' {
            flush(&mut s, out);
        } else {
            s.push(c[i]);
        }
        i += 1;
    }
    flush(&mut s, out);
}

pub fn statements_of(src: &str) -> Vec<String> {
    let mut o = Vec::new();
    for l in src_lines(src) { split_statements(&l, &mut o); }
    o
}

pub type DiagramKind = &'static str;

pub fn diagram_kind(src: &str) -> Option<DiagramKind> {
    let k = kind(&statements_of(src))?;
    if k == "graph" || k == "flowchart" { Some("flowchart") }
    else if k.starts_with("statediagram") { Some("state") }
    else if k.starts_with("classdiagram") { Some("class") }
    else if k == "erdiagram" { Some("er") }
    else if k == "sequencediagram" { Some("sequence") }
    else { None }
}

fn line_kind(s: &str) -> LineKind {
    if s.contains('=') { LineKind::Thick }
    else if s.contains('.') { LineKind::Dotted }
    else { LineKind::Solid }
}

fn skip(a: &[char], mut i: usize) -> usize {
    while matches!(a.get(i), Some(' ') | Some('\t')) { i += 1; }
    i
}

fn read_shape(
    a: &[char], start: usize, close: &str, sh: Shape,
) -> (Shape, Option<String>, usize) {
    let mut i = start;
    while matches!(a.get(i), Some(' ') | Some('\t')) { i += 1; }
    let quoted = a.get(i) == Some(&'"');
    let mut q = false;
    let mut s = String::new();
    while i < a.len() {
        if quoted && a[i] == '"' {
            q = !q; s.push(a[i]); i += 1; continue;
        }
        if !q && a[i..].iter().collect::<String>().starts_with(close) {
            return (sh, Some(clean_label(&s)), i + close.len());
        }
        s.push(a[i]); i += 1;
    }
    (sh, Some(clean_label(&s)), i)
}

fn read_shape_at(a: &[char], i: usize) -> (Shape, Option<String>, usize) {
    if i >= a.len() { return (Shape::Rect, None, i); }
    match (a[i], a.get(i + 1).copied()) {
        ('[', Some('[')) => read_shape(a, i + 2, "]]", Shape::Rect),
        ('[', Some('(')) => read_shape(a, i + 2, ")]", Shape::Round),
        ('[', _) => read_shape(a, i + 1, "]", Shape::Rect),
        ('(', Some('(')) => read_shape(a, i + 2, "))", Shape::Round),
        ('(', Some('[')) => read_shape(a, i + 2, "])", Shape::Round),
        ('(', _) => read_shape(a, i + 1, ")", Shape::Round),
        ('{', Some('{')) => read_shape(a, i + 2, "}}", Shape::Diamond),
        ('{', _) => read_shape(a, i + 1, "}", Shape::Diamond),
        ('>', _) => read_shape(a, i + 1, "]", Shape::Rect),
        _ => (Shape::Rect, None, i),
    }
}

fn parse_node(
    a: &[char], start: usize, g: &mut Graph,
) -> Option<(usize, usize)> {
    let mut i = skip(a, start);
    let b = i;
    while i < a.len() && is_id_char(a[i]) { i += 1; }
    if b == i { return None; }
    let id: String = a[b..i].iter().collect();
    let (sh, lab, mut n) = read_shape_at(a, i);
    let x = g.node_index(&id, lab.as_deref(), sh)?;
    if a.get(n..n + 3) == Some(&[':', ':', ':']) {
        let z = n + 3;
        let mut k = z;
        while k < a.len() && (is_id_char(a[k]) || a[k] == '-') { k += 1; }
        while k > z && a[k - 1] == '-' { k -= 1; }
        if k > z { n = k; }
    }
    Some((x, n))
}

fn parse_node_group(
    a: &[char], start: usize, g: &mut Graph,
) -> Option<(Vec<usize>, usize)> {
    let (x, mut i) = parse_node(a, start, g)?;
    let mut v = vec![x];
    loop {
        let j = skip(a, i);
        if a.get(j) != Some(&'&') { return Some((v, i)); }
        let (x, n) = parse_node(a, j + 1, g)?;
        v.push(x); i = n;
    }
}

fn trailing_head(a: &[char], i: usize) -> Option<(Head, usize)> {
    let h = match a.get(i) {
        Some('o') => Head::Circle,
        Some('x') => Head::Cross,
        _ => return None,
    };
    let ok = a.get(i + 1).map_or(true, |c| matches!(c, ' ' | '\t' | '|' | '&' | ';'));
    ok.then_some((h, i + 1))
}

fn parse_link(
    a: &[char], start: usize,
) -> Option<(Head, Head, LineKind, Option<String>, usize)> {
    let mut i = skip(a, start);
    let mut left = Head::None;
    if matches!(a.get(i), Some('o') | Some('x')) &&
       matches!(a.get(i + 1), Some('-') | Some('.') | Some('=')) {
        left = if a[i] == 'o' { Head::Circle } else { Head::Cross };
        i += 1;
    }
    let b = i;
    while i < a.len() && "-.=<>".contains(a[i]) { i += 1; }
    if b == i { return None; }
    let op = a[b..i].iter().collect::<String>();
    if left == Head::None && op.starts_with('<') { left = Head::Arrow; }
    let mut right = if op.contains('>') { Head::Arrow } else { Head::None };
    if right == Head::None {
        if let Some((h, n)) = trailing_head(a, i) { right = h; i = n; }
    }
    if a.get(i) == Some(&'|') {
        i += 1;
        let b = i;
        while i < a.len() && a[i] != '|' { i += 1; }
        let x = clean_label(&a[b..i].iter().collect::<String>());
        if i < a.len() { i += 1; }
        return Some((left, right, line_kind(&op), ne(x), i));
    }
    if right == Head::None {
        let b = skip(a, i);
        let mut j = b;
        while j < a.len() && !"-.=<>".contains(a[j]) { j += 1; }
        if j > b && j < a.len() && a[j] != '<' {
            let text = a[b..j].iter().collect::<String>();
            let z = j;
            while j < a.len() && "-.=<>".contains(a[j]) { j += 1; }
            let op2 = a[z..j].iter().collect::<String>();
            right = if op2.contains('>') { Head::Arrow } else { Head::None };
            if right == Head::None {
                if let Some((h, n)) = trailing_head(a, j) { right = h; j = n; }
            }
            let lk = if op == "-" { line_kind(&op2) } else { line_kind(&op) };
            return Some((left, right, lk, ne(clean_label(&text)), j));
        }
    }
    Some((left, right, line_kind(&op), None, i))
}

fn parse_statement(s: &str, g: &mut Graph) {
    let a: Vec<char> = s.chars().collect();
    let Some((mut prev, mut i)) = parse_node_group(&a, 0, g) else { return };
    loop {
        i = skip(&a, i);
        if i >= a.len() { break; }
        let Some((lf, rt, lk, lab, n)) = parse_link(&a, i) else { break };
        let Some((to, j)) = parse_node_group(&a, n, g) else { break };
        for &f0 in &prev {
            for &t0 in &to {
                let rev = lf == Head::Arrow && rt != Head::Arrow;
                let (f, t, hf, ht) = if rev {
                    (t0, f0, Head::Arrow, rt)
                } else {
                    (f0, t0, lf, rt)
                };
                if !g.push_edge(Edge {
                    from: f,
                    to: t,
                    label: lab.clone(),
                    head_to: ht,
                    head_from: hf,
                    line: lk,
                }) { return; }
            }
        }
        prev = to; i = j;
    }
}

fn parse_subgraph_decl(s: &str) -> (String, String) {
    if s.starts_with('"') {
        if let Some(x) = s[1..].find('"') {
            let z = &s[1..x + 1];
            return (z.into(), decode_html_entities(z));
        }
    }
    if let Some(x) = s.find('[') {
        let id = s[..x].trim().to_string();
        let label = clean_label(s[x + 1..].trim_end_matches(']').trim());
        if !id.is_empty() && !label.is_empty() { return (id, label); }
    }
    (s.into(), s.into())
}

pub fn parse_graph(src: &str) -> Option<Graph> {
    let ss = statements_of(src);
    let k = kind(&ss)?;
    if k != "graph" && k != "flowchart" { return None; }
    let mut g = Graph::new(parse_dir(ws(&ss[0]).get(1).copied().unwrap_or("TB")));
    let mut stack = Vec::new();
    for s in ss.iter().skip(1) {
        let f = ascii_lower(first(s));
        if f == "subgraph" {
            if g.groups.len() >= MAX_GROUPS || stack.len() >= MAX_GROUP_DEPTH { return None; }
            let (id, label) = parse_subgraph_decl(s["subgraph".len()..].trim());
            g.groups.push(crate::grok_mermaid::graph::Group {
                id, label, parent: stack.last().copied(),
            });
            stack.push(g.groups.len() - 1);
            g.cur_group = stack.last().copied();
        } else if f == "end" {
            stack.pop(); g.cur_group = stack.last().copied();
        } else if !matches!(f.as_str(),
            "classdef" | "class" | "style" | "linkstyle" | "click" | "direction") {
            parse_statement(s, &mut g);
            if g.over_cap { return None; }
        }
    }
    (!g.nodes.is_empty()).then_some(g)
}

fn drop_style_tags(s: &str) -> String {
    let a: Vec<char> = s.chars().collect();
    let mut o = String::new();
    let mut i = 0;
    while i < a.len() {
        if a.get(i..i + 3) == Some(&[':', ':', ':']) {
            let b = i + 3;
            let mut j = b;
            while j < a.len() && (is_id_char(a[j]) || a[j] == '-') { j += 1; }
            while j > b && a[j - 1] == '-' { j -= 1; }
            if j > b { i = j; continue; }
        }
        o.push(a[i]); i += 1;
    }
    o
}

fn state_endpoint(g: &mut Graph, id: &str, source: bool) -> Option<usize> {
    if id == "[*]" {
        g.node_index(if source { "[*]start" } else { "[*]end" }, Some("●"), Shape::Round)
    } else {
        g.node_index(id, None, Shape::Round)
    }
}

fn parse_state_decl(s: &str, g: &mut Graph) -> Option<()> {
    let r = s["state".len()..].trim().trim_end_matches('{').trim();
    if r.is_empty() { return Some(()); }
    if r.starts_with('"') {
        let q = r[1..].find('"')? + 1;
        let label = &r[1..q];
        let z = r[q + 1..].trim();
        let id = z.strip_prefix("as").unwrap_or(label).trim();
        g.node_label(id, &decode_html_entities(label)).map(|_| ())
    } else {
        let mut id = r;
        let mut sh = Shape::Round;
        let mut stereo = false;
        if let Some(p) = r.find("<<") {
            let x = r[p + 2..].trim_end_matches(">>").trim();
            if x == "choice" { sh = Shape::Diamond; }
            id = r[..p].trim(); stereo = true;
        }
        if id.is_empty() || id.chars().any(char::is_whitespace) { return None; }
        g.node_index(id, stereo.then_some(id), sh).map(|_| ())
    }
}

fn parse_transition(s: &str, g: &mut Graph) -> Option<()> {
    let mut rest = s;
    let mut prev = None;
    loop {
        let Some((lhs, rhs)) = once(rest, "-->") else { break };
        let from_id = lhs.trim().trim_end_matches('-').trim();
        let from = if let Some(x) = prev {
            if !from_id.is_empty() { return None; }
            x
        } else {
            state_endpoint(g, from_id, true)?
        };
        let (to_raw, tail) = if let Some(x) = rhs.find("-->") {
            (&rhs[..x], &rhs[x..])
        } else { (rhs, "") };
        let (to0, lab) = once(to_raw, ":")
            .map(|(a, b)| (a, Some(decode_html_entities(b.trim()))))
            .unwrap_or((to_raw, None));
        let to_id = to0.trim().trim_start_matches('>').trim_end_matches('-').trim();
        let to = state_endpoint(g, to_id, false)?;
        if !g.push_edge(Edge {
            from,
            to,
            label: lab.and_then(ne),
            head_to: Head::Arrow,
            head_from: Head::None,
            line: LineKind::Solid,
        }) {
            return Some(());
        }
        prev = Some(to);
        rest = tail;
    }
    Some(())
}

fn parse_state_desc(s: &str, g: &mut Graph) -> Option<()> {
    if let Some((a, b)) = once(s, ":") {
        let id = a.trim();
        let d = b.trim();
        if id.is_empty() || d.is_empty() || id.chars().any(char::is_whitespace) { return None; }
        g.node_label(id, &decode_html_entities(d)).map(|_| ())
    } else {
        if s.chars().any(char::is_whitespace) { return None; }
        g.node_index(s, None, Shape::Round).map(|_| ())
    }
}

pub fn parse_state(src: &str) -> Option<Graph> {
    let ss = statements_of(src);
    if !kind(&ss)?.starts_with("statediagram") { return None; }
    let mut g = Graph::new(parse_dir("TB"));
    let mut note = false;
    for raw in ss.iter().skip(1) {
        if note {
            if ascii_lower(raw) == "end note" { note = false; }
            continue;
        }
        let s = drop_style_tags(raw);
        let f = ascii_lower(first(&s));
        if f == "direction" {
            g.dir = parse_dir(ws(&s).get(1).copied().unwrap_or(""));
        } else if f == "note" {
            if !s.contains(':') { note = true; }
        } else if f == "state" {
            parse_state_decl(&s, &mut g)?;
        } else if s.contains("-->") {
            parse_transition(&s, &mut g)?;
        } else if !matches!(f.as_str(),
            "classdef" | "class" | "hide" | "scale" | "}" | "--") {
            parse_state_desc(&s, &mut g)?;
        }
        if g.over_cap { return None; }
    }
    (!g.nodes.is_empty()).then_some(g)
}

pub fn push_member(i: &mut ClassInfo, raw: &str) {
    if raw.starts_with("<<") {
        if let Some((a, _)) = once(&raw[2..], ">>") { i.annotation = Some(a.trim().into()); }
        return;
    }
    let x = decode_html_entities(&display_generics(raw.trim()));
    let l = if x.contains('(') { &mut i.methods } else { &mut i.attrs };
    if l.len() < MAX_MEMBERS { l.push(x); }
    else if l.len() == MAX_MEMBERS { l.push("…".into()); }
}

fn display_generics(s: &str) -> String {
    let mut o = String::new();
    let mut open = false;
    for c in s.chars() {
        if c == '~' { o.push(if open { '>' } else { '<' }); open = !open; }
        else { o.push(c); }
    }
    o
}

struct ClassRel {
    from: String, to: String, hf: Head, ht: Head, line: LineKind, label: Option<String>,
}

fn strip_cardinality_suffix(s: &str) -> (String, String) {
    let t = s.trim_end();
    if t.ends_with('"') {
        if let Some(q) = t[..t.len() - 1].rfind('"') {
            return (t[..q].trim_end().into(), t[q + 1..t.len() - 1].into());
        }
    }
    (t.into(), String::new())
}

fn strip_cardinality_prefix(s: &str) -> (String, String) {
    let t = s.trim_start();
    if t.starts_with('"') {
        if let Some(q) = t[1..].find('"') {
            return (t[q + 2..].trim_start().into(), t[1..q + 1].into());
        }
    }
    (t.into(), String::new())
}

fn parse_class_relation(s: &str) -> Option<ClassRel> {
    let ops: &[(&str, Head, Head, LineKind)] = &[
        ("<|--", Head::Triangle, Head::None, LineKind::Solid),
        ("--|>", Head::None, Head::Triangle, LineKind::Solid),
        ("<|..", Head::Triangle, Head::None, LineKind::Dotted),
        ("..|>", Head::None, Head::Triangle, LineKind::Dotted),
        ("*--", Head::DiamondFill, Head::None, LineKind::Solid),
        ("--*", Head::None, Head::DiamondFill, LineKind::Solid),
        ("o--", Head::DiamondOpen, Head::None, LineKind::Solid),
        ("--o", Head::None, Head::DiamondOpen, LineKind::Solid),
        ("<--", Head::Arrow, Head::None, LineKind::Solid),
        ("-->", Head::None, Head::Arrow, LineKind::Solid),
        ("<..", Head::Arrow, Head::None, LineKind::Dotted),
        ("..>", Head::None, Head::Arrow, LineKind::Dotted),
        ("--", Head::None, Head::None, LineKind::Solid),
        ("..", Head::None, Head::None, LineKind::Dotted),
    ];
    let mut found = None;
    for (p, hf, ht, l) in ops {
        if let Some(x) = s.find(p) { found = Some((x, *p, *hf, *ht, *l)); break; }
    }
    let (p, op, hf, ht, line) = found?;
    let (lhs, cf) = strip_cardinality_suffix(&s[..p]);
    let (rhs, ct) = strip_cardinality_prefix(&s[p + op.len()..]);
    let (to, lab) = once(&rhs, ":")
        .map(|(a, b)| (a.trim(), ne(decode_html_entities(b.trim()))))
        .unwrap_or((rhs.trim(), None));
    if lhs.is_empty() || to.is_empty() ||
       lhs.chars().any(char::is_whitespace) || to.chars().any(char::is_whitespace) {
        return None;
    }
    let label = ne([cf, lab.unwrap_or_default(), ct].join(" ").trim().into());
    Some(ClassRel { from: lhs, to: to.into(), hf, ht, line, label })
}

pub fn parse_class(src: &str) -> Option<(Graph, Vec<ClassInfo>)> {
    let ss = statements_of(src);
    if !kind(&ss)?.starts_with("classdiagram") { return None; }
    let mut g = Graph::new(parse_dir("TB"));
    let mut inf = Vec::new();
    let mut cur = None;
    let sync = |g: &Graph, i: &mut Vec<ClassInfo>| {
        while i.len() < g.nodes.len() { i.push(empty_class_info()); }
    };
    for raw in ss.iter().skip(1) {
        if let Some(n) = cur {
            if raw == "}" { cur = None; } else { push_member(&mut inf[n], raw); }
            continue;
        }
        let s = drop_style_tags(raw);
        let f = ascii_lower(first(&s));
        if f == "direction" {
            g.dir = parse_dir(ws(&s).get(1).copied().unwrap_or("")); continue;
        }
        if matches!(f.as_str(), "note" | "callback" | "click" | "link" | "style" |
            "cssclass" | "classdef" | "namespace" | "}") { continue; }
        if f == "class" {
            let r = s[5..].trim();
            let open = r.ends_with('{');
            let n = if open { r[..r.len() - 1].trim() } else { r };
            if n.is_empty() || n.chars().any(char::is_whitespace) { return None; }
            let x = g.node_index(n, None, Shape::Rect)?;
            sync(&g, &mut inf);
            if open { cur = Some(x); }
            continue;
        }
        if s.starts_with("<<") {
            let (a, b) = once(&s[2..], ">>")?;
            let n = b.trim();
            if n.is_empty() || n.chars().any(char::is_whitespace) { return None; }
            let x = g.node_index(n, None, Shape::Rect)?;
            sync(&g, &mut inf); inf[x].annotation = Some(a.trim().into()); continue;
        }
        if let Some(r) = parse_class_relation(&s) {
            let a = g.node_index(&r.from, None, Shape::Rect)?;
            let b = g.node_index(&r.to, None, Shape::Rect)?;
            if g.edges.len() >= MAX_EDGES { return None; }
            g.edges.push(crate::grok_mermaid::graph::Edge {
                from: a, to: b, label: r.label, head_to: r.ht,
                head_from: r.hf, line: r.line,
            });
            sync(&g, &mut inf); continue;
        }
        let (a, b) = once(&s, ":")?;
        if a.trim().is_empty() || b.trim().is_empty() { return None; }
        let x = g.node_index(a.trim(), None, Shape::Rect)?;
        sync(&g, &mut inf); push_member(&mut inf[x], b.trim());
    }
    if g.nodes.is_empty() { None } else { sync(&g, &mut inf); Some((g, inf)) }
}

pub fn push_er_attribute(i: &mut ClassInfo, raw: &str) {
    let x = raw.split('"').next().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
    if x.is_empty() { return; }
    let x = decode_html_entities(&x);
    if i.attrs.len() < MAX_MEMBERS { i.attrs.push(x); }
    else if i.attrs.len() == MAX_MEMBERS { i.attrs.push("…".into()); }
}

fn er_entity(g: &mut Graph, i: &mut Vec<ClassInfo>, s: &str) -> Option<usize> {
    let x = if let Some(p) = s.find('[') {
        let id = &s[..p];
        let l = clean_label(s[p + 1..].trim_end_matches(']'));
        if id.is_empty() || l.is_empty() { return None; }
        g.node_label(id, &l)?
    } else { g.node_index(s, None, Shape::Rect)? };
    while i.len() < g.nodes.len() { i.push(empty_class_info()); }
    Some(x)
}

fn er_card(s: &str) -> Option<&'static str> {
    match s {
        "|o" | "o|" => Some("0..1"),
        "||" => Some("1"),
        "}o" | "o{" => Some("0..*"),
        "}|" | "|{" => Some("1..*"),
        _ => None,
    }
}

fn parse_er_op(s: &str) -> Option<(&'static str, &'static str, LineKind)> {
    if s.len() != 6 || !s.is_ascii() { return None; }
    let line = match &s[2..4] {
        "--" => LineKind::Solid,
        ".." => LineKind::Dotted,
        _ => return None,
    };
    Some((er_card(&s[..2])?, er_card(&s[4..])?, line))
}

fn split_er_relationship(s: &str) -> Option<(&str, Option<&str>)> {
    let (r, l) = once(s, ":").map_or((s, None), |(a, b)| (a, Some(b.trim())));
    ws(r).iter().any(|x| parse_er_op(x).is_some()).then_some((r, l))
}

pub fn parse_er(src: &str) -> Option<(Graph, Vec<ClassInfo>)> {
    let ss = statements_of(src);
    if kind(&ss)? != "erdiagram" { return None; }
    let mut g = Graph::new(parse_dir("TB"));
    let mut inf = Vec::new();
    let mut cur = None;
    for s in ss.iter().skip(1) {
        if let Some(x) = cur {
            if s == "}" { cur = None; } else { push_er_attribute(&mut inf[x], s); }
            continue;
        }
        if let Some((r, lab)) = split_er_relationship(s) {
            let p = ws(r);
            if p.len() != 3 { return None; }
            let (cl, cr, line) = parse_er_op(p[1])?;
            let a = er_entity(&mut g, &mut inf, p[0])?;
            let b = er_entity(&mut g, &mut inf, p[2])?;
            if g.edges.len() >= MAX_EDGES { return None; }
            let x = lab.map(clean_label).unwrap_or_default();
            g.edges.push(crate::grok_mermaid::graph::Edge {
                from: a, to: b,
                label: ne(format!("{cl} {x} {cr}").trim().into()),
                head_to: Head::None, head_from: Head::None, line,
            });
        } else {
            let open = s.ends_with('{');
            let d = if open { s[..s.len() - 1].trim() } else { s.trim() };
            if d.is_empty() || ws(d).len() != 1 { return None; }
            let x = er_entity(&mut g, &mut inf, d)?;
            if open { cur = Some(x); }
        }
    }
    if g.nodes.is_empty() { None } else { Some((g, inf)) }
}

pub type SeqHead = &'static str;

#[derive(Clone, Debug)]
pub enum NoteAnchor {
    Over { from: usize, to: usize },
    Left { at: usize },
    Right { at: usize },
}

#[derive(Clone, Debug)]
pub enum SeqItem {
    Message { from: usize, to: usize, text: Option<String>, dashed: bool, head: SeqHead },
    Note { anchor: NoteAnchor, text: String },
    Divider { text: String },
}

pub struct Sequence {
    pub labels: Vec<String>,
    pub index: HashMap<String, usize>,
    pub items: Vec<SeqItem>,
}

impl Sequence {
    pub fn new() -> Self {
        Self { labels: vec![], index: HashMap::new(), items: vec![] }
    }
    pub fn participant(&mut self, id: &str, label: Option<&str>) -> Option<usize> {
        if let Some(&x) = self.index.get(id) {
            if let Some(l) = label { self.labels[x] = l.into(); }
            return Some(x);
        }
        if self.labels.len() >= MAX_NODES { return None; }
        let x = self.labels.len();
        self.index.insert(id.into(), x);
        self.labels.push(label.unwrap_or(id).into());
        Some(x)
    }
}

fn parse_note_anchor(s: &str, q: &mut Sequence) -> Option<(NoteAnchor, String)> {
    let lo = ascii_lower(s);
    let (k, r) = if lo.starts_with("over ") {
        ("over", &s[5..])
    } else if lo.starts_with("left of ") {
        ("left", &s[8..])
    } else if lo.starts_with("right of ") {
        ("right", &s[9..])
    } else { return None };
    let (ids, text) = once(r, ":")?;
    let p: Vec<&str> = ids.split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
    let a = q.participant(p.first().copied()?, None)?;
    let text = decode_html_entities(text.trim());
    if k == "left" { return Some((NoteAnchor::Left { at: a }, text)); }
    if k == "right" { return Some((NoteAnchor::Right { at: a }, text)); }
    let b = if p.len() > 1 { q.participant(p[1], None)? } else { a };
    Some((NoteAnchor::Over { from: a.min(b), to: a.max(b) }, text))
}

fn parse_seq_message(
    s: &str, q: &mut Sequence,
) -> Option<(usize, usize, Option<String>, bool, SeqHead)> {
    let ops: &[(&str, bool, SeqHead)] = &[
        ("-->>", true, "arrow"), ("->>", false, "arrow"),
        ("--x", true, "cross"), ("-x", false, "cross"),
        ("--)", true, "arrow"), ("-)", false, "arrow"),
        ("-->", true, "arrow"), ("->", false, "arrow"),
    ];
    let mut found = None;
    for (p, d, h) in ops {
        if let Some(i) = s.find(p) { found = Some((i, *p, *d, *h)); break; }
    }
    let (i, op, dashed, head) = found?;
    let from = s[..i].trim();
    if from.is_empty() { return None; }
    let r = s[i + op.len()..].trim_start().trim_start_matches(['+', '-']).trim_start();
    let (to, text) = once(r, ":")
        .map(|(a, b)| (a.trim(), ne(decode_html_entities(b.trim()))))
        .unwrap_or((r, None));
    if to.is_empty() { return None; }
    Some((q.participant(from, None)?, q.participant(to, None)?, text, dashed, head))
}

pub fn parse_sequence(src: &str) -> Option<Sequence> {
    let ss = statements_of(src);
    if kind(&ss)? != "sequencediagram" { return None; }
    let mut q = Sequence::new();
    let mut auto = false;
    let mut count = 0;
    let mut blocks = Vec::new();
    for s in ss.iter().skip(1) {
        let f = ascii_lower(first(s));
        if f == "participant" || f == "actor" {
            let r = s[first(s).len()..].trim();
            if r.is_empty() { return None; }
            if let Some((a, b)) = once(r, " as ") {
                q.participant(a.trim(), Some(clean_label(b).as_str()))?;
            } else { q.participant(r, None)?; }
        } else if f == "autonumber" {
            auto = true;
        } else if f == "note" {
            let (a, text) = parse_note_anchor(&s[first(s).len()..].trim(), &mut q)?;
            q.items.push(SeqItem::Note { anchor: a, text });
        } else if ["loop", "alt", "opt", "par", "critical", "break", "else", "and", "option"]
            .contains(&f.as_str()) {
            if ["else", "and", "option"].contains(&f.as_str()) && blocks.last() != Some(&true) {
                continue;
            }
            if !["else", "and", "option"].contains(&f.as_str()) { blocks.push(true); }
            q.items.push(SeqItem::Divider { text: decode_html_entities(s) });
        } else if f == "rect" || f == "box" {
            blocks.push(false);
        } else if f == "end" {
            if blocks.pop() == Some(true) {
                q.items.push(SeqItem::Divider { text: "end".into() });
            }
        } else if matches!(f.as_str(),
            "activate" | "deactivate" | "create" | "destroy" | "title" |
            "acctitle" | "accdescr" | "links" | "link" | "properties") {
        } else {
            let (from, to, mut text, dashed, head) = parse_seq_message(s, &mut q)?;
            if auto {
                count += 1;
                text = Some(match text {
                    Some(x) => format!("{count}. {x}"),
                    None => format!("{count}."),
                });
            }
            q.items.push(SeqItem::Message { from, to, text, dashed, head });
        }
        if q.items.len() >= MAX_EDGES { return None; }
    }
    (!q.labels.is_empty()).then_some(q)
}
