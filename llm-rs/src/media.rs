//! Media data types for LLM conversations (images, PDFs, etc.).
//!
//! Provides `MediaData` for referencing media files on disk (lazy-loaded and
//! cached) and `ContentPart` for mixing text and media in user messages.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// References a media file in a session's media directory.
///
/// Serializes only `relative_path` and `media_type`. The `cached_data`
/// field is lazily populated on first call to `get_data()`.
#[derive(Clone)]
pub struct MediaData {
    /// Relative path from the media dir, e.g. "uuid.png".
    relative_path: String,

    /// MIME type, e.g. "image/png", "image/jpeg".
    media_type: String,

    /// Lazily cached (media_dir, bytes). Set on first `get_data()` call.
    cached_data: OnceLock<(PathBuf, Vec<u8>)>,
}

impl std::fmt::Debug for MediaData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaData")
            .field("relative_path", &self.relative_path)
            .field("media_type", &self.media_type)
            .field("cached", &self.cached_data.get().is_some())
            .finish()
    }
}

impl MediaData {
    /// Create a new `MediaData` with the given relative path and media type.
    pub fn new(relative_path: String, media_type: String) -> Self {
        // Defense-in-depth: validate against path traversal in case callers
        // forgot to validate upstream.
        if relative_path.is_empty()
            || relative_path.contains('/')
            || relative_path.contains('\\')
            || relative_path.contains("..")
        {
            tracing::error!(
                "MediaData::new() called with potentially dangerous relative_path (caller should have validated): {:?}",
                relative_path
            );
        }
        Self {
            relative_path,
            media_type,
            cached_data: OnceLock::new(),
        }
    }

    /// Get the media bytes, loading from disk on first call.
    ///
    /// `media_dir` is the absolute path to the session's media directory.
    /// On first call, reads `media_dir / relative_path` and caches the bytes
    /// together with the directory path. Subsequent calls validate that the
    /// same `media_dir` is used and return the cached bytes.
    pub fn get_data(&self, media_dir: &Path) -> Result<&[u8]> {
        if let Some((cached_dir, cached_bytes)) = self.cached_data.get() {
            if cached_dir == media_dir {
                return Ok(cached_bytes);
            }
            return Err(anyhow::anyhow!(
                "MediaData::get_data called with different media_dir than cached"
            ));
        }

        // Canonicalize and verify the resolved path stays within media_dir
        let path = resolve_media_path(media_dir, &self.relative_path)?;
        let bytes = std::fs::read(&path)
            .with_context(|| format!("Failed to read media file: {}", path.display()))?;

        self.cached_data
            .set((media_dir.to_path_buf(), bytes))
            .map_err(|_| anyhow::anyhow!("MediaData cached_data already set (race condition)"))?;

        Ok(&self.cached_data.get().expect("just set").1)
    }

    /// Return the media type (MIME type) of this media.
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    /// Return the relative path of this media file within the media directory.
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }
}

impl Serialize for MediaData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("MediaData", 2)?;
        state.serialize_field("relative_path", &self.relative_path)?;
        state.serialize_field("media_type", &self.media_type)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for MediaData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct MediaDataHelper {
            relative_path: String,
            media_type: String,
        }

        let helper = MediaDataHelper::deserialize(deserializer)?;
        Ok(MediaData {
            relative_path: helper.relative_path,
            media_type: helper.media_type,
            cached_data: OnceLock::new(),
        })
    }
}

/// A single part of a user message — either plain text or a media reference.
///
/// **Important:** `Text` must be listed before `Media` so that
/// `#[serde(untagged)]` deserialization tries `Text(String)` first.
/// A `String` would always successfully deserialize, preventing `Media`
/// from ever matching if it were listed first.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentPart {
    /// Plain text content.
    Text(String),
    /// A media reference (image, PDF, etc.).
    Media(MediaData),
}

impl ContentPart {
    /// Returns the text content if this is a `Text` variant, or `None` otherwise.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentPart::Text(t) => Some(t.as_str()),
            ContentPart::Media(_) => None,
        }
    }
}

/// Join all `Text` parts into a single string, skipping non-text parts.
pub fn join_text_parts(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("")
}

impl From<String> for ContentPart {
    fn from(s: String) -> Self {
        ContentPart::Text(s)
    }
}

impl From<MediaData> for ContentPart {
    fn from(media: MediaData) -> Self {
        ContentPart::Media(media)
    }
}

impl PartialEq<str> for ContentPart {
    fn eq(&self, other: &str) -> bool {
        match self {
            ContentPart::Text(t) => t == other,
            ContentPart::Media(_) => false,
        }
    }
}

impl PartialEq<&str> for ContentPart {
    fn eq(&self, other: &&str) -> bool {
        match self {
            ContentPart::Text(t) => t == *other,
            ContentPart::Media(_) => false,
        }
    }
}

/// Resolve `media_dir / filename` and verify the canonicalized result is
/// still within `media_dir`. Returns the canonical path.
pub fn resolve_media_path(media_dir: &Path, filename: &str) -> Result<PathBuf> {
    let joined = media_dir.join(filename);
    let canonical = std::fs::canonicalize(&joined)
        .with_context(|| format!("Failed to resolve media path: {}", joined.display()))?;
    let canonical_dir = std::fs::canonicalize(media_dir)
        .with_context(|| format!("Failed to resolve media dir: {}", media_dir.display()))?;
    if !canonical.starts_with(&canonical_dir) {
        anyhow::bail!("Media path escapes its directory: {}", joined.display());
    }
    Ok(canonical)
}

/// Validate that a filename is a safe basename — rejects empty strings,
/// path separators, and directory traversal attempts.
pub fn validate_media_filename(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        anyhow::bail!("Invalid media filename");
    }
    Ok(())
}

/// Determine media type from a file extension.
pub fn media_type_from_extension(filename: &str) -> &'static str {
    let lower = filename.to_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".bmp") {
        "image/bmp"
    } else if lower.ends_with(".tiff") || lower.ends_with(".tif") {
        "image/tiff"
    } else if lower.ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    }
}

// ---------------------------------------------------------------------------
// Image processing pipeline
// ---------------------------------------------------------------------------

/// Maximum image dimensions (2000×2000).
const MAX_DIMENSION: u32 = 2000;

/// Maximum input size in bytes (50 MB) — reject anything larger upfront.
const MAX_INPUT_SIZE: usize = 50 * 1024 * 1024;

/// Maximum output file size in bytes (5 MB).
const MAX_FILE_SIZE: u64 = 5 * 1024 * 1024;

/// JPEG quality starting point.
const JPEG_QUALITY: u8 = 85;

/// Maximum number of dimension-shrinking attempts before giving up.
const MAX_SHRINK_ATTEMPTS: u32 = 3;

/// Process an image from raw bytes: decode, resize if > 2000 px, re-encode.
///
/// Returns `(processed_bytes, media_type, extension)`.
///
/// # Algorithm
///
/// 1. Decode with format auto-detection.
/// 2. If either dimension > 2000 px, scale down proportionally to fit within
///    2000×2000 using Lanczos3.
/// 3. Check for an alpha channel (`color().has_alpha()`).
/// 4. Re-encode:
///    - PNG if an alpha channel is present (with size check; shrinks if > 5 MB).
///    - JPEG (quality 85) otherwise.
/// 5. If the encoded JPEG (or PNG) exceeds 5 MB, iteratively reduce quality
///    for JPEG or shrink dimensions for PNG, retrying from step 2 (up to 3 attempts).
pub fn process_image(data: &[u8]) -> Result<(Vec<u8>, String, String)> {
    use std::io::Cursor;

    use image::GenericImageView;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::imageops::FilterType;

    if data.len() > MAX_INPUT_SIZE {
        anyhow::bail!(
            "Image input too large: {} bytes (max {})",
            data.len(),
            MAX_INPUT_SIZE
        );
    }

    let mut img = image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .context("Failed to guess image format")?
        .decode()
        .context("Failed to decode image")?;

    for _attempt in 0..MAX_SHRINK_ATTEMPTS {
        // Step 2 – resize if either dimension exceeds MAX_DIMENSION
        let (w, h) = img.dimensions();
        if w > MAX_DIMENSION || h > MAX_DIMENSION {
            let scale = MAX_DIMENSION as f64 / w.max(h) as f64;
            let new_w = (w as f64 * scale) as u32;
            let new_h = (h as f64 * scale) as u32;
            img = img.resize(new_w, new_h, FilterType::Lanczos3);
        }

        // Step 3 – check for alpha
        if img.color().has_alpha() {
            // Step 4a – encode as PNG, check size
            let mut buf = Vec::new();
            let encoder = PngEncoder::new(&mut buf);
            img.write_with_encoder(encoder)
                .context("Failed to encode PNG")?;
            if (buf.len() as u64) <= MAX_FILE_SIZE {
                return Ok((buf, "image/png".to_string(), "png".to_string()));
            }
            // PNG too large — shrink dimensions and retry
            let (w, h) = img.dimensions();
            let new_w = (w / 2).max(1);
            let new_h = (h / 2).max(1);
            img = img.resize(new_w, new_h, FilterType::Lanczos3);
            continue;
        }

        // Step 4b-5 – encode as JPEG, reduce quality if needed
        let qualities: [u8; 4] = [JPEG_QUALITY, 75, 60, 40];
        for &quality in &qualities {
            let mut buf = Vec::new();
            let encoder = JpegEncoder::new_with_quality(&mut buf, quality);
            img.write_with_encoder(encoder)
                .context("Failed to encode JPEG")?;

            if (buf.len() as u64) <= MAX_FILE_SIZE {
                return Ok((buf, "image/jpeg".to_string(), "jpg".to_string()));
            }
        }

        // Step 6 – still too large → shrink dimensions by 50 % and retry
        let (w, h) = img.dimensions();
        let new_w = (w / 2).max(1);
        let new_h = (h / 2).max(1);
        img = img.resize(new_w, new_h, FilterType::Lanczos3);
    }

    // All attempts exhausted — return whatever we have at JPEG quality 40
    let mut buf = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut buf, 40);
    img.write_with_encoder(encoder)
        .context("Failed to encode JPEG (fallback)")?;
    Ok((buf, "image/jpeg".to_string(), "jpg".to_string()))
}
