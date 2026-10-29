use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Lazy handle to a file path (the Rust shape of `Bun.file`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFile {
    path: PathBuf,
}

impl RuntimeFile {
    pub fn text(&self) -> io::Result<String> {
        fs::read_to_string(&self.path)
    }

    pub fn bytes(&self) -> io::Result<Vec<u8>> {
        fs::read(&self.path)
    }

    pub fn exists(&self) -> bool {
        fs::metadata(&self.path).is_ok()
    }

    pub fn delete(&self) -> io::Result<()> {
        fs::remove_file(&self.path)
    }
}

pub fn bun_file(path: impl AsRef<Path>) -> RuntimeFile {
    RuntimeFile {
        path: path.as_ref().to_path_buf(),
    }
}

/// Write `data` and return the number of bytes written.
pub fn bun_write(path: impl AsRef<Path>, data: impl AsRef<[u8]>) -> io::Result<usize> {
    let bytes = data.as_ref();
    fs::write(path, bytes)?;
    Ok(bytes.len())
}
