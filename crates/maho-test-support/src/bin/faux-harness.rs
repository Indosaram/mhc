use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut scenario = None;
    let mut out = None;
    let mut omo = false;
    let mut native = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scenario" => scenario = args.next(),
            "--out" => out = args.next(),
            "--omo" => omo = true,
            "--native" => native = true,
            other => {
                eprintln!("faux-harness: unknown argument {other}");
                return ExitCode::from(2);
            }
        }
    }

    let Some(scenario) = scenario else {
        eprintln!("faux-harness: --scenario required");
        return ExitCode::from(2);
    };
    let Some(out) = out else {
        eprintln!("faux-harness: --out required");
        return ExitCode::from(2);
    };

    let script = match maho_test_support::faux::load_script(&scenario) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("faux-harness: failed to load script {scenario}: {e}");
            return ExitCode::from(2);
        }
    };

    let session = maho_test_support::faux_session::FauxSession::new(script);
    let session = if omo { session.with_extension("omo") } else { session };

    let result = if native {
        match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime.block_on(session.run_native()),
            Err(error) => Err(Box::new(error) as Box<dyn std::error::Error + Send + Sync>),
        }
    } else { session.run_and_serialize() };
    match result {
        Ok(json) => {
            let formatted = match serde_json::to_string_pretty(&json) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("faux-harness: failed to serialize json: {e}");
                    return ExitCode::from(1);
                }
            };
            if let Err(e) = std::fs::write(&out, formatted) {
                eprintln!("faux-harness: failed to write {out}: {e}");
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("faux-harness: execution failed: {e}");
            ExitCode::from(1)
        }
    }
}
