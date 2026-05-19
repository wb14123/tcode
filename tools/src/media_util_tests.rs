#[cfg(test)]
mod tests {
    use std::path::Path;

    use anyhow::Result;
    use image::{ImageBuffer, Rgb};

    use crate::media_util::save_image_to_media;
    use crate::media_util::save_pdf_to_media;

    fn test_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/media_util")
    }

    fn temp_dir() -> std::path::PathBuf {
        let dir = test_root().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).expect("failed to create test dir");
        dir
    }

    /// Create a minimal RGB PNG image as bytes (100x100).
    fn make_test_png_bytes() -> Vec<u8> {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(100, 100, |x, y| Rgb([x as u8, y as u8, 128u8]));
        let mut buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut buf);
        img.write_with_encoder(encoder)
            .expect("failed to encode test PNG");
        buf
    }

    /// Create a minimal valid PDF as bytes.
    fn make_test_pdf_bytes() -> Vec<u8> {
        let mut doc = lopdf::Document::with_version("1.4");

        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let content_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();

        // Minimal content stream
        let content_stream = lopdf::Stream::new(lopdf::Dictionary::new(), b" ".to_vec());
        doc.objects
            .insert(content_id, lopdf::Object::Stream(content_stream));

        // Page dictionary
        let mut page_dict = lopdf::Dictionary::new();
        page_dict.set("Type", lopdf::Object::Name(b"Page".to_vec()));
        page_dict.set("Parent", lopdf::Object::Reference(pages_id));
        page_dict.set("Contents", lopdf::Object::Reference(content_id));
        page_dict.set(
            "MediaBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(612),
                lopdf::Object::Integer(792),
            ]),
        );
        page_dict.set(
            "Resources",
            lopdf::Object::Dictionary(lopdf::Dictionary::new()),
        );
        doc.objects
            .insert(page_id, lopdf::Object::Dictionary(page_dict));

        // Pages dictionary
        let mut pages_dict = lopdf::Dictionary::new();
        pages_dict.set("Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages_dict.set(
            "Kids",
            lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
        );
        pages_dict.set("Count", lopdf::Object::Integer(1));
        doc.objects
            .insert(pages_id, lopdf::Object::Dictionary(pages_dict));

        // Catalog
        let mut catalog_dict = lopdf::Dictionary::new();
        catalog_dict.set("Type", lopdf::Object::Name(b"Catalog".to_vec()));
        catalog_dict.set("Pages", lopdf::Object::Reference(pages_id));
        doc.objects
            .insert(catalog_id, lopdf::Object::Dictionary(catalog_dict));
        doc.trailer
            .set("Root", lopdf::Object::Reference(catalog_id));

        doc.compress();

        let mut buf = Vec::new();
        doc.save_to(&mut buf).expect("failed to serialize test PDF");
        buf
    }

    /// Create a PDF with `n` pages.
    fn make_multipage_pdf_bytes(n: usize) -> Vec<u8> {
        let mut doc = lopdf::Document::with_version("1.4");

        let pages_id = doc.new_object_id();
        let catalog_id = doc.new_object_id();

        let mut page_ids = Vec::new();
        let mut kids = Vec::new();

        for _ in 0..n {
            let page_id = doc.new_object_id();
            let content_id = doc.new_object_id();

            let content_stream = lopdf::Stream::new(lopdf::Dictionary::new(), b" ".to_vec());
            doc.objects
                .insert(content_id, lopdf::Object::Stream(content_stream));

            let mut page_dict = lopdf::Dictionary::new();
            page_dict.set("Type", lopdf::Object::Name(b"Page".to_vec()));
            page_dict.set("Parent", lopdf::Object::Reference(pages_id));
            page_dict.set("Contents", lopdf::Object::Reference(content_id));
            page_dict.set(
                "MediaBox",
                lopdf::Object::Array(vec![
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(612),
                    lopdf::Object::Integer(792),
                ]),
            );
            page_dict.set(
                "Resources",
                lopdf::Object::Dictionary(lopdf::Dictionary::new()),
            );
            doc.objects
                .insert(page_id, lopdf::Object::Dictionary(page_dict));

            kids.push(lopdf::Object::Reference(page_id));
            page_ids.push(page_id);
        }

        // Pages dictionary
        let mut pages_dict = lopdf::Dictionary::new();
        pages_dict.set("Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages_dict.set("Kids", lopdf::Object::Array(kids));
        pages_dict.set("Count", lopdf::Object::Integer(n as i64));
        doc.objects
            .insert(pages_id, lopdf::Object::Dictionary(pages_dict));

        // Catalog
        let mut catalog_dict = lopdf::Dictionary::new();
        catalog_dict.set("Type", lopdf::Object::Name(b"Catalog".to_vec()));
        catalog_dict.set("Pages", lopdf::Object::Reference(pages_id));
        doc.objects
            .insert(catalog_id, lopdf::Object::Dictionary(catalog_dict));
        doc.trailer
            .set("Root", lopdf::Object::Reference(catalog_id));

        doc.compress();

        let mut buf = Vec::new();
        doc.save_to(&mut buf)
            .expect("failed to serialize multi-page PDF");
        buf
    }

    // ── Test 1: save_image_to_media with valid PNG ─────────────────────────

    #[tokio::test]
    async fn save_image_to_media_valid_png() -> Result<()> {
        let media_dir = temp_dir();
        let png_bytes = make_test_png_bytes();

        let result = save_image_to_media(png_bytes, "test.png", &media_dir).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());

        let (media, annotation) = result?;

        // Annotation starts with "[Image: "
        assert!(
            annotation.starts_with("[Image: "),
            "annotation should start with '[Image: ', got: {annotation}"
        );

        // RGB images get converted to JPEG by process_image
        assert_eq!(
            media.media_type(),
            "image/jpeg",
            "expected image/jpeg for RGB image"
        );

        // File exists on disk
        let file_path = media_dir.join(media.relative_path());
        assert!(
            file_path.exists(),
            "file should exist at {}",
            file_path.display()
        );

        // File extension should be jpg
        assert!(
            media.relative_path().ends_with(".jpg"),
            "expected .jpg extension, got: {}",
            media.relative_path()
        );

        Ok(())
    }

    // ── Test 2: save_image_to_media with oversized image ───────────────────

    #[tokio::test]
    async fn save_image_to_media_oversized() -> Result<()> {
        let media_dir = temp_dir();
        // 51 MB = 51 * 1024 * 1024 bytes (exceeds 50 MB MAX_INPUT_SIZE)
        let huge_data = vec![0u8; 51 * 1024 * 1024];

        let result = save_image_to_media(huge_data, "oversized.png", &media_dir).await;
        assert!(result.is_err(), "expected error for oversized image");

        // The error chain should contain a size complaint (either from
        // process_image directly or wrapped in context).
        let err = format!("{:?}", result.unwrap_err());
        assert!(
            err.contains("too large") || err.contains("Failed to process image"),
            "error should indicate failure processing the oversized image, got: {err}"
        );

        Ok(())
    }

    // ── Test 3: save_pdf_to_media with valid PDF ───────────────────────────

    #[tokio::test]
    async fn save_pdf_to_media_valid_pdf() -> Result<()> {
        let media_dir = temp_dir();
        let pdf_bytes = make_test_pdf_bytes();

        // Verify the test PDF is valid by parsing it first
        assert!(
            pdf_bytes.starts_with(b"%PDF-"),
            "test PDF should start with %PDF-"
        );

        let result = save_pdf_to_media(pdf_bytes, "test.pdf", &media_dir).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());

        let (media, annotation) = result?;

        // Annotation starts with "[PDF: "
        assert!(
            annotation.starts_with("[PDF: "),
            "annotation should start with '[PDF: ', got: {annotation}"
        );

        // Annotation contains "(application/pdf)"
        assert!(
            annotation.contains("(application/pdf)"),
            "annotation should contain '(application/pdf)', got: {annotation}"
        );

        // Media type is application/pdf
        assert_eq!(media.media_type(), "application/pdf");

        // File exists on disk
        let file_path = media_dir.join(media.relative_path());
        assert!(
            file_path.exists(),
            "file should exist at {}",
            file_path.display()
        );

        // File extension should be pdf
        assert!(
            media.relative_path().ends_with(".pdf"),
            "expected .pdf extension, got: {}",
            media.relative_path()
        );

        Ok(())
    }

    // ── Test 4: save_pdf_to_media with invalid bytes ───────────────────────

    #[tokio::test]
    async fn save_pdf_to_media_invalid_bytes() -> Result<()> {
        let media_dir = temp_dir();
        let garbage = b"this is definitely not a PDF file at all".to_vec();

        let result = save_pdf_to_media(garbage, "not-a-pdf.bin", &media_dir).await;
        assert!(result.is_err(), "expected error for non-PDF bytes");

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("wrong magic bytes"),
            "error should mention 'wrong magic bytes', got: {err}"
        );

        Ok(())
    }

    // ── Test 5: save_pdf_to_media with too-many-pages PDF ──────────────────

    #[tokio::test]
    async fn save_pdf_to_media_too_many_pages() -> Result<()> {
        let media_dir = temp_dir();
        let pdf_bytes = make_multipage_pdf_bytes(101);

        let result = save_pdf_to_media(pdf_bytes, "many-pages.pdf", &media_dir).await;
        assert!(result.is_err(), "expected error for PDF with 101 pages");

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("has 101 pages"),
            "error should mention 'has 101 pages', got: {err}"
        );

        Ok(())
    }
}
