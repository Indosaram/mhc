//! senpi packages/coding-agent/test/prompt-templates.test.ts cases for the substitution, argument
//! parsing, expansion and argument-hint core of `prompt_templates` (ledger row crates-maho-core-09).

use crate::prompt_templates::{
    LoadPromptTemplatesOptions, PromptTemplate, expand_prompt_template, load_prompt_templates,
    parse_command_args, substitute_args,
};
use crate::source_info::{SourceInfo, SourceOrigin, SourceScope};

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn numbered(prefix: &str, count: usize, offset: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("{prefix}{}", index + offset))
        .collect()
}

#[test]
fn substitute_args_wildcards_and_positionals() {
    let cases: &[(&str, &[&str], &str)] = &[
        ("Test: $ARGUMENTS", &["a", "b", "c"], "Test: a b c"),
        ("Test: $@", &["a", "b", "c"], "Test: a b c"),
        ("$ARGUMENTS", &["$1", "$ARGUMENTS"], "$1 $ARGUMENTS"),
        ("$@", &["$100", "$1"], "$100 $1"),
        ("$ARGUMENTS", &["$100", "$1"], "$100 $1"),
        (
            "$1: $ARGUMENTS",
            &["prefix", "a", "b"],
            "prefix: prefix a b",
        ),
        ("$1: $@", &["prefix", "a", "b"], "prefix: prefix a b"),
        ("Test: $ARGUMENTS", &[], "Test: "),
        ("Test: $@", &[], "Test: "),
        ("Test: $1", &[], "Test: "),
        ("$ARGUMENTS and $ARGUMENTS", &["a", "b"], "a b and a b"),
        ("$@ and $@", &["a", "b"], "a b and a b"),
        ("$@ and $ARGUMENTS", &["a", "b"], "a b and a b"),
        (
            "$1 $2: $ARGUMENTS",
            &["arg100", "@user"],
            "arg100 @user: arg100 @user",
        ),
        ("$1 $2 $3 $4 $5", &["a", "b"], "a b   "),
        ("$ARGUMENTS", &["日本語", "🎉", "café"], "日本語 🎉 café"),
        (
            "$1 $2",
            &["line1\nline2", "tab\tthere"],
            "line1\nline2 tab\tthere",
        ),
        ("$1$2", &["a", "b"], "ab"),
        (
            "$ARGUMENTS",
            &["first arg", "second arg"],
            "first arg second arg",
        ),
        ("Test: $ARGUMENTS", &["only"], "Test: only"),
        ("Test: $@", &["only"], "Test: only"),
        ("$0", &["a", "b"], ""),
        ("$1.5", &["a"], "a.5"),
        ("pre$ARGUMENTS", &["a", "b"], "prea b"),
        ("pre$@", &["a", "b"], "prea b"),
        ("$ARGUMENTS", &["a", "", "c"], "a  c"),
        (
            "$ARGUMENTS",
            &["  leading  ", "trailing  "],
            "  leading   trailing  ",
        ),
        (
            "Prefix $ARGUMENTS suffix",
            &["ARGUMENTS"],
            "Prefix ARGUMENTS suffix",
        ),
        ("$A $$ $ $ARGS", &["a"], "$A $$ $ $ARGS"),
        (
            "$arguments $Arguments $ARGUMENTS",
            &["a", "b"],
            "$arguments $Arguments a b",
        ),
        ("$@ and $ARGUMENTS", &["x", "y", "z"], "x y z and x y z"),
        ("$ARGUMENTS and $@", &["x", "y", "z"], "x y z and x y z"),
        ("$1 $2 $3", &["a", "b", "c"], "a b c"),
        ("Price: \\$100", &[], "Price: \\"),
        (
            "$1: $@ ($ARGUMENTS)",
            &["first", "second", "third"],
            "first: first second third (first second third)",
        ),
        ("Just plain text", &["a", "b"], "Just plain text"),
        ("$1 $2 $@", &["a", "b", "c"], "a b a b c"),
    ];
    for (template, values, expected) in cases {
        assert_eq!(
            substitute_args(template, &args(values)),
            *expected,
            "{template:?} {values:?}"
        );
    }
    let long = numbered("arg", 100, 0);
    assert_eq!(substitute_args("$ARGUMENTS", &long), long.join(" "));
    assert_eq!(
        substitute_args("$10 $12 $15", &numbered("val", 15, 0)),
        "val9 val11 val14"
    );
}

#[test]
fn substitute_args_positional_defaults() {
    let cases: &[(&str, &[&str], &str)] = &[
        (
            "List exactly ${1:-7} next steps",
            &[],
            "List exactly 7 next steps",
        ),
        ("${1:-default}\n${1:-default}", &[], "default\ndefault"),
        (
            "List exactly ${1:-7} next steps",
            &["3"],
            "List exactly 3 next steps",
        ),
        ("Mode: ${1:-brief}", &[""], "Mode: brief"),
        ("${1:-7} ${2:-brief}", &[], "7 brief"),
        ("${1:-7} ${2:-brief}", &["3"], "3 brief"),
        ("${1:-7} ${2:-brief}", &["3", "verbose"], "3 verbose"),
        ("${1:-7}", &["$ARGUMENTS"], "$ARGUMENTS"),
        ("${1:-7}", &["$1"], "$1"),
        ("${1:-$ARGUMENTS}", &["a", "b"], "a"),
        ("${3:-$ARGUMENTS}", &["a", "b"], "$ARGUMENTS"),
        ("${1:-seven steps}", &[], "seven steps"),
        ("${3:-fallback}", &["a", "b"], "fallback"),
        ("$1 ${2:-x} $ARGUMENTS", &["a"], "a x a"),
    ];
    for (template, values, expected) in cases {
        assert_eq!(
            substitute_args(template, &args(values)),
            *expected,
            "{template:?} {values:?}"
        );
    }
}

#[test]
fn substitute_args_array_slicing() {
    let cases: &[(&str, &[&str], &str)] = &[
        ("${@:2}", &["a", "b", "c", "d"], "b c d"),
        ("${@:1}", &["a", "b", "c"], "a b c"),
        ("${@:3}", &["a", "b", "c", "d"], "c d"),
        ("${@:2:2}", &["a", "b", "c", "d"], "b c"),
        ("${@:1:1}", &["a", "b", "c"], "a"),
        ("${@:3:1}", &["a", "b", "c", "d"], "c"),
        ("${@:2:3}", &["a", "b", "c", "d", "e"], "b c d"),
        ("${@:99}", &["a", "b"], ""),
        ("${@:5}", &["a", "b"], ""),
        ("${@:10:5}", &["a", "b"], ""),
        ("${@:2:0}", &["a", "b", "c"], ""),
        ("${@:1:0}", &["a", "b"], ""),
        ("${@:2:99}", &["a", "b", "c"], "b c"),
        ("${@:1:10}", &["a", "b"], "a b"),
        ("${@:2} vs $@", &["a", "b", "c"], "b c vs a b c"),
        (
            "First: ${@:1:1}, All: $@",
            &["x", "y", "z"],
            "First: x, All: x y z",
        ),
        ("${@:1}", &["${@:2}", "test"], "${@:2} test"),
        ("${@:2}", &["a", "${@:3}", "c"], "${@:3} c"),
        ("$1: ${@:2}", &["cmd", "arg1", "arg2"], "cmd: arg1 arg2"),
        ("$1 $2 ${@:3}", &["a", "b", "c", "d"], "a b c d"),
        ("${@:0}", &["a", "b", "c"], "a b c"),
        ("${@:2}", &[], ""),
        ("${@:1}", &[], ""),
        ("${@:1}", &["only"], "only"),
        ("${@:2}", &["only"], ""),
        (
            "Process ${@:2} with $1",
            &["tool", "file1", "file2"],
            "Process file1 file2 with tool",
        ),
        ("${@:1:1} and ${@:2}", &["a", "b", "c"], "a and b c"),
        (
            "${@:1:2} vs ${@:3:2}",
            &["a", "b", "c", "d", "e"],
            "a b vs c d",
        ),
        (
            "${@:2}",
            &["cmd", "first arg", "second arg"],
            "first arg second arg",
        ),
        (
            "${@:2}",
            &["cmd", "$100", "@user", "#tag"],
            "$100 @user #tag",
        ),
        ("${@:1}", &["日本語", "🎉", "café"], "日本語 🎉 café"),
        (
            "Run $1 on ${@:2:2}, then process $@",
            &["eslint", "file1.ts", "file2.ts", "file3.ts"],
            "Run eslint on file1.ts file2.ts, then process eslint file1.ts file2.ts file3.ts",
        ),
        ("prefix${@:2}suffix", &["a", "b", "c"], "prefixb csuffix"),
    ];
    for (template, values, expected) in cases {
        assert_eq!(
            substitute_args(template, &args(values)),
            *expected,
            "{template:?} {values:?}"
        );
    }
    assert_eq!(
        substitute_args("${@:5:100}", &numbered("arg", 10, 1)),
        "arg5 arg6 arg7 arg8 arg9 arg10"
    );
}

#[test]
fn parse_command_args_cases() {
    let cases: &[(&str, &[&str])] = &[
        ("a b c", &["a", "b", "c"]),
        ("\"first arg\" second", &["first arg", "second"]),
        ("'first arg' second", &["first arg", "second"]),
        (
            "\"double\" 'single' \"double again\"",
            &["double", "single", "double again"],
        ),
        ("", &[]),
        ("a  b   c", &["a", "b", "c"]),
        ("a\tb\tc", &["a", "b", "c"]),
        ("\"\" \" \"", &[" "]),
        ("$100 @user #tag", &["$100", "@user", "#tag"]),
        ("日本語 🎉 café", &["日本語", "🎉", "café"]),
        ("\"line1\nline2\" second", &["line1\nline2", "second"]),
        (
            "label-2\n\nHere is some description #2.",
            &["label-2", "Here", "is", "some", "description", "#2."],
        ),
        ("a\n\n\tb  c", &["a", "b", "c"]),
        ("\"quoted \\\"text\\\"\"", &["quoted \\text\\"]),
        ("a b c   ", &["a", "b", "c"]),
        ("   a b c", &["a", "b", "c"]),
    ];
    for (input, expected) in cases {
        assert_eq!(parse_command_args(input), args(expected), "{input:?}");
    }
}

fn template(content: &str) -> PromptTemplate {
    PromptTemplate {
        name: "arg-test".to_owned(),
        description: "test".to_owned(),
        argument_hint: None,
        requires_arguments: false,
        content: content.to_owned(),
        source_info: SourceInfo {
            path: "/tmp/arg-test.md".to_owned(),
            source: "local".to_owned(),
            scope: SourceScope::Temporary,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        },
        file_path: "/tmp/arg-test.md".to_owned(),
    }
}

#[test]
fn expand_prompt_template_splits_on_unquoted_newlines() {
    let templates = [template("- arg1: $1\n- rest: ${@:2}")];
    assert_eq!(
        expand_prompt_template(
            "/arg-test label-2\n\nHere is some description #2.",
            &templates
        ),
        "- arg1: label-2\n- rest: Here is some description #2."
    );
    assert_eq!(
        expand_prompt_template("/arg-test\nlabel-2", &[template("arg1: $1")]),
        "arg1: label-2"
    );
}

#[test]
fn parse_and_substitute_integration() {
    let parsed = parse_command_args("Button \"onClick handler\" \"disabled support\"");
    assert_eq!(
        substitute_args("Create component $1 with features: $ARGUMENTS", &parsed),
        "Create component Button with features: Button onClick handler disabled support"
    );
    assert_eq!(
        substitute_args(
            "Create a React component named $1 with features: $ARGUMENTS",
            &parsed
        ),
        "Create a React component named Button with features: Button onClick handler disabled support"
    );
    let features = parse_command_args("feature1 feature2 feature3");
    assert_eq!(
        substitute_args("Implement: $@", &features),
        substitute_args("Implement: $ARGUMENTS", &features)
    );
}

#[test]
fn load_prompt_templates_argument_hint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let write = |name: &str, content: &str| {
        std::fs::write(dir.path().join(format!("{name}.md")), content).expect("write")
    };
    write(
        "pr",
        "---\ndescription: Review PRs from URLs with structured issue and code analysis\nargument-hint: \"<PR-URL>\"\n---\nYou are given one or more GitHub PR URLs: $@",
    );
    write(
        "wr",
        "---\ndescription: Finish the current task end-to-end with changelog, commit, and push\nargument-hint: \"[instructions]\"\n---\nWrap it. Additional instructions: $ARGUMENTS",
    );
    write(
        "cl",
        "---\ndescription: Audit changelog entries before release\n---\nAudit changelog entries.",
    );
    write(
        "empty-hint",
        "---\ndescription: A command with empty hint\nargument-hint: \"\"\n---\nDo something",
    );
    write(
        "is",
        "---\ndescription: Analyze GitHub issues\nargument-hint: \"<issue>\"\n---\nAnalyze: $ARGUMENTS",
    );
    let options = LoadPromptTemplatesOptions {
        cwd: dir.path().to_string_lossy().into_owned(),
        agent_dir: dir.path().join("agent").to_string_lossy().into_owned(),
        prompt_paths: vec![dir.path().to_string_lossy().into_owned()],
        include_defaults: false,
    };
    let (templates, _) = load_prompt_templates(&options);
    let hint = |name: &str| {
        templates
            .iter()
            .find(|template| template.name == name)
            .map(|template| template.argument_hint.clone())
    };
    assert_eq!(hint("pr"), Some(Some("<PR-URL>".to_owned())));
    assert_eq!(hint("wr"), Some(Some("[instructions]".to_owned())));
    assert_eq!(hint("cl"), Some(None));
    assert_eq!(hint("empty-hint"), Some(None));
    assert_eq!(hint("is"), Some(Some("<issue>".to_owned())));
    let pr = templates
        .iter()
        .find(|template| template.name == "pr")
        .map(|template| template.description.as_str());
    assert_eq!(
        pr,
        Some("Review PRs from URLs with structured issue and code analysis")
    );
}
