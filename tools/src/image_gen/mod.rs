//! The `generate_image` tool: generate or edit images via the provider's image API.

#[cfg(test)]
mod image_gen_tests;

use std::path::Path;
use std::time::Duration;

use base64::Engine;
use llm_rs::image::{ImageInput, ImageParams, ImageRequest};
use llm_rs::media::{ContentPart, MediaData};
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;

use crate::file_permission::check_file_read_permission;
use crate::media_util;

/// Default timeout for a single image API call, in seconds.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Maximum number of input images allowed for an edit request.
const MAX_EDIT_IMAGES: usize = 5;

/// Maximum size of a single edit input image (50 MiB).
const MAX_EDIT_IMAGE_BYTES: u64 = 50 * 1024 * 1024;

/// Generate a new image from a text prompt, or edit existing image files when
/// `referenced_image_paths` is set.
///
/// The generated image is saved into the session's media directory and its
/// absolute path is returned; the image is also shown to the user immediately.
///
/// `n` and `output_format` are requests to the backend and may be ignored: some
/// backends always return a single PNG per call. Whatever the backend returns is
/// what is saved, so the returned path's extension always matches the actual
/// bytes. To get several images or a different format, call this tool several
/// times.
///
/// The returned file is already displayed to the user, so do not render markdown
/// images or links for it. The saved path can be passed back to `generate_image`
/// with `referenced_image_paths` to edit it, or used with any other file tool the
/// session provides.
///
/// Editing an image the user uploaded through the web UI is not supported,
/// because the uploaded file's path is not available; pass explicit filesystem
/// paths in `referenced_image_paths`.
#[tool]
#[allow(clippy::too_many_arguments)]
pub fn generate_image(
    ctx: ToolContext,
    /// The image description, or for an edit the instruction describing the
    /// change to make.
    prompt: String,
    /// Paths of existing image files to edit. Omit to generate a new image. At
    /// most 5 paths, each a png, jpg, jpeg, or webp file on the local filesystem.
    #[serde(default)]
    referenced_image_paths: Option<Vec<String>>,
    /// Number of images to request (1-10). Omit to send no count and get the
    /// backend's default. Some backends ignore this and return one image.
    #[serde(default)]
    n: Option<u32>,
    /// Requested image size, e.g. "1024x1024" or "auto". Omit to send "auto".
    #[serde(default)]
    size: Option<String>,
    /// Requested quality: "low", "medium", "high", or "auto". Omit to send "auto".
    #[serde(default)]
    quality: Option<String>,
    /// Set true to request a transparent background; otherwise an opaque
    /// background is requested.
    #[serde(default)]
    transparent_background: Option<bool>,
    /// Requested output format: "png", "jpeg", or "webp". Omit to send no format.
    /// Some backends ignore this.
    #[serde(default)]
    output_format: Option<String>,
    /// Timeout in seconds for the image API call (default 300). Bounds only the
    /// API call, not file reads or permission prompts.
    #[serde(default)]
    timeout_seconds: Option<u64>,
) -> impl tokio_stream::Stream<Item = Result<ContentPart, String>> {
    async_stream::stream! {
        let media_dir = match ctx.session_dir.as_ref() {
            Some(dir) => dir.media_dir(),
            None => {
                yield Err("Cannot generate image: no session directory is configured.".to_string());
                return;
            }
        };

        let llm = match ctx.llm.as_ref() {
            Some(llm) => llm,
            None => {
                yield Err(
                    "Image generation is unavailable: no LLM provider is configured.".to_string(),
                );
                return;
            }
        };
        if !llm.supports_image_generation() {
            yield Err("Image generation is not supported by the current provider.".to_string());
            return;
        }

        let background = if transparent_background.unwrap_or(false) {
            "transparent"
        } else {
            "opaque"
        }
        .to_string();

        let params = ImageParams {
            prompt,
            n,
            size,
            quality,
            background,
            output_format,
        };

        let request = match referenced_image_paths {
            Some(paths) if !paths.is_empty() => {
                if paths.len() > MAX_EDIT_IMAGES {
                    yield Err(format!(
                        "referenced_image_paths accepts at most {MAX_EDIT_IMAGES} paths, got {}.",
                        paths.len()
                    ));
                    return;
                }

                let mut images = Vec::with_capacity(paths.len());
                for raw_path in &paths {
                    let path = Path::new(raw_path);
                    let canonical = match tokio::fs::canonicalize(path).await {
                        Ok(canonical) => canonical,
                        Err(error) => {
                            yield Err(format!(
                                "Failed to resolve referenced image {}: {error}",
                                path.display()
                            ));
                            return;
                        }
                    };

                    if let Err(error) =
                        check_file_read_permission(&ctx.permission, &canonical, false).await
                    {
                        yield Err(error.to_string());
                        return;
                    }

                    let metadata = match tokio::fs::metadata(&canonical).await {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            yield Err(format!(
                                "Failed to read referenced image {}: {error}",
                                canonical.display()
                            ));
                            return;
                        }
                    };
                    if metadata.len() > MAX_EDIT_IMAGE_BYTES {
                        yield Err(format!(
                            "Referenced image {} is too large ({} bytes, max {} bytes).",
                            canonical.display(),
                            metadata.len(),
                            MAX_EDIT_IMAGE_BYTES
                        ));
                        return;
                    }

                    let media_type = match edit_media_type(&canonical) {
                        Some(media_type) => media_type,
                        None => {
                            yield Err(format!(
                                "referenced_image_paths must be png, jpg, jpeg, or webp files: {}",
                                canonical.display()
                            ));
                            return;
                        }
                    };

                    let data = match tokio::fs::read(&canonical).await {
                        Ok(data) => data,
                        Err(error) => {
                            yield Err(format!(
                                "Failed to read referenced image {}: {error}",
                                canonical.display()
                            ));
                            return;
                        }
                    };

                    images.push(ImageInput {
                        media_type: media_type.to_string(),
                        data,
                    });
                }

                ImageRequest::Edit { params, images }
            }
            _ => ImageRequest::Generate(params),
        };

        let mut pending_id = ctx.media_sink.as_ref().map(|sink| sink.media_started());

        let timeout_secs = timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECS);
        let response = match tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            llm.generate_image(request),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                fail_pending(&ctx, pending_id.take());
                yield Err(format!("Image generation failed: {error}"));
                return;
            }
            Err(_) => {
                fail_pending(&ctx, pending_id.take());
                yield Err(format!(
                    "Image generation timed out after {timeout_secs} seconds."
                ));
                return;
            }
        };

        let mut lines = Vec::new();
        for (index, encoded) in response.images.iter().enumerate() {
            let bytes = match base64::engine::general_purpose::STANDARD.decode(encoded) {
                Ok(bytes) => bytes,
                Err(error) => {
                    fail_pending(&ctx, pending_id.take());
                    yield Err(format!(
                        "Failed to decode image {index} from the provider: {error}"
                    ));
                    return;
                }
            };

            let (media_type, extension) = media_util::sniff_image_type(&bytes)
                .unwrap_or_else(|| format_to_media_type(response.output_format.as_deref()));

            let (media, annotation) = match media_util::save_raw_image_to_media(
                bytes,
                media_type,
                extension,
                &media_dir,
            )
            .await
            {
                Ok(saved) => saved,
                Err(error) => {
                    fail_pending(&ctx, pending_id.take());
                    yield Err(format!("Failed to save generated image: {error}"));
                    return;
                }
            };

            match pending_id.take() {
                Some(id) => emit_output(&ctx, &id, media),
                None => {
                    if let Some(id) = ctx.media_sink.as_ref().map(|sink| sink.media_started()) {
                        emit_output(&ctx, &id, media);
                    }
                }
            }

            lines.push(annotation);
        }

        fail_pending(&ctx, pending_id.take());

        if lines.is_empty() {
            yield Err("Image generation returned no images.".to_string());
            return;
        }

        let count = lines.len();
        let hint = if count == 1 {
            "Generated 1 image. Pass its path back to generate_image with \
             referenced_image_paths to edit it, or use it with any other file tool \
             the session provides. It is already displayed to the user - do not \
             render markdown images or links."
                .to_string()
        } else {
            format!(
                "Generated {count} images. Pass a path back to generate_image with \
                 referenced_image_paths to edit it, or use them with any other file \
                 tool the session provides. They are already displayed to the user - \
                 do not render markdown images or links."
            )
        };

        let mut output = lines.join("\n");
        output.push('\n');
        output.push_str(&hint);

        yield Ok(ContentPart::Text(output));
    }
}

/// Map an API-reported output format to `(media_type, extension)`, falling back
/// to PNG for an unknown or absent value.
fn format_to_media_type(output_format: Option<&str>) -> (&'static str, &'static str) {
    match output_format {
        Some("jpeg") | Some("jpg") => ("image/jpeg", "jpg"),
        Some("webp") => ("image/webp", "webp"),
        _ => ("image/png", "png"),
    }
}

/// Media type for an edit input, derived from its file extension.
fn edit_media_type(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

/// Publish a produced media file for an already-started id.
fn emit_output(ctx: &ToolContext, id: &str, media: MediaData) {
    if let Some(sink) = ctx.media_sink.as_ref() {
        sink.media_output(id, media);
    }
}

/// Mark a started-but-unresolved media id as failed.
fn fail_pending(ctx: &ToolContext, id: Option<String>) {
    if let (Some(sink), Some(id)) = (ctx.media_sink.as_ref(), id) {
        sink.media_failed(&id);
    }
}
