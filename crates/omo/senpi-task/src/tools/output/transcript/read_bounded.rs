//! Bounded transcript file reader (port of `tools/output/transcript/read-bounded.ts`).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

pub const MAX_TRANSCRIPT_SOURCE_BYTES: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedFileText {
    pub text: String,
    pub truncated: bool,
}

/// Reads into `buffer` until it is full or EOF is reached; unread bytes stay zeroed.
fn read_into(file: &mut File, buffer: &mut [u8]) -> io::Result<()> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Reads a file as UTF-8 text, keeping at most `maximum_bytes` bytes (head + `\n` + tail).
/// Returns `Ok(None)` when the file does not exist.
pub fn read_bounded_file_text(
    path: impl AsRef<Path>,
    maximum_bytes: Option<usize>,
) -> io::Result<Option<BoundedFileText>> {
    let maximum_bytes = maximum_bytes.unwrap_or(MAX_TRANSCRIPT_SOURCE_BYTES);
    let mut file = match File::open(path.as_ref()) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let size = file.metadata()?.len();

    if size <= maximum_bytes as u64 {
        let mut buffer = vec![0u8; usize::try_from(size).unwrap_or(maximum_bytes)];
        read_into(&mut file, &mut buffer)?;
        return Ok(Some(BoundedFileText {
            text: String::from_utf8_lossy(&buffer).into_owned(),
            truncated: false,
        }));
    }

    let head_bytes = maximum_bytes.saturating_sub(1) / 2;
    let tail_bytes = maximum_bytes - head_bytes - 1;
    let mut buffer = vec![0u8; maximum_bytes];
    file.seek(SeekFrom::Start(0))?;
    read_into(&mut file, &mut buffer[..head_bytes])?;
    buffer[head_bytes] = 0x0a;
    file.seek(SeekFrom::Start(size - tail_bytes as u64))?;
    read_into(&mut file, &mut buffer[head_bytes + 1..])?;
    Ok(Some(BoundedFileText {
        text: String::from_utf8_lossy(&buffer).into_owned(),
        truncated: true,
    }))
}
