//! Port of `packages/coding-agent/src/engine-trust/launch-environment.ts` (omo#10762).

use std::collections::BTreeMap;

use super::environment_names::{EnvironmentEntries, delete_name, has_name, set_name, values_of};
use super::marker::{
    ENGINE_TRUST_HOP_ENV, ENGINE_TRUST_MARKER_ENV, ENGINE_TRUST_RESTORE_NAMES,
    EngineTrustContractError, EngineTrustContractErrorCode, EngineTrustMarker, EngineTrustOwner,
    EngineTrustRestore, parse_engine_trust_marker, serialize_engine_trust_marker,
};

const EXTRA_CA_ENV: &str = "NODE_EXTRA_CA_CERTS";
const SERVER_REMOVED_NAMES: [&str; 2] = ["NODE_USE_SYSTEM_CA", "BUN_OPTIONS"];
const ALWAYS_RESTORED_NAMES: [&str; 4] = [
    "NODE_EXTRA_CA_CERTS",
    "NODE_USE_SYSTEM_CA",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeignMarkerReason {
    Unparseable,
    FileMismatch,
    Ambiguous,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustMarkerArrival {
    None,
    Addressed(Box<EngineTrustMarker>),
    Foreign(ForeignMarkerReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustArrival {
    pub hop: bool,
    pub marker: TrustMarkerArrival,
}

pub fn classify_trust_arrival(env: &EnvironmentEntries, platform: &str) -> TrustArrival {
    let hop = has_name(env, ENGINE_TRUST_HOP_ENV, platform);
    let marker_texts = values_of(env, ENGINE_TRUST_MARKER_ENV, platform);
    if marker_texts.is_empty() {
        return TrustArrival {
            hop,
            marker: TrustMarkerArrival::None,
        };
    }
    let extra_files = values_of(env, EXTRA_CA_ENV, platform);
    if marker_texts.len() > 1 || extra_files.len() > 1 {
        return TrustArrival {
            hop,
            marker: TrustMarkerArrival::Foreign(ForeignMarkerReason::Ambiguous),
        };
    }
    let Some(marker) = parse_engine_trust_marker(&marker_texts[0]) else {
        return TrustArrival {
            hop,
            marker: TrustMarkerArrival::Foreign(ForeignMarkerReason::Unparseable),
        };
    };
    if extra_files.first().map(String::as_str) != Some(marker.file.as_str()) {
        return TrustArrival {
            hop,
            marker: TrustMarkerArrival::Foreign(ForeignMarkerReason::FileMismatch),
        };
    }
    TrustArrival {
        hop,
        marker: TrustMarkerArrival::Addressed(Box::new(marker)),
    }
}

pub struct TrustLaunchOptions<'a> {
    pub marker: &'a EngineTrustMarker,
    /// True only for the launcher's own trust exec.
    pub hop: bool,
    pub platform: &'a str,
}

fn representable_path<'a>(
    path: &'a str,
    platform: &str,
) -> Result<&'a str, EngineTrustContractError> {
    let separator = if platform == "win32" { ';' } else { ':' };
    if path.contains(separator) {
        return Err(EngineTrustContractError {
            code: EngineTrustContractErrorCode::TrustPathUnrepresentable,
            message: "engine trust closure path holds a list separator".to_owned(),
        });
    }
    Ok(path)
}

pub fn build_trust_launch_environment(
    base: &EnvironmentEntries,
    options: &TrustLaunchOptions<'_>,
) -> Result<BTreeMap<String, String>, EngineTrustContractError> {
    let marker = options.marker;
    let platform = options.platform;
    let mut env = base.clone();
    if marker.owner == EngineTrustOwner::Server {
        for name in SERVER_REMOVED_NAMES {
            delete_name(&mut env, name, platform);
        }
    }
    if let Some(defaults) = &marker.defaults {
        set_name(
            &mut env,
            "SSL_CERT_FILE",
            representable_path(&defaults.file, platform)?,
            platform,
        );
        set_name(
            &mut env,
            "SSL_CERT_DIR",
            representable_path(&defaults.dir, platform)?,
            platform,
        );
    }
    set_name(&mut env, EXTRA_CA_ENV, &marker.file, platform);
    let serialized = serialize_engine_trust_marker(marker)?;
    set_name(&mut env, ENGINE_TRUST_MARKER_ENV, &serialized, platform);
    if options.hop {
        set_name(&mut env, ENGINE_TRUST_HOP_ENV, "1", platform);
    } else {
        delete_name(&mut env, ENGINE_TRUST_HOP_ENV, platform);
    }
    Ok(env
        .into_iter()
        .filter_map(|(k, v)| v.map(|val| (k, val)))
        .collect())
}

fn restore_slot<'a>(restore: &'a EngineTrustRestore, name: &str) -> Option<&'a str> {
    match name {
        "NODE_EXTRA_CA_CERTS" => restore
            .node_extra_ca_certs
            .as_ref()
            .and_then(|v| v.as_deref()),
        "NODE_USE_SYSTEM_CA" => restore
            .node_use_system_ca
            .as_ref()
            .and_then(|v| v.as_deref()),
        "SSL_CERT_FILE" => restore.ssl_cert_file.as_ref().and_then(|v| v.as_deref()),
        "SSL_CERT_DIR" => restore.ssl_cert_dir.as_ref().and_then(|v| v.as_deref()),
        "BUN_OPTIONS" => restore.bun_options.as_ref().and_then(|v| v.as_deref()),
        _ => None,
    }
}

pub fn apply_engine_trust_restore(
    env: &mut EnvironmentEntries,
    restore: &EngineTrustRestore,
    platform: &str,
) {
    let names: &[&str] = if restore.bun_options.is_none() {
        &ALWAYS_RESTORED_NAMES
    } else {
        &ENGINE_TRUST_RESTORE_NAMES
    };
    for name in names {
        match restore_slot(restore, name) {
            Some(value) => set_name(env, name, value, platform),
            None => delete_name(env, name, platform),
        }
    }
    delete_name(env, ENGINE_TRUST_MARKER_ENV, platform);
    delete_name(env, ENGINE_TRUST_HOP_ENV, platform);
}

#[cfg(test)]
mod tests {
    use super::super::marker::{EngineTrustAutomatic, EngineTrustDefaults};
    use super::*;

    #[test]
    fn classifies_addressed_foreign_and_restores_env() {
        let marker = EngineTrustMarker {
            v: 1,
            file: "/trust/ca.pem".into(),
            sha256: "b".repeat(64),
            automatic: EngineTrustAutomatic::Os,
            fallback_reason: None,
            owner: EngineTrustOwner::Server,
            user: None,
            defaults: Some(EngineTrustDefaults {
                file: "/trust/def.pem".into(),
                dir: "/trust/dir".into(),
            }),
            restore: EngineTrustRestore {
                node_extra_ca_certs: Some(Some("/user/extra.pem".into())),
                ..Default::default()
            },
        };
        let built = build_trust_launch_environment(
            &BTreeMap::from([("BUN_OPTIONS".to_owned(), Some("--use-system-ca".to_owned()))]),
            &TrustLaunchOptions {
                marker: &marker,
                hop: true,
                platform: "linux",
            },
        )
        .expect("build");
        assert!(!built.contains_key("BUN_OPTIONS"));
        let entries: EnvironmentEntries = built.into_iter().map(|(k, v)| (k, Some(v))).collect();
        let arrival = classify_trust_arrival(&entries, "linux");
        assert!(arrival.hop);
        assert_eq!(
            arrival.marker,
            TrustMarkerArrival::Addressed(Box::new(marker.clone()))
        );

        let mut restored = entries;
        apply_engine_trust_restore(&mut restored, &marker.restore, "linux");
        assert_eq!(
            restored.get("NODE_EXTRA_CA_CERTS"),
            Some(&Some("/user/extra.pem".to_owned()))
        );
        assert!(!restored.contains_key(ENGINE_TRUST_MARKER_ENV));
        assert!(!restored.contains_key(ENGINE_TRUST_HOP_ENV));
    }
}
