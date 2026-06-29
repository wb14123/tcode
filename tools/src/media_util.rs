use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use llm_rs::media::{MediaData, process_image};
use llm_rs::tool::ToolContext;
use uuid::Uuid;

/// Maximum image download size (50 MB).
pub const MAX_IMAGE_SIZE: u64 = 50 * 1024 * 1024;

/// Maximum PDF download size (20 MB).
pub const MAX_PDF_SIZE: u64 = 20 * 1024 * 1024;

/// Check that media is supported and return the media directory path.
///
/// `action` describes what's being done, e.g. `"read image file"`, `"fetch PDF URL"`.
pub fn require_media_dir(ctx: &ToolContext, action: &str) -> Result<PathBuf> {
    if !ctx.supports_media {
        bail!(
            "Cannot {}: visual input is disabled. Ask the user to set `supports_media = true` in their tcode config if their model supports media.",
            action
        );
    }
    ctx.session_dir
        .as_ref()
        .map(|d| d.media_dir())
        .ok_or_else(|| anyhow!("Cannot {}: no media directory configured.", action))
}

/// Process image bytes and save to the media directory.
///
/// Returns the `MediaData` pointing to the saved file and a text annotation
/// like `[Image: {source_label} ({media_type})]`.
///
/// # Errors
///
/// Returns an error if image processing fails or the file cannot be written.
pub async fn save_image_to_media(
    data: Vec<u8>,
    source_label: &str,
    media_dir: &Path,
) -> Result<(MediaData, String)> {
    let (processed_bytes, media_type, extension) =
        process_image(&data).context("Failed to process image")?;

    tokio::fs::create_dir_all(media_dir)
        .await
        .with_context(|| format!("Failed to create media directory {}", media_dir.display()))?;

    let filename = format!("{}.{}", Uuid::new_v4(), extension);
    let image_path = media_dir.join(&filename);

    tokio::fs::write(&image_path, &processed_bytes)
        .await
        .with_context(|| {
            format!(
                "Failed to write processed image to {}",
                image_path.display()
            )
        })?;

    let annotation = format!("[Image: {} ({})]", source_label, media_type);

    Ok((MediaData::new(filename, media_type), annotation))
}

/// Validate and save PDF bytes to the media directory.
///
/// Performs validation checks:
/// - PDF magic bytes (`%PDF-`)
/// - Parsable by `lopdf`
/// - Not encrypted
/// - Page count ≤ 100
///
/// Returns the `MediaData` pointing to the saved file and a text annotation
/// like `[PDF: {source_label} (application/pdf)]`.
///
/// # Errors
///
/// Returns an error if validation fails or the file cannot be written.
pub async fn save_pdf_to_media(
    data: Vec<u8>,
    source_label: &str,
    media_dir: &Path,
) -> Result<(MediaData, String)> {
    llm_rs::media::validate_pdf(&data)?;

    tokio::fs::create_dir_all(media_dir)
        .await
        .with_context(|| format!("Failed to create media directory {}", media_dir.display()))?;

    let filename = format!("{}.pdf", Uuid::new_v4());
    let pdf_path = media_dir.join(&filename);

    tokio::fs::write(&pdf_path, &data)
        .await
        .with_context(|| format!("Failed to save PDF to {}", pdf_path.display()))?;

    let annotation = format!("[PDF: {} (application/pdf)]", source_label);

    Ok((
        MediaData::new(filename, "application/pdf".to_string()),
        annotation,
    ))
}
