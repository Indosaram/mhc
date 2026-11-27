//! Port of `packages/coding-agent/src/modes/rpc/machine-op/terminal-input.ts`.
//!
//! The relay host's stdin path: one input queue per handle so writes never interleave; `paste`
//! brackets the text when the program turned bracketed paste on; named keys go `KEY_GAP_MS` apart.

use crate::machine_op::live_handle::{After, HandleEvent, LiveHandle};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::{broadcast, oneshot};

const KEY_GAP_MS: u64 = 30;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

fn named_key(lower: &str) -> Option<&'static [u8]> {
    match lower {
        "enter" | "return" => Some(b"\r"),
        "tab" => Some(b"\t"),
        "escape" | "esc" => Some(b"\x1b"),
        "backspace" => Some(b"\x7f"),
        "space" => Some(b" "),
        "up" => Some(b"\x1b[A"),
        "down" => Some(b"\x1b[B"),
        "right" => Some(b"\x1b[C"),
        "left" => Some(b"\x1b[D"),
        "home" => Some(b"\x1b[H"),
        "end" => Some(b"\x1b[F"),
        "delete" => Some(b"\x1b[3~"),
        "pageup" => Some(b"\x1b[5~"),
        "pagedown" => Some(b"\x1b[6~"),
        _ => None,
    }
}

/// `ctrl+c`, `alt+x`, `enter`, a single character; `None` for a key this encoder does not know.
pub fn encode_key(key: &str) -> Option<Vec<u8>> {
    let k = key.to_ascii_lowercase();
    if let Some(ctrl_rest) = k.strip_prefix("ctrl+") {
        let mut chars = ctrl_rest.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            let code = ch as u32;
            if (0x40..=0x7f).contains(&code) {
                return Some(vec![(code as u8) & 0x1f]);
            }
        }
        return None;
    }
    if let Some(alt_rest) = key.strip_prefix("alt+")
        && !alt_rest.is_empty()
    {
        let rest = encode_key(alt_rest)?;
        let mut out = Vec::with_capacity(1 + rest.len());
        out.push(0x1b);
        out.extend_from_slice(&rest);
        return Some(out);
    }
    if let Some(named) = named_key(&k) {
        return Some(named.to_vec());
    }
    if key.chars().count() == 1 {
        return Some(key.as_bytes().to_vec());
    }
    None
}

#[derive(Clone, Debug, Default)]
pub struct StdinInput {
    pub data: Option<Vec<u8>>,
    pub keys: Option<Vec<Vec<u8>>>,
    pub paste: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StdinEffect {
    pub accepted_bytes: usize,
    pub observed: Option<bool>,
}

pub struct InputQueues {
    after: After,
    queues: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl InputQueues {
    pub fn new(after: After) -> Self {
        Self {
            after,
            queues: Mutex::new(HashMap::new()),
        }
    }

    fn lock_for(&self, handle_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            map.entry(handle_id.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    fn release_if_idle(&self, handle_id: &str, lock: Arc<tokio::sync::Mutex<()>>) {
        let mut map = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
        if Arc::strong_count(&lock) == 2 {
            map.remove(handle_id);
        }
    }

    pub async fn write(
        &self,
        h: &Arc<LiveHandle>,
        input: StdinInput,
        expect_ms: Option<u64>,
    ) -> StdinEffect {
        let handle_lock = self.lock_for(&h.id);
        let _guard = handle_lock.lock().await;
        let effect = self.run(h, input, expect_ms).await;
        drop(_guard);
        self.release_if_idle(&h.id, handle_lock);
        effect
    }

    async fn run(
        &self,
        h: &Arc<LiveHandle>,
        input: StdinInput,
        expect_ms: Option<u64>,
    ) -> StdinEffect {
        let watch = expect_ms.map(|ms| {
            let rx = h.subscribe();
            let (tx, timeout_rx) = oneshot::channel::<()>();
            let cancel = (self.after)(
                ms,
                Box::new(move || {
                    let _ = tx.send(());
                }),
            );
            (rx, timeout_rx, cancel)
        });

        let mut accepted = 0usize;
        if let Some(data) = &input.data
            && !data.is_empty()
        {
            let bracket = input.paste && h.bracketed_paste();
            if bracket {
                h.write(PASTE_START);
            }
            h.write(data);
            accepted += data.len();
            if bracket {
                h.write(PASTE_END);
            }
        }
        if let Some(keys) = &input.keys {
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    let (tx, rx) = oneshot::channel::<()>();
                    let _cancel = (self.after)(
                        KEY_GAP_MS,
                        Box::new(move || {
                            let _ = tx.send(());
                        }),
                    );
                    let _ = rx.await;
                }
                h.write(key);
                accepted += key.len();
            }
        }

        let observed = match watch {
            None => None,
            Some((mut rx, mut timeout_rx, cancel)) => {
                let seen = loop {
                    tokio::select! {
                        biased;
                        ev = rx.recv() => {
                            match ev {
                                Ok(HandleEvent::Output { .. }) => break true,
                                Ok(HandleEvent::State(st)) if !matches!(
                                    st,
                                    crate::machine_op::live_handle::MachineLifecycle::Running
                                        | crate::machine_op::live_handle::MachineLifecycle::WaitingInput
                                ) => break false,
                                Ok(_) => continue,
                                Err(broadcast::error::RecvError::Lagged(_)) => break true,
                                Err(broadcast::error::RecvError::Closed) => break false,
                            }
                        }
                        _ = &mut timeout_rx => break false,
                    }
                };
                cancel();
                Some(seen)
            }
        };

        StdinEffect {
            accepted_bytes: accepted,
            observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_named_ctrl_alt_and_single_char_keys() {
        assert_eq!(encode_key("enter"), Some(b"\r".to_vec()));
        assert_eq!(encode_key("ctrl+c"), Some(vec![0x03]));
        assert_eq!(encode_key("alt+x"), Some(vec![0x1b, b'x']));
        assert_eq!(encode_key("alt+enter"), Some(vec![0x1b, b'\r']));
        assert_eq!(encode_key("a"), Some(b"a".to_vec()));
        assert_eq!(encode_key("unknown_key"), None);
        assert_eq!(encode_key("ctrl+enter"), None);
    }
}
