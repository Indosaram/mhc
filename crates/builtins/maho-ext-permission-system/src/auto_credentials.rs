//! Port of `permission-system/auto-credentials.ts` (senpi 903b8336): the credential-shaped path
//! names the `auto` preset never approves. Distinct from `external_dir::is_credential_path`, which
//! guards mhc's own `auth.json` for the `credential_file` permission.

use regex::Regex;
use std::sync::LazyLock;

static CREDENTIAL_PATH_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)(^|[/\\=])\.(ssh|aws|gnupg|kube|docker|azure|gcloud)([/\\]|$)",
        r"(?i)(^|[/\\=])\.config[/\\](gh|gcloud|hub|op)([/\\]|$)",
        r"(?i)(^|[/\\=])\.(netrc|npmrc|pypirc|pgpass|git-credentials|htpasswd|vault-token|envrc|terraformrc)$",
        r"(?i)(^|[/\\=])\.env(\.[^/\\]*)?$",
        r"(?i)(^|[/\\=])id_(rsa|dsa|ecdsa|ed25519)(\.pub)?$",
        r"(?i)\.(pem|key|p12|pfx|jks|keystore|kdbx|ppk|asc|gpg)$",
        r"(?i)(^|[/\\=])\.?credentials(\.json|\.toml|\.tfrc\.json)?$",
        r"(?i)(^|[/\\=])auth\.json$",
        r"(?i)(^|[/\\=])(\.[a-z]+_history|fish_history)$",
        r"(?i)(^|[/\\=])\.m2[/\\]settings\.xml$",
        r"(?i)(^|[/\\])Library[/\\]Keychains([/\\]|$)",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("static regex"))
    .collect()
});

pub fn is_credential_path(value: &str) -> bool {
    CREDENTIAL_PATH_PATTERNS
        .iter()
        .any(|pattern| pattern.is_match(value))
}
