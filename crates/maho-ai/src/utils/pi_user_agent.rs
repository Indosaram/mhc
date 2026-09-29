//! Port of senpi packages/ai/src/utils/pi-user-agent.ts.

use crate::types::ProviderHeaders;

fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

fn os_release() -> String {
    sysinfo_release().unwrap_or_default()
}

#[cfg(unix)]
fn sysinfo_release() -> Option<String> {
    let output = std::process::Command::new("uname").arg("-r").output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(not(unix))]
fn sysinfo_release() -> Option<String> {
    None
}

/// `pi (<platform> <release>; <arch>)` with Node's platform and arch names.
pub fn get_pi_user_agent() -> String {
    static AGENT: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| format!("pi ({} {}; {})", node_platform(), os_release(), node_arch()));
    AGENT.clone()
}

pub fn force_pi_user_agent(headers: &mut ProviderHeaders) {
    headers.retain(|name, _| !name.eq_ignore_ascii_case("user-agent"));
    headers.insert("User-Agent".to_owned(), Some(get_pi_user_agent()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forces_a_single_pi_user_agent_header() {
        let mut headers: ProviderHeaders =
            [("user-agent".to_owned(), Some("x".to_owned())), ("Accept".to_owned(), None)].into_iter().collect();
        force_pi_user_agent(&mut headers);
        assert_eq!(headers.len(), 2);
        let agent = headers["User-Agent"].clone().expect("agent");
        assert!(agent.starts_with(&format!("pi ({} ", node_platform())) && agent.ends_with(&format!("; {})", node_arch())));
    }
}
