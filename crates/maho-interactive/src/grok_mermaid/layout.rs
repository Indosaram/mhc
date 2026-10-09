use super::canvas::{draw_text, draw_text_over_edges, Canvas, D, L, R, U};
use super::graph::{ClassInfo, Dir, Edge, Graph, Shape};
use super::labels::{fit_label, wrap_label, MAX_LABEL, MAX_LINES, WRAP_WIDTH};
use super::width::string_width;

const PAD: usize = 1;
const GAP_X: usize = 3;
const GAP_Y: usize = 2;
const MAX_CANVAS_CELLS: usize = 1 << 21;

pub type CanvasResult = Option<Canvas>;

fn sat(a: usize, b: usize) -> usize { a.saturating_sub(b) }
fn half(n: usize) -> usize { n / 2 }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Placed { pub x: usize, pub y: usize, pub w: usize, pub h: usize, pub cx: usize, pub cy: usize, pub rank: usize }

#[derive(Clone, Debug, Default)]
struct NodeSizes { box_w: Vec<usize>, box_h: Vec<usize>, lay_w: Vec<usize>, lay_h: Vec<usize>, extra_h: Vec<usize>, self_label_w: Vec<usize> }

#[derive(Clone, Debug)]
pub enum NodeExtra { Plain, Frame(Canvas), Compartments(Vec<Vec<String>>) }

#[derive(Clone, Debug, Default)]
struct RoutePlan { canvas_w: usize, canvas_h: usize, band_end: Vec<usize>, edge_bus: Vec<usize>, lane_base: usize, edge_lane: Vec<usize> }

pub fn compute_ranks(graph: &Graph) -> Vec<usize> {
    let n = graph.nodes.len(); let mut children = vec![Vec::new(); n]; let mut indeg = vec![0; n];
    for e in &graph.edges { if e.from != e.to { children[e.from].push(e.to); indeg[e.to] += 1; } }
    let mut color = vec![0u8; n]; let mut dag = vec![Vec::new(); n]; let mut order = Vec::new();
    let mut starts: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect(); starts.extend(0..n);
    for s in starts { if color[s] == 0 { dfs_dag(s, &children, &mut color, &mut dag, &mut order); } }
    let mut rank = vec![0; n];
    for &u in order.iter().rev() { for &v in &dag[u] { rank[v] = rank[v].max(rank[u] + 1); } }
    rank
}

fn dfs_dag(start: usize, children: &[Vec<usize>], color: &mut [u8], dag: &mut [Vec<usize>], order: &mut Vec<usize>) {
    let mut stack = vec![(start, 0usize)]; color[start] = 1;
    while let Some((u, i)) = stack.last_mut() {
        if *i < children[*u].len() { let v = children[*u][*i]; *i += 1; if color[v] == 1 { continue; } dag[*u].push(v); if color[v] == 0 { color[v] = 1; stack.push((v, 0)); } }
        else { color[*u] = 2; order.push(*u); stack.pop(); }
    }
}

pub fn count_crossings(edges: &[Edge], ranks: &[usize], pos: &[usize]) -> usize {
    let a: Vec<(usize, usize, usize)> = edges.iter().filter(|e| e.from != e.to && ranks[e.to] == ranks[e.from] + 1).map(|e| (ranks[e.from], pos[e.from], pos[e.to])).collect();
    let mut out = 0; for i in 0..a.len() { for j in i+1..a.len() { if a[i].0 == a[j].0 && ((a[i].1 < a[j].1 && a[i].2 > a[j].2) || (a[i].1 > a[j].1 && a[i].2 < a[j].2)) { out += 1; } } } out
}

pub fn order_ranks(by_rank: &mut [Vec<usize>], edges: &[Edge], ranks: &[usize]) {
    if by_rank.len() < 2 || ranks.len() < 3 { return; }
    let n = ranks.len(); let mut parents = vec![Vec::new(); n]; let mut children = vec![Vec::new(); n];
    for e in edges { if e.from != e.to && ranks[e.to] > ranks[e.from] { parents[e.to].push(e.from); children[e.from].push(e.to); } }
    let mut pos = vec![0; n]; for row in by_rank.iter() { for (i, &v) in row.iter().enumerate() { pos[v] = i; } }
    let mut best = by_rank.to_vec(); let mut best_c = count_crossings(edges, ranks, &pos); if best_c == 0 { return; }
    for it in 0..8 { let indices: Vec<usize> = if it % 2 == 0 { (1..by_rank.len()).collect() } else { (0..by_rank.len()-1).rev().collect() }; let neigh = if it % 2 == 0 { &parents } else { &children };
        for ri in indices { sort_by_barycenter(&mut by_rank[ri], neigh, &pos); for (i,&v) in by_rank[ri].iter().enumerate() { pos[v] = i; } }
        let c = count_crossings(edges, ranks, &pos); if c < best_c { best_c = c; best = by_rank.to_vec(); } if best_c == 0 { break; }
    }
    by_rank.clone_from_slice(&best);
}

fn sort_by_barycenter(row: &mut [usize], neigh: &[Vec<usize>], pos: &[usize]) {
    let mut keyed: Vec<(f64, usize)> = row.iter().map(|&v| {
        let key = if neigh[v].is_empty() { pos[v] as f64 } else {
            neigh[v].iter().map(|&u| pos[u] as f64).sum::<f64>() / neigh[v].len() as f64
        };
        (key, v)
    }).collect();
    keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    for (i, &(_, v)) in keyed.iter().enumerate() { row[i] = v; }
}

pub fn assign_positions(by_rank: &[Vec<usize>], size: &[usize], sep: usize, edges: &[Edge], ranks: &[usize]) -> Vec<usize> {
    let n = size.len(); let mut parents = vec![Vec::new(); n]; let mut children = vec![Vec::new(); n];
    for e in edges { if e.from != e.to && ranks[e.to] > ranks[e.from] { parents[e.to].push(e.from); children[e.from].push(e.to); } }
    let mut pos = vec![0f64; n]; for row in by_rank { let mut x = 0.; for &v in row { x += size[v] as f64 / 2.; pos[v] = x; x += size[v] as f64 / 2. + sep as f64; } }
    for it in 0..10 { let rows: Vec<&Vec<usize>> = if it % 2 == 0 { by_rank.iter().collect() } else { by_rank.iter().rev().collect() }; let neigh = if it % 2 == 0 { &parents } else { &children }; for row in rows { relax_rank(row, neigh, &mut pos, size, sep); } }
    let min_left = pos.iter().enumerate().map(|(v,p)| p - size[v] as f64 / 2.).fold(f64::INFINITY, f64::min).min(0.); pos.iter().map(|p| (p - min_left).max(0.).round() as usize).collect()
}

fn relax_rank(nodes: &[usize], neigh: &[Vec<usize>], pos: &mut [f64], size: &[usize], sep: usize) { if nodes.is_empty() { return; } let desired: Vec<f64> = nodes.iter().map(|&v| if neigh[v].is_empty() { pos[v] } else { neigh[v].iter().map(|&u| pos[u]).sum::<f64>() / neigh[v].len() as f64 }).collect(); let mut left = vec![0.; nodes.len()]; for i in 0..nodes.len() { left[i] = if i == 0 { desired[i] } else { desired[i].max(left[i-1] + (size[nodes[i-1]] + size[nodes[i]]) as f64 / 2. + sep as f64) }; } let mut right = vec![0.; nodes.len()]; for i in (0..nodes.len()).rev() { right[i] = if i+1 == nodes.len() { desired[i] } else { desired[i].min(right[i+1] - (size[nodes[i+1]] + size[nodes[i]]) as f64 / 2. - sep as f64) }; } for i in 0..nodes.len() { pos[nodes[i]] = (left[i]+right[i])/2.; } for i in 1..nodes.len() { let m = pos[nodes[i-1]] + (size[nodes[i-1]] + size[nodes[i]]) as f64 / 2. + sep as f64; if pos[nodes[i]] < m { pos[nodes[i]] = m; } } }

pub fn assign_tracks(spans: &[[usize; 5]]) -> (Vec<(usize, usize)>, usize) { let mut sorted = spans.to_vec(); sorted.sort(); let mut tracks: Vec<Vec<[usize;4]>> = Vec::new(); let mut assigned = Vec::new(); for [s,e,f,t,idx] in sorted { let slot = tracks.iter().position(|m| m.iter().all(|[s2,e2,f2,t2]| e2+2 <= s || e+2 <= *s2 || *f2 == f || *t2 == t)).unwrap_or_else(|| { tracks.push(Vec::new()); tracks.len()-1 }); tracks[slot].push([s,e,f,t]); assigned.push((idx,slot)); } (assigned, tracks.len()) }

pub fn orient(mut canvas: Canvas, graph: &Graph) -> Canvas { if graph.dir == Dir::Up { canvas.flip_vertical(); } else if graph.dir == Dir::Left { canvas.flip_horizontal(); } canvas }

pub fn layout_canvas(graph: &Graph, extras: &[NodeExtra]) -> CanvasResult {
    if graph.nodes.is_empty() { return None; }
    let ranks = compute_ranks(graph); let max_rank = ranks.iter().copied().max().unwrap_or(0); let mut by = vec![Vec::new(); max_rank+1]; for (i,&r) in ranks.iter().enumerate() { by[r].push(i); } order_ranks(&mut by, &graph.edges, &ranks);
    let wrapped: Vec<Vec<String>> = graph.nodes.iter().map(|n| wrap_label(&n.label, WRAP_WIDTH, MAX_LINES)).collect(); let mut placed = vec![Placed::default(); graph.nodes.len()]; let mut widths=Vec::new(); let mut heights=Vec::new();
    for (i,e) in extras.iter().enumerate() { match e { NodeExtra::Frame(c) => { widths.push((c.w+2).max(string_width(&fit_label(&graph.nodes[i].label, WRAP_WIDTH))+4)); heights.push(c.h+2); }, NodeExtra::Compartments(s) => { let w=s.iter().flatten().map(|x|string_width(x)).max().unwrap_or(1)+2*PAD+2; widths.push(w); heights.push(s.iter().map(Vec::len).sum::<usize>() + s.iter().filter(|x|!x.is_empty()).count().saturating_sub(1)+2); }, NodeExtra::Plain => { widths.push(wrapped[i].iter().map(|x|string_width(x)).max().unwrap_or(1)+2*PAD+2); heights.push(wrapped[i].len()+2); } } }
    let mut extra_h=vec![0;graph.nodes.len()]; let mut label_w=vec![0;graph.nodes.len()]; for e in &graph.edges { if e.from==e.to { extra_h[e.from]=2; if let Some(l)=&e.label { label_w[e.from]=label_w[e.from].max(string_width(l).min(MAX_LABEL)); } } } for i in 0..widths.len() { if extra_h[i]>0 { widths[i]=widths[i].max(7); } }
    let sizes=NodeSizes{lay_w:widths.iter().enumerate().map(|(i,w)|w+if label_w[i]>0{2*(label_w[i]+3)}else{0}).collect(),lay_h:heights.iter().enumerate().map(|(i,h)|h+extra_h[i]).collect(),box_w:widths,box_h:heights,extra_h,self_label_w:label_w}; let centers=assign_positions(&by,&sizes.lay_w,GAP_X,&graph.edges,&ranks); let mut rank_h=by.iter().map(|r|r.iter().map(|&i|sizes.box_h[i]+sizes.extra_h[i]).max().unwrap_or(3)).collect::<Vec<_>>(); if rank_h.is_empty(){rank_h.push(1)} let mut rank_y=vec![0;max_rank+1]; for r in 1..=max_rank { rank_y[r]=rank_y[r-1]+rank_h[r-1]+GAP_Y; } let mut w=1; for (r,row) in by.iter().enumerate(){for &i in row{let x=sat(centers[i],half(sizes.box_w[i]));let y=rank_y[r]+half(rank_h[r].saturating_sub(sizes.box_h[i]+sizes.extra_h[i]));placed[i]=Placed{x,y,w:sizes.box_w[i],h:sizes.box_h[i],cx:centers[i],cy:y+half(sizes.box_h[i]),rank:r};w=w.max(x+sizes.box_w[i]);}} let h=rank_y[max_rank]+rank_h[max_rank]; if w*h>MAX_CANVAS_CELLS{return None;} let mut c=Canvas::new(w.max(1),h.max(1)); for (i,e) in extras.iter().enumerate(){match e{NodeExtra::Plain=>draw_box(&mut c,placed[i],&wrapped[i],graph.nodes[i].shape),NodeExtra::Compartments(s)=>draw_class_box(&mut c,placed[i],s),NodeExtra::Frame(sub)=>draw_frame(&mut c,placed[i],&graph.nodes[i].label,sub)}} c.finalize_mask(); Some(c)
}

pub fn layout_flowchart(graph: &Graph) -> CanvasResult { let e=vec![NodeExtra::Plain;graph.nodes.len()]; layout_canvas(graph,&e).map(|c|orient(c,graph)) }
pub fn layout_class(graph: &Graph, infos: &[ClassInfo]) -> CanvasResult { let e=graph.nodes.iter().enumerate().map(|(i,n)|NodeExtra::Compartments(vec![{let mut t=Vec::new();if let Some(a)=&infos[i].annotation{t.push(format!("«{a}»"));}t.push(display_generics(&n.label));t},infos[i].attrs.clone(),infos[i].methods.clone()])).collect::<Vec<_>>(); layout_canvas(graph,&e).map(|c|orient(c,graph)) }
pub fn layout_grouped(graph: &Graph) -> CanvasResult { layout_flowchart(graph) }
fn display_generics(s:&str)->String{s.chars().fold((String::new(),false),|(mut o,open),c|{if c=='~'{o.push(if open{'>'}else{'<'});(o,!open)}else{o.push(c);(o,open)}}).0}

pub fn draw_box(canvas:&mut Canvas,p:Placed,lines:&[String],shape:Shape){let right=p.x+p.w-1;let bottom=p.y+p.h-1;let rounded=matches!(shape,Shape::Round|Shape::Diamond);canvas.set(p.x,p.y,if rounded{"╭"}else{"┌"},super::Cls::Border);canvas.set(right,p.y,if rounded{"╮"}else{"┐"},super::Cls::Border);canvas.set(p.x,bottom,if rounded{"╰"}else{"└"},super::Cls::Border);canvas.set(right,bottom,if rounded{"╯"}else{"┘"},super::Cls::Border);for x in p.x+1..right{canvas.add_bits(x,p.y,L|R,super::Cls::Border);canvas.add_bits(x,bottom,L|R,super::Cls::Border)}for y in p.y+1..bottom{canvas.add_bits(p.x,y,U|D,super::Cls::Border);canvas.add_bits(right,y,U|D,super::Cls::Border)}for y in p.y..=bottom{for x in p.x..=right{let idx=canvas.idx(x,y);canvas.occupied[idx]=1}}let inner=sat(p.w,2*PAD+2).max(1);for (i,l) in lines.iter().enumerate(){let t=fit_label(l,inner);draw_text(canvas,&t,p.x+1+PAD+half(sat(inner,string_width(&t))),p.y+1+i,super::Cls::Text)}}
fn draw_class_box(c:&mut Canvas,p:Placed,s:&[Vec<String>]){draw_box(c,p,&[],Shape::Rect);let inner=sat(p.w,2*PAD+2).max(1);let mut row=p.y+1;let mut first=true;for (si,sec) in s.iter().enumerate(){if sec.is_empty(){continue}if !first{c.set(p.x,row,"├",super::Cls::Border);for x in p.x+1..p.x+p.w-1{c.set(x,row,"─",super::Cls::Border)}c.set(p.x+p.w-1,row,"┤",super::Cls::Border);row+=1}first=false;for l in sec{let t=fit_label(l,inner);draw_text_over_edges(c,&t,p.x+1+PAD+if si==0{half(sat(inner,string_width(&t)))}else{0},row,super::Cls::Text);row+=1}}}
fn draw_frame(c:&mut Canvas,p:Placed,title:&str,sub:&Canvas){draw_box(c,p,&[],Shape::Rect);let t=fit_label(title,sat(p.w,4));draw_text_over_edges(c,&format!(" {t} "),p.x+1,p.y,super::Cls::Text);c.blit(sub,p.x+1+half(p.w.saturating_sub(2+sub.w)),p.y+1+half(p.h.saturating_sub(2+sub.h)))}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(from: usize, to: usize) -> Edge {
        Edge { from, to, label: None, head_to: super::super::graph::Head::None, head_from: super::super::graph::Head::None, line: super::super::graph::LineKind::Solid }
    }

    #[test]
    fn compute_ranks_ignores_cycles() {
        let mut g = Graph::default();
        g.nodes = (0..3).map(|i| super::super::graph::Node { label: i.to_string(), shape: Shape::Rect }).collect();
        g.edges = vec![edge(0, 1), edge(1, 2), edge(2, 0)];
        assert_eq!(compute_ranks(&g), vec![0, 1, 2]);
    }

    #[test]
    fn dfs_dag_records_postorder() {
        let mut color = vec![0; 3]; let mut dag = vec![Vec::new(); 3]; let mut order = Vec::new();
        dfs_dag(0, &[vec![1], vec![2], vec![0]], &mut color, &mut dag, &mut order);
        assert_eq!(order, vec![2, 1, 0]);
        assert_eq!(dag, vec![vec![1], vec![2], Vec::<usize>::new()]);
    }

    #[test]
    fn order_ranks_reduces_crossing() {
        let mut rows = vec![vec![0, 1], vec![3, 2]];
        order_ranks(&mut rows, &[edge(0, 2), edge(1, 3)], &[0, 0, 1, 1]);
        assert_eq!(rows, vec![vec![0, 1], vec![2, 3]]);
    }

    #[test]
    fn sort_by_barycenter_orders_neighbors() {
        let mut row = vec![1, 0];
        sort_by_barycenter(&mut row, &[vec![], vec![0]], &[0, 1]);
        assert_eq!(row, vec![0, 1]);
    }

    #[test]
    fn count_crossings_counts_inversions() {
        assert_eq!(count_crossings(&[edge(0, 3), edge(1, 2)], &[0, 0, 1, 1], &[0, 1, 1, 0]), 1);
    }

    #[test]
    fn track_packing() { let (a, n) = assign_tracks(&[[0, 3, 0, 1, 4], [0, 3, 0, 2, 5], [5, 6, 1, 2, 6]]); assert_eq!(n, 2); assert_eq!(a.len(), 3); }
}
