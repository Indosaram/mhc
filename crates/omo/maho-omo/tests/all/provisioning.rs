//! Port of `omo-senpi/src/extension/toolkit-path-provisioning.test.ts` (5 cases) plus the
//! dag-sdk-root provisioner, whose upstream module has no test file.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use maho_omo::{
    DAG_SDK_ROOT_ENV, ProvisioningOptions, TOOLKIT_BIN_ENV, path_delimiter, provision_dag_sdk_root, provision_toolkit_path,
};

struct Fixture {
    dir: tempfile::TempDir,
    applied: Arc<Mutex<Vec<(String, Option<String>)>>>,
}

impl Default for Fixture {
    fn default() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        Self { dir, applied: Arc::new(Mutex::new(Vec::new())) }
    }
}

impl Fixture {
    fn new() -> Self {
        Self::default()
    }

    fn base_dir(&self) -> PathBuf {
        self.dir.path().join("agent-toolkit")
    }

    fn options(&self, env: &[(&str, Option<&str>)]) -> ProvisioningOptions {
        let map: BTreeMap<String, Option<String>> =
            env.iter().map(|(name, value)| ((*name).to_owned(), value.map(str::to_owned))).collect();
        let sink_log = Arc::clone(&self.applied);
        let env_map = map.clone();
        ProvisioningOptions {
            toolkit_base_dir: Some(self.base_dir()),
            dag_sdk_base_dir: Some(self.dir.path().to_path_buf()),
            env: Some(Arc::new(move |name: &str| env_map.get(name).cloned().flatten())),
            sink: Some(Arc::new(move |name: &str, value: Option<&str>| {
                sink_log.lock().unwrap_or_else(PoisonError::into_inner).push((name.to_owned(), value.map(str::to_owned)));
            })),
        }
    }

    fn applied(&self) -> Vec<(String, Option<String>)> {
        self.applied.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[test]
fn a_bare_path_with_no_preset_bin_prepends_the_dir_and_sets_the_bin() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.base_dir()).expect("toolkit dir");
    let options = fixture.options(&[("PATH", Some("/usr/bin:/bin"))]);

    let updates = provision_toolkit_path(&options);

    let base = fixture.base_dir().to_string_lossy().into_owned();
    assert_eq!(updates, vec![
        ("PATH".to_owned(), Some(format!("{base}{}/usr/bin:/bin", path_delimiter()))),
        (TOOLKIT_BIN_ENV.to_owned(), Some(format!("{base}/cli.js"))),
    ]);
}

#[test]
fn a_preset_bin_is_kept_while_the_dir_is_still_prepended() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.base_dir()).expect("toolkit dir");
    let options = fixture.options(&[
        ("PATH", Some("/usr/bin")),
        (TOOLKIT_BIN_ENV, Some("/preset/omo-agent-toolkit.js")),
    ]);

    let updates = provision_toolkit_path(&options);

    let base = fixture.base_dir().to_string_lossy().into_owned();
    assert_eq!(updates, vec![("PATH".to_owned(), Some(format!("{base}{}/usr/bin", path_delimiter())))]);
    assert!(!updates.iter().any(|(name, _)| name == TOOLKIT_BIN_ENV));
}

#[test]
fn an_empty_preset_bin_is_treated_as_unset() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.base_dir()).expect("toolkit dir");
    let options = fixture.options(&[("PATH", Some("/usr/bin")), (TOOLKIT_BIN_ENV, Some(""))]);

    let updates = provision_toolkit_path(&options);

    let base = fixture.base_dir().to_string_lossy().into_owned();
    assert_eq!(updates, vec![
        ("PATH".to_owned(), Some(format!("{base}{}/usr/bin", path_delimiter()))),
        (TOOLKIT_BIN_ENV.to_owned(), Some(format!("{base}/cli.js"))),
    ]);
}

#[test]
fn running_twice_keeps_exactly_one_leading_toolkit_entry() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.base_dir()).expect("toolkit dir");
    let base = fixture.base_dir().to_string_lossy().into_owned();
    let first = fixture.options(&[("PATH", Some("/usr/bin"))]);
    provision_toolkit_path(&first);
    let prepended = format!("{base}{}/usr/bin", path_delimiter());

    let second = fixture.options(&[("PATH", Some(&prepended))]);
    let updates = provision_toolkit_path(&second);

    assert!(updates.iter().all(|(name, _)| name != "PATH"), "PATH already leads with the toolkit dir");
    let entries: Vec<&str> = prepended.split(path_delimiter()).collect();
    assert_eq!(entries[0], base);
    assert_eq!(entries.iter().filter(|entry| **entry == base).count(), 1);
}

#[test]
fn an_absent_toolkit_dir_leaves_the_environment_untouched() {
    let fixture = Fixture::new();
    let options = fixture.options(&[("PATH", Some("/usr/bin"))]);

    assert!(provision_toolkit_path(&options).is_empty());
    assert!(fixture.applied().is_empty());
}

#[test]
fn dag_sdk_root_is_published_only_for_an_existing_directory() {
    let fixture = Fixture::new();
    let present = fixture.options(&[]);
    assert_eq!(
        provision_dag_sdk_root(&present),
        vec![(DAG_SDK_ROOT_ENV.to_owned(), Some(fixture.dir.path().to_string_lossy().into_owned()))]
    );

    let absent_dir = tempfile::tempdir().expect("tempdir");
    let mut absent = fixture.options(&[]);
    absent.dag_sdk_base_dir = Some(absent_dir.path().join("missing"));
    assert!(provision_dag_sdk_root(&absent).is_empty());
}

#[test]
fn default_options_leave_every_injection_unset() {
    let options = ProvisioningOptions::default();

    assert!(options.toolkit_base_dir.is_none());
    assert!(options.dag_sdk_base_dir.is_none());
    assert!(options.env.is_none());
    assert!(options.sink.is_none());
}
