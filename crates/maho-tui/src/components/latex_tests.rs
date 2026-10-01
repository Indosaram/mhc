use super::*;

fn latex(formula: &str) -> String {
    latex_to_unicode(formula)
}

#[test]
fn converts_every_relational_algebra_join_command_to_its_unicode_operator() {
    assert_eq!(latex(r"R \bowtie S"), "R \u{22c8} S");
    assert_eq!(latex(r"R \Join S"), "R \u{22c8} S");
    assert_eq!(latex(r"R \ltimes S"), "R \u{22c9} S");
    assert_eq!(latex(r"R \rtimes S"), "R \u{22ca} S");
    assert_eq!(latex(r"R \leftouterjoin S"), "R \u{27d5} S");
    assert_eq!(latex(r"R \rightouterjoin S"), "R \u{27d6} S");
    assert_eq!(latex(r"R \fullouterjoin S"), "R \u{27d7} S");
}

#[test]
fn converts_join_commands_inside_grouped_expressions() {
    assert_eq!(latex(r"(R \ltimes S) \bowtie T"), "(R \u{22c9} S) \u{22c8} T");
    assert_eq!(latex(r"\{R \fullouterjoin S\}"), "{R \u{27d7} S}");
}

#[test]
fn leaves_longer_commands_that_merely_share_a_join_prefix_literal() {
    assert_eq!(latex(r"\bowtieX"), r"\bowtieX");
    assert_eq!(latex(r"\ltimesfoo"), r"\ltimesfoo");
}

#[test]
fn converts_greek_and_operator_symbols() {
    assert_eq!(latex(r"\alpha + \beta"), "\u{3b1} + \u{3b2}");
    assert_eq!(latex(r"\sum_{i=1}^{n} i"), "\u{2211}\u{1d62}\u{208c}\u{2081}\u{207f} i");
    assert_eq!(latex(r"\int_0^\infty"), "\u{222b}\u{2080}^\u{221e}");
    assert_eq!(latex(r"a \ne b \le c \ge d"), "a \u{2260} b \u{2264} c \u{2265} d");
    assert_eq!(latex(r"\Omega \omega \Phi \phi"), "\u{3a9} \u{3c9} \u{3a6} \u{3d5}");
}

#[test]
fn keeps_scripts_literal_when_no_script_form_exists() {
    assert_eq!(latex(r"x^q"), "x^q");
    assert_eq!(latex(r"x_q"), "x_q");
    assert_eq!(latex(r"x^{ab}"), "x^{ab}");
    assert_eq!(latex(r"x^"), "x^");
    assert_eq!(latex(r"x_"), "x_");
}

#[test]
fn renders_frac_sqrt_and_left_right_delimiters() {
    assert_eq!(latex(r"\frac{a}{b}"), "(a)\u{2044}(b)");
    assert_eq!(latex(r"\sqrt{x+1}"), "\u{221a}(x+1)");
    assert_eq!(latex(r"\left( x \right)"), "( x )");
    assert_eq!(latex(r"\left. x \right|"), " x |");
    assert_eq!(latex(r"\left\{ x \right\}"), "{ x }");
}

#[test]
fn handles_style_commands_and_unknown_grouping() {
    assert_eq!(latex(r"\mathrm{abc}"), "abc");
    assert_eq!(latex(r"\text{hello}"), "hello");
    assert_eq!(latex(r"\operatorname{foo}"), "foo");
    assert_eq!(latex(r"\mathbf x"), r"\mathbfx");
    assert_eq!(latex(r"\mathrm"), r"\mathrm");
    assert_eq!(latex(r"\unknowncmd{x}"), r"\unknowncmd{x}");
    assert_eq!(latex(r"\unknowncmd"), r"\unknowncmd");
}

#[test]
fn converts_escaped_punctuation_commands() {
    assert_eq!(latex(r"a\%b"), "a%b");
    assert_eq!(latex(r"a\&b"), "a&b");
    assert_eq!(latex(r"a\#b"), "a#b");
    assert_eq!(latex(r"a\_b"), "a_b");
    assert_eq!(latex(r"a\{b\}c"), "a{b}c");
    assert_eq!(latex(r"a\$b"), "a$b");
    assert_eq!(latex(r"a\\b"), r"a\\b");
    assert_eq!(latex(r"a\,b"), "a b");
    assert_eq!(latex(r"a\!b"), "ab");
}

#[test]
fn falls_back_literally_beyond_the_conversion_budgets() {
    let over_budget = format!(r"{}", "x".repeat(MAX_FORMULA_LENGTH + 1));
    assert_eq!(latex(&over_budget), over_budget);

    let deep = format!("{}{}", "{".repeat(MAX_NESTING_DEPTH + 2), "}".repeat(MAX_NESTING_DEPTH + 2));
    assert_eq!(latex(&deep), deep);
}

#[test]
fn anchors_leading_combining_marks_and_normalizes_whitespace() {
    assert_eq!(latex("\u{0301}abc"), "\u{25cc}\u{0301}abc");
    assert_eq!(latex("  x   +\n y  "), "x + y");
    assert_eq!(latex(""), "");
    assert_eq!(latex("   "), "");
}

#[test]
fn keeps_chinese_and_cjk_formulas_readable() {
    assert_eq!(latex(r"\text{速度} = \frac{d}{t}"), "速度 = (d)\u{2044}(t)");
    assert_eq!(latex("速度"), "速度");
}

#[test]
fn handles_quad_and_command_argument_scanning() {
    assert_eq!(latex(r"a \quad b"), "a    b");
    assert_eq!(latex(r"\quad"), "  ");
    assert_eq!(latex(r"\foo{a}{b}"), r"\foo{a}{b}");
    assert_eq!(latex(r"\foo a"), r"\foo a");
}
