use maho_ext_tool_search::engine::{bm25::*,document::*};
#[test] fn pinned_locale_compare_orders_case_accents_and_equivalent_marks() {
    let documents=["Z","é","É","a","é","E","A","e"].map(|name|ToolSearchDocument{name:name.into(),label:"same".into(),aliases:vec![],keywords:vec![],description:Some("routing".into()),search_text:None,source:ToolSearchSource::Extension,group:"catalog".into(),owner_label:"Catalog".into(),registration_id:name.into()});
    let results=build_bm25_index(&documents).search("same",25,&Bm25SearchOptions{exact_match:Some(false),..Default::default()});
    assert_eq!(results[0].score,results[1].score);
    assert!(results[2..].windows(2).all(|pair|pair[0].score==pair[1].score));
    assert_eq!(results.iter().map(|r|r.name.as_str()).collect::<Vec<_>>(),["é","É","a","A","e","E","é","Z"]);
}
