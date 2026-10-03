use crate::{interpreters::detect::InterpreterDetection, tool::types::{EvalLanguage, EvalRuntimeInfo}};

pub fn js_runtime_info(node_version: &str, bun_version: Option<&str>, exec_path: &str) -> EvalRuntimeInfo {
    match bun_version.filter(|version| !version.is_empty()) {
        Some(version) => EvalRuntimeInfo { name: "bun".into(), version: version.into(), path: Some(exec_path.into()) },
        None => EvalRuntimeInfo { name: "node".into(), version: node_version.into(), path: Some(exec_path.into()) },
    }
}

pub fn js_runtime_label(node_version: &str, bun_version: Option<&str>) -> String {
    let runtime = js_runtime_info(node_version, bun_version, "");
    format!("{} {}", runtime.name, runtime.version)
}

pub fn runtimes_from_availability(availability: &[(EvalLanguage, InterpreterDetection)], js: EvalRuntimeInfo) -> Vec<(EvalLanguage, EvalRuntimeInfo)> {
    let mut runtimes = vec![(EvalLanguage::Js, js)];
    for language in [EvalLanguage::Py, EvalLanguage::Rb, EvalLanguage::Jl] {
        if let Some((_, InterpreterDetection::Detected { path, version, resolved_path })) = availability.iter().find(|(candidate, _)| *candidate == language) {
            let name = match language { EvalLanguage::Py => "python", EvalLanguage::Rb => "ruby", EvalLanguage::Jl => "julia", EvalLanguage::Js => unreachable!() };
            runtimes.push((language, EvalRuntimeInfo { name: name.into(), version: version.clone(), path: Some(resolved_path.as_ref().map_or_else(|| path.clone(), |path| path.to_string_lossy().into_owned())) }));
        }
    }
    runtimes
}
