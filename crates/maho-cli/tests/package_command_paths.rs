//! senpi package-command-paths.test.ts cases that drive `main([...])`, through the real binary.
use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("isolated root");
        for dir in ["agent", "project", "local-package", ".omo"] {
            std::fs::create_dir_all(root.path().join(dir)).expect("fixture dir");
        }
        std::fs::write(root.path().join(".omo/onboarding-completed"), "{}\n")
            .expect("onboarding done");
        Self { root }
    }
    fn agent(&self) -> std::path::PathBuf {
        self.root.path().join("agent")
    }
    fn project(&self) -> std::path::PathBuf {
        self.root.path().join("project")
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_mhc"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env(maho_core::config::env_agent_dir_var(), self.agent())
            .current_dir(self.project())
            .args(args)
            .output()
            .expect("real mhc")
    }
    fn global_packages(&self) -> Vec<String> {
        let text = std::fs::read_to_string(self.agent().join("settings.json"))
            .unwrap_or_else(|_| "{}".to_owned());
        let settings: serde_json::Value = serde_json::from_str(&text).expect("settings JSON");
        settings["packages"]
            .as_array()
            .map(|packages| {
                packages
                    .iter()
                    .filter_map(|package| package.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn project_settings(&self, value: serde_json::Value) {
        let dir = self.project().join(maho_core::config::config_dir_name());
        std::fs::create_dir_all(&dir).expect("project config dir");
        std::fs::write(dir.join("settings.json"), value.to_string()).expect("project settings");
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn global_relative_local_package_paths_persist_relative_to_settings_json() {
    let fixture = Fixture::new();
    let relative = fixture.project().join("packages/local-package");
    std::fs::create_dir_all(&relative).unwrap();
    let output = fixture.run(&["install", "./packages/local-package"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let packages = fixture.global_packages();
    assert_eq!(packages.len(), 1);
    assert_eq!(
        std::fs::canonicalize(maho_core::paths::lexical_resolve(
            &fixture.agent().join(&packages[0]).to_string_lossy()
        ))
        .unwrap(),
        std::fs::canonicalize(&relative).unwrap()
    );
}

#[test]
fn local_packages_are_removed_through_a_trailing_slash_path() {
    let fixture = Fixture::new();
    let source = format!("{}/", fixture.root.path().join("local-package").display());
    assert!(fixture.run(&["install", &source]).status.success());
    assert_eq!(fixture.global_packages().len(), 1);
    let removed = fixture.run(&["remove", &source]);
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    assert!(fixture.global_packages().is_empty());
}

#[test]
fn list_skips_untrusted_project_packages_and_honours_remembered_trust() {
    let fixture = Fixture::new();
    fixture.project_settings(serde_json::json!({"packages": ["npm:@project/pkg"]}));
    let untrusted = fixture.run(&["list"]);
    let stdout = text(&untrusted.stdout);
    assert!(stdout.contains("No packages installed."), "{stdout}");
    assert!(!stdout.contains("Project packages:"), "{stdout}");
    maho_core::trust_manager::ProjectTrustStore::new(&fixture.agent().to_string_lossy())
        .set(&fixture.project().to_string_lossy(), Some(true))
        .unwrap();
    let trusted = fixture.run(&["list"]);
    let stdout = text(&trusted.stdout);
    assert!(trusted.status.success(), "{}", text(&trusted.stderr));
    assert!(
        stdout.contains("Project packages:") && stdout.contains("npm:@project/pkg"),
        "{stdout}"
    );
    let overridden = fixture.run(&["list", "--no-approve"]);
    assert!(text(&overridden.stdout).contains("No packages installed."));
}

#[test]
fn install_help_unknown_options_and_missing_sources_are_friendly() {
    let fixture = Fixture::new();
    let app = "mhc";
    let help = fixture.run(&["install", "--help"]);
    assert!(help.status.success());
    assert!(text(&help.stdout).contains("Usage:"));
    assert!(text(&help.stdout).contains(&format!("{app} install <source> [-l]")));
    assert!(help.stderr.is_empty(), "{}", text(&help.stderr));
    let unknown = fixture.run(&["install", "--unknown"]);
    assert_eq!(unknown.status.code(), Some(1));
    let stderr = text(&unknown.stderr);
    assert!(
        stderr.contains("Unknown option --unknown for \"install\"."),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "Use \"{app} --help\" or \"{app} install <source> [-l] [--approve|--no-approve]\"."
        )),
        "{stderr}"
    );
    let missing = fixture.run(&["install"]);
    assert_eq!(missing.status.code(), Some(1));
    let stderr = text(&missing.stderr);
    assert!(stderr.contains("Missing install source."), "{stderr}");
    assert!(
        stderr.contains(&format!("Usage: {app} install <source> [-l]")),
        "{stderr}"
    );
}

#[test]
fn update_models_refuses_another_target() {
    let fixture = Fixture::new();
    let output = fixture.run(&["update", "--models", "--self"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("--models cannot be combined with --self"));
}
