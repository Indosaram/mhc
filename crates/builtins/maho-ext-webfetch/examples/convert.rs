use std::io::{self, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let cases: Vec<serde_json::Value> = serde_json::from_str(&input)?;
    let mut outputs = Vec::new();
    for case in cases {
        let html = case["html"].as_str().ok_or("html must be a string")?;
        let url = case["url"].as_str().ok_or("url must be a string")?;
        outputs.push(serde_json::json!({
            "markdown": maho_ext_webfetch::webfetch::content::html_to_markdown(html, url),
            "text": maho_ext_webfetch::webfetch::content::html_to_text(html, url),
        }));
    }
    println!("{}", serde_json::to_string(&outputs)?);
    Ok(())
}
