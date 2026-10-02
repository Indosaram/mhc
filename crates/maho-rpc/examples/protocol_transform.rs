use std::io::{self,Read,Write};
use maho_rpc::{json_event::to_json_event,jsonl::{JsonlLineReader,LineRecord,MAX_RPC_LINE_CHARACTERS,serialize_json_line},media_placeholders::omit_inline_media};
use serde_json::Value;

fn main() -> Result<(),Box<dyn std::error::Error>> {
    let mut reader = JsonlLineReader::new(MAX_RPC_LINE_CHARACTERS)?;
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut buffer = [0;8192];
    loop {
        let count = stdin.read(&mut buffer)?;
        let records = if count == 0 { reader.finish() } else { reader.push(&buffer[..count]) };
        for record in records {
            match record {
                LineRecord::Line(line) => {
                    let value:Value = serde_json::from_str(&line)?;
                    let delta = to_json_event(&value)?;
                    let media = omit_inline_media(delta.as_ref());
                    stdout.write_all(serialize_json_line(media.as_ref())?.as_bytes())?;
                }
                LineRecord::Oversized => return Err("oversized input record".into()),
            }
        }
        if count == 0 { break; }
    }
    stdout.flush()?;
    Ok(())
}
