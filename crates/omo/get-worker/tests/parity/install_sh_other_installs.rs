//! Port of `test/install-sh-other-installs.test.ts`: the installer's other-install
//! handling, driven as a real subprocess with a fake `bun` and a fake old `omo`.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    work: PathBuf,
    new_omo: PathBuf,
    old_omo: PathBuf,
    package_dir: PathBuf,
    unrelated: PathBuf,
    path: String,
}

fn write_file(path: &Path, content: &str, executable: bool) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory");
    }
    fs::write(path, content).expect("write file");
    if executable {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

fn relative(from_dir: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from_dir.components().collect();
    let target: Vec<_> = to.components().collect();
    let common = from
        .iter()
        .zip(target.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for component in &target[common..] {
        out.push(component);
    }
    out
}

fn fixture(failing_removal: bool) -> Fixture {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = fs::canonicalize(temp.path()).expect("canonical root");
    let home = root.join("home");
    let work = root.join("work");
    let bun_root = root.join("bun");
    let bun_bin = bun_root.join("bin");
    let package_dir = bun_root.join("install/global/node_modules/omo-ai");
    let entry = package_dir.join("bin/omo");

    write_file(
        &package_dir.join("package.json"),
        &serde_json::json!({ "name": "omo-ai", "version": "4.0.0", "bin": { "omo": "bin/omo" } })
            .to_string(),
        false,
    );
    write_file(&entry, "#!/bin/sh\necho 'omo 4.0.0'\n", true);
    fs::create_dir_all(&bun_bin).expect("bun bin");
    std::os::unix::fs::symlink(relative(&bun_bin, &entry), bun_bin.join("omo"))
        .expect("symlink the old shim");

    let new_omo = root.join("new-bin/omo");
    write_file(&new_omo, "#!/bin/sh\necho 'omo 5.0.0'\n", true);

    let tools = root.join("tools");
    let bun_body = if failing_removal {
        "#!/bin/sh\nexit 1\n"
    } else {
        "#!/bin/sh\nrm -rf \"$BUN_INSTALL/install/global/node_modules/omo-ai\"\nrm -f \"$BUN_INSTALL/bin/omo\"\n"
    };
    write_file(&tools.join("bun"), bun_body, true);

    let unrelated = package_dir.join("../unrelated-package/keep.txt");
    write_file(&unrelated, "keep\n", false);
    fs::create_dir_all(&work).expect("work dir");

    let path = format!(
        "{}:{}:{}:/usr/bin:/bin",
        root.join("new-bin").display(),
        bun_bin.display(),
        tools.display()
    );
    Fixture {
        _root: temp,
        root,
        home,
        work,
        new_omo,
        old_omo: bun_bin.join("omo"),
        package_dir,
        unrelated,
        path,
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn installer_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/install.sh")
}

fn report(fixture: &Fixture, body: &str, input: Option<&str>) -> Output {
    let script = format!("source {}\n{}", shell_quote(&installer_path()), body);
    let mut child = Command::new("/bin/bash")
        .arg("-c")
        .arg(script)
        .env("HOME", &fixture.home)
        .env("PATH", &fixture.path)
        .env("OMO_INSTALL_SOURCE_ONLY", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bash");
    if let Some(input) = input {
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
    }
    drop(child.stdin.take());
    child.wait_with_output().expect("wait for bash")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn accepting_the_interactive_prompt_removes_the_bun_global_install_and_leaves_one_omo_on_path() {
    let f = fixture(false);
    let body = format!(
        "is_interactive() {{ return 0; }}\nreport_other_installs {} 0 {}\ntype -ap omo",
        shell_quote(&f.new_omo),
        shell_quote(&f.work)
    );
    let output = report(&f, &body, Some("yes\n"));

    assert_eq!(output.status.code(), Some(0));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains(&format!(
            "Remove the other omo install at {}? [y/N]",
            f.old_omo.display()
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("Removed the other omo install at {}.", f.old_omo.display())),
        "{stderr}"
    );
    let stdout = stdout_of(&output);
    let expected = f.new_omo.display().to_string();
    let lines: Vec<&str> = stdout.trim().split('\n').collect();
    assert_eq!(lines, vec![expected.as_str()]);
    assert!(!f.package_dir.exists());
    assert_eq!(fs::read_to_string(&f.unrelated).expect("unrelated"), "keep\n");
}

#[test]
fn a_non_interactive_run_never_deletes_without_the_explicit_flag() {
    let f = fixture(false);
    let body = format!(
        "is_interactive() {{ return 1; }}\nreport_other_installs {} 0 {}",
        shell_quote(&f.new_omo),
        shell_quote(&f.work)
    );
    let output = report(&f, &body, None);

    assert_eq!(output.status.code(), Some(0));
    assert!(
        stderr_of(&output).contains(
            "Nothing was removed in this non-interactive run. Re-run with --remove-other-installs"
        ),
        "{}",
        stderr_of(&output)
    );
    let manifest = fs::read_to_string(f.package_dir.join("package.json")).expect("package.json");
    assert!(manifest.contains("\"name\":\"omo-ai\""), "{manifest}");
    assert_eq!(fs::read_to_string(&f.unrelated).expect("unrelated"), "keep\n");
}

#[test]
fn the_explicit_flag_removes_only_the_verified_omo_package_and_shim() {
    let f = fixture(false);
    let lookalike = f.old_omo.parent().expect("parent").join("omo-helper");
    write_file(&lookalike, "unrelated\n", false);
    let body = format!(
        "report_other_installs {} 1 {}",
        shell_quote(&f.new_omo),
        shell_quote(&f.work)
    );
    let output = report(&f, &body, None);

    assert_eq!(output.status.code(), Some(0));
    assert!(fs::symlink_metadata(&f.old_omo).is_err());
    assert!(!f.package_dir.join("package.json").exists());
    assert_eq!(fs::read_to_string(&lookalike).expect("lookalike"), "unrelated\n");
    assert_eq!(fs::read_to_string(&f.unrelated).expect("unrelated"), "keep\n");
}

#[test]
fn declining_the_prompt_changes_nothing_on_disk() {
    let f = fixture(false);
    let before = fs::read_to_string(f.package_dir.join("package.json")).expect("package.json");
    let body = format!(
        "is_interactive() {{ return 0; }}\nreport_other_installs {} 0 {}",
        shell_quote(&f.new_omo),
        shell_quote(&f.work)
    );
    let output = report(&f, &body, Some("no\n"));

    assert_eq!(output.status.code(), Some(0));
    assert!(
        stderr_of(&output).contains("Kept it. Remove it later with:"),
        "{}",
        stderr_of(&output)
    );
    let after = fs::read_to_string(f.package_dir.join("package.json")).expect("package.json");
    assert_eq!(after, before);
    assert_eq!(fs::read_to_string(&f.unrelated).expect("unrelated"), "keep\n");
}

#[test]
fn a_failed_removal_leaves_the_new_install_working_and_prints_the_exact_command() {
    let f = fixture(true);
    let body = format!(
        "report_other_installs {} 1 {}\n{} --version",
        shell_quote(&f.new_omo),
        shell_quote(&f.work),
        shell_quote(&f.new_omo)
    );
    let output = report(&f, &body, None);

    assert_eq!(output.status.code(), Some(0));
    let stdout = stdout_of(&output);
    let stderr = stderr_of(&output);
    assert!(stdout.contains("omo 5.0.0"), "{stdout}");
    assert!(stderr.contains("the new install still works"), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "BUN_INSTALL={} bun remove -g omo-ai",
            f.root.join("bun").display()
        )),
        "{stderr}"
    );
    let manifest = fs::read_to_string(f.package_dir.join("package.json")).expect("package.json");
    assert!(manifest.contains("\"name\":\"omo-ai\""), "{manifest}");
}

#[test]
fn an_unverified_look_alike_omo_is_never_removed() {
    let f = fixture(false);
    fs::remove_file(&f.old_omo).expect("remove the bun shim");
    let foreign = f.old_omo.parent().expect("parent").join("omo");
    write_file(&foreign, "#!/bin/sh\necho look-alike\n", true);
    let body = format!(
        "report_other_installs {} 1 {}",
        shell_quote(&f.new_omo),
        shell_quote(&f.work)
    );
    let output = report(&f, &body, None);

    assert_eq!(output.status.code(), Some(0));
    assert!(
        stderr_of(&output).contains("could not be verified, so nothing was removed"),
        "{}",
        stderr_of(&output)
    );
    assert!(fs::read_to_string(&foreign).expect("foreign").contains("look-alike"));
    assert_eq!(fs::read_to_string(&f.unrelated).expect("unrelated"), "keep\n");
}

#[test]
fn a_prior_standalone_receipt_permits_removing_only_that_old_launcher() {
    let f = fixture(false);
    fs::remove_file(&f.old_omo).expect("remove the bun shim");
    let old_standalone = f.old_omo.parent().expect("parent").join("omo");
    write_file(&old_standalone, "#!/bin/sh\necho 'omo 3.0.0'\n", true);
    write_file(
        &f.home.join(".omo/install.json"),
        &serde_json::json!({
            "method": "standalone",
            "binPath": old_standalone.display().to_string(),
        })
        .to_string(),
        false,
    );
    let neighbor = old_standalone.parent().expect("parent").join("keep");
    write_file(&neighbor, "keep\n", false);
    let body = format!(
        "report_other_installs {} 1 {}",
        shell_quote(&f.new_omo),
        shell_quote(&f.work)
    );
    let output = report(&f, &body, None);

    assert_eq!(output.status.code(), Some(0));
    assert!(fs::symlink_metadata(&old_standalone).is_err());
    assert_eq!(fs::read_to_string(&neighbor).expect("neighbor"), "keep\n");
}
