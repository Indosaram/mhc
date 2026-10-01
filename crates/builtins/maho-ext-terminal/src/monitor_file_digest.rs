use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

pub fn digest_file_handle(handle: &mut File) -> io::Result<String> {
    const SAMPLE_SIZE: usize = 64 * 1024;
    let size = handle.metadata()?.len();
    let mut hash = Sha256::new();
    let first_len = usize::try_from(size.min(65_536)).map_err(io::Error::other)?;
    let mut first = vec![0; first_len];
    if !first.is_empty() {
        handle.seek(SeekFrom::Start(0))?;
        let _bytes_read = handle.read(&mut first)?;
        hash.update(&first);
    }
    if size > 65_536 {
        let mut middle = vec![0; SAMPLE_SIZE];
        handle.seek(SeekFrom::Start((size - 65_536) / 2))?;
        let _bytes_read = handle.read(&mut middle)?;
        hash.update(&middle);
        let mut last = vec![0; SAMPLE_SIZE];
        handle.seek(SeekFrom::Start(size - 65_536))?;
        let _bytes_read = handle.read(&mut last)?;
        hash.update(&last);
    }
    Ok(format!("{size}:{:x}",hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn empty_digest_and_small_file() -> io::Result<()> {
        let mut file = tempfile::tempfile()?;
        assert_eq!(digest_file_handle(&mut file)?,format!("0:{:x}",Sha256::digest([])));
        file.write_all(b"abc")?;
        assert_eq!(digest_file_handle(&mut file)?,format!("3:{:x}",Sha256::digest(b"abc")));
        Ok(())
    }

    #[test]
    fn large_file_samples_middle_and_end() -> io::Result<()> {
        let mut file = tempfile::tempfile()?;
        let bytes = vec![b'x'; 200_000]; file.write_all(&bytes)?;
        let mut expected = Sha256::new();
        expected.update(&bytes[..65_536]); expected.update(&bytes[67_232..132_768]); expected.update(&bytes[134_464..]);
        assert_eq!(digest_file_handle(&mut file)?,format!("200000:{:x}",expected.finalize()));
        let before = digest_file_handle(&mut file)?;
        file.seek(SeekFrom::Start(100_000))?; file.write_all(b"changed")?;
        assert_ne!(digest_file_handle(&mut file)?,before);
        Ok(())
    }
}
