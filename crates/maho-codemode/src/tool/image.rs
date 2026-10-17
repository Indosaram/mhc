use std::{path::PathBuf, sync::Arc};
use base64::Engine;
use serde_json::Value;
use crate::output::{streaming_output::{OutputSink, OutputSinkOptions, DEFAULT_MAX_BYTES}, streaming_output_buffer::TailBuffer};
use super::image_resize::{EvalImageContent, EvalImageSdk, resize_eval_image};

pub const MAX_DISPLAY_IMAGES_PER_CELL: usize = 8;
pub const MAX_DISPLAY_IMAGE_BYTES_PER_CELL: usize = 24 * 1024 * 1024;
pub const MAX_JSON_OUTPUTS_PER_CELL: usize = 64;

pub struct EvalOutputOptions {
    pub artifact_path: Option<PathBuf>,
    pub head_bytes: usize,
    pub max_columns: usize,
    pub provider: Option<String>,
    pub api: Option<String>,
    pub image_sdk: Arc<dyn EvalImageSdk>,
}

pub struct EvalOutputResult {
    pub output: String,
    pub images: Vec<EvalImageContent>,
    pub json_outputs: Vec<Value>,
    pub has_markdown: bool,
    pub truncated: bool,
    pub notice: Option<String>,
    pub meta: Option<Value>,
}

pub struct EvalOutputCollector {
    options: EvalOutputOptions,
    sink: OutputSink,
    aggregate: TailBuffer,
    display_images: Vec<EvalImageContent>,
    images: Vec<EvalImageContent>,
    json_outputs: Vec<Value>,
    display_image_bytes: usize,
    display_images_elided: usize,
    json_outputs_elided: usize,
    has_markdown: bool,
    images_processed: bool,
}

impl EvalOutputCollector {
    pub fn new(options: EvalOutputOptions) -> Self {
        let sink = OutputSink::new(OutputSinkOptions {artifact_path:options.artifact_path.clone(),head_bytes:options.head_bytes,max_columns:options.max_columns,..Default::default()});
        Self { options, sink, aggregate:TailBuffer::new(DEFAULT_MAX_BYTES * 2),display_images:Vec::new(),images:Vec::new(),json_outputs:Vec::new(),display_image_bytes:0,display_images_elided:0,json_outputs_elided:0,has_markdown:false,images_processed:false }
    }

    pub async fn push(&mut self, text: &str) -> Result<(), String> {
        self.aggregate.append(text);
        self.sink.push(text,0).await.map_err(|error|error.to_string())
    }
    pub fn aggregate_text(&self) -> &str { self.aggregate.text() }

    pub async fn display(&mut self, mime_type: &str, data_base64: &str) -> Result<(), String> {
        if mime_type.starts_with("image/") {
            let Some((data, detected_mime)) = validate_display_image(data_base64) else {
                self.push("[display: image dropped — the image data is not valid base64 (truncated or corrupted?)]\n").await?;
                return Ok(());
            };
            if detected_mime.is_empty() {
                self.push("[display: image dropped — the image data is not a PNG, JPEG, GIF, or WebP image]\n").await?;
                return Ok(());
            }
            if self.display_images.len() >= MAX_DISPLAY_IMAGES_PER_CELL || self.display_image_bytes + data.len() > MAX_DISPLAY_IMAGE_BYTES_PER_CELL { self.display_images_elided += 1; return Ok(()); }
            self.display_images.push(EvalImageContent {data,mime_type:detected_mime});
            self.display_image_bytes += self.display_images.last().map_or(0, |image| image.data.len());
            return Ok(());
        }
        let data = base64::engine::general_purpose::STANDARD.decode(data_base64).map_err(|error|error.to_string())?;
        let text = String::from_utf8_lossy(&data);
        if mime_type == "application/json" {
            if self.json_outputs.len() >= MAX_JSON_OUTPUTS_PER_CELL { self.json_outputs_elided += 1; return Ok(()); }
            let value:Value = serde_json::from_str(&text).map_err(|error|format!("Invalid {mime_type} display payload: {error}"))?;
            let pretty = serde_json::to_string_pretty(&value).map_err(|error|error.to_string())?;
            self.json_outputs.push(sanitize_display_json(&value));
            let units = pretty.encode_utf16().count();
            let pretty = if units > 8000 {
                let prefix:Vec<_> = pretty.encode_utf16().take(8000).collect();
                format!("{}\n[…{}ch elided…]",String::from_utf16_lossy(&prefix),units-8000)
            } else { pretty };
            self.push(&format!("display[{}]:\n{pretty}\n",self.json_outputs.len())).await?;
        } else {
            if mime_type == "text/markdown" { self.has_markdown = true; }
            self.push(&if text.ends_with('\n') {text.into_owned()} else {format!("{text}\n")}).await?;
        }
        Ok(())
    }

    pub async fn push_value(&mut self, text: &str) -> Result<(), String> {
        self.aggregate.append(text);
        self.sink.push_unclamped(text).await.map_err(|error|error.to_string())
    }

    pub async fn flush(&mut self) -> Result<(), String> { self.sink.dump(None).await.map(|_|()).map_err(|error|error.to_string()) }

    pub async fn finish(&mut self) -> Result<EvalOutputResult, String> {
        if !self.images_processed {
            self.images_processed = true;
            for source in std::mem::take(&mut self.display_images) {
                let resized = resize_eval_image(source,self.options.provider.as_deref(),self.options.api.as_deref(),self.options.image_sdk.as_ref()).await?;
                let description = resized.dimension_note.unwrap_or_else(||format!("[{}]",resized.image.mime_type));
                self.images.push(resized.image);
                self.push(&format!("display image {}: {description}\n",self.images.len())).await?;
            }
        }
        if self.display_images_elided + self.json_outputs_elided > 0 {
            self.push(&format!("[{} display image(s) and {} JSON output(s) elided beyond per-cell caps]\n",self.display_images_elided,self.json_outputs_elided)).await?;
        }
        let mut summary = self.sink.dump(None).await.map_err(|error|error.to_string())?;
        if !summary.truncated && summary.total_lines > crate::output::streaming_output::DEFAULT_MAX_LINES {
            let truncated=crate::output::streaming_output::truncate_tail(&summary.output,crate::output::streaming_output::TruncationOptions {max_lines:crate::output::streaming_output::DEFAULT_MAX_LINES,max_bytes:usize::MAX});
            if summary.artifact_id.is_none() && let Some(path)=&self.options.artifact_path {
                tokio::fs::write(path,self.aggregate.text()).await.map_err(|error|error.to_string())?;
                summary.artifact_id=Some(path.clone());
            }
            summary.output=truncated.content;
            summary.truncated=true;
            summary.output_lines=truncated.output_lines;
            summary.output_bytes=truncated.output_bytes;
        }
        let meta = truncation_meta_from_summary(&summary,self.options.max_columns);
        Ok(EvalOutputResult {output:summary.output.trim_end().into(),images:self.images.clone(),json_outputs:self.json_outputs.clone(),has_markdown:self.has_markdown,truncated:summary.truncated,notice:summary.artifact_id.as_ref().map(|path|crate::output::output_meta::artifact_notice(&path.to_string_lossy())),meta})
    }
}

fn validate_display_image(encoded: &str) -> Option<(String, String)> {
    let data = encoded.chars().filter(|character| !character.is_whitespace()).collect::<String>();
    if data.is_empty() || data.len() % 4 != 0 || !data.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')) {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(&data).ok()?;
    let detected = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {"image/png"}
        else if bytes.starts_with(&[0xff,0xd8,0xff]) && bytes.get(3) != Some(&0xf7) {"image/jpeg"}
        else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {"image/gif"}
        else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {"image/webp"}
        else { return Some((String::new(), String::new())); };
    Some((data, detected.into()))
}

fn sanitize_display_json(value: &Value) -> Value {
    const MAX_DEPTH: usize = 8;
    const MAX_ITEMS: usize = 256;
    const MAX_STRING: usize = 4000;
    fn bound(value: &Value, depth: usize) -> Value {
        if depth >= MAX_DEPTH { return Value::String("[…depth capped…]".into()); }
        match value {
            Value::String(text) if text.len() > MAX_STRING => Value::String(format!("{}…", &text[..text.floor_char_boundary(MAX_STRING)])),
            Value::Array(items) => Value::Array(items.iter().take(MAX_ITEMS).map(|item| bound(item, depth + 1)).collect()),
            Value::Object(items) => Value::Object(items.iter().take(MAX_ITEMS).map(|(key, item)| (key.clone(), bound(item, depth + 1))).collect()),
            _ => value.clone(),
        }
    }
    bound(value, 0)
}

fn truncation_meta_from_summary(summary: &crate::output::streaming_output::OutputSummary, max_columns: usize) -> Option<Value> {
    use serde_json::json;
    if !summary.truncated {return None;}
    let mut meta=json!({"totalLines":summary.total_lines,"totalBytes":summary.total_bytes,"outputLines":summary.output_lines,"outputBytes":summary.output_bytes});
    if let Some(artifact)=&summary.artifact_id {meta["artifactId"]=json!(artifact.to_string_lossy());}
    if let Some(elided_bytes)=summary.elided_bytes.filter(|bytes|*bytes>0) {
        let elided_lines=summary.elided_lines.unwrap_or(summary.total_lines.saturating_sub(summary.output_lines));
        let kept_lines=summary.output_lines.saturating_sub(1);
        let head_lines=kept_lines.div_ceil(2);
        let tail_lines=kept_lines-head_lines;
        meta["direction"]=json!("middle");meta["truncatedBy"]=json!("middle");meta["elidedBytes"]=json!(elided_bytes);meta["elidedLines"]=json!(elided_lines);
        if head_lines>0 {meta["headRange"]=json!({"start":1,"end":head_lines});}
        if tail_lines>0 {meta["tailRange"]=json!({"start":summary.total_lines-tail_lines+1,"end":summary.total_lines});}
    } else {
        let dropped_bytes=summary.total_bytes.saturating_sub(summary.output_bytes);
        let clamped=summary.column_truncated_lines.unwrap_or(0);
        let column_only=clamped>0 && summary.column_dropped_bytes.unwrap_or(0)>=dropped_bytes;
        let byte_capped=summary.total_bytes.saturating_sub(summary.column_dropped_bytes.unwrap_or(0))>DEFAULT_MAX_BYTES;
        meta["direction"]=json!("tail");meta["truncatedBy"]=json!(if column_only {"columns"} else if byte_capped {"bytes"} else {"lines"});
        if column_only {meta["maxColumns"]=json!(max_columns);meta["columnTruncatedLines"]=json!(clamped);}
        else if byte_capped {meta["maxBytes"]=json!(DEFAULT_MAX_BYTES);}
        meta["shownRange"]=json!({"start":summary.total_lines.saturating_sub(summary.output_lines).saturating_add(1).max(1),"end":summary.total_lines});
    }
    Some(meta)
}

#[cfg(test)]
mod image_validation_tests {
    use super::*;

    #[test]
    fn validates_payload_signatures_and_overrides_declared_mime() {
        assert_eq!(validate_display_image("iVBORw0KGgoAAAANSUhEUg==").map(|(_, mime)| mime), Some("image/png".into()));
        assert_eq!(validate_display_image("/9j/4AAQSkZJRg==").map(|(_, mime)| mime), Some("image/jpeg".into()));
        assert!(validate_display_image("AAAA").is_some_and(|(_, mime)| mime.is_empty()));
        assert!(validate_display_image("not base64").is_none());
    }
}
