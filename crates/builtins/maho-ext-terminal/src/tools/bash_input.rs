use super::context::{TerminalToolResult, error_result, text_result};
use crate::runtime_session::TerminalRuntimeSession;
use crate::shared::encode_keys;
pub fn bash_input_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","properties":{"bash_id":{"type":"string","description":"Session id returned by a run_in_background bash call."},"input":{"type":"string","description":"Literal text to write to the session's stdin."},"keys":{"type":"array","items":{"type":"string"},"description":"Named keys to send, e.g. ['ctrl+c'], ['enter'], ['up']. Sent after `input` when both are given."},"submit":{"type":"boolean","description":"Append Enter (\\r) after `input`. Default true when `input` is a non-empty string."}},"required":["bash_id"]})
}
pub struct BashInputInput<'a> {
    pub bash_id: &'a str,
    pub input: Option<&'a str>,
    pub keys: &'a [String],
    pub submit: Option<bool>,
}
/// What bash-input.ts uses of a runtime: `exited` and `session.write`.
pub trait BashInputRuntime {
    fn exited(&self) -> Result<bool, crate::runtime_session::RuntimeError>;
    fn write(&mut self, bytes: &[u8]) -> Result<(), crate::runtime_session::RuntimeError>;
}
impl BashInputRuntime for TerminalRuntimeSession {
    fn exited(&self) -> Result<bool, crate::runtime_session::RuntimeError> {
        TerminalRuntimeSession::exited(self)
    }
    fn write(&mut self, bytes: &[u8]) -> Result<(), crate::runtime_session::RuntimeError> {
        TerminalRuntimeSession::write(self, bytes)
    }
}
pub fn execute_bash_input<R: BashInputRuntime + ?Sized>(
    runtime: Option<&mut R>,
    input: BashInputInput<'_>,
) -> TerminalToolResult {
    let Some(runtime) = runtime else {
        return error_result(format!(
            "No terminal session found with id: {}",
            input.bash_id
        ));
    };
    match runtime.exited() {
        Ok(true) => {
            return error_result(format!(
                "Session {} is not running; cannot send input.",
                input.bash_id
            ));
        }
        Err(error) => return error_result(error.to_string()),
        Ok(false) => {}
    }
    let mut notes = Vec::new();
    if let Some(text) = input.input {
        let submit = input.submit.unwrap_or(!text.is_empty());
        let data = if submit {
            format!("{text}\r")
        } else {
            text.to_owned()
        };
        if let Err(error) = runtime.write(data.as_bytes()) {
            return error_result(format!("Failed to write input: {error}"));
        }
        notes.push(if submit {
            "wrote input + Enter".to_owned()
        } else {
            "wrote input".to_owned()
        });
    }
    if !input.keys.is_empty() {
        let keys = encode_keys(input.keys);
        if !keys.data.is_empty() {
            if let Err(error) = runtime.write(keys.data.as_bytes()) {
                return error_result(format!("Failed to send keys: {error}"));
            }
            notes.push(format!(
                "sent {} key(s)",
                input.keys.len() - keys.unknown.len()
            ));
        }
        if !keys.unknown.is_empty() {
            notes.push(format!("ignored unknown keys: {}", keys.unknown.join(", ")));
        }
    }
    if notes.is_empty() {
        return error_result("Nothing to send: provide `input` and/or `keys`.");
    }
    text_result(format!("Sent to {}: {}.", input.bash_id, notes.join("; ")))
}
#[cfg(test)]
mod tests {
    use super::*;
    /// A runtime that records every write; `fail` makes the next write fail like a closed PTY.
    #[derive(Default)]
    struct FakeRuntime {
        exited: bool,
        writes: Vec<String>,
        fail: bool,
    }
    impl BashInputRuntime for FakeRuntime {
        fn exited(&self) -> Result<bool, crate::runtime_session::RuntimeError> {
            Ok(self.exited)
        }
        fn write(&mut self, bytes: &[u8]) -> Result<(), crate::runtime_session::RuntimeError> {
            if self.fail {
                return Err(crate::runtime_session::RuntimeError::Pty(
                    std::io::Error::other("pty session is closed").into(),
                ));
            }
            self.writes
                .push(String::from_utf8_lossy(bytes).into_owned());
            Ok(())
        }
    }
    fn send(
        runtime: &mut FakeRuntime,
        input: Option<&str>,
        keys: &[&str],
        submit: Option<bool>,
    ) -> TerminalToolResult {
        let keys = keys.iter().map(|key| (*key).to_owned()).collect::<Vec<_>>();
        execute_bash_input(
            Some(runtime),
            BashInputInput {
                bash_id: "bash_1",
                input,
                keys: &keys,
                submit,
            },
        )
    }
    #[test]
    fn input_defaults_to_submit_and_respects_submit_false() {
        let mut runtime = FakeRuntime::default();
        let result = send(&mut runtime, Some("ls"), &[], None);
        assert_eq!(
            result.content[0].text,
            "Sent to bash_1: wrote input + Enter."
        );
        let result = send(&mut runtime, Some("partial"), &[], Some(false));
        assert_eq!(result.content[0].text, "Sent to bash_1: wrote input.");
        // An empty string is still written, but never auto-submitted.
        let result = send(&mut runtime, Some(""), &[], None);
        assert_eq!(result.content[0].text, "Sent to bash_1: wrote input.");
        let result = send(&mut runtime, Some(""), &[], Some(true));
        assert_eq!(
            result.content[0].text,
            "Sent to bash_1: wrote input + Enter."
        );
        assert_eq!(runtime.writes, ["ls\r", "partial", "", "\r"]);
    }
    #[test]
    fn keys_follow_input_and_unknown_keys_are_reported() {
        let mut runtime = FakeRuntime::default();
        let result = send(
            &mut runtime,
            Some("y"),
            &["ctrl+c", "bogus", "up"],
            Some(false),
        );
        assert_eq!(result.is_error, None);
        assert_eq!(
            result.content[0].text,
            "Sent to bash_1: wrote input; sent 2 key(s); ignored unknown keys: bogus."
        );
        assert_eq!(runtime.writes, ["y", "\x03\x1b[A"]);
        // Only unknown keys: nothing is written, the note still reports them.
        let mut runtime = FakeRuntime::default();
        let result = send(&mut runtime, None, &["nope"], None);
        assert_eq!(result.is_error, None);
        assert_eq!(
            result.content[0].text,
            "Sent to bash_1: ignored unknown keys: nope."
        );
        assert!(runtime.writes.is_empty());
    }
    #[test]
    fn nothing_to_send_exited_and_failed_writes_are_errors() {
        let mut runtime = FakeRuntime::default();
        let result = send(&mut runtime, None, &[], None);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.content[0].text,
            "Nothing to send: provide `input` and/or `keys`."
        );
        let mut exited = FakeRuntime {
            exited: true,
            ..Default::default()
        };
        let result = send(&mut exited, Some("x"), &[], None);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.content[0].text,
            "Session bash_1 is not running; cannot send input."
        );
        assert!(exited.writes.is_empty());
        let mut closed = FakeRuntime {
            fail: true,
            ..Default::default()
        };
        let result = send(&mut closed, Some("x"), &[], None);
        assert_eq!(
            result.content[0].text,
            "Failed to write input: pty session is closed"
        );
        let result = send(&mut closed, None, &["enter"], None);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.content[0].text,
            "Failed to send keys: pty session is closed"
        );
        let missing = execute_bash_input::<FakeRuntime>(
            None,
            BashInputInput {
                bash_id: "bash_999",
                input: Some("x"),
                keys: &[],
                submit: None,
            },
        );
        assert_eq!(missing.is_error, Some(true));
        assert_eq!(
            missing.content[0].text,
            "No terminal session found with id: bash_999"
        );
    }
    use std::time::Duration;
    #[tokio::test]
    async fn literal_input_and_enter_reach_live_pty_without_polling()
    -> Result<(), crate::runtime_session::RuntimeError> {
        let mut runtime = TerminalRuntimeSession::start(
            "read",
            crate::shared::test_sh_options(
                "stty -echo; printf 'ready\\n'; read value; printf 'received:%s\\n' \"$value\"",
            ),
        )?;
        let (history, mut output) = runtime.subscribe_output()?;
        let mut observed = history;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !observed.contains("ready") {
                observed.push_str(&output.recv().await.unwrap());
            }
        })
        .await
        .unwrap();
        let mut exit = runtime.subscribe_exit();
        let result = execute_bash_input(
            Some(&mut runtime),
            BashInputInput {
                bash_id: "mon_saved",
                input: Some("hello"),
                keys: &[],
                submit: None,
            },
        );
        assert_eq!(result.is_error, None);
        tokio::time::timeout(Duration::from_secs(5), async {
            while exit.borrow_and_update().is_none() {
                exit.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(runtime.full_output()?.contains("received:hello"));
        assert_eq!(
            execute_bash_input(
                Some(&mut runtime),
                BashInputInput {
                    bash_id: "mon_saved",
                    input: Some("late"),
                    keys: &[],
                    submit: None
                }
            )
            .is_error,
            Some(true)
        );
        runtime.dispose()
    }
    #[test]
    fn missing_input_and_real_pty_resize() -> Result<(), crate::runtime_session::RuntimeError> {
        assert_eq!(
            execute_bash_input::<TerminalRuntimeSession>(
                None,
                BashInputInput {
                    bash_id: "bash_999",
                    input: Some("x"),
                    keys: &[],
                    submit: None
                }
            )
            .is_error,
            Some(true)
        );
        let runtime = TerminalRuntimeSession::start(
            "read",
            crate::shared::test_sh_options("read value").timeout(Duration::from_secs(5)),
        )?;
        let resized =
            super::super::bash_resize::execute_bash_resize(Some(&runtime), "bash_1", 100.0, 30.0);
        assert_eq!(resized.is_error, None);
        assert_eq!(resized.content[0].text, "Resized bash_1 to 100x30.");
        runtime.dispose()
    }
}
