//! The sealed internal hop as a bounded byte stream (senpi `internal-record-stream.ts`, senpi#3795): record
//! boundaries are invisible to the JSONL reader above it. Each direction has its own key, derived with HKDF-SHA256
//! from the internal key, salted by both handshake nonces, info `senpi-rpc-internal <direction>`.
//!
//! Reads are pulled by the reader, so wire backpressure is the reader's. A peer that ends after complete records has
//! sent them verifiably: they are delivered first, and only an incomplete trailing record is truncation. A record
//! that fails framing or verification closes the stream, logged once by its code (never key or plaintext material).
//! Each write seals at most `INTERNAL_RECORD_MAX` bytes into one record, and the next write waits until that record
//! has reached the wire.
use crate::internal_record_cipher::{
    INTERNAL_RECORD_MAX, InternalDirection, InternalRecordCipher, InternalRecordError,
};
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalRole {
    Client,
    Server,
}

fn derive(key: &[u8], salt: &[u8], direction: InternalDirection) -> [u8; 32] {
    let mut okm = [0u8; 32];
    let info = format!("senpi-rpc-internal {}", direction.label());
    // 32 bytes is far below HKDF-SHA256's 255 * 32 output limit.
    if hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), key)
        .expand(info.as_bytes(), &mut okm)
        .is_err()
    {
        unreachable!("32-byte HKDF output is always valid");
    }
    okm
}

/// senpi `createInternalRecordStream`. `prefix` holds wire bytes the handshake already read past its last frame.
pub fn create_internal_record_stream<S>(
    wire: S,
    key: &[u8],
    client_nonce: &[u8],
    server_nonce: &[u8],
    role: InternalRole,
    prefix: Vec<u8>,
) -> InternalRecordStream<S> {
    let mut salt = client_nonce.to_vec();
    salt.extend_from_slice(server_nonce);
    let cipher = |direction| InternalRecordCipher::new(&derive(key, &salt, direction), direction);
    let (send, receive) = match role {
        InternalRole::Client => (InternalDirection::C2s, InternalDirection::S2c),
        InternalRole::Server => (InternalDirection::S2c, InternalDirection::C2s),
    };
    InternalRecordStream {
        wire,
        sender: cipher(send),
        receiver: cipher(receive),
        raw: prefix,
        raw_offset: 0,
        header: [0; 4],
        header_used: 0,
        body: None,
        body_used: 0,
        plaintext: Vec::new(),
        plaintext_offset: 0,
        wire_done: false,
        failed: None,
        outgoing: Vec::new(),
        outgoing_offset: 0,
        sealed_len: None,
    }
}

pub struct InternalRecordStream<S> {
    wire: S,
    sender: InternalRecordCipher,
    receiver: InternalRecordCipher,
    raw: Vec<u8>,
    raw_offset: usize,
    header: [u8; 4],
    header_used: usize,
    body: Option<Vec<u8>>,
    body_used: usize,
    plaintext: Vec<u8>,
    plaintext_offset: usize,
    wire_done: bool,
    failed: Option<&'static str>,
    outgoing: Vec<u8>,
    outgoing_offset: usize,
    /// The plaintext length of the record in `outgoing`, reported once that record has reached the wire.
    sealed_len: Option<usize>,
}

impl<S> InternalRecordStream<S> {
    /// The raw wire under the seal, for tests that play a substituted relay.
    #[cfg(test)]
    pub(crate) fn into_inner_for_test(self) -> S {
        self.wire
    }

    fn fail(&mut self, code: &'static str) -> io::Error {
        if self.failed.is_none() {
            self.failed = Some(code);
            if code.starts_with("internal_record") {
                eprintln!("senpi rpc: internal hop closed: {code}");
            }
        }
        io::Error::other(code)
    }

    /// Consumes buffered wire bytes until one record opens or the bytes run out.
    fn consume(&mut self) -> Result<bool, InternalRecordError> {
        while self.raw_offset < self.raw.len() {
            if self.body.is_none() {
                let n = (4 - self.header_used).min(self.raw.len() - self.raw_offset);
                self.header[self.header_used..self.header_used + n]
                    .copy_from_slice(&self.raw[self.raw_offset..self.raw_offset + n]);
                self.header_used += n;
                self.raw_offset += n;
                if self.header_used != 4 {
                    break;
                }
                let length = u32::from_be_bytes(self.header) as usize;
                if !(17..=INTERNAL_RECORD_MAX + 16).contains(&length) {
                    return Err(InternalRecordError("internal_record_length"));
                }
                self.header_used = 0;
                self.body = Some(vec![0; length]);
            }
            let Some(body) = self.body.as_mut() else {
                break;
            };
            let n = (body.len() - self.body_used).min(self.raw.len() - self.raw_offset);
            body[self.body_used..self.body_used + n]
                .copy_from_slice(&self.raw[self.raw_offset..self.raw_offset + n]);
            self.body_used += n;
            self.raw_offset += n;
            if self.body_used == body.len() {
                let body = self.body.take().unwrap_or_default();
                self.body_used = 0;
                self.plaintext = self.receiver.open(&body)?;
                self.plaintext_offset = 0;
                return Ok(true);
            }
        }
        self.raw.clear();
        self.raw_offset = 0;
        Ok(false)
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for InternalRecordStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if let Some(code) = this.failed {
                return Poll::Ready(Err(io::Error::other(code)));
            }
            if this.plaintext_offset < this.plaintext.len() {
                let n = buf
                    .remaining()
                    .min(this.plaintext.len() - this.plaintext_offset);
                buf.put_slice(&this.plaintext[this.plaintext_offset..this.plaintext_offset + n]);
                this.plaintext_offset += n;
                return Poll::Ready(Ok(()));
            }
            match this.consume() {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => return Poll::Ready(Err(this.fail(error.0))),
            }
            if this.wire_done {
                if this.header_used != 0 || this.body.is_some() {
                    return Poll::Ready(Err(this.fail("internal_record_truncated")));
                }
                return Poll::Ready(Ok(()));
            }
            let mut chunk = vec![0u8; 16 * 1024];
            let mut read = ReadBuf::new(&mut chunk);
            match ready!(Pin::new(&mut this.wire).poll_read(cx, &mut read)) {
                Err(error) => {
                    this.failed = Some("internal_hop_lost");
                    return Poll::Ready(Err(error));
                }
                Ok(()) if read.filled().is_empty() => this.wire_done = true,
                Ok(()) => {
                    let filled = read.filled().len();
                    chunk.truncate(filled);
                    this.raw = chunk;
                    this.raw_offset = 0;
                }
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> InternalRecordStream<S> {
    fn poll_outgoing(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.outgoing_offset < self.outgoing.len() {
            let written = ready!(
                Pin::new(&mut self.wire).poll_write(cx, &self.outgoing[self.outgoing_offset..])
            )?;
            if written == 0 {
                return Poll::Ready(Err(io::Error::other("internal_hop_lost")));
            }
            self.outgoing_offset += written;
        }
        self.outgoing.clear();
        self.outgoing_offset = 0;
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for InternalRecordStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.failed.is_some() {
            return Poll::Ready(Err(io::Error::other("internal_hop_lost")));
        }
        // A write completes only once its record is downstream (senpi: await each downstream write); a caller
        // re-polling after Pending gets the sealed record's length, not a second record.
        if this.sealed_len.is_some() {
            ready!(this.poll_outgoing(cx))?;
            return Poll::Ready(Ok(this.sealed_len.take().unwrap_or(0)));
        }
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let end = buf.len().min(INTERNAL_RECORD_MAX);
        this.outgoing = this
            .sender
            .seal(&buf[..end])
            .map_err(|error| io::Error::other(error.0))?;
        this.outgoing_offset = 0;
        this.sealed_len = Some(end);
        ready!(this.poll_outgoing(cx))?;
        Poll::Ready(Ok(this.sealed_len.take().unwrap_or(0)))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_outgoing(cx))?;
        Pin::new(&mut this.wire).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_outgoing(cx))?;
        Pin::new(&mut this.wire).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const KEY: [u8; 32] = [7; 32];
    const CLIENT: [u8; 32] = [11; 32];
    const SERVER: [u8; 32] = [13; 32];

    fn c2s() -> InternalRecordCipher {
        let mut salt = CLIENT.to_vec();
        salt.extend_from_slice(&SERVER);
        InternalRecordCipher::new(
            &derive(&KEY, &salt, InternalDirection::C2s),
            InternalDirection::C2s,
        )
    }

    #[test]
    fn derives_the_independent_hkdf_key() {
        let mut salt = CLIENT.to_vec();
        salt.extend_from_slice(&SERVER);
        assert_eq!(
            hex::encode(derive(&KEY, &salt, InternalDirection::C2s)),
            "2142357ff086a7d3f778ed6d60855ad6d41e5bcac0d45921a22405b9206ea483"
        );
    }

    #[test]
    fn derives_the_independent_s2c_hkdf_key() {
        // HKDF-SHA256(ikm=[7;32], salt=[11;32]||[13;32], info="senpi-rpc-internal s2c", 32), computed independently.
        let mut salt = CLIENT.to_vec();
        salt.extend_from_slice(&SERVER);
        assert_eq!(
            hex::encode(derive(&KEY, &salt, InternalDirection::S2c)),
            "1502be6dfe733a9d081ec07bf76bdea99ff2593594647a0862f146d0e477b8af"
        );
    }

    #[tokio::test]
    async fn preserves_a_131_kib_jsonl_line_across_records_and_backpressure() {
        let (a, b) = tokio::io::duplex(32);
        let mut client =
            create_internal_record_stream(a, &KEY, &CLIENT, &SERVER, InternalRole::Client, vec![]);
        let mut host =
            create_internal_record_stream(b, &KEY, &CLIENT, &SERVER, InternalRole::Server, vec![]);
        let line = format!("{{\"text\":\"{}\"}}\n", "x".repeat(131 * 1024));
        let expected = line.clone();
        let writer = tokio::spawn(async move {
            client.write_all(line.as_bytes()).await.expect("write");
            client.flush().await.expect("flush");
            client
        });
        let mut received = vec![0; expected.len()];
        host.read_exact(&mut received).await.expect("read");
        assert_eq!(received, expected.as_bytes());
        let mut client = writer.await.expect("join");
        host.write_all(b"reply\n").await.expect("reply");
        host.flush().await.expect("flush");
        let mut reply = [0; 6];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(&reply, b"reply\n");
    }

    #[tokio::test]
    async fn handles_fragmented_headers_and_coalesced_records() {
        let (mut input, wire) = tokio::io::duplex(1024);
        let mut stream = create_internal_record_stream(
            wire,
            &KEY,
            &CLIENT,
            &SERVER,
            InternalRole::Server,
            vec![],
        );
        let mut sender = c2s();
        let mut bytes = sender.seal(b"one").expect("seal");
        bytes.extend(sender.seal(b"two").expect("seal"));
        input.write_all(&bytes[..1]).await.expect("w");
        input.write_all(&bytes[1..5]).await.expect("w");
        input.write_all(&bytes[5..]).await.expect("w");
        input.shutdown().await.expect("end");
        let mut received = Vec::new();
        stream.read_to_end(&mut received).await.expect("read");
        assert_eq!(received, b"onetwo");
    }

    #[tokio::test]
    async fn closes_on_incomplete_or_invalid_header_body_or_length_at_eof() {
        for part in ["header", "body", "length"] {
            let (mut input, wire) = tokio::io::duplex(1024);
            let mut stream = create_internal_record_stream(
                wire,
                &KEY,
                &CLIENT,
                &SERVER,
                InternalRole::Server,
                vec![],
            );
            let record = c2s().seal(b"never released").expect("seal");
            let bytes = match part {
                "header" => record[..2].to_vec(),
                "body" => record[..record.len() - 1].to_vec(),
                _ => vec![0; 4],
            };
            input.write_all(&bytes).await.expect("w");
            input.shutdown().await.expect("end");
            let mut received = Vec::new();
            assert!(stream.read_to_end(&mut received).await.is_err(), "{part}");
            assert!(received.is_empty(), "{part}");
        }
    }
}
