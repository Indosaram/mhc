use thiserror::Error;

pub const DEFAULT_MAX_FRAME_LENGTH: u32 = 16 * 1024 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct FrameError(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Open,
    Ended,
    Failed,
}

pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    let length = u32::try_from(payload.len())
        .map_err(|_| FrameError("Frame payload exceeds the unsigned 32-bit length limit".into()))?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

pub struct FrameDecoder {
    header: [u8; 4],
    header_length: usize,
    max_frame_length: u32,
    payload: Vec<u8>,
    expected: Option<usize>,
    state: State,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_FRAME_LENGTH)
    }
}

impl FrameDecoder {
    pub fn new(max_frame_length: u32) -> Self {
        Self {
            header: [0; 4],
            header_length: 0,
            max_frame_length,
            payload: Vec::new(),
            expected: None,
            state: State::Open,
        }
    }

    fn check_open(&self) -> Result<(), FrameError> {
        match self.state {
            State::Open => Ok(()),
            State::Ended => Err(FrameError("Frame decoder has ended".into())),
            State::Failed => Err(FrameError("Frame decoder has failed".into())),
        }
    }

    fn fail(&mut self, message: String) -> FrameError {
        self.state = State::Failed;
        self.header_length = 0;
        self.payload.clear();
        self.expected = None;
        FrameError(message)
    }

    pub fn push(&mut self, mut chunk: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        self.check_open()?;
        let mut frames = Vec::new();
        while !chunk.is_empty() {
            if self.expected.is_none() {
                let count = (4 - self.header_length).min(chunk.len());
                self.header[self.header_length..self.header_length + count]
                    .copy_from_slice(&chunk[..count]);
                self.header_length += count;
                chunk = &chunk[count..];
                if self.header_length < 4 {
                    continue;
                }
                self.header_length = 0;
                let length = u32::from_be_bytes(self.header);
                if length > self.max_frame_length {
                    return Err(self.fail(format!(
                        "Frame length {length} exceeds configured limit of {}",
                        self.max_frame_length
                    )));
                }
                if length == 0 {
                    frames.push(Vec::new());
                    continue;
                }
                self.expected = Some(
                    usize::try_from(length)
                        .map_err(|_| self.fail("Frame length cannot fit in memory".into()))?,
                );
            }
            if let Some(expected) = self.expected {
                let count = (expected - self.payload.len()).min(chunk.len());
                // Grow only with received bytes, never from an untrusted declared length.
                self.payload.extend_from_slice(&chunk[..count]);
                chunk = &chunk[count..];
                if self.payload.len() == expected {
                    frames.push(std::mem::take(&mut self.payload));
                    self.expected = None;
                }
            }
        }
        Ok(frames)
    }

    pub fn end(&mut self) -> Result<(), FrameError> {
        self.check_open()?;
        if self.header_length != 0 || self.expected.is_some() {
            return Err(self.fail("Truncated frame at end of stream".into()));
        }
        self.state = State::Ended;
        Ok(())
    }
}
