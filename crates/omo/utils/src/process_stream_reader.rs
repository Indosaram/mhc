//! Drain a child stream into a UTF-8 string (chunks are concatenated before decoding).

use std::io::{self, Read};

pub fn read_process_stream(stream: Option<impl Read>) -> io::Result<String> {
    let mut buffer = Vec::new();
    if let Some(mut stream) = stream {
        stream.read_to_end(&mut buffer)?;
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}
