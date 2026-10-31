//! Port of senpi `experimental/micro/main.ts` (target 59d58cee): parse `--continue`/`-c`, open the
//! micro runtime, run the screen host, then close. The screen host (`tui.ts`) is injected because
//! it is not ported yet; the binary entry point is owned by `main.rs` (senpi-interactive).

use std::future::Future;
use std::rc::Rc;

use super::api::{MicroController, MicroViewSource};
use super::runtime::{OpenMicroOptions, OpenMicroResult};

/// senpi `parseArgs(argv)`.
pub fn parse_args(argv: &[String]) -> Result<OpenMicroOptions, String> {
    let mut options = OpenMicroOptions::default();
    for arg in argv {
        if arg == "--continue" || arg == "-c" {
            options.continue_session = true;
        } else {
            return Err(format!("Unknown argument: {arg}"));
        }
    }
    Ok(options)
}

/// senpi's module body: `openMicro`, then `runMicroTui(view, controller)`, closing in `finally`.
pub async fn run_micro<Open, OpenFut, Tui, TuiFut>(
    argv: &[String],
    open: Open,
    tui: Tui,
) -> Result<(), String>
where
    Open: FnOnce(OpenMicroOptions) -> OpenFut,
    OpenFut: Future<Output = Result<OpenMicroResult, String>>,
    Tui: FnOnce(Rc<dyn MicroViewSource>, Rc<dyn MicroController>) -> TuiFut,
    TuiFut: Future<Output = Result<(), String>>,
{
    let micro = open(parse_args(argv)?).await?;
    let view: Rc<dyn MicroViewSource> = micro.view.clone();
    let controller: Rc<dyn MicroController> = micro.controller.clone();
    let result = tui(view, controller).await;
    micro.close().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn continue_flags_select_the_newest_session() {
        assert!(!parse_args(&args(&[])).expect("no args").continue_session);
        assert!(
            parse_args(&args(&["--continue"]))
                .expect("long")
                .continue_session
        );
        assert!(parse_args(&args(&["-c"])).expect("short").continue_session);
    }

    #[test]
    fn unknown_arguments_are_rejected() {
        assert_eq!(
            parse_args(&args(&["-c", "--model"])).err().as_deref(),
            Some("Unknown argument: --model")
        );
    }
}
