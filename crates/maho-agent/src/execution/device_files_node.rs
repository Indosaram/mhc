//! Port of senpi `packages/agent/src/execution/device-files-node.ts`.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::device_files::{
    DeviceFileError, DeviceFileErrorCode, DeviceFileKind, DeviceFileOptions, DeviceFileStat,
    DeviceFileWriteOptions, DeviceFiles, DeviceRunRequest, DeviceRunResult, FILE_CHANNEL_MAX_BYTES,
};

fn device_io_error(err: &std::io::Error, path: &str) -> DeviceFileError {
    match err.kind() {
        std::io::ErrorKind::NotFound => DeviceFileError::new(DeviceFileErrorCode::NotFound, path),
        std::io::ErrorKind::AlreadyExists => {
            DeviceFileError::new(DeviceFileErrorCode::Exists, path)
        }
        _ => {
            #[cfg(unix)]
            if matches!(
                err.raw_os_error(),
                Some(libc::EISDIR | libc::ENOTDIR | libc::ENXIO)
            ) {
                return DeviceFileError::new(DeviceFileErrorCode::NotAFile, path);
            }
            DeviceFileError::new(DeviceFileErrorCode::Io, err.to_string())
        }
    }
}

fn open_regular_sync(
    path: &Path,
    path_str: &str,
    read: bool,
    write: bool,
    create: bool,
    truncate: bool,
    exclusive: bool,
) -> Result<File, DeviceFileError> {
    let mut opts = OpenOptions::new();
    opts.read(read).write(write);
    if exclusive {
        opts.create_new(true);
    } else {
        opts.create(create).truncate(truncate);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NONBLOCK);
    }
    let file = opts.open(path).map_err(|e| device_io_error(&e, path_str))?;
    let meta = file.metadata().map_err(|e| device_io_error(&e, path_str))?;
    if !meta.is_file() {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::NotAFile,
            path_str,
        ));
    }
    Ok(file)
}

fn read_bounded_sync(
    path_str: &str,
    signal: Option<&AbortSignal>,
) -> Result<Vec<u8>, DeviceFileError> {
    if signal.is_some_and(AbortSignal::aborted) {
        return Err(DeviceFileError::new(DeviceFileErrorCode::Aborted, path_str));
    }
    let mut file = open_regular_sync(
        Path::new(path_str),
        path_str,
        true,
        false,
        false,
        false,
        false,
    )?;
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if signal.is_some_and(AbortSignal::aborted) {
            return Err(DeviceFileError::new(DeviceFileErrorCode::Aborted, path_str));
        }
        let n = file
            .read(&mut buf)
            .map_err(|e| device_io_error(&e, path_str))?;
        if n == 0 {
            break;
        }
        if out.len() + n > FILE_CHANNEL_MAX_BYTES {
            return Err(DeviceFileError::new(
                DeviceFileErrorCode::TooLarge,
                path_str,
            ));
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

fn stat_of(meta: &Metadata) -> DeviceFileStat {
    let kind = if meta.is_file() {
        DeviceFileKind::File
    } else if meta.is_dir() {
        DeviceFileKind::Directory
    } else {
        DeviceFileKind::Other
    };
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0);
    DeviceFileStat {
        kind,
        size: meta.len(),
        mtime_ms,
    }
}

async fn run_direct(
    request: DeviceRunRequest,
    signal: Option<AbortSignal>,
) -> Result<DeviceRunResult, DeviceFileError> {
    if signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::Aborted,
            "aborted",
        ));
    }
    let mut cmd = Command::new(&request.exe);
    cmd.args(&request.argv)
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(env) = &request.env {
        cmd.envs(env);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Some(input) = &request.stdin {
            let _ = stdin.write_all(input).await;
        }
        let _ = stdin.shutdown().await;
    }

    let wait_future = child.wait_with_output();
    let timeout_dur = request.timeout_ms.map(Duration::from_millis);

    let output = tokio::select! {
        res = async {
            match timeout_dur {
                Some(dur) => tokio::time::timeout(dur, wait_future).await.map_err(|_| ()),
                None => Ok(wait_future.await),
            }
        } => {
            match res {
                Ok(Ok(out)) => out,
                Ok(Err(e)) => {
                    return Err(DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()));
                }
                Err(()) => {
                    return Err(DeviceFileError::new(
                        DeviceFileErrorCode::Io,
                        "process timed out",
                    ));
                }
            }
        }
        _ = async {
            match &signal {
                Some(sig) => sig.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        } => {
            return Err(DeviceFileError::new(
                DeviceFileErrorCode::Aborted,
                "aborted",
            ));
        }
    };

    #[cfg(unix)]
    let exit_signal = {
        use std::os::unix::process::ExitStatusExt;
        output.status.signal().map(|s| format!("SIG{s}"))
    };
    #[cfg(not(unix))]
    let exit_signal = None;

    Ok(DeviceRunResult {
        code: output.status.code(),
        signal: exit_signal,
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

pub struct LocalDeviceFiles;

impl DeviceFiles for LocalDeviceFiles {
    fn read(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<Vec<u8>, DeviceFileError>> {
        let path_owned = path.to_owned();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                read_bounded_sync(&path_owned, options.signal.as_ref())
            })
            .await
            .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))?
        })
    }

    fn write(
        &self,
        path: &str,
        data: Vec<u8>,
        options: DeviceFileOptions,
        write_options: DeviceFileWriteOptions,
    ) -> BoxFuture<'_, Result<(), DeviceFileError>> {
        let path_owned = path.to_owned();
        Box::pin(async move {
            if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Err(DeviceFileError::new(
                    DeviceFileErrorCode::Aborted,
                    path_owned,
                ));
            }
            if data.len() > FILE_CHANNEL_MAX_BYTES {
                return Err(DeviceFileError::new(
                    DeviceFileErrorCode::TooLarge,
                    path_owned,
                ));
            }
            tokio::task::spawn_blocking(move || {
                let p = Path::new(&path_owned);
                if let Some(parent) = p.parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| device_io_error(&e, &path_owned))?;
                }
                let mut file = open_regular_sync(
                    p,
                    &path_owned,
                    false,
                    true,
                    true,
                    !write_options.exclusive,
                    write_options.exclusive,
                )?;
                file.write_all(&data)
                    .map_err(|e| device_io_error(&e, &path_owned))
            })
            .await
            .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))?
        })
    }

    fn list(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<Vec<String>, DeviceFileError>> {
        let path_owned = path.to_owned();
        Box::pin(async move {
            if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Err(DeviceFileError::new(
                    DeviceFileErrorCode::Aborted,
                    path_owned,
                ));
            }
            let mut entries = tokio::fs::read_dir(&path_owned)
                .await
                .map_err(|e| device_io_error(&e, &path_owned))?;
            let mut names = Vec::new();
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| device_io_error(&e, &path_owned))?
            {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
            Ok(names)
        })
    }

    fn stat(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<DeviceFileStat, DeviceFileError>> {
        let path_owned = path.to_owned();
        Box::pin(async move {
            if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Err(DeviceFileError::new(
                    DeviceFileErrorCode::Aborted,
                    path_owned,
                ));
            }
            let meta = tokio::fs::symlink_metadata(&path_owned)
                .await
                .map_err(|e| device_io_error(&e, &path_owned))?;
            Ok(stat_of(&meta))
        })
    }

    fn rename(
        &self,
        from: &str,
        to: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<(), DeviceFileError>> {
        let from_owned = from.to_owned();
        let to_owned = to.to_owned();
        Box::pin(async move {
            if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Err(DeviceFileError::new(
                    DeviceFileErrorCode::Aborted,
                    from_owned,
                ));
            }
            tokio::fs::rename(&from_owned, &to_owned)
                .await
                .map_err(|e| device_io_error(&e, &from_owned))
        })
    }

    fn remove(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<(), DeviceFileError>> {
        let path_owned = path.to_owned();
        Box::pin(async move {
            if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Err(DeviceFileError::new(
                    DeviceFileErrorCode::Aborted,
                    path_owned,
                ));
            }
            let meta = tokio::fs::symlink_metadata(&path_owned)
                .await
                .map_err(|e| device_io_error(&e, &path_owned))?;
            if meta.is_dir() {
                tokio::fs::remove_dir_all(&path_owned)
                    .await
                    .map_err(|e| device_io_error(&e, &path_owned))
            } else {
                tokio::fs::remove_file(&path_owned)
                    .await
                    .map_err(|e| device_io_error(&e, &path_owned))
            }
        })
    }

    fn mkdir(
        &self,
        path: &str,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<(), DeviceFileError>> {
        let path_owned = path.to_owned();
        Box::pin(async move {
            if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Err(DeviceFileError::new(
                    DeviceFileErrorCode::Aborted,
                    path_owned,
                ));
            }
            tokio::fs::create_dir_all(&path_owned)
                .await
                .map_err(|e| device_io_error(&e, &path_owned))
        })
    }

    fn run(
        &self,
        request: DeviceRunRequest,
        options: DeviceFileOptions,
    ) -> BoxFuture<'_, Result<DeviceRunResult, DeviceFileError>> {
        Box::pin(run_direct(request, options.signal))
    }

    fn is_local(&self) -> bool {
        true
    }
}

pub fn local_device_files() -> Arc<dyn DeviceFiles> {
    Arc::new(LocalDeviceFiles)
}
