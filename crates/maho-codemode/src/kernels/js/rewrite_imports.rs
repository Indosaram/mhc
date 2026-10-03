use tree_sitter::Node;

pub const DYNAMIC_IMPORT_CALLEE: &str = "(typeof __senpi_import__ === \"function\" ? __senpi_import__ : (specifier, options) => import(specifier, options))";

fn text<'a>(node: Node<'_>, code: &'a str) -> &'a str { &code[node.byte_range()] }

fn string_value(node: Node<'_>, code: &str) -> Option<String> {
    let raw = text(node, code);
    let quote = raw.chars().next()?;
    let value = raw.strip_prefix(quote)?.strip_suffix(quote)?;
    let mut chars = value.chars().peekable();
    let mut units = Vec::new();
    while let Some(character) = chars.next() {
        let decoded = if character != '\\' { character } else {
            match chars.next()? {
                '\n' | '\u{2028}' | '\u{2029}' => continue,
                '\r' => { if chars.peek() == Some(&'\n') { chars.next(); } continue; }
                'b' => '\u{8}',
                'f' => '\u{c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'v' => '\u{b}',
                '0' => '\0',
                escape @ ('x' | 'u') => {
                    let mut codepoint = 0_u32;
                    if escape == 'u' && chars.peek() == Some(&'{') {
                        chars.next();
                        let mut digits = 0;
                        loop {
                            let digit = chars.next()?;
                            if digit == '}' { break; }
                            codepoint = codepoint.checked_mul(16_u32)?.checked_add(digit.to_digit(16)?)?;
                            digits += 1;
                        }
                        if digits == 0 { return None; }
                    } else {
                        for _ in 0..if escape == 'x' { 2 } else { 4 } {
                            codepoint = codepoint * 16 + chars.next()?.to_digit(16)?;
                        }
                    }
                    if codepoint <= 0xffff {
                        units.push(u16::try_from(codepoint).ok()?);
                        continue;
                    }
                    char::from_u32(codepoint)?
                }
                other => other,
            }
        };
        units.extend_from_slice(decoded.encode_utf16(&mut [0; 2]));
    }
    String::from_utf16(&units).ok()
}

fn import_declaration(node: Node<'_>, code: &str) -> Option<String> {
    let source = node.child_by_field_name("source")?;
    let source = serde_json::to_string(&string_value(source, code)?).ok()?;
    let mut default_name = None;
    let mut namespace = None;
    let mut named = Vec::new();
    let mut attributes = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        match current.kind() {
            "import_clause" => {
                let mut cursor = current.walk();
                for child in current.named_children(&mut cursor) {
                    if child.kind() == "identifier" { default_name = Some(text(child, code).to_owned()); }
                }
            }
            "namespace_import" => {
                let mut cursor = current.walk();
                namespace = current.named_children(&mut cursor).find(|child| child.kind() == "identifier").map(|child| text(child, code).to_owned());
            }
            "import_specifier" => {
                let imported = current.child_by_field_name("name")?;
                let imported = if imported.kind() == "string" { string_value(imported, code)? } else { text(imported, code).to_owned() };
                let local = current.child_by_field_name("alias").map(|alias| text(alias, code).to_owned()).unwrap_or_else(|| imported.clone());
                named.push(if imported == local { imported } else { format!("{imported}: {local}") });
            }
            "pair" => {
                let key = current.child_by_field_name("key")?;
                let value = current.child_by_field_name("value")?;
                let key = if key.kind() == "string" { serde_json::to_string(&string_value(key, code)?).ok()? } else { text(key, code).to_owned() };
                attributes.push(format!("{key}: {}", serde_json::to_string(&string_value(value, code)?).ok()?));
            }
            _ => {}
        }
        let mut cursor = current.walk();
        let children: Vec<_> = current.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    let call = if attributes.is_empty() { format!("__senpi_import__({source})") } else { format!("__senpi_import__({source}, {{ with: {{ {} }} }})", attributes.join(", ")) };
    if !named.is_empty() {
        if let Some(default) = default_name { named.insert(0, format!("default: {default}")); }
        Some(format!("const {{ {} }} = await {call};", named.join(", ")))
    } else if let Some(namespace) = namespace {
        Some(if let Some(default) = default_name { format!("const {namespace} = await {call}; const {default} = {namespace}.default;") } else { format!("const {namespace} = await {call};") })
    } else if let Some(default) = default_name { Some(format!("const {default} = (await {call}).default;")) }
    else { Some(format!("await {call};")) }
}

pub fn rewrite_imports(code: &str) -> String {
    if !code.contains("import") { return code.into(); }
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()).expect("TypeScript grammar");
    let Some(tree) = parser.parse(code, None) else { return code.into(); };
    if tree.root_node().has_error() { return code.into(); }
    let mut edits = Vec::new();
    let mut cursor = tree.root_node().walk();
    for node in tree.root_node().named_children(&mut cursor) {
        if node.kind() == "import_statement" && let Some(replacement) = import_declaration(node, code) {
            edits.push((node.start_byte(), node.end_byte(), replacement));
        }
    }
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression" && let Some(function) = node.child_by_field_name("function") && function.kind() == "import" {
            edits.push((function.start_byte(), function.end_byte(), DYNAMIC_IMPORT_CALLEE.into()));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.0));
    let mut output = code.to_owned();
    for (start, end, replacement) in edits { output.replace_range(start..end, &replacement); }
    output
}
