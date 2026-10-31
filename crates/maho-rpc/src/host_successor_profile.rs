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

/// senpi `startSuccessor`: the successor inherits the predecessor generation's lifetime owner (`owner.json`), or is
/// recorded explicitly unowned when the predecessor had none or its record is unreadable.
///
/// ADAPTATION: senpi also passes the owner's pipe on (`--owner-fd`) when this process IS that owner. Rust's
/// `Command` exposes only the standard slots and this crate denies the `unsafe` a `pre_exec` `dup2` needs, so the
/// successor's supervisor binds the inherited owner by its OS start identity instead.
pub fn inherit_lifetime_owner(
    paths: &crate::host_daemon_paths::HostDaemonPaths,
    predecessor_instance: Option<&str>,
    successor_instance: &str,
) -> std::io::Result<()> {
    use crate::host_lifecycle_activity::{OwnerRecord, read_host_owner, write_host_owner};
    let owner = predecessor_instance
        .map(|instance| {
            read_host_owner(&crate::host_daemon_paths::generation_paths(paths, instance).dir)
        })
        .and_then(|record| match record {
            OwnerRecord::Owned(owner) => Some(owner),
            OwnerRecord::Unowned | OwnerRecord::Unknown => None,
        });
    write_host_owner(
        &crate::host_daemon_paths::generation_paths(paths, successor_instance).dir,
        owner.as_ref(),
    )
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
    fn a_successor_inherits_the_predecessor_owner_or_none() {
        use crate::host_lifecycle_activity::{
            HostLifetimeOwner, OwnerRecord, read_host_owner, write_host_owner,
        };
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("/tmp/s.sock", temp.path());
        for instance in ["old", "new", "next"] {
            std::fs::create_dir_all(
                crate::host_daemon_paths::generation_paths(&paths, instance).dir,
            )
            .unwrap();
        }
        let owner = HostLifetimeOwner {
            pid: 7,
            start_time: "100".into(),
        };
        write_host_owner(
            &crate::host_daemon_paths::generation_paths(&paths, "old").dir,
            Some(&owner),
        )
        .unwrap();
        inherit_lifetime_owner(&paths, Some("old"), "new").unwrap();
        assert_eq!(
            read_host_owner(&crate::host_daemon_paths::generation_paths(&paths, "new").dir),
            OwnerRecord::Owned(owner)
        );
        inherit_lifetime_owner(&paths, None, "next").unwrap();
        assert_eq!(
            read_host_owner(&crate::host_daemon_paths::generation_paths(&paths, "next").dir),
            OwnerRecord::Unowned
        );
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
