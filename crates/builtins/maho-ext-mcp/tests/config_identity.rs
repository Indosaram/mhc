//! Ports of senpi test/mcp/3858-reference-identities.test.ts and 3858-reference-migration.test.ts
//! (adoption half; the held-list service fixture is exercised through `adopt_mcp_cached_server`).
use hmac::{Hmac, Mac};
use maho_ext_mcp::{
    auth::catalog_identity::{legacy_mcp_credential_identity, mcp_credential_identity},
    catalog_cache::{McpCachedServerCatalog, McpCatalogCacheFile, read_mcp_catalog_cache},
    catalog_cache_adoption::{
        McpCacheAdoption, McpCacheIdentity, adopt_mcp_cached_server, legacy_mcp_cache_identity,
    },
    config::{hash_config, legacy_hash_config},
    config_identity_key::is_ephemeral_identity,
    config_schema::{ServerConfigWire, Transport},
};
use serde_json::json;
use sha2::Sha256;
use std::{collections::BTreeMap, path::Path};

fn command(command: &str, args: &[&str]) -> ServerConfigWire {
    ServerConfigWire {
        command: Some(command.into()),
        args: Some(args.iter().map(|arg| (*arg).to_owned()).collect()),
        ..Default::default()
    }
}

fn bearer(url: &str) -> ServerConfigWire {
    ServerConfigWire {
        transport: Some(Transport::Http),
        url: Some(url.into()),
        bearer_token_env: Some("TOKEN".into()),
        ..Default::default()
    }
}

fn token(value: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("TOKEN".to_owned(), value.to_owned())])
}

#[test]
fn publishes_a_complete_private_installation_key_and_reuses_it() {
    let root = tempfile::tempdir().unwrap();
    let config = command("fixture", &["synthetic-secret"]);
    let first = hash_config(&config, Some(root.path())).unwrap();
    let path = root.path().join("config-identity.key");
    assert_eq!(std::fs::read(&path).unwrap().len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(!is_ephemeral_identity(Some(&first)));
    assert_eq!(hash_config(&config, Some(root.path())).unwrap(), first);
    assert_ne!(
        hash_config(&command("fixture", &["rotated"]), Some(root.path())).unwrap(),
        first
    );
}

#[test]
fn uses_hmac_rather_than_an_offline_guessable_bearer_fingerprint() {
    let root = tempfile::tempdir().unwrap();
    let identity = mcp_credential_identity(
        &bearer("https://example.invalid"),
        "fixture",
        Some(root.path()),
        Some(&token("1234")),
    );
    let key = std::fs::read(root.path().join("config-identity.key")).unwrap();
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
    mac.update(json!(["mcp-catalog-bearer", "1234"]).to_string().as_bytes());
    let expected = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(identity.as_deref(), Some(expected.as_str()));
}

fn assert_invalid_key_is_untrusted_and_untouched(root: &Path, before: &[u8]) {
    let config = command("fixture", &[]);
    let identity = hash_config(&config, Some(root)).unwrap();
    assert!(identity.starts_with("ephemeral:"), "{identity}");
    assert_eq!(hash_config(&config, Some(root)).unwrap(), identity);
    assert_eq!(
        std::fs::read(root.join("config-identity.key")).unwrap(),
        before
    );
}

#[test]
fn does_not_replace_or_trust_a_partial_key() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("config-identity.key"), b"short").unwrap();
    assert_invalid_key_is_untrusted_and_untouched(root.path(), b"short");
}

#[test]
fn does_not_replace_or_trust_an_oversize_key() {
    let root = tempfile::tempdir().unwrap();
    let key = vec![b'k'; 33];
    let path = root.path().join("config-identity.key");
    std::fs::write(&path, &key).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_invalid_key_is_untrusted_and_untouched(root.path(), &key);
}

#[cfg(unix)]
#[test]
fn does_not_replace_or_trust_a_key_with_loose_or_readonly_permissions() {
    use std::os::unix::fs::PermissionsExt;
    for mode in [0o644, 0o400] {
        let root = tempfile::tempdir().unwrap();
        let key = vec![b'k'; 32];
        let path = root.path().join("config-identity.key");
        std::fs::write(&path, &key).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_invalid_key_is_untrusted_and_untouched(root.path(), &key);
    }
}

#[test]
fn keeps_a_completed_published_key_when_an_abandoned_temporary_file_exists() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("config-identity.key.abandoned.tmp"), b"").unwrap();
    let key = [7u8; 32];
    let path = root.path().join("config-identity.key");
    std::fs::write(&path, key).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert!(
        !hash_config(&command("fixture", &[]), Some(root.path()))
            .unwrap()
            .starts_with("ephemeral:")
    );
    assert_eq!(std::fs::read(&path).unwrap(), key);
}

fn legacy_cache(config_hash: &str, credential_identity: &str) -> McpCatalogCacheFile {
    McpCatalogCacheFile {
        version: 1,
        servers: BTreeMap::from([(
            "fx".to_owned(),
            McpCachedServerCatalog {
                modern: false,
                config_hash: config_hash.into(),
                credential_identity: Some(credential_identity.into()),
                fetched_at: chrono::Utc::now().timestamp_millis() as f64,
                tools: vec![json!({"name":"offline","inputSchema":{"type":"object"}})],
                resources: vec![],
                prompts: vec![],
                instructions: None,
            },
        )]),
    }
}

// #3858: offline lazy catalogs migrate only for the same config AND account.
fn adopt(
    account: &str,
) -> (
    tempfile::TempDir,
    Option<McpCachedServerCatalog>,
    String,
    String,
) {
    let root = tempfile::tempdir().unwrap();
    let config = bearer("http://127.0.0.1:9/mcp");
    let legacy_hash = legacy_hash_config(&config).unwrap();
    let legacy_identity = legacy_mcp_credential_identity(
        &config,
        "fx",
        Some(root.path()),
        Some(&token("old-account")),
    )
    .unwrap();
    let cache = legacy_cache(&legacy_hash, &legacy_identity);
    let env = token(account);
    let current = McpCacheIdentity {
        config_hash: hash_config(&config, Some(root.path())).unwrap(),
        credential_identity: mcp_credential_identity(&config, "fx", Some(root.path()), Some(&env))
            .unwrap(),
    };
    let adopted = adopt_mcp_cached_server(
        McpCacheAdoption {
            cache: &cache,
            agent_dir: Some(root.path()),
            server_name: "fx",
            current,
            legacy: Box::new(|| {
                legacy_mcp_cache_identity(&config, "fx", Some(root.path()), Some(&env))
            }),
        },
        chrono::Utc::now().timestamp_millis() as f64,
    );
    (root, adopted, legacy_hash, legacy_identity)
}

#[test]
fn adopts_legacy_cache_for_the_same_credentials() {
    let (root, adopted, legacy_hash, legacy_identity) = adopt("old-account");
    let adopted = adopted.expect("same account adopts the legacy entry");
    assert_eq!(adopted.tools[0]["name"], "offline");
    let migrated = read_mcp_catalog_cache(root.path()).servers["fx"].clone();
    assert_ne!(migrated.config_hash, legacy_hash);
    assert_ne!(
        migrated.credential_identity.as_deref(),
        Some(legacy_identity.as_str())
    );
    assert_eq!(migrated, adopted);
}

#[test]
fn does_not_adopt_legacy_cache_for_changed_credentials() {
    let (root, adopted, _, _) = adopt("new-account");
    assert!(adopted.is_none());
    assert!(read_mcp_catalog_cache(root.path()).servers.is_empty());
}
