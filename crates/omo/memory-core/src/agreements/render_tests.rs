use pretty_assertions::assert_eq;

use super::render_agreements_section;

#[test]
fn empty_ledger_renders_nothing() {
    assert_eq!(render_agreements_section(""), "");
    assert_eq!(render_agreements_section("not a ledger line"), "");
}

#[test]
fn renders_sorted_lines_with_flag() {
    let body = "- [project] B agent <!-- id: bbbbbbbbbb; by: agent; n: 3; since: 2026-01-02; status: needs-human -->\n- [global] A human <!-- id: aaaaaaaaaa; by: human; n: 0; since: 2026-01-03; status: active -->\n";
    let out = render_agreements_section(body);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "<agreements>");
    assert_eq!(
        lines[1],
        "<projection>$MEMORY_DIR/system/agreements.md</projection>"
    );
    assert_eq!(lines[3], "- [global] [human] [broken x0] A human");
    assert_eq!(
        lines[4],
        "- [project] [agent] [broken x3] NEEDS-HUMAN: B agent"
    );
    assert_eq!(lines[5], "</agreements>");
}
