//! Regex sources for the marked v18 grammar used by `components/markdown.ts`.
//!
//! Generated from the pinned marked v18.0.13 `src/rules.ts` (gfm, non-pedantic). Character
//! classes are rewritten from JavaScript semantics to their exact Rust equivalents (`\s`,
//! `\w`, `\d`, `\b`), because Rust's Unicode-aware shorthands are wider than JavaScript's.

/// marked `other.codeRemoveIndent` (flags `gm`).
pub const OTHER_CODEREMOVEINDENT: &str = r#"^(?: {0,3}\t| {1,4})"#;

/// marked `other.outputLinkReplace` (flags `g`).
pub const OTHER_OUTPUTLINKREPLACE: &str = r#"\\([\[\]])"#;

/// marked `other.indentCodeCompensation` (flags ``).
pub const OTHER_INDENTCODECOMPENSATION: &str = r#"^([\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+)(?:```)"#;

/// marked `other.beginningSpace` (flags ``).
pub const OTHER_BEGINNINGSPACE: &str = r#"^[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+"#;

/// marked `other.endingHash` (flags ``).
pub const OTHER_ENDINGHASH: &str = r#"#$"#;

/// marked `other.startingSpaceChar` (flags ``).
pub const OTHER_STARTINGSPACECHAR: &str = r#"^ "#;

/// marked `other.endingSpaceChar` (flags ``).
pub const OTHER_ENDINGSPACECHAR: &str = r#" $"#;

/// marked `other.endingSpaceTabChar` (flags ``).
pub const OTHER_ENDINGSPACETABCHAR: &str = r#"[ \t]$"#;

/// marked `other.nonSpaceChar` (flags ``).
pub const OTHER_NONSPACECHAR: &str = r#"[^ ]"#;

/// marked `other.newLineCharGlobal` (flags `g`).
pub const OTHER_NEWLINECHARGLOBAL: &str = r#"\n"#;

/// marked `other.tabCharGlobal` (flags `g`).
pub const OTHER_TABCHARGLOBAL: &str = r#"\t"#;

/// marked `other.multipleSpaceGlobal` (flags `g`).
pub const OTHER_MULTIPLESPACEGLOBAL: &str = r#"[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+"#;

/// marked `other.blankLine` (flags ``).
pub const OTHER_BLANKLINE: &str = r#"^[ \t]*$"#;

/// marked `other.doubleBlankLine` (flags ``).
pub const OTHER_DOUBLEBLANKLINE: &str = r#"\n[ \t]*\n[ \t]*$"#;

/// marked `other.blockquoteStart` (flags ``).
pub const OTHER_BLOCKQUOTESTART: &str = r#"^ {0,3}>"#;

/// marked `other.blockquoteSetextReplace` (flags `g`).
pub const OTHER_BLOCKQUOTESETEXTREPLACE: &str = r#"\n {0,3}((?:=+|-+) *)(?=\n|$)"#;

/// marked `other.blockquoteSetextReplace2` (flags `gm`).
pub const OTHER_BLOCKQUOTESETEXTREPLACE2: &str = r#"^ {0,3}>[ \t]?"#;

/// marked `other.listReplaceNesting` (flags `g`).
pub const OTHER_LISTREPLACENESTING: &str = r#"^ {1,4}(?=( {4})*[^ ])"#;

/// marked `other.listIsTask` (flags ``).
pub const OTHER_LISTISTASK: &str = r#"^\[[ xX]\] +[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]"#;

/// marked `other.listReplaceTask` (flags ``).
pub const OTHER_LISTREPLACETASK: &str = r#"^\[[ xX]\] +"#;

/// marked `other.listTaskCheckbox` (flags ``).
pub const OTHER_LISTTASKCHECKBOX: &str = r#"\[[ xX]\]"#;

/// marked `other.anyLine` (flags ``).
pub const OTHER_ANYLINE: &str = r#"\n.*\n"#;

/// marked `other.hrefBrackets` (flags ``).
pub const OTHER_HREFBRACKETS: &str = r#"^<(.*)>$"#;

/// marked `other.tableDelimiter` (flags ``).
pub const OTHER_TABLEDELIMITER: &str = r#"[:|]"#;

/// marked `other.tableAlignChars` (flags `g`).
pub const OTHER_TABLEALIGNCHARS: &str = r#"^\||\| *$"#;

/// marked `other.tableRowBlankLine` (flags ``).
pub const OTHER_TABLEROWBLANKLINE: &str = r#"\n[ \t]*$"#;

/// marked `other.tableAlignRight` (flags ``).
pub const OTHER_TABLEALIGNRIGHT: &str = r#"^ *-+: *$"#;

/// marked `other.tableAlignCenter` (flags ``).
pub const OTHER_TABLEALIGNCENTER: &str = r#"^ *:-+: *$"#;

/// marked `other.tableAlignLeft` (flags ``).
pub const OTHER_TABLEALIGNLEFT: &str = r#"^ *:-+ *$"#;

/// marked `other.startATag` (flags `i`).
pub const OTHER_STARTATAG: &str = r#"^<a "#;

/// marked `other.endATag` (flags `i`).
pub const OTHER_ENDATAG: &str = r#"^<\/a>"#;

/// marked `other.startPreScriptTag` (flags `i`).
pub const OTHER_STARTPRESCRIPTTAG: &str = r#"^<(pre|code|kbd|script)([\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|>)"#;

/// marked `other.endPreScriptTag` (flags `i`).
pub const OTHER_ENDPRESCRIPTTAG: &str = r#"^<\/(pre|code|kbd|script)([\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|>)"#;

/// marked `other.startAngleBracket` (flags ``).
pub const OTHER_STARTANGLEBRACKET: &str = r#"^<"#;

/// marked `other.endAngleBracket` (flags ``).
pub const OTHER_ENDANGLEBRACKET: &str = r#">$"#;

/// marked `other.pedanticHrefTitle` (flags ``).
pub const OTHER_PEDANTICHREFTITLE: &str = r#"^([^'"]*[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}])[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+(['"])(.*)\2"#;

/// marked `other.unicodeAlphaNumeric` (flags `u`).
pub const OTHER_UNICODEALPHANUMERIC: &str = r#"[\p{L}\p{N}]"#;

/// marked `other.escapeTest` (flags ``).
pub const OTHER_ESCAPETEST: &str = r#"[&<>"']"#;

/// marked `other.escapeReplace` (flags `g`).
pub const OTHER_ESCAPEREPLACE: &str = r#"[&<>"']"#;

/// marked `other.escapeTestNoEncode` (flags ``).
pub const OTHER_ESCAPETESTNOENCODE: &str = r#"[<>"']|&(?!(#[0-9]{1,7}|#[Xx][a-fA-F0-9]{1,6}|[0-9A-Za-z_]+);)"#;

/// marked `other.escapeReplaceNoEncode` (flags `g`).
pub const OTHER_ESCAPEREPLACENOENCODE: &str = r#"[<>"']|&(?!(#[0-9]{1,7}|#[Xx][a-fA-F0-9]{1,6}|[0-9A-Za-z_]+);)"#;

/// marked `other.caret` (flags `g`).
pub const OTHER_CARET: &str = r#"(^|[^\[])\^"#;

/// marked `other.percentDecode` (flags `g`).
pub const OTHER_PERCENTDECODE: &str = r#"%25"#;

/// marked `other.findPipe` (flags `g`).
pub const OTHER_FINDPIPE: &str = r#"\|"#;

/// marked `other.splitPipe` (flags ``).
pub const OTHER_SPLITPIPE: &str = r#" \|"#;

/// marked `other.slashPipe` (flags `g`).
pub const OTHER_SLASHPIPE: &str = r#"\\\|"#;

/// marked `other.carriageReturn` (flags `g`).
pub const OTHER_CARRIAGERETURN: &str = r#"\r\n|\r"#;

/// marked `other.spaceLine` (flags `gm`).
pub const OTHER_SPACELINE: &str = r#"^ +$"#;

/// marked `other.notSpaceStart` (flags ``).
pub const OTHER_NOTSPACESTART: &str = r#"^[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*"#;

/// marked `other.endingNewline` (flags ``).
pub const OTHER_ENDINGNEWLINE: &str = r#"\n$"#;

/// marked `block.blockquote` (flags ``).
pub const BLOCK_BLOCKQUOTE: &str = r#"^( {0,3}> ?(([^\n]+(?:\n(?! {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)| {0,3}#{1,6}(?:[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|$)| {0,3}>| {0,3}(?:`{3,}(?=[^`\n]*(?:\n|$))|~~~)[^\n]*(?:\n|$)| {0,3}(?:[*+-]|[0-9]{1,9}[.)])(?:[ \t]|\n|$)|<\/?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)(?: +|\n|\/?>)|<(?:script|pre|style|textarea|!--)|[ \t]+\n)[^\n]+)*)|[^\n]*)(?:\n|$))+"#;

/// marked `block.code` (flags ``).
pub const BLOCK_CODE: &str = r#"^((?: {4}| {0,3}\t)[^\n]+(?:\n(?:[ \t]*(?:\n|$))*)?)+"#;

/// marked `block.def` (flags ``).
pub const BLOCK_DEF: &str = r#"^ {0,3}\[((?![\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*\])(?:\\[\s\S]|[^\[\]\\])+)\]: *(?:\n[ \t]*)?([^<\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}][^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*|<.*?>)(?:(?: +(?:\n[ \t]*)?| *\n[ \t]*)((?:"(?:\\"?|[^"\\])*"|'[^'\n]*(?:\n[^'\n]+)*\n?'|\([^()]*\))))? *(?:\n+|$)"#;

/// marked `block.fences` (flags ``).
pub const BLOCK_FENCES: &str = r#"^ {0,3}(`{3,}(?=[^`\n]*(?:\n|$))|~{3,})([^\n]*)(?:\n|$)(?:|([\s\S]*?)(?:\n|$))(?: {0,3}\1[~`]* *(?=\n|$)|$)"#;

/// marked `block.heading` (flags ``).
pub const BLOCK_HEADING: &str = r#"^ {0,3}(#{1,6})(?=[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|$)(.*)(?:\n+|$)"#;

/// marked `block.hr` (flags ``).
pub const BLOCK_HR: &str = r#"^ {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)"#;

/// marked `block.html` (flags `i`).
pub const BLOCK_HTML: &str = r#"^ {0,3}(?:<(script|pre|style|textarea)[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}>][\s\S]*?(?:<\/\1>[^\n]*\n*|$)|<!--(?:-?>|[\s\S]*?(?:-->|$))[^\n]*(\n+|$)|<\?[\s\S]*?(?:\?>[^\n]*\n*|$)|<![A-Z][\s\S]*?(?:>[^\n]*\n*|$)|<!\[CDATA\[[\s\S]*?(?:\]\]>[^\n]*\n*|$)|<\/?(address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)(?: +|\n|\/?>)[\s\S]*?(?:(?:\n[ 	]*)+\n|$)|<(?!script|pre|style|textarea)([a-z][a-z0-9-]*)(?: +[a-zA-Z:_][0-9A-Za-z_.:-]*(?: *= *"[^"\n]*"| *= *'[^'\n]*'| *= *[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}"'=<>`]+)?)*? *\/?>(?=[ \t]*(?:\n|$))[\s\S]*?(?:(?:\n[ 	]*)+\n|$)|<\/(?!script|pre|style|textarea)[a-z][a-z0-9-]*[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*>(?=[ \t]*(?:\n|$))[\s\S]*?(?:(?:\n[ 	]*)+\n|$))"#;

/// marked `block.lheading` (flags ``).
pub const BLOCK_LHEADING: &str = r#"^(?! {0,3}(?:[*+-]|[0-9]{1,9}[.)]) |(?: {4}| {0,3}\t)| {0,3}(?:`{3,}|~{3,})| {0,3}>| {0,3}#{1,6}(?:[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|$)| {0,3}<[^\n>]+>\n| {0,3}\|?(?:[:\- ]*\|)+[\:\- ]*\n)((?:.|\n(?![\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*?\n| {0,3}(?:[*+-]|[0-9]{1,9}[.)]) |(?: {4}| {0,3}\t)| {0,3}(?:`{3,}|~{3,})| {0,3}>| {0,3}#{1,6}(?:[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|$)| {0,3}<[^\n>]+>\n| {0,3}\|?(?:[:\- ]*\|)+[\:\- ]*\n))+?)\n {0,3}(=+|-+) *(?:\n+|$)"#;

/// marked `block.list` (flags ``).
pub const BLOCK_LIST: &str = r#"^( {0,3}(?:[*+-]|[0-9]{1,9}[.)]))([ \t][^\n]*?)?(?:\n|$)"#;

/// marked `block.newline` (flags ``).
pub const BLOCK_NEWLINE: &str = r#"^(?:[ \t]*(?:\n|$))+"#;

/// marked `block.paragraph` (flags ``).
pub const BLOCK_PARAGRAPH: &str = r#"^([^\n]+(?:\n(?! {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)| {0,3}#{1,6}(?:[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|$)| {0,3}>| {0,3}(?:`{3,}(?=[^`\n]*(?:\n|$))|~~~)[^\n]*(?:\n|$)| {0,3}(?:[*+-]|1[.)])[ \t]+[^ \t\n]|<\/?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)(?: +|\n|\/?>)|<(?:script|pre|style|textarea|!--)| *([^\n ].*)\n {0,3}((?:\| *)?:?-+:? *(?:\| *:?-+:? *)*(?:\| *)?)(?:\n((?:(?! *\n| {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)| {0,3}#{1,6}(?:[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|$)| {0,3}>|(?: {4}| {0,3}	)[^\n]| {0,3}(?:`{3,}(?=[^`\n]*(?:\n|$))|~~~)[^\n]*(?:\n|$)| {0,3}(?:[*+-]|1[.)])[ \t]|<\/?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)(?: +|\n|\/?>)|<(?:script|pre|style|textarea|!--)).*(?:\n|$))*)\n*|$)|[ \t]+\n)[^\n]+)*)"#;

/// marked `block.table` (flags ``).
pub const BLOCK_TABLE: &str = r#"^ *([^\n ].*)\n {0,3}((?:\| *)?:?-+:? *(?:\| *:?-+:? *)*(?:\| *)?)(?:\n((?:(?! *\n| {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)| {0,3}#{1,6}(?:[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|$)| {0,3}>|(?: {4}| {0,3}	)[^\n]| {0,3}(?:`{3,}(?=[^`\n]*(?:\n|$))|~~~)[^\n]*(?:\n|$)| {0,3}(?:[*+-]|1[.)])[ \t]|<\/?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)(?: +|\n|\/?>)|<(?:script|pre|style|textarea|!--)).*(?:\n|$))*)\n*|$)"#;

/// marked `block.text` (flags ``).
pub const BLOCK_TEXT: &str = r#"^[^\n]+"#;

/// marked `inline._backpedal` (flags ``).
pub const INLINE__BACKPEDAL: &str = r#"(?:[^?!.,:;*_'"~()&]+|\([^)]*\)|&(?![a-zA-Z0-9]+;$)|[?!.,:;*_'"~)]+(?!$))+"#;

/// marked `inline.anyPunctuation` (flags `gu`).
pub const INLINE_ANYPUNCTUATION: &str = r#"\\([\p{P}\p{S}])"#;

/// marked `inline.autolink` (flags ``).
pub const INLINE_AUTOLINK: &str = r#"^<([a-zA-Z][a-zA-Z0-9+.-]{1,31}:[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\x00-\x1f<>]*|[a-zA-Z0-9.!#$%&'*+/=?_`{|}~-]+(@)[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)+(?![-_]))>"#;

/// marked `inline.blockSkip` (flags `g`).
pub const INLINE_BLOCKSKIP: &str = r#"\[(?:[^\[\]`]|(?<a>`+)[^`]+\k<a>(?!`))*?\]\((?:\\[\s\S]|[^\\\(\)]|\((?:\\[\s\S]|[^\\\(\)])*\))*\)|(?<!`)()(?<b>`+)[^`]+\k<b>(?!`)|<(?! )[^<>]*?>"#;

/// marked `inline.br` (flags ``).
pub const INLINE_BR: &str = r#"^( {2,}|\\)\n(?![\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*$)[ \t]*"#;

/// marked `inline.code` (flags ``).
pub const INLINE_CODE: &str = r#"^(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)"#;

/// marked `inline.del` (flags ``).
pub const INLINE_DEL: &str = r#"^(~~?)(?=[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}~])((?:\\[\s\S]|[^\\])*?(?:\\[\s\S]|[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}~\\]))\1(?=[^~]|$)"#;

/// marked `inline.delLDelim` (flags `u`).
pub const INLINE_DELLDELIM: &str = r#"^~~?(?:((?!~)[\p{P}\p{S}])|[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}~])"#;

/// marked `inline.delRDelim` (flags `gu`).
pub const INLINE_DELRDELIM: &str = r#"^[^~]+(?=[^~])|(?!~)[\p{P}\p{S}](~~?)(?=[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|$)|[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}](~~?)(?!~)(?=[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}]|$)|(?!~)[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}](~~?)(?=[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}])|[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}](~~?)(?!~)(?=[\p{P}\p{S}])|(?!~)[\p{P}\p{S}](~~?)(?!~)(?=[\p{P}\p{S}])|[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}](~~?)(?=[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}])"#;

/// marked `inline.emStrongLDelim` (flags `u`).
pub const INLINE_EMSTRONGLDELIM: &str = r#"^(?:\*+(?:((?!\*)(?!~)[\p{P}\p{S}])|([^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}*]))?)|^_+(?:((?!_)(?!~)[\p{P}\p{S}])|([^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}_]))?"#;

/// marked `inline.emStrongRDelimAst` (flags `gu`).
pub const INLINE_EMSTRONGRDELIMAST: &str = r#"^[^_*]*?__[^_*]*?\*[^_*]*?(?=__)|[^*]+(?=[^*])|(?!\*)(?!~)[\p{P}\p{S}](\*+)(?=[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|$)|(?:[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}]|~)(\*+)(?!\*)(?=(?!~)[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}]|$)|(?!\*)(?!~)[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}](\*+)(?=(?:[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}]|~))|[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}](\*+)(?!\*)(?=(?!~)[\p{P}\p{S}])|(?!\*)(?!~)[\p{P}\p{S}](\*+)(?!\*)(?=(?!~)[\p{P}\p{S}])|(?:[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}]|~)(\*+)(?=(?:[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}]|~))"#;

/// marked `inline.emStrongRDelimUnd` (flags `gu`).
pub const INLINE_EMSTRONGRDELIMUND: &str = r#"^[^_*]*?\*\*[^_*]*?_[^_*]*?(?=\*\*)|[^_]+(?=[^_])|(?!_)[\p{P}\p{S}](_+)(?=[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|$)|[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}](_+)(?!_)(?=[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}]|$)|(?!_)[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}](_+)(?=[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}])|[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}](_+)(?!_)(?=[\p{P}\p{S}])|(?!_)[\p{P}\p{S}](_+)(?!_)(?=[\p{P}\p{S}])"#;

/// marked `inline.escape` (flags ``).
pub const INLINE_ESCAPE: &str = r##"^\\([!"#$%&'()*+,\-./:;<=>?@\[\]\\^_`{|}~])"##;

/// marked `inline.link` (flags ``).
pub const INLINE_LINK: &str = r#"^!?\[((?:\[(?:\[(?:\\[\s\S]|[^\[\]\\])*\]|\\[\s\S]|[^\[\]\\])*\]|\\[\s\S]|`+(?!`)[^`]*?`+(?!`)|``+(?=\])|[^\[\]\\`])*?)\]\([\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*(<(?:\\.|[^\n<>\\])+>|[^ \t\n\x00-\x1f]+|(?=\)))(?:(?:[ \t]+(?:\n[ \t]*)?|\n[ \t]*)("(?:\\"?|[^"\\])*"|'(?:\\'?|[^'\\])*'|\((?:\\\)?|[^)\\])*\)))?[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*\)"#;

/// marked `inline.nolink` (flags ``).
pub const INLINE_NOLINK: &str = r#"^!?\[((?![\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*\])(?:\\[\s\S]|[^\[\]\\])+)\](?:\[\])?"#;

/// marked `inline.punctuation` (flags `u`).
pub const INLINE_PUNCTUATION: &str = r#"^((?![*_])[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\p{P}\p{S}])"#;

/// marked `inline.reflink` (flags ``).
pub const INLINE_REFLINK: &str = r#"^!?\[((?:\[(?:\[(?:\\[\s\S]|[^\[\]\\])*\]|\\[\s\S]|[^\[\]\\])*\]|\\[\s\S]|`+(?!`)[^`]*?`+(?!`)|``+(?=\])|[^\[\]\\`])*?)\]\[((?![\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*\])(?:\\[\s\S]|[^\[\]\\])+)\]"#;

/// marked `inline.reflinkSearch` (flags `g`).
pub const INLINE_REFLINKSEARCH: &str = r#"!?\[((?:[^\[\]\\`]*(?:\[(?:\[(?:\\[\s\S]|[^\[\]\\])*\]|\\[\s\S]|[^\[\]\\])*\]|\\[\s\S]|`+(?!`)[^`]*?`+(?!`)|``+(?=\]))){0,999}?[^\[\]\\`]*?)\]\[((?![\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*\])(?:\\[\s\S]|[^\[\]\\]){1,999})\]|!?\[((?![\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*\])(?:\\[\s\S]|[^\[\]\\]){1,999})\](?:\[\])?(?!\()"#;

/// marked `inline.tag` (flags ``).
pub const INLINE_TAG: &str = r#"^<!--(?:-?>|[\s\S]*?-->)|^<\/[a-zA-Z][a-zA-Z0-9-]*[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*>|^<[a-zA-Z][a-zA-Z0-9-]*(?:[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+[a-zA-Z:_][0-9A-Za-z_.:-]*(?:[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*=[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*"[^"]*"|[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*=[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*'[^']*'|[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*=[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}"'=<>`]+)?)*?[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*\/?>|^<\?[\s\S]*?\?>|^<![a-zA-Z]+[\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}][\s\S]*?>|^<!\[CDATA\[[\s\S]*?\]\]>"#;

/// marked `inline.text` (flags ``).
pub const INLINE_TEXT: &str = r#"^(`+|~+|[^`~])(?:(?=[`~])|(?= {2,}\n)|(?=[a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-]+@)|[\s\S]*?(?:(?=[\\<!\[`*~_]|(?:(?<=[0-9A-Za-z_])(?![0-9A-Za-z_])|(?<![0-9A-Za-z_])(?=[0-9A-Za-z_]))_|[hH][tT][tT][pP][sS]?|[fF][tT][pP]:\/\/|www\.|$)|[^ ](?= {2,}\n)|[^a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-](?=[a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-]+@)))"#;

/// marked `inline.url` (flags ``).
pub const INLINE_URL: &str = r#"^((?:[hH][tT][tT][pP][sS]?|[fF][tT][pP]):\/\/|www\.)(?:[a-zA-Z0-9\-]+\.?)+[^\t\n\x0B\x0C\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}<]*|^[A-Za-z0-9._+-]+(@)[a-zA-Z0-9-_]+(?:\.[a-zA-Z0-9-_]*[a-zA-Z0-9])+(?![0-9A-Za-z_-])"#;

