//! Port of `packages/coding-agent/src/utils/bundled-bun-shim.ts` (omo#9362, omo#10762).

use crate::engine_trust::actions::engine_trust_action;
use crate::engine_trust::marker::EngineTrustMarker;

pub const UNREPRESENTABLE_EXIT: i32 = 78;

fn notice_text(trust: &EngineTrustMarker) -> Option<String> {
    let reason = trust.fallback_reason?;
    let action = engine_trust_action(reason);
    Some(format!(
        "omo notice: certificate import is degraded ({}); some TLS connections may fail. {}",
        reason.as_str(),
        action.text
    ))
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn posix_shim_files(
    exec_path: &str,
    dir: &str,
    trust: Option<&EngineTrustMarker>,
) -> Vec<(String, String)> {
    [("bun", false), ("bunx", true)]
        .into_iter()
        .map(|(name, bunx_arg)| {
            let extra = if bunx_arg { " x" } else { "" };
            let target = format!("exec {}{extra} \"$@\"", quote(exec_path));
            let Some(trust) = trust else {
                return (
                    name.to_owned(),
                    format!("#!/bin/sh\nBUN_BE_BUN=1 {target}\n"),
                );
            };
            let mut lines = vec!["#!/bin/sh".to_owned()];
            if let Some(notice) = notice_text(trust) {
                let sentinel = quote(&format!("{dir}/.fallback-noted"));
                lines.push(format!(
                    "[ -e {sentinel} ] || {{ printf '%s\\n' {} >&2; true 2>/dev/null > {sentinel} || :; }}",
                    quote(&notice)
                ));
            }
            if trust.owner == crate::engine_trust::EngineTrustOwner::Server {
                lines.push("unset NODE_USE_SYSTEM_CA BUN_OPTIONS".to_owned());
            }
            let closure = match &trust.defaults {
                None => String::new(),
                Some(defaults) => format!(
                    " SSL_CERT_FILE={} SSL_CERT_DIR={}",
                    quote(&defaults.file),
                    quote(&defaults.dir)
                ),
            };
            lines.push(format!(
                "BUN_BE_BUN=1 NODE_EXTRA_CA_CERTS={}{} {}",
                quote(&trust.file),
                closure,
                target
            ));
            lines.push(String::new());
            (name.to_owned(), lines.join("\n"))
        })
        .collect()
}

fn cmd_representable(value: &str) -> bool {
    !value.chars().any(|c| c == '"' || c <= '\u{001f}')
}

fn cmd_value(value: &str) -> String {
    value.replace('%', "%%")
}

fn cmd_echo(value: &str) -> String {
    let escaped = cmd_value(value);
    let mut out = String::with_capacity(escaped.len());
    for ch in escaped.chars() {
        if matches!(ch, '^' | '&' | '|' | '<' | '>' | '(' | ')') {
            out.push('^');
        }
        out.push(ch);
    }
    out
}

pub fn cmd_shim_files(
    exec_path: &str,
    dir: &str,
    trust: Option<&EngineTrustMarker>,
) -> Vec<(String, String)> {
    let Some(trust) = trust else {
        return [("bun.cmd", false), ("bunx.cmd", true)]
            .into_iter()
            .map(|(name, bunx_arg)| {
                let extra = if bunx_arg { "x " } else { "" };
                let run = format!("\"{exec_path}\" {extra}%*");
                (
                    name.to_owned(),
                    format!("@echo off\r\nset BUN_BE_BUN=1\r\n{run}\r\nexit /b %ERRORLEVEL%\r\n"),
                )
            })
            .collect();
    };
    let defaults_file = trust.defaults.as_ref().map_or("", |d| d.file.as_str());
    let defaults_dir = trust.defaults.as_ref().map_or("", |d| d.dir.as_str());
    if ![
        exec_path,
        dir,
        trust.file.as_str(),
        defaults_file,
        defaults_dir,
    ]
    .into_iter()
    .all(cmd_representable)
    {
        let action = engine_trust_action(
            crate::engine_trust::EngineTrustActionReason::TrustPathUnrepresentable,
        );
        let failing = format!(
            "@echo off\r\n>&2 echo omo: trust-path-unrepresentable: {}\r\nexit /b {UNREPRESENTABLE_EXIT}\r\n",
            cmd_echo(action.text)
        );
        return vec![
            ("bun.cmd".to_owned(), failing.clone()),
            ("bunx.cmd".to_owned(), failing),
        ];
    }
    [("bun.cmd", false), ("bunx.cmd", true)]
        .into_iter()
        .map(|(name, bunx_arg)| {
            let extra = if bunx_arg { "x " } else { "" };
            let run = format!("\"{}\" {extra}%*", cmd_value(exec_path));
            let mut lines = vec!["@echo off".to_owned(), "setlocal".to_owned()];
            if let Some(notice) = notice_text(trust) {
                let sentinel = format!("\"{}\\.fallback-noted\"", cmd_value(dir));
                lines.push(format!(
                    "if not exist {sentinel} ( >&2 echo {}& type nul > {sentinel} )",
                    cmd_echo(&notice)
                ));
            }
            if trust.owner == crate::engine_trust::EngineTrustOwner::Server {
                lines.push("set \"NODE_USE_SYSTEM_CA=\"".to_owned());
                lines.push("set \"BUN_OPTIONS=\"".to_owned());
            }
            lines.push("set \"BUN_BE_BUN=1\"".to_owned());
            lines.push(format!(
                "set \"NODE_EXTRA_CA_CERTS={}\"",
                cmd_value(&trust.file)
            ));
            if let Some(defaults) = &trust.defaults {
                lines.push(format!(
                    "set \"SSL_CERT_FILE={}\"",
                    cmd_value(&defaults.file)
                ));
                lines.push(format!("set \"SSL_CERT_DIR={}\"", cmd_value(&defaults.dir)));
            }
            lines.push(run);
            lines.push("exit /b %ERRORLEVEL%".to_owned());
            lines.push(String::new());
            (name.to_owned(), lines.join("\r\n"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_trust::{
        EngineTrustAutomatic, EngineTrustDefaults, EngineTrustFallbackReason, EngineTrustOwner,
        EngineTrustRestore,
    };

    #[test]
    fn generates_posix_and_cmd_shims_with_trust_and_fallback_notice() {
        let trust = EngineTrustMarker {
            v: 1,
            file: "/trust/ca.pem".into(),
            sha256: "d".repeat(64),
            automatic: EngineTrustAutomatic::Fallback,
            fallback_reason: Some(EngineTrustFallbackReason::HelperTimeout),
            owner: EngineTrustOwner::Server,
            user: None,
            defaults: Some(EngineTrustDefaults {
                file: "/trust/def.pem".into(),
                dir: "/trust/certs".into(),
            }),
            restore: EngineTrustRestore::default(),
        };
        let posix = posix_shim_files("/usr/local/bin/senpi", "/tmp/shim", Some(&trust));
        assert!(posix[0].1.contains("unset NODE_USE_SYSTEM_CA BUN_OPTIONS"));
        assert!(posix[0].1.contains("SSL_CERT_FILE='/trust/def.pem'"));
        assert!(posix[1].1.contains(" x \"$@\""));

        let cmd = cmd_shim_files("C:\\bun.exe", "C:\\shim\"bad", Some(&trust));
        assert!(
            cmd[0]
                .1
                .contains(&format!("exit /b {UNREPRESENTABLE_EXIT}"))
        );
    }
}
