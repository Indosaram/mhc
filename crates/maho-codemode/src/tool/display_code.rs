use tree_sitter::Node;
use super::types::EvalLanguage;

struct Gap {start:usize,end:usize,content_depth:usize,next_depth:usize}
fn children(node:Node<'_>) -> Vec<Node<'_>> {
    let mut cursor=node.walk();
    node.named_children(&mut cursor).filter(|child|child.kind()!="comment").collect()
}
fn sibling_gaps(nodes:&[Node<'_>],depth:usize,gaps:&mut Vec<Gap>) {
    for pair in nodes.windows(2) {gaps.push(Gap {start:pair[0].end_byte(),end:pair[1].start_byte(),content_depth:depth,next_depth:depth});}
}
fn collect_gaps(node:Node<'_>,depth:usize,code:&str,gaps:&mut Vec<Gap>) {
    let nodes=children(node);
    let breaks=match node.kind() {
        "statement_block"=>!nodes.is_empty(),
        "array"=>nodes.len()>=2 && code[node.byte_range()].encode_utf16().count()>60 && !code[node.byte_range()].contains('\n'),
        _=>false,
    };
    if breaks {
        gaps.push(Gap {start:node.start_byte()+1,end:nodes[0].start_byte(),content_depth:depth+1,next_depth:depth+1});
        sibling_gaps(&nodes,depth+1,gaps);
        gaps.push(Gap {start:nodes[nodes.len()-1].end_byte(),end:node.end_byte()-1,content_depth:depth+1,next_depth:depth});
    }
    for child in nodes {collect_gaps(child,if breaks {depth+1} else {depth},code,gaps);}
}

pub fn display_code(code:&str,language:EvalLanguage) -> String {
    if language!=EvalLanguage::Js || !code.split('\n').any(|line|line.encode_utf16().count()>100) {return code.into();}
    let mut parser=tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()).expect("TypeScript grammar");
    let Some(tree)=parser.parse(code,None) else {return code.into();};
    if tree.root_node().has_error() {return code.into();}
    let mut gaps=Vec::new();
    let nodes=children(tree.root_node());
    sibling_gaps(&nodes,0,&mut gaps);
    for node in nodes {collect_gaps(node,0,code,&mut gaps);}
    gaps.sort_by_key(|gap|gap.start);
    let mut output=String::new();
    let mut cursor=0;
    for gap in gaps {
        output.push_str(&code[cursor..gap.start]);
        let mut rest=code[gap.start..gap.end].trim();
        if rest.starts_with([',',';']) {output.push_str(&rest[..1]);rest=rest[1..].trim();}
        for line in rest.split('\n').map(str::trim).filter(|line|!line.is_empty()) {
            output.push('\n');output.push_str(&"  ".repeat(gap.content_depth));output.push_str(line);
        }
        output.push('\n');output.push_str(&"  ".repeat(gap.next_depth));
        cursor=gap.end;
    }
    output.push_str(&code[cursor..]);
    output.trim().into()
}
