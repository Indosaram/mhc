use maho_cli::utils::ansi::strip_ansi;
#[test]
fn upstream_generated_compatibility_inputs_match_reference_regex() {
    let reference = regex::Regex::new(r"(?:\x1b\][\s\S]*?(?:\x07|\x1b\\|\x{009c}))|[\x1b\x{009b}][\[\]()#;?]*(?:\d{1,4}(?:[;:]\d{0,4})*)?[\dA-PR-TZcf-nq-uy=><~]").unwrap();
    let chars = ['a','f','0','1',';',':','[',']','(',')','#','?','m','P','_','\\','\x07','\x1b','\u{009b}','\u{009c}','\u{0090}','\u{009d}'];
    for character in chars {
        let mut inputs = vec![format!("x\x1b{character}y"), format!("x\u{009b}{character}y")];
        for second in chars.iter().step_by(3) { inputs.push(format!("x\x1b{character}{second}y")); }
        for input in inputs { assert_eq!(strip_ansi(&input), reference.replace_all(&input, "")); }
    }
}
#[test] fn upstream_ris_does_not_leak_final_byte() { assert_eq!(strip_ansi("\x1bcdone"), "done"); }
#[test] fn upstream_single_byte_escape_sequences_are_removed() { for code in ('g'..='m').chain('r'..='t') { assert_eq!(strip_ansi(&format!("\x1b{code}ok")), "ok"); } }
#[test] fn upstream_common_tool_output_sequences_are_removed() { assert_eq!(strip_ansi("a\x1b[31mred\x1b[0m\x1b]8;;https://example.com\x07link\x1b]8;;\x07z"), "aredlinkz"); }
