//! Sealing export and self-verifying bundle assembly.
use super::bundle::*;
use super::ownership::*;
use serde_json::json;
use std::path::{Path, PathBuf};

pub struct ExportSessionForMoveOptions<'a> {
    pub session_file: &'a Path,
    pub move_id: &'a str,
    pub target: &'a str,
    pub dropped_user_messages: &'a [String],
    pub bundle_dir: &'a Path,
    pub extension_state_dir: Option<&'a Path>,
    pub resolve_extension_state: Option<&'a dyn Fn() -> Result<Option<PathBuf>, String>>,
    pub guard: Option<&'a dyn Fn() -> Result<(), String>>,
    pub locked_guard: Option<&'a dyn Fn() -> Result<(), String>>,
}
pub async fn export_session_for_move(
    options: ExportSessionForMoveOptions<'_>,
) -> Result<MoveManifest, String> {
    super::session_lock::with_session_move_lock(
        options.session_file,
        async {
            if let Some(guard) = options.locked_guard {
                guard()?;
            }
            write_move_bundle(&options).await
        },
        options.guard,
    )
    .await
}
async fn write_move_bundle(
    options: &ExportSessionForMoveOptions<'_>,
) -> Result<MoveManifest, String> {
    let move_id = parse_move_id(&json!(options.move_id)).map_err(|e| e.to_string())?;
    let entries = super::entries(options.session_file);
    let session_id = parse_plain_id(&json!(header_session_id(&entries)), "session id")
        .map_err(|e| e.to_string())?;
    match session_ownership(&entries) {
        SessionOwnership::Writable => super::append_custom(
            options.session_file,
            SESSION_RELEASED,
            json!({"reason":"move", "move_id":move_id,"session_id":session_id,"target":options.target,"released_at":crate::session_manager::now_iso_millis()}),
        )?,
        SessionOwnership::Refused(owner)
            if owner.state == RefusedState::Moving && owner.move_id == move_id => {}
        SessionOwnership::Refused(owner) => {
            return Err(
                SessionMovedAwayError::new(&options.session_file.to_string_lossy(), owner)
                    .to_string(),
            );
        }
    }
    let state = match options.resolve_extension_state {
        Some(resolve) => resolve()?,
        None => match options.extension_state_dir {
            Some(dir) => Some(dir.to_owned()),
            None => {
                super::extension_state::sealed_outgoing_state_dir(options.session_file, &move_id)?
            }
        },
    };
    if let Some(state) = &state
        && !state.exists() {
            return Err(format!(
                "The extension state the move release staged is missing ({}); the session was not exported without it.",
                state.display()
            ));
        }
    let staging = PathBuf::from(format!(
        "{}.partial-{}",
        options.bundle_dir.display(),
        std::process::id()
    ));
    super::remove(&staging)?;
    super::copy(options.session_file, &staging.join(SESSION_BUNDLE_PATH))?;
    let sidecars = copy_sidecars(
        options.session_file.parent().unwrap_or(Path::new(".")),
        &session_id,
        &staging,
    )
    .await?;
    let state_files = copy_extension_state(state.as_deref(), &staging).await?;
    let manifest = MoveManifest {
        format: MOVE_BUNDLE_FORMAT,
        move_id,
        session_id,
        session_file_name: options
            .session_file
            .file_name()
            .ok_or("missing session filename")?
            .to_string_lossy()
            .into_owned(),
        source_cwd: entries
            .first()
            .and_then(|e| e["cwd"].as_str())
            .unwrap_or_default()
            .into(),
        repository: entries
            .iter()
            .filter(|e| e["customType"] == "repository-identity")
            .filter_map(|e| parse_repository_identity(&e["data"]))
            .next_back(),
        session: BundleFile {
            path: SESSION_BUNDLE_PATH.into(),
            sha256: sha256_file_async(&staging.join(SESSION_BUNDLE_PATH))
                .await
                .map_err(|e| e.to_string())?,
        },
        sidecars,
        dropped_user_messages: options.dropped_user_messages.to_vec(),
        payloads: if state_files.is_empty() {
            vec![]
        } else {
            vec![MovePayload {
                kind: "extension-state".into(),
                files: state_files,
            }]
        },
    };
    tokio::fs::write(
        staging.join(MANIFEST_FILE),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?
        ),
    )
    .await
    .map_err(|e| e.to_string())?;
    super::remove(options.bundle_dir)?;
    std::fs::rename(staging, options.bundle_dir).map_err(|e| e.to_string())?;
    Ok(manifest)
}
async fn copy_sidecars(
    dir: &Path,
    session_id: &str,
    staging: &Path,
) -> Result<Vec<BundleFile>, String> {
    let mut sources = Vec::new();
    let extensions = dir.join("extensions");
    if extensions.exists() {
        for entry in std::fs::read_dir(extensions).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let source = entry.path().join(format!(
                "{}.json",
                crate::session_sidecar_store::encoded_session_id(session_id)
            ));
            if entry.file_type().map_err(|e| e.to_string())?.is_dir() && source.exists() {
                sources.push(source);
            }
        }
    }
    sources.extend(super::files_under(
        &dir.join("resident-blobs").join(session_id),
    )?);
    copy_list("files", dir, &sources, staging).await
}
async fn copy_extension_state(
    dir: Option<&Path>,
    staging: &Path,
) -> Result<Vec<BundleFile>, String> {
    match dir {
        Some(dir) => copy_list("state", dir, &super::files_under(dir)?, staging).await,
        None => Ok(vec![]),
    }
}
async fn copy_list(
    prefix: &str,
    root: &Path,
    sources: &[PathBuf],
    staging: &Path,
) -> Result<Vec<BundleFile>, String> {
    let mut files = Vec::new();
    for source in sources {
        let path = format!(
            "{prefix}/{}",
            source
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/")
        );
        let to = staging.join(&path);
        super::copy(source, &to)?;
        files.push(BundleFile {
            path,
            sha256: sha256_file_async(&to).await.map_err(|e| e.to_string())?,
        });
    }
    Ok(files)
}
