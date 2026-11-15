use serde_json::Value;

pub const DEFAULT_IMAGE_MODEL: &str = "gpt-image-2.6-sunburst";

/// The native Responses `image_generation` tool has no client-side retry, so it stays on the newest
/// model verified end to end through the ChatGPT subscription backend until 2.6 is proven there
/// (senpi#5820).
pub const NATIVE_IMAGE_MODEL: &str = "gpt-image-2.5-sunburst";

pub const IMAGE_MODEL_NAMES: &[(&str, &str)] = &[
    ("gpt-image-2.6-sunburst", "GPT Image 2.6 Sunburst"),
    ("gpt-image-2.6-flare", "GPT Image 2.6 Flare"),
    ("gpt-image-2.5-sunburst", "GPT Image 2.5 Sunburst"),
    ("gpt-image-2.5-flare", "GPT Image 2.5 Flare"),
    ("gpt-image-2", "GPT Image 2"),
];

/// Gateway allowlists lag OpenAI releases: a 2.6 id an endpoint does not serve answers on its 2.5
/// counterpart, which also supplies the 2.6 row's input modalities and rates (the 2.6 pair bills at
/// the 2.5 rates).
pub const IMAGE_MODEL_FALLBACKS: &[(&str, &str)] = &[
    ("gpt-image-2.6-sunburst", "gpt-image-2.5-sunburst"),
    ("gpt-image-2.6-flare", "gpt-image-2.5-flare"),
];

/// Only GPT Image 2.6 accepts the `complexity` request field; older models reject it.
pub fn supports_complexity(model: &str) -> bool {
    matches!(model, "gpt-image-2.6-sunburst" | "gpt-image-2.6-flare")
}

pub fn model_display_name(model: &str) -> Option<&'static str> {
    IMAGE_MODEL_NAMES
        .iter()
        .find(|(id, _)| *id == model)
        .map(|(_, name)| *name)
}

pub fn fallback_model(model: &str) -> Option<&'static str> {
    IMAGE_MODEL_FALLBACKS
        .iter()
        .find(|(id, _)| *id == model)
        .map(|(_, fallback)| *fallback)
}
pub struct GenerateImageBase<'a> {
    pub model: &'a str,
    pub size: &'a str,
    pub quality: &'a str,
    pub background: &'a str,
    pub output_format: &'a str,
    pub requested: usize,
    pub source: &'a str,
}
#[derive(Clone, Copy)]
pub enum FailureReason {
    MissingConfig,
    ProviderNativeBypass,
    InvalidParams,
    WriteFailed,
    ProviderError,
}
impl FailureReason {
    fn wire(self) -> &'static str {
        match self {
            Self::MissingConfig => "missing_config",
            Self::ProviderNativeBypass => "provider_native_bypass",
            Self::InvalidParams => "invalid_params",
            Self::WriteFailed => "write_failed",
            Self::ProviderError => "provider_error",
        }
    }
}
pub fn failure(message: &str, reason: FailureReason, base: GenerateImageBase<'_>) -> Value {
    serde_json::json!({"content":[{"type":"text","text":message}],"details":{
        "model":base.model,"size":base.size,"quality":base.quality,"background":base.background,
        "outputFormat":base.output_format,"requested":base.requested,"source":base.source,
        "paths":[],"generated":0,"revisedPrompts":[],"error":message,"reason":reason.wire()
    }})
}

pub fn parameters() -> Value {
    serde_json::json!({
      "type": "object",
      "required": [
        "prompt"
      ],
      "properties": {
        "prompt": {
          "type": "string",
          "minLength": 1,
          "maxLength": 32000,
          "description": "Detailed description of the desired image, including the end state for edits."
        },
        "model": {
          "anyOf": [
            {
              "type": "string",
              "const": "gpt-image-2.6-sunburst"
            },
            {
              "type": "string",
              "const": "gpt-image-2.6-flare"
            },
            {
              "type": "string",
              "const": "gpt-image-2.5-sunburst"
            },
            {
              "type": "string",
              "const": "gpt-image-2.5-flare"
            },
            {
              "type": "string",
              "const": "gpt-image-2"
            }
          ],
          "default": "gpt-image-2.6-sunburst",
          "description": "gpt-image-2.6-sunburst (default) is the most capable: highest quality, dense text, and the most consistent details across repeated edits; gpt-image-2.6-flare is the fast model for iteration and layouts. The 2.5 pair and gpt-image-2 are previous generations."
        },
        "size": {
          "type": "string",
          "default": "auto",
          "description": "auto, 1024x1024, 1536x1024, 1024x1536, or any WIDTHxHEIGHT with both divisible by 16, aspect between 1:3 and 3:1, max 3840x2160 (e.g. 2048x2048, 2048x1152, 3840x2160)"
        },
        "quality": {
          "anyOf": [
            {
              "type": "string",
              "const": "auto"
            },
            {
              "type": "string",
              "const": "low"
            },
            {
              "type": "string",
              "const": "medium"
            },
            {
              "type": "string",
              "const": "high"
            },
            {
              "type": "string",
              "const": "xhigh"
            },
            {
              "type": "string",
              "const": "max"
            }
          ],
          "default": "auto",
          "description": "Rendering quality. Defaults to auto. xhigh/max need gpt-image-2.5 or 2.6 and are slower and costlier."
        },
        "complexity": {
          "anyOf": [
            {
              "type": "string",
              "const": "low"
            },
            {
              "type": "string",
              "const": "medium"
            },
            {
              "type": "string",
              "const": "high"
            }
          ],
          "description": "GPT Image 2.6 only: how much the model plans before rendering, at no extra cost. Omitted means high. low suits focused edits; medium/high help dense text and complex layouts but are slower and may change more."
        },
        "reference_image_paths": {
          "type": "array",
          "items": {
            "type": "string",
            "minLength": 1
          },
          "minItems": 1,
          "maxItems": 5,
          "description": "Local PNG/JPEG/WEBP images to edit or use as references (1-5 files, each at most 50 MB). Paths may be absolute or relative to the working directory."
        },
        "mask_image_path": {
          "type": "string",
          "minLength": 1,
          "description": "Inpainting mask (PNG with an alpha channel, same size as the first reference image): transparent areas are repainted. Requires reference_image_paths."
        },
        "background": {
          "anyOf": [
            {
              "type": "string",
              "const": "auto"
            },
            {
              "type": "string",
              "const": "transparent"
            },
            {
              "type": "string",
              "const": "opaque"
            }
          ],
          "default": "auto",
          "description": "Output background. transparent needs output_format png or webp and keeps the alpha channel; auto lets the model decide."
        },
        "output_format": {
          "anyOf": [
            {
              "type": "string",
              "const": "png"
            },
            {
              "type": "string",
              "const": "jpeg"
            },
            {
              "type": "string",
              "const": "webp"
            }
          ],
          "default": "png",
          "description": "File format. png (default) is lossless and supports transparency; jpeg is fastest; webp is small and supports transparency."
        },
        "output_compression": {
          "type": "integer",
          "minimum": 0,
          "maximum": 100,
          "description": "Compression level 0-100 for jpeg or webp output only (100 = best quality)."
        },
        "moderation": {
          "anyOf": [
            {
              "type": "string",
              "const": "auto"
            },
            {
              "type": "string",
              "const": "low"
            }
          ],
          "default": "auto",
          "description": "Content filter strictness: auto (standard) or low (less restrictive)."
        },
        "n": {
          "type": "integer",
          "minimum": 1,
          "maximum": 10,
          "default": 1,
          "description": "How many variants of this one prompt to generate; distinct assets need distinct calls."
        },
        "output_path": {
          "type": "string",
          "minLength": 1,
          "description": "Where to write the image, relative to the working directory. The extension must match output_format (.png, .jpg/.jpeg, .webp) or be omitted. Defaults to generated-images/."
        }
      },
      "additionalProperties": false
    })
}
