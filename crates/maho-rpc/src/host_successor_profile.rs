//! The launch profile a handoff's successor records (senpi `startSuccessor`'s `launchProfileId`).

/// senpi `startSuccessor`: the successor's record names the launch profile of the host argv it runs
/// (`hostLaunchProfile(["--mode","rpc","--multi-session",...hostArgs], cwd)`), so an idle handover - whose caller
/// states none - records the same profile id an ensure would. A stated profile is kept.
pub fn successor_launch_profile_id(options: &crate::host_handoff::HandoffOptions) -> String {
    if !options.launch_profile_id.is_empty() {
        return options.launch_profile_id.clone();
    }
    let cwd = std::env::current_dir()
        .map(|cwd| cwd.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::protocol_identity::host_launch_profile(
        &crate::host_ensure::host_child_argv(&options.host_args),
        &cwd,
    )
    .map(|profile| profile.profile_id)
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(
        host_args: Vec<String>,
        launch_profile_id: &str,
    ) -> crate::host_handoff::HandoffOptions {
        crate::host_handoff::HandoffOptions {
            host_args,
            policy: None,
            env: Default::default(),
            expected_runtime_build_id: None,
            launch_profile_id: launch_profile_id.into(),
            readiness_ms: 1,
            launch: None,
        }
    }

    #[test]
    fn an_unstated_successor_profile_is_derived_from_its_host_argv() {
        let host_args = vec!["--socket".to_owned(), "/s".to_owned()];
        let derived = successor_launch_profile_id(&options(host_args.clone(), ""));
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let expected = crate::protocol_identity::host_launch_profile(
            &crate::host_ensure::host_child_argv(&host_args),
            &cwd,
        )
        .unwrap()
        .profile_id;
        assert!(!derived.is_empty());
        assert_eq!(derived, expected);
        assert_eq!(
            successor_launch_profile_id(&options(vec![], "stated")),
            "stated"
        );
    }
}
