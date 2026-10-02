//! Weighted BM25 mirror of engine/bm25.ts.
use std::collections::{BTreeMap, BTreeSet};
use super::document::{ToolSearchDocument, ToolSearchSource};

#[derive(Clone, Debug, PartialEq)]
pub struct Bm25Result { pub name: String, pub score: f64, pub exact: bool, pub coverage: f64, pub doc: ToolSearchDocument }
#[derive(Clone, Copy, Debug)]
pub struct Bm25Precision { pub min_coverage: f64, pub min_score_ratio: f64 }
pub const DEFAULT_BM25_PRECISION: Bm25Precision = Bm25Precision { min_coverage: 0.5, min_score_ratio: 0.35 };
#[derive(Clone, Debug, Default)]
pub struct Bm25SearchOptions { pub source: Option<ToolSearchSource>, pub group: Option<String>, pub exact_match: Option<bool>, pub precision: Option<Bm25Precision> }
struct IndexedDoc { doc: ToolSearchDocument, term_freq: BTreeMap<String, f64>, length: f64, exact_names: BTreeSet<String> }
pub struct Bm25Index { indexed: Vec<IndexedDoc>, doc_freq: BTreeMap<String, f64>, avg_length: f64, doc_count: f64 }

pub fn build_bm25_index(docs: &[ToolSearchDocument]) -> Bm25Index {
    let indexed: Vec<_> = docs.iter().map(|doc| {
        let mut term_freq = BTreeMap::new();
        let mut add = |text: &str, weight: f64| {
            for token in tokenize_tool_text(text) { *term_freq.entry(stem_token(&token)).or_insert(0.0) += weight; }
        };
        add(&doc.name, 3.0); add(&doc.label, 3.0);
        for name in doc.aliases.iter().chain(&doc.keywords) { add(name, 3.0); }
        add(&doc.group, 2.0); add(&doc.owner_label, 2.0);
        add(doc.description.as_deref().unwrap_or(""), 1.0);
        add(doc.search_text.as_deref().unwrap_or(""), 1.0);
        let length = term_freq.values().sum();
        let exact_names = std::iter::once(&doc.name).chain(std::iter::once(&doc.label)).chain(&doc.aliases).chain(&doc.keywords).map(|s| normalize_tool_name(s)).collect();
        IndexedDoc { doc: doc.clone(), term_freq, length, exact_names }
    }).collect();
    let mut doc_freq = BTreeMap::new();
    let mut doc_count = 0.0;
    let mut total_length = 0.0;
    for entry in &indexed {
        doc_count += 1.0; total_length += entry.length;
        for term in entry.term_freq.keys() { *doc_freq.entry(term.clone()).or_insert(0.0) += 1.0; }
    }
    let avg_length = if indexed.is_empty() { 0.0 } else { total_length / doc_count };
    Bm25Index { indexed, doc_freq, avg_length, doc_count }
}

impl Bm25Index {
    pub fn search(&self, query: &str, limit: usize, options: &Bm25SearchOptions) -> Vec<Bm25Result> {
        let mut query_terms=Vec::new();
        for token in tokenize_tool_text(query) { let term=stem_token(&token); if !query_terms.contains(&term) { query_terms.push(term); } }
        if query_terms.is_empty() { return Vec::new(); }
        let stopwords = "a an and are be by can do for from how i in is it me my need of on or please that the this to tool use using via want we with you your";
        let stopwords: BTreeSet<_> = stopwords.split_whitespace().collect();
        let mut content: BTreeSet<_> = query_terms.iter().filter(|t| !stopwords.contains(t.as_str())).collect();
        if content.is_empty() { content = query_terms.iter().collect(); }
        let normalized = normalize_tool_name(query);
        let mut results = Vec::new();
        for entry in &self.indexed {
            if options.source.is_some_and(|s| entry.doc.source != s) || options.group.as_ref().is_some_and(|g| &entry.doc.group != g) { continue; }
            let exact = options.exact_match != Some(false) && !normalized.is_empty() && entry.exact_names.contains(&normalized);
            let mut score = 0.0;
            for term in &query_terms {
                if let Some(tf) = entry.term_freq.get(term) {
                    let df = self.doc_freq.get(term).copied().unwrap_or(0.0);
                    let idf = (1.0 + (self.doc_count - df + 0.5) / (df + 0.5)).ln().max(0.0);
                    let avg = if self.avg_length > 0.0 { self.avg_length } else { 1.0 };
                    let denom = tf + 0.9 * (1.0 - 0.4 + (0.4 * entry.length) / avg);
                    score += idf * (tf * 1.9 / denom);
                }
            }
            if !exact && score <= 0.0 { continue; }
            let mut matched = 0.0;
            let mut count = 0.0;
            for term in &content { count += 1.0; if entry.term_freq.contains_key(*term) { matched += 1.0; } }
            results.push(Bm25Result { name: entry.doc.name.clone(), score, exact, coverage: matched / count, doc: entry.doc.clone() });
        }
        results.sort_by(|a,b| b.exact.cmp(&a.exact).then_with(|| b.score.total_cmp(&a.score)).then_with(|| a.name.cmp(&b.name)));
        if let Some(precision) = options.precision {
            let floor = results.iter().find(|r| !r.exact).map_or(0.0, |r| r.score) * precision.min_score_ratio;
            results.retain(|r| r.exact || (r.coverage >= precision.min_coverage && r.score >= floor));
        }
        results.truncate(limit); results
    }
}

pub fn stem_token(token: &str) -> String {
    if token.len() < 4 || !token.ends_with('s') || token.ends_with("ss") || token.ends_with("us") { return token.into(); }
    if let Some(prefix) = token.strip_suffix("ies") { format!("{prefix}y") } else { token[..token.len()-1].into() }
}
pub fn tokenize_tool_text(text: &str) -> Vec<String> {
    let chars: Vec<_> = text.chars().collect();
    let mut separated = String::new();
    for (i, c) in chars.iter().copied().enumerate() {
        if i > 0 && c.is_ascii_uppercase() && ((chars[i-1].is_ascii_lowercase() || chars[i-1].is_ascii_digit()) || (chars[i-1].is_ascii_uppercase() && chars.get(i+1).is_some_and(char::is_ascii_lowercase))) { separated.push(' '); }
        separated.push(c);
    }
    separated.split(|c: char| !c.is_ascii_alphanumeric()).filter(|s| !s.is_empty()).map(str::to_ascii_lowercase).collect()
}
pub fn normalize_tool_name(name: &str) -> String { name.to_lowercase().chars().filter(|c| !matches!(*c,'-'|'_'|'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).collect() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn upstream_generalized_fields_are_searchable() {
        let mut group=doc("group_hit");group.group="payments".into();
        let mut owner=doc("owner_hit");owner.owner_label="Accounting".into();
        let mut description=doc("description_hit");description.description=Some("invoices".into());
        let mut supplemental=doc("search_text_hit");supplemental.search_text=Some("reconciliation".into());
        let index=build_bm25_index(&[group,owner,description,supplemental]);
        for (query,name) in [("payments","group_hit"),("accounting","owner_hit"),("invoices","description_hit"),("reconciliation","search_text_hit")] {
            let results=index.search(query,25,&Default::default());assert_eq!(results[0].name,name);assert!(!results[0].exact);
        }
    }
    fn doc(name: &str) -> ToolSearchDocument { ToolSearchDocument { name: name.into(), label: name.into(), aliases: vec![], description: None, search_text: None, keywords: vec![], source: ToolSearchSource::Extension, group: "utilities".into(), owner_label: "Utilities".into(), registration_id: format!("registration:{name}") } }
    #[test] fn scoring_preserves_source_query_term_accumulation_order() { let mut document=doc("rank"); document.label="Rank".into(); document.description=Some("zeta alpha beta gamma delta delta delta".into()); let result=build_bm25_index(&[document]).search("zeta alpha delta beta gamma",25,&Bm25SearchOptions{exact_match:Some(false),..Default::default()}); assert_eq!(result[0].score,1.5711867033904954); }
    #[test] fn tokenizer() { assert_eq!(tokenize_tool_text("HTTPServerV2 resolveLibraryId get-library-docs"), ["http","server","v2","resolve","library","id","get","library","docs"]); }
    #[test] fn normalized() { assert_eq!(normalize_tool_name("Get-Library_Docs"), "getlibrarydocs"); }
    #[test] fn normalization_uses_ecmascript_whitespace() { assert_eq!(normalize_tool_name("Get\u{feff}Library\u{0085}Docs"),"getlibrary\u{0085}docs"); }
    #[test] fn generalized_exact_names() { let mut d=doc("lookup"); d.label="Find Package".into(); d.aliases.push("resolve-library-id".into()); d.keywords.push("dependency catalog".into()); let index=build_bm25_index(&[d]); for q in ["find_package","RESOLVE LIBRARY ID","dependency-catalog"] { assert!(index.search(q,25,&Default::default())[0].exact); } }
    #[test] fn keyword_weights() { let mut a=doc("keyword"); a.keywords.push("ledger".into()); let mut b=doc("description"); b.description=Some("ledger".into()); let r=build_bm25_index(&[a,b]).search("ledger",25,&Bm25SearchOptions { exact_match:Some(false), ..Default::default() }); assert_eq!(r[0].name,"keyword"); assert!(r[0].score>=r[1].score); }
    #[test] fn optional_fields() { let i=build_bm25_index(&[doc("bare_tool")]); for q in [""," ","--- !!!","the and or"] { assert!(i.search(q,25,&Default::default()).is_empty()); } assert!(build_bm25_index(&[]).search("anything",25,&Default::default()).is_empty()); }
    #[test] fn source_is_not_indexed() { let i=build_bm25_index(&[doc("alpha")]); for q in ["extension","mcp"] { assert!(i.search(q,25,&Default::default()).is_empty()); } }
    #[test] fn source_filter() { let mut a=doc("search_a"); a.source=ToolSearchSource::Mcp; let b=doc("search_b"); let r=build_bm25_index(&[a,b]).search("search",25,&Bm25SearchOptions { source:Some(ToolSearchSource::Extension), ..Default::default() }); assert_eq!(r.len(),1); assert_eq!(r[0].name,"search_b"); }
    #[test] fn group_filter() { let mut a=doc("search_a"); a.group="docs".into(); let b=doc("search_b"); let r=build_bm25_index(&[a,b]).search("search",25,&Bm25SearchOptions { group:Some("docs".into()), ..Default::default() }); assert_eq!(r.len(),1); assert_eq!(r[0].name,"search_a"); }
    #[test] fn stable_ties() { let i=build_bm25_index(&[doc("b_tool"),doc("a_tool")]); let a=i.search("tool",25,&Default::default()); let b=i.search("tool",25,&Default::default()); assert_eq!(a,b); assert_eq!(a[0].name,"a_tool"); }
    #[test] fn plural_stems() { assert_eq!(stem_token("libraries"),"library"); assert_eq!(stem_token("messages"),"message"); assert_eq!(stem_token("status"),"status"); assert_eq!(stem_token("class"),"class"); }
    #[test] fn precision_drops_incidental_terms() { let mut a=doc("full"); a.description=Some("search weather forecast city".into()); let mut b=doc("partial"); b.description=Some("search".into()); let r=build_bm25_index(&[a,b]).search("search weather forecast city",25,&Bm25SearchOptions { precision:Some(DEFAULT_BM25_PRECISION), ..Default::default() }); assert_eq!(r.len(),1); }
    #[test] fn zero_limit() { assert!(build_bm25_index(&[doc("search")]).search("search",0,&Default::default()).is_empty()); }
    #[test] fn upstream_group_and_owner_weights_exceed_description() {
        let mut documents=Vec::new();
        for (name,field) in [("description_candidate",0),("group_candidate",1),("owner_candidate",2)] {
            let mut document=doc(name);document.label="plain".into();document.group="plain".into();document.owner_label="plain".into();document.description=Some("plain".into());
            match field {0=>document.description=Some("routing".into()),1=>document.group="routing".into(),_=>document.owner_label="routing".into()};documents.push(document);
        }
        let results=build_bm25_index(&documents).search("routing",10,&Bm25SearchOptions{exact_match:Some(false),..Default::default()});
        assert_eq!(results.iter().map(|result|result.name.as_str()).collect::<Vec<_>>(),["group_candidate","owner_candidate","description_candidate"]);assert_eq!(results[0].score,results[1].score);assert!(results[1].score>results[2].score);
    }
    #[test] fn upstream_relative_score_floor_is_independent_of_coverage() {
        let mut full=doc("full_match");full.description=Some("alpha beta gamma delta".into());
        let mut partial=doc("partial_match");partial.description=Some("alpha and a great many other unrelated words fill this description out".into());let index=build_bm25_index(&[full,partial]);
        let options=|ratio|Bm25SearchOptions{precision:Some(Bm25Precision{min_coverage:0.0,min_score_ratio:ratio}),..Default::default()};
        let results=index.search("alpha beta",10,&options(0.0));assert_eq!(results.iter().map(|result|result.name.as_str()).collect::<Vec<_>>(),["full_match","partial_match"]);assert_eq!(results[0].coverage,1.0);assert_eq!(results[1].coverage,0.5);
        let results=index.search("alpha beta",10,&options(0.9));assert_eq!(results.len(),1);assert_eq!(results[0].name,"full_match");
    }
    #[test] fn upstream_precision_ignores_stopwords_and_folds_plurals() {
        let mut task=doc("task_get");task.description=Some("Reads one entry of the shared team task list".into());task.keywords=vec!["task details".into(),"read a task".into()];
        let mut weather=doc("weather_forecast");weather.description=Some("Get hourly weather forecasts and rain predictions".into());
        let index=build_bm25_index(&[task,weather]);let options=Bm25SearchOptions{precision:Some(DEFAULT_BM25_PRECISION),..Default::default()};
        for (query,name) in [("a tool to read the task","task_get"),("weather forecasts","weather_forecast"),("hourly rain forecast","weather_forecast")] {
            let results=index.search(query,10,&options);assert_eq!(results.len(),1);assert_eq!(results[0].name,name);assert_eq!(results[0].coverage,1.0);
        }
    }
    #[test] fn exact_names_bypass_precision_thresholds() {
        let index=build_bm25_index(&[doc("thread_handoff")]);let options=Bm25SearchOptions{precision:Some(Bm25Precision{min_coverage:2.0,min_score_ratio:2.0}),..Default::default()};
        let results=index.search("thread-handoff",10,&options);assert_eq!(results.len(),1);assert_eq!(results[0].name,"thread_handoff");assert!(results[0].exact);
        assert!(index.search("thread handoff",10,&Bm25SearchOptions{exact_match:Some(false),..options}).is_empty());
    }
    #[test] fn upstream_mixed_catalog_precision_rejects_incidental_hits() {
        let definitions=[("x_search","Searches X (Twitter) posts through xAI. Date-bound every time-sensitive query.",vec!["X posts","tweets","twitter search"]),("thread_handoff","Moves the current request to an old session so the previous conversation continues there instead of here",vec!["continue in old session","hand off to a previous session"]),("task_get","Reads one entry of the shared team task list",vec!["task details","read a task"]),("weather_forecast","Get hourly weather forecasts and rain predictions",vec![])];
        let documents:Vec<_>=definitions.into_iter().map(|(name,description,keywords)|{let mut document=doc(name);document.group="catalog".into();document.owner_label="catalog".into();document.description=Some(description.into());document.keywords=keywords.into_iter().map(String::from).collect();document}).collect();
        let index=build_bm25_index(&documents);let query="recall memory search previous conversation messages";assert!(index.search(query,10,&Default::default()).iter().any(|result|result.name=="x_search"));let precise=Bm25SearchOptions{precision:Some(DEFAULT_BM25_PRECISION),..Default::default()};assert!(index.search(query,10,&precise).is_empty());
        let results=index.search("twitter search for tweets",10,&precise);assert_eq!(results.len(),1);assert_eq!(results[0].name,"x_search");
    }
    #[test] fn upstream_source_and_group_filters_intersect() {
        let mut mcp=doc("mcp_docs_search");mcp.source=ToolSearchSource::Mcp;mcp.group="docs".into();
        let mut extension=doc("extension_docs_search");extension.group="docs".into();let mut files=doc("extension_files_search");files.group="files".into();
        let index=build_bm25_index(&[mcp,extension,files]);let results=index.search("search",10,&Bm25SearchOptions{source:Some(ToolSearchSource::Extension),group:Some("docs".into()),..Default::default()});
        assert_eq!(results.len(),1);assert_eq!(results[0].name,"extension_docs_search");assert_eq!(results[0].doc.source,ToolSearchSource::Extension);assert_eq!(results[0].doc.group,"docs");
    }
}
