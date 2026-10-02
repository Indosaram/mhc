use serde_json::Value;

pub const DEFAULT_IMAGE_MODEL: &str = "gpt-image-2.5-sunburst";

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
      "default": "gpt-image-2.5-sunburst",
      "description": "Sunburst (default) is the most capable: highest quality, precise edits, reference fidelity, best for final assets; Flare is the small model optimized for speed, with quality comparable to gpt-image-2; gpt-image-2 is the previous generation."
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
      "description": "Rendering quality. Defaults to auto. xhigh/max are gpt-image-2.5 only, slower and costlier."
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
