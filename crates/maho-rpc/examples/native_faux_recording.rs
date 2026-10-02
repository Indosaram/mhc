use maho_rpc::{json_event::to_json_event, jsonl::serialize_json_line};
use maho_test_support::{faux::load_script, faux_session::FauxSession};
use std::io::Write;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let result = FauxSession::new(load_script("hello")?).run_native().await?;
    let events = result["events"].as_array().ok_or("missing native events")?;
    let path = std::env::args().nth(1).ok_or("usage: native_faux_recording <output.jsonl>")?;
    let mut output = std::fs::File::create(path)?;
    for event in events {
        output.write_all(serialize_json_line(to_json_event(event)?.as_ref())?.as_bytes())?;
    }
    Ok(())
}
