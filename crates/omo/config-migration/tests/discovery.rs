use std::collections::{BTreeMap, BTreeSet};

use config_migration::{
    CONFIG_JSONC_MIGRATION_ID, ConfigMigrationDiscoveryFileSystem, ConfigMigrationDiscoveryOptions,
    DiscoveredLegacyConfigSource, DiscoveryFsError, LegacyConfigMigrationGroup,
    LegacyConfigSourceKind, OPENCODE_CONFIG_MIGRATION_ID, PathOperations, Platform,
    discover_legacy_config_groups,
};
use pretty_assertions::assert_eq;

const WIN: PathOperations = PathOperations::Win32;
const POSIX: PathOperations = PathOperations::Posix;

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

fn discover(options: &ConfigMigrationDiscoveryOptions<'_>) -> Vec<LegacyConfigMigrationGroup> {
    discover_legacy_config_groups(options).expect("discovery succeeds")
}

fn paths(group: &LegacyConfigMigrationGroup) -> Vec<&str> {
    group
        .sources
        .iter()
        .map(|source| source.path.as_str())
        .collect()
}

fn profile<'a>(
    group: &'a LegacyConfigMigrationGroup,
    name: &str,
) -> &'a DiscoveredLegacyConfigSource {
    group
        .sources
        .iter()
        .find(|source| source.profile.as_deref() == Some(name))
        .expect("profile source")
}

fn children(files: &BTreeSet<String>, prefix: &str) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    for path in files {
        if let Some(rest) = path.strip_prefix(prefix) {
            let head = rest.split('\\').next().unwrap_or_default().to_string();
            if !entries.contains(&head) {
                entries.push(head);
            }
        }
    }
    entries
}

struct WindowsMemoryFileSystem {
    files: BTreeSet<String>,
}

impl WindowsMemoryFileSystem {
    fn new(paths: &[&str]) -> Self {
        WindowsMemoryFileSystem {
            files: paths
                .iter()
                .map(|path| WIN.normalize(path).to_lowercase())
                .collect(),
        }
    }
}

impl ConfigMigrationDiscoveryFileSystem for WindowsMemoryFileSystem {
    fn exists(&self, path: &str) -> bool {
        self.files.contains(&WIN.normalize(path).to_lowercase())
    }

    fn read_dir(&self, path: &str) -> Result<Vec<String>, DiscoveryFsError> {
        let normalized = WIN.normalize(path);
        let trimmed = normalized.trim_end_matches(['\\', '/']).to_lowercase();
        Ok(children(&self.files, &format!("{trimmed}\\")))
    }

    fn realpath(&self, path: &str) -> Result<String, DiscoveryFsError> {
        Ok(WIN.normalize(path))
    }
}

struct ShortPathMemoryFileSystem {
    files: BTreeSet<String>,
    long_home: String,
    short_home: String,
}

impl ShortPathMemoryFileSystem {
    fn new(short_home: &str, long_home: &str, paths: &[&str]) -> Self {
        let mut file_system = ShortPathMemoryFileSystem {
            files: BTreeSet::new(),
            long_home: WIN.normalize(long_home),
            short_home: WIN.normalize(short_home),
        };
        file_system.files = paths.iter().map(|path| file_system.key(path)).collect();
        file_system
    }

    fn expand(&self, path: &str) -> String {
        WIN.normalize(path)
            .replacen(&self.short_home, &self.long_home, 1)
    }

    fn key(&self, path: &str) -> String {
        self.expand(path).to_lowercase()
    }
}

impl ConfigMigrationDiscoveryFileSystem for ShortPathMemoryFileSystem {
    fn exists(&self, path: &str) -> bool {
        let key = self.key(path);
        self.files.contains(&key)
            || self
                .files
                .iter()
                .any(|file| file.starts_with(&format!("{key}\\")))
    }

    fn read_dir(&self, path: &str) -> Result<Vec<String>, DiscoveryFsError> {
        let key = self.key(path);
        let trimmed = key.trim_end_matches(['\\', '/']);
        Ok(children(&self.files, &format!("{trimmed}\\")))
    }

    fn realpath(&self, path: &str) -> Result<String, DiscoveryFsError> {
        if !self.exists(path) {
            return Err(DiscoveryFsError::not_found(path));
        }
        Ok(self.expand(path))
    }
}

struct PosixMemoryFileSystem {
    directories: BTreeMap<String, Vec<String>>,
    files: BTreeSet<String>,
    strip_trailing_slash: bool,
}

impl ConfigMigrationDiscoveryFileSystem for PosixMemoryFileSystem {
    fn exists(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    fn read_dir(&self, path: &str) -> Result<Vec<String>, DiscoveryFsError> {
        Ok(self.directories.get(path).cloned().unwrap_or_default())
    }

    fn realpath(&self, path: &str) -> Result<String, DiscoveryFsError> {
        if self.strip_trailing_slash {
            return Ok(path.strip_suffix('/').unwrap_or(path).to_string());
        }
        Ok(path.to_string())
    }
}

fn posix_file_system(
    files: &[&str],
    directories: &[(&str, &[&str])],
    strip_trailing_slash: bool,
) -> PosixMemoryFileSystem {
    PosixMemoryFileSystem {
        directories: directories
            .iter()
            .map(|(path, entries)| {
                (
                    (*path).to_string(),
                    entries.iter().map(|entry| (*entry).to_string()).collect(),
                )
            })
            .collect(),
        files: files.iter().map(|path| (*path).to_string()).collect(),
        strip_trailing_slash,
    }
}

#[test]
fn custom_active_profile_default_tauri_and_walked_project_roots_are_deduplicated_and_separated_from_config_jsonc()
 {
    // given
    let home_dir = "/home/alice";
    let file_system = posix_file_system(
        &[
            "/home/alice/.config/opencode/oh-my-openagent.jsonc",
            "/home/alice/.config/opencode/profiles/kimi/oh-my-openagent.jsonc",
            "/home/alice/.config/opencode/profiles/quiet/oh-my-opencode.json",
            "/home/alice/.config/ai.opencode.desktop/oh-my-openagent.json",
            "/work/repo/.opencode/oh-my-openagent.jsonc",
            "/home/alice/.maho/config.jsonc",
        ],
        &[(
            "/home/alice/.config/opencode/profiles",
            &["kimi", "quiet", "__test"],
        )],
        true,
    );
    let environment = env(&[
        ("HOME", home_dir),
        (
            "OPENCODE_CONFIG_DIR",
            "/home/alice/.config/opencode/profiles/kimi/",
        ),
        ("XDG_CONFIG_HOME", "/home/alice/.config"),
    ]);

    // when
    let groups = discover(&ConfigMigrationDiscoveryOptions {
        cwd: "/work/repo/packages/app",
        environment: &environment,
        file_system: Some(&file_system),
        home_dir,
        path_operations: POSIX,
        platform: Some(Platform::Linux),
        tauri_config_dirs: None,
    });

    // then
    let ids: Vec<&str> = groups.iter().map(|group| group.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![OPENCODE_CONFIG_MIGRATION_ID, CONFIG_JSONC_MIGRATION_ID]
    );
    let opencode_group = &groups[0];
    assert_eq!(
        paths(opencode_group),
        vec![
            "/home/alice/.config/opencode/oh-my-openagent.jsonc",
            "/home/alice/.config/opencode/profiles/kimi/oh-my-openagent.jsonc",
            "/home/alice/.config/opencode/profiles/quiet/oh-my-opencode.json",
            "/home/alice/.config/ai.opencode.desktop/oh-my-openagent.json",
            "/work/repo/.opencode/oh-my-openagent.jsonc",
        ]
    );
    let kimi = profile(opencode_group, "kimi");
    assert_eq!(
        kimi.base_root.as_deref(),
        Some("/home/alice/.config/opencode")
    );
    assert!(kimi.is_active_profile);
    assert_eq!(kimi.kind, LegacyConfigSourceKind::ProfileConfig);
    let kimi_count = opencode_group
        .sources
        .iter()
        .filter(|source| source.path.contains("/profiles/kimi/"))
        .count();
    assert_eq!(kimi_count, 1);
    assert!(
        !opencode_group
            .sources
            .iter()
            .any(|source| source.path.contains("__test"))
    );
    let config_jsonc: Vec<(LegacyConfigSourceKind, &str)> = groups[1]
        .sources
        .iter()
        .map(|source| (source.kind, source.path.as_str()))
        .collect();
    assert_eq!(
        config_jsonc,
        vec![(
            LegacyConfigSourceKind::ConfigJsonc,
            "/home/alice/.maho/config.jsonc"
        )]
    );
}

#[test]
fn injected_windows_short_path_expands_through_realpath_to_the_canonical_source() {
    // given
    let short_home = "C:\\Users\\RUNNER~1\\AppData\\Local\\Temp\\omo-config-migrate-A1B2\\home";
    let long_home = "C:\\Users\\runner\\AppData\\Local\\Temp\\omo-config-migrate-A1B2\\home";
    let source_path = WIN.join(&[short_home, ".config", "opencode", "oh-my-openagent.json"]);
    let file_system = ShortPathMemoryFileSystem::new(short_home, long_home, &[&source_path]);
    let config_dir = WIN.join(&[short_home, ".config", "opencode"]);
    let environment = env(&[("HOME", short_home), ("OPENCODE_CONFIG_DIR", &config_dir)]);
    let cwd = WIN.join(&[short_home, "project"]);

    // when
    let groups = discover(&ConfigMigrationDiscoveryOptions {
        cwd: &cwd,
        environment: &environment,
        file_system: Some(&file_system),
        home_dir: short_home,
        path_operations: WIN,
        platform: Some(Platform::Linux),
        tauri_config_dirs: None,
    });

    // then
    let expected = WIN.join(&[long_home, ".config", "opencode", "oh-my-openagent.json"]);
    let found: Vec<(&str, &str)> = groups[0]
        .sources
        .iter()
        .map(|source| (source.config_path.as_str(), source.path.as_str()))
        .collect();
    assert_eq!(found, vec![(expected.as_str(), expected.as_str())]);
}

#[test]
fn win32_trailing_separators_and_casing_keep_the_active_profile_canonical() {
    // given
    let file_system = WindowsMemoryFileSystem::new(&[
        "C:\\Users\\Alice\\AppData\\Roaming\\opencode\\oh-my-openagent.jsonc",
        "C:\\Users\\Alice\\AppData\\Roaming\\opencode\\profiles\\kimi\\oh-my-openagent.jsonc",
    ]);
    let environment = env(&[
        ("APPDATA", "c:\\users\\alice\\AppData\\Roaming\\"),
        ("HOME", "C:\\Users\\Alice"),
        (
            "OPENCODE_CONFIG_DIR",
            "C:\\USERS\\ALICE\\APPDATA\\ROAMING\\OPENCODE\\profiles\\Kimi\\",
        ),
    ]);

    // when
    let groups = discover(&ConfigMigrationDiscoveryOptions {
        cwd: "C:\\Users\\Alice\\work\\repo",
        environment: &environment,
        file_system: Some(&file_system),
        home_dir: "C:\\Users\\Alice",
        path_operations: WIN,
        platform: Some(Platform::Win32),
        tauri_config_dirs: None,
    });

    // then
    assert_eq!(groups[0].sources.len(), 2);
    let kimi = profile(&groups[0], "kimi");
    assert_eq!(
        (kimi.base_root.as_deref(), kimi.is_active_profile),
        (Some("C:\\USERS\\ALICE\\APPDATA\\ROAMING\\OPENCODE"), true)
    );
}

#[test]
fn wsl_windows_xdg_path_keeps_the_mounted_active_profile_root() {
    // given
    let file_system = posix_file_system(
        &["/mnt/c/Users/Alice/.config/opencode/profiles/kimi/oh-my-openagent.jsonc"],
        &[("/mnt/c/Users/Alice/.config/opencode/profiles", &["kimi"])],
        true,
    );
    let environment = env(&[
        ("HOME", "/home/alice"),
        (
            "OPENCODE_CONFIG_DIR",
            "/mnt/c/Users/Alice/.config/opencode/profiles/kimi/",
        ),
        ("WSL_DISTRO_NAME", "Ubuntu"),
        ("XDG_CONFIG_HOME", "/mnt/c/Users/Alice/.config"),
    ]);

    // when
    let groups = discover(&ConfigMigrationDiscoveryOptions {
        cwd: "/home/alice/work",
        environment: &environment,
        file_system: Some(&file_system),
        home_dir: "/home/alice",
        path_operations: POSIX,
        platform: Some(Platform::Linux),
        tauri_config_dirs: None,
    });

    // then
    let kimi = profile(&groups[0], "kimi");
    assert_eq!(
        (kimi.base_root.as_deref(), kimi.is_active_profile),
        (Some("/mnt/c/Users/Alice/.config/opencode"), true)
    );
}

#[cfg(unix)]
#[test]
fn symlinked_profile_directory_is_discovered_through_its_real_path() {
    // given
    let fixture = tempfile::tempdir().expect("tempdir");
    let fixture_root = fixture.path().to_string_lossy().into_owned();
    let profile_target = fixture.path().join("profile-target");
    let profiles_dir = fixture.path().join("opencode").join("profiles");
    std::fs::create_dir_all(&profile_target).expect("mkdir target");
    std::fs::create_dir_all(&profiles_dir).expect("mkdir profiles");
    std::fs::write(profile_target.join("oh-my-openagent.jsonc"), "{}").expect("write");
    std::os::unix::fs::symlink(&profile_target, profiles_dir.join("linked")).expect("symlink");
    let environment = env(&[("HOME", &fixture_root), ("XDG_CONFIG_HOME", &fixture_root)]);

    // when
    let groups = discover(&ConfigMigrationDiscoveryOptions {
        cwd: &fixture_root,
        environment: &environment,
        file_system: None,
        home_dir: &fixture_root,
        path_operations: POSIX,
        platform: Some(Platform::Linux),
        tauri_config_dirs: None,
    });

    // then
    let real = std::fs::canonicalize(profile_target.join("oh-my-openagent.jsonc"))
        .expect("realpath")
        .to_string_lossy()
        .into_owned();
    let found: Vec<(LegacyConfigSourceKind, &str, Option<&str>)> = groups[0]
        .sources
        .iter()
        .map(|source| (source.kind, source.path.as_str(), source.profile.as_deref()))
        .collect();
    assert_eq!(
        found,
        vec![(
            LegacyConfigSourceKind::ProfileConfig,
            real.as_str(),
            Some("linked")
        )]
    );
}

#[test]
fn posix_path_operations_on_a_windows_host_discover_xdg_appdata_and_tauri_roots_once() {
    // given
    let home_dir = "C:\\Users\\Alice";
    let xdg_root = POSIX.join(&[home_dir, ".config", "opencode"]);
    let app_data = "C:\\Users\\Alice\\AppData\\Roaming";
    let app_data_root = WIN.join(&[app_data, "opencode"]);
    let tauri_root = WIN.join(&[app_data, "ai.opencode.desktop"]);
    let tauri_dev_root = WIN.join(&[app_data, "ai.opencode.desktop.dev"]);
    let file_system = WindowsMemoryFileSystem::new(&[
        &POSIX.join(&[&xdg_root, "oh-my-openagent.jsonc"]),
        &WIN.join(&[&app_data_root, "oh-my-openagent.jsonc"]),
        &WIN.join(&[&tauri_root, "oh-my-openagent.jsonc"]),
        &WIN.join(&[&tauri_dev_root, "oh-my-opencode.json"]),
    ]);
    let xdg_home = POSIX.join(&[home_dir, ".config"]);
    let environment = env(&[
        ("APPDATA", app_data),
        ("OPENCODE_CONFIG_DIR", &xdg_root),
        ("XDG_CONFIG_HOME", &xdg_home),
    ]);

    // when
    let groups = discover(&ConfigMigrationDiscoveryOptions {
        cwd: "C:\\Users\\Alice\\project",
        environment: &environment,
        file_system: Some(&file_system),
        home_dir,
        path_operations: POSIX,
        platform: Some(Platform::Win32),
        tauri_config_dirs: None,
    });

    // then
    assert_eq!(
        paths(&groups[0]),
        vec![
            WIN.join(&[&xdg_root, "oh-my-openagent.jsonc"]),
            WIN.join(&[&app_data_root, "oh-my-openagent.jsonc"]),
            WIN.join(&[&tauri_root, "oh-my-openagent.jsonc"]),
            WIN.join(&[&tauri_dev_root, "oh-my-opencode.json"]),
        ]
    );
}

#[test]
fn non_windows_host_keeps_default_xdg_and_tauri_source_paths_unchanged() {
    // given
    let config_home = "/custom/config";
    let file_system = posix_file_system(
        &[
            "/custom/config/opencode/oh-my-openagent.jsonc",
            "/custom/config/ai.opencode.desktop/oh-my-opencode.json",
        ],
        &[],
        false,
    );
    let environment = env(&[("HOME", "/home/alice"), ("XDG_CONFIG_HOME", config_home)]);

    // when
    let groups = discover(&ConfigMigrationDiscoveryOptions {
        cwd: "/home/alice/project",
        environment: &environment,
        file_system: Some(&file_system),
        home_dir: "/home/alice",
        path_operations: POSIX,
        platform: Some(Platform::Linux),
        tauri_config_dirs: None,
    });

    // then
    assert_eq!(
        paths(&groups[0]),
        vec![
            "/custom/config/opencode/oh-my-openagent.jsonc",
            "/custom/config/ai.opencode.desktop/oh-my-opencode.json",
        ]
    );
}
