use super::types::EvalLanguage;
use regex::Regex;
use std::sync::LazyLock;

/// Why an omitted run language was filled: the only enabled kernel, or unambiguous code markers
/// (`senpi#5154`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EvalLanguageInferenceBasis {
    #[serde(rename = "only-enabled-language")]
    OnlyEnabledLanguage,
    #[serde(rename = "code-markers")]
    CodeMarkers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvalLanguageInference {
    pub language: EvalLanguage,
    pub basis: EvalLanguageInferenceBasis,
}

/// The argument key validation sets when it filled the language, so execution can tell the model.
pub const EVAL_LANGUAGE_INFERRED_KEY: &str = "language_inferred";

struct LanguageSignals {
    strong: &'static [LazyLock<Regex>],
    weak: &'static [LazyLock<Regex>],
}

// `print(` is a prelude helper in both runtimes, so it is no signal at all; these forms are
// JS-only or Python-only.
static PY_STRONG: [LazyLock<Regex>; 3] = [
    LazyLock::new(|| {
        Regex::new(r"(?m)^[ \t]*def[ \t]+[A-Za-z_]\w*[ \t]*\(").expect("py def regex")
    }),
    LazyLock::new(|| {
        Regex::new(
            r"(?m)^[ \t]*import[ \t]+[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*(?:[ \t]+as[ \t]+[A-Za-z_]\w*)?(?:[ \t]*,[ \t]*[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*(?:[ \t]+as[ \t]+[A-Za-z_]\w*)?)*[ \t]*$",
        )
        .expect("py import regex")
    }),
    LazyLock::new(|| {
        Regex::new(
            r"(?m)^[ \t]*from[ \t]+\.*[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*[ \t]+import[ \t]+[\w(*]",
        )
        .expect("py from-import regex")
    }),
];

static PY_WEAK: [LazyLock<Regex>; 3] = [
    LazyLock::new(|| {
        Regex::new(r"\b(?:None|True|False|elif|lambda|self)\b").expect("py keywords regex")
    }),
    LazyLock::new(|| {
        Regex::new(
            r"(?m)^[ \t]*(?:if|elif|else|for|while|with|try|except|finally|class|async[ \t]+def)\b.*:[ \t]*(?:#.*)?$",
        )
        .expect("py colon block regex")
    }),
    LazyLock::new(|| Regex::new(r"(?m)^[ \t]*#").expect("py comment regex")),
];

static JS_STRONG: [LazyLock<Regex>; 8] = [
    LazyLock::new(|| {
        Regex::new(r"\bawait[ \t]+[A-Za-z_$][\w$.]*[ \t]*\(").expect("js await regex")
    }),
    LazyLock::new(|| Regex::new(r"`").expect("js backtick regex")),
    LazyLock::new(|| Regex::new(r"(?:JSON|Object|Array)\.[A-Za-z_$]").expect("js global regex")),
    LazyLock::new(|| {
        Regex::new(r"(?m)^[ \t]*(?:const|let)[ \t]+[A-Za-z_$\[{]").expect("js decl regex")
    }),
    LazyLock::new(|| Regex::new(r"=>").expect("js arrow regex")),
    LazyLock::new(|| Regex::new(r"\bconsole\.log\(").expect("js console.log regex")),
    LazyLock::new(|| {
        Regex::new(r#"(?m)^[ \t]*import[ \t].*\bfrom[ \t]*["']"#).expect("js import-from regex")
    }),
    LazyLock::new(|| Regex::new(r#"(?m)^[ \t]*import[ \t]*["']"#).expect("js side-effect import")),
];

static JS_WEAK: [LazyLock<Regex>; 5] = [
    LazyLock::new(|| {
        Regex::new(r"\b(?:null|undefined|function|var|typeof|new)\b").expect("js keywords regex")
    }),
    LazyLock::new(|| Regex::new(r"===|!==|&&|\|\||\+\+").expect("js operators regex")),
    LazyLock::new(|| Regex::new(r"(?m);[ \t]*$").expect("js semicolon regex")),
    LazyLock::new(|| Regex::new(r"(?m)^[ \t]*//").expect("js line comment regex")),
    LazyLock::new(|| Regex::new(r"(?m)[{}][ \t]*$").expect("js brace regex")),
];

fn has_js_builtin_global(code: &str) -> bool {
    // Match JS `(?<![\w.$])(?:JSON|Object|Array)\.[A-Za-z_$]` without lookbehind (not supported by
    // the `regex` crate).
    for m in JS_STRONG[2].find_iter(code) {
        let before = code[..m.start()].chars().next_back();
        if !before.is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$' | '.')) {
            return true;
        }
    }
    false
}

fn signals(code: &str, signals: &LanguageSignals, is_js: bool) -> (bool, bool) {
    let has_strong = signals.strong.iter().enumerate().any(|(idx, pattern)| {
        if is_js && idx == 2 {
            has_js_builtin_global(code)
        } else {
            pattern.is_match(code)
        }
    });
    let has_any = has_strong || signals.weak.iter().any(|pattern| pattern.is_match(code));
    (has_strong, has_any)
}

/// Classifies a cell body as `"js"` or `"py"` only on a strong marker of one language with no
/// signal of the other; anything else, including any conflicting signal, is unclassified.
pub fn classify_eval_code(code: &str) -> Option<&'static str> {
    let py_sig = LanguageSignals {
        strong: &PY_STRONG,
        weak: &PY_WEAK,
    };
    let js_sig = LanguageSignals {
        strong: &JS_STRONG,
        weak: &JS_WEAK,
    };
    let (py_strong, py_any) = signals(code, &py_sig, false);
    let (js_strong, js_any) = signals(code, &js_sig, true);
    if py_strong && !js_any {
        return Some("py");
    }
    if js_strong && !py_any {
        return Some("js");
    }
    None
}

/// The language an omitted run language may be filled with: the only enabled kernel, or (when `js`
/// and `py` are both enabled) the classified code.
pub fn infer_eval_language(
    code: Option<&str>,
    languages: &[EvalLanguage],
) -> Option<EvalLanguageInference> {
    if let [only] = languages {
        return Some(EvalLanguageInference {
            language: *only,
            basis: EvalLanguageInferenceBasis::OnlyEnabledLanguage,
        });
    }
    if let Some(code) = code
        && classify_eval_code(code) == Some("js")
        && languages.contains(&EvalLanguage::Js)
    {
        return Some(EvalLanguageInference {
            language: EvalLanguage::Js,
            basis: EvalLanguageInferenceBasis::CodeMarkers,
        });
    }
    None
}

/// The line an inferred-language result leads with, so the model sees the fill and passes
/// `language` next time.
pub fn eval_language_inferred_note(
    language: EvalLanguage,
    basis: EvalLanguageInferenceBasis,
) -> String {
    let lang = match language {
        EvalLanguage::Js => "js",
    };
    let basis_str = match basis {
        EvalLanguageInferenceBasis::OnlyEnabledLanguage => "only-enabled-language",
        EvalLanguageInferenceBasis::CodeMarkers => "code-markers",
    };
    format!(
        "{EVAL_LANGUAGE_INFERRED_KEY}: {lang} ({basis_str}). This eval run omitted language; pass language explicitly."
    )
}
