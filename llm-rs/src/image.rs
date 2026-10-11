//! Provider-agnostic image generation request/response types.
//!
//! These describe the OpenAI Images API surface (`images/generations` and
//! `images/edits`): the request body is built here so every caller produces the
//! same JSON, and the response is parsed here so callers only see the returned
//! base64 payloads.

use base64::Engine;
use serde::Serialize;

/// A request to generate a new image or edit existing ones.
#[derive(Clone, Debug, PartialEq)]
pub enum ImageRequest {
    Generate(ImageParams),
    Edit {
        params: ImageParams,
        images: Vec<ImageInput>,
    },
}

/// Common image parameters shared by generation and edits.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageParams {
    pub prompt: String,
    /// Number of images to request. `None` omits the field from the request.
    pub n: Option<u32>,
    /// Requested size (`1024x1024`, `auto`, ...). `None` sends `"auto"`.
    pub size: Option<String>,
    /// Requested quality (`low`/`medium`/`high`/`auto`). `None` sends `"auto"`.
    pub quality: Option<String>,
    /// `"opaque"` or `"transparent"`.
    pub background: String,
    /// Requested output format (`png`/`jpeg`/`webp`). `None` omits the field.
    pub output_format: Option<String>,
}

/// An input image for an edit, carried as raw bytes plus its media type.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageInput {
    pub media_type: String,
    pub data: Vec<u8>,
}

impl ImageInput {
    /// Build the `data:<media_type>;base64,<...>` URI the API expects.
    pub fn to_data_uri(&self) -> String {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&self.data);
        format!("data:{};base64,{}", self.media_type, encoded)
    }
}

/// The images returned by an image request.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageResponse {
    /// One `b64_json` payload per returned item, in response order.
    pub images: Vec<String>,
    /// Output format echoed by the API; used only as a file-extension fallback.
    pub output_format: Option<String>,
}

impl ImageResponse {
    /// Parse an Images API response body.
    ///
    /// A missing `data` array, a missing `b64_json`, or a non-string `b64_json`
    /// is an error; an empty `data` array yields no images.
    pub fn from_json(body: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(body)
            .map_err(|e| format!("failed to parse image response: {e}"))?;

        let data = value
            .get("data")
            .ok_or_else(|| "image response is missing the 'data' array".to_string())?;
        let items = data
            .as_array()
            .ok_or_else(|| "image response 'data' is not an array".to_string())?;

        let mut images = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let b64 = item
                .get("b64_json")
                .ok_or_else(|| format!("image response data[{index}] is missing 'b64_json'"))?;
            let b64 = b64
                .as_str()
                .ok_or_else(|| format!("image response data[{index}].b64_json is not a string"))?;
            images.push(b64.to_string());
        }

        let output_format = value
            .get("output_format")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        Ok(Self {
            images,
            output_format,
        })
    }
}

impl ImageRequest {
    /// Serialize the request body for the OpenAI Images API, injecting the
    /// profile-level `model`.
    pub fn to_json_body(&self, model: &str) -> Result<String, String> {
        match self {
            ImageRequest::Generate(params) => {
                serde_json::to_string(&GenerateBody::new(params, model)).map_err(|e| e.to_string())
            }
            ImageRequest::Edit { params, images } => {
                let images = images.iter().map(|input| ImageUrl {
                    image_url: input.to_data_uri(),
                });
                serde_json::to_string(&EditBody::new(params, model, images.collect()))
                    .map_err(|e| e.to_string())
            }
        }
    }
}

#[derive(Serialize)]
struct GenerateBody<'a> {
    prompt: &'a str,
    background: &'a str,
    model: &'a str,
    quality: &'a str,
    size: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_format: Option<&'a str>,
}

impl<'a> GenerateBody<'a> {
    fn new(params: &'a ImageParams, model: &'a str) -> Self {
        Self {
            prompt: &params.prompt,
            background: &params.background,
            model,
            quality: params.quality.as_deref().unwrap_or("auto"),
            size: params.size.as_deref().unwrap_or("auto"),
            n: params.n,
            output_format: params.output_format.as_deref(),
        }
    }
}

#[derive(Serialize)]
struct EditBody<'a> {
    images: Vec<ImageUrl>,
    prompt: &'a str,
    background: &'a str,
    model: &'a str,
    quality: &'a str,
    size: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_format: Option<&'a str>,
}

impl<'a> EditBody<'a> {
    fn new(params: &'a ImageParams, model: &'a str, images: Vec<ImageUrl>) -> Self {
        Self {
            images,
            prompt: &params.prompt,
            background: &params.background,
            model,
            quality: params.quality.as_deref().unwrap_or("auto"),
            size: params.size.as_deref().unwrap_or("auto"),
            n: params.n,
            output_format: params.output_format.as_deref(),
        }
    }
}

#[derive(Serialize)]
struct ImageUrl {
    image_url: String,
}
