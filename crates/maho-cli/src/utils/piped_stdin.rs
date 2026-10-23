//! Port of senpi `utils/piped-stdin.ts`: read piped stdin without hanging on a silent pipe.
//!
//! Harnesses (ssh, CI runners, spawn with stdio "pipe") hand children a stdin that is open but never
//! written or closed. Slow real producers need well over 150 ms, hence the 2 s grace window.
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

pub const PIPED_STDIN_GRACE_MS: u64 = 2_000;

pub struct ReadPipedStdinOptions<'a> {
    pub is_tty: bool,
    pub has_prompt_argument: bool,
    pub force_read: bool,
    pub grace: Duration,
    pub is_regular_file: bool,
    pub on_skip: Option<&'a mut dyn FnMut()>,
}

/// Whether the process stdin is a regular file (`fstat(0).isFile()`); false when it cannot be told.
pub fn stdin_is_regular_file() -> bool {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        std::io::stdin()
            .as_fd()
            .try_clone_to_owned()
            .and_then(|fd| std::fs::File::from(fd).metadata())
            .is_ok_and(|metadata| metadata.is_file())
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// `trim()` of everything read, `None` when empty, skipped (tty, or silent within the grace window).
pub async fn read_piped_stdin<R: AsyncRead + Unpin>(
    reader: &mut R,
    options: ReadPipedStdinOptions<'_>,
) -> std::io::Result<Option<String>> {
    if !options.force_read && options.is_tty {
        return Ok(None);
    }
    let read_to_eof = options.force_read || !options.has_prompt_argument || options.is_regular_file;
    let mut data = Vec::new();
    let mut chunk = [0_u8; 8192];
    let mut awaiting_first_chunk = !read_to_eof;
    loop {
        let read = if awaiting_first_chunk {
            match tokio::time::timeout(options.grace, reader.read(&mut chunk)).await {
                Ok(read) => read?,
                Err(_) => {
                    if let Some(on_skip) = options.on_skip {
                        on_skip();
                    }
                    return Ok(None);
                }
            }
        } else {
            reader.read(&mut chunk).await?
        };
        if read == 0 {
            break;
        }
        // Any data chunk cancels the grace timer.
        awaiting_first_chunk = false;
        data.extend_from_slice(&chunk[..read]);
    }
    let text = String::from_utf8_lossy(&data);
    let trimmed = text.trim();
    Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn options<'a>(has_prompt_argument: bool, force_read: bool) -> ReadPipedStdinOptions<'a> {
        ReadPipedStdinOptions {
            is_tty: false,
            has_prompt_argument,
            force_read,
            grace: Duration::from_millis(PIPED_STDIN_GRACE_MS),
            is_regular_file: false,
            on_skip: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn silent_pipe_with_prompt_is_skipped_and_reports() {
        let (_writer, mut reader) = tokio::io::duplex(64);
        let mut skipped = false;
        let mut on_skip = || skipped = true;
        let mut opts = options(true, false);
        opts.on_skip = Some(&mut on_skip);
        let result = read_piped_stdin(&mut reader, opts).await;
        assert_eq!(result.ok(), Some(None));
        assert!(skipped);
    }

    #[tokio::test(start_paused = true)]
    async fn data_before_the_grace_window_is_read_to_eof_and_trimmed() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        writer.write_all(b"  hello\n").await.ok();
        drop(writer);
        let result = read_piped_stdin(&mut reader, options(true, false)).await;
        assert_eq!(result.ok(), Some(Some("hello".to_owned())));
    }

    #[tokio::test(start_paused = true)]
    async fn no_prompt_or_force_read_waits_for_eof() {
        for (prompt, force) in [(false, false), (true, true)] {
            let (mut writer, mut reader) = tokio::io::duplex(64);
            let producer = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(PIPED_STDIN_GRACE_MS * 2)).await;
                writer.write_all(b"late").await.ok();
            });
            let result = read_piped_stdin(&mut reader, options(prompt, force)).await;
            assert_eq!(result.ok(), Some(Some("late".to_owned())));
            producer.await.ok();
        }
    }

    #[tokio::test]
    async fn tty_is_ignored_unless_forced() {
        let (_writer, mut reader) = tokio::io::duplex(64);
        let mut opts = options(false, false);
        opts.is_tty = true;
        assert_eq!(read_piped_stdin(&mut reader, opts).await.ok(), Some(None));
    }

    #[tokio::test(start_paused = true)]
    async fn empty_input_is_none() {
        let (writer, mut reader) = tokio::io::duplex(64);
        drop(writer);
        assert_eq!(
            read_piped_stdin(&mut reader, options(false, false))
                .await
                .ok(),
            Some(None)
        );
    }
}
