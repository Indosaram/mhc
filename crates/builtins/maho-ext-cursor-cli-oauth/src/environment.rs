use std::collections::BTreeMap;

pub const CURSOR_AGENT_ENVIRONMENT_PASSTHROUGH: [&str; 5] =
    ["PATH", "TERM", "LANG", "LC_ALL", "FORCE_COLOR"];

/// Build the complete child environment; never inherit authentication or remote-session markers.
pub fn cursor_agent_environment(home: &str, source: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::from([
        ("HOME".into(), home.into()),
        ("AGENT_CLI_CREDENTIAL_STORE".into(), "file".into()),
    ]);
    for name in CURSOR_AGENT_ENVIRONMENT_PASSTHROUGH {
        if let Some(value) = source.get(name) {
            environment.insert(name.into(), value.clone());
        }
    }
    environment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_allowlist() {
        assert_eq!(CURSOR_AGENT_ENVIRONMENT_PASSTHROUGH, ["PATH", "TERM", "LANG", "LC_ALL", "FORCE_COLOR"]);
    }

    #[test]
    fn pins_home_and_store_and_excludes_parent_secrets() {
        let source = BTreeMap::from([
            ("PATH".into(), "/usr/bin".into()), ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "en_US.UTF-8".into()), ("FORCE_COLOR".into(), "1".into()),
            ("HOME".into(), "/host".into()), ("AGENT_CLI_CREDENTIAL_STORE".into(), "keychain".into()),
            ("SSH_CONNECTION".into(), "remote".into()), ("SSH_CLIENT".into(), "remote".into()),
            ("SSH_TTY".into(), "/dev/tty".into()), ("MOSH_SERVER".into(), "1".into()),
            ("VSCODE_SSH_HOST".into(), "remote".into()), ("CURSOR_AGENT_CLI_ASSUME_SSH".into(), "1".into()),
            ("SENPI_TRANSPORT_SECRET".into(), "must-not-leak".into()),
        ]);
        assert_eq!(cursor_agent_environment("/accounts/default/home", &source), BTreeMap::from([
            ("HOME".into(), "/accounts/default/home".into()), ("AGENT_CLI_CREDENTIAL_STORE".into(), "file".into()),
            ("PATH".into(), "/usr/bin".into()), ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "en_US.UTF-8".into()), ("FORCE_COLOR".into(), "1".into()),
        ]));
    }

    #[test]
    fn absent_variables_are_omitted() {
        let env = cursor_agent_environment("/home", &BTreeMap::from([("PATH".into(), "/bin".into())]));
        assert_eq!(env.keys().map(String::as_str).collect::<Vec<_>>(), ["AGENT_CLI_CREDENTIAL_STORE", "HOME", "PATH"]);
    }
}
