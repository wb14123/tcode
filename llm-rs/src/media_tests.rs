#[cfg(test)]
mod tests {
    use crate::llm::LLMMessage;
    use crate::media::{ContentPart, MediaData, process_image};
    use image::{GenericImageView, ImageBuffer, Rgb, Rgba};
    use std::path::Path;

    fn test_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/media")
    }

    fn temp_dir() -> std::path::PathBuf {
        let dir = test_root().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).expect("failed to create test dir");
        dir
    }

    fn write_png_to_dir(dir: &Path, filename: &str, width: u32, height: u32) -> std::path::PathBuf {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(width, height, |x, y| Rgb([x as u8, y as u8, 128u8]));
        let path = dir.join(filename);
        img.save(&path).expect("failed to write test PNG");
        path
    }

    // ======== MediaData tests ========

    #[test]
    fn test_media_data_new() {
        let media = MediaData::new("abc.png".to_string(), "image/png".to_string());
        assert_eq!(media.relative_path(), "abc.png");
        assert_eq!(media.media_type(), "image/png");
    }

    #[test]
    fn test_media_data_get_data_caching() -> anyhow::Result<()> {
        let dir = temp_dir();
        let path = write_png_to_dir(&dir, "test.png", 10, 10);
        let expected = std::fs::read(&path)?;

        let media = MediaData::new("test.png".to_string(), "image/png".to_string());

        let data1 = media.get_data(&dir)?;
        assert_eq!(data1, expected.as_slice());

        // Second call should return the same bytes from cache.
        let data2 = media.get_data(&dir)?;
        assert_eq!(data2, expected.as_slice());

        // Verify it's the same pointer (same cache hit, not re-read).
        assert!(std::ptr::eq(data1.as_ptr(), data2.as_ptr()));

        Ok(())
    }

    #[test]
    fn test_media_data_get_data_different_dir() -> anyhow::Result<()> {
        let dir1 = temp_dir();
        write_png_to_dir(&dir1, "test.png", 10, 10);
        let dir2 = temp_dir();

        let media = MediaData::new("test.png".to_string(), "image/png".to_string());

        // First call with dir1 succeeds.
        let _data = media.get_data(&dir1)?;

        // Second call with dir2 should fail.
        let result = media.get_data(&dir2);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("different media_dir"),
            "expected 'different media_dir' error, got: {err}"
        );

        Ok(())
    }

    #[test]
    fn test_media_data_serialization() -> anyhow::Result<()> {
        let media = MediaData::new("abc.png".to_string(), "image/png".to_string());
        let json = serde_json::to_value(&media)?;

        let obj = json.as_object().expect("expected JSON object");
        assert_eq!(obj.len(), 2, "expected exactly 2 fields");
        assert_eq!(obj["relative_path"], "abc.png");
        assert_eq!(obj["media_type"], "image/png");
        // Ensure no extra fields like cached_data leaked.
        assert!(!obj.contains_key("cached_data"));

        Ok(())
    }

    #[test]
    fn test_media_data_deserialization() -> anyhow::Result<()> {
        let json = serde_json::json!({
            "relative_path": "abc.png",
            "media_type": "image/png"
        });
        let media: MediaData = serde_json::from_value(json)?;

        assert_eq!(media.relative_path(), "abc.png");
        assert_eq!(media.media_type(), "image/png");

        // cached_data should be empty — trying to get_data on a nonexistent
        // file should fail (not panic).
        let nonexistent = std::path::Path::new("/nonexistent/images");
        let result = media.get_data(nonexistent);
        assert!(result.is_err());

        Ok(())
    }

    #[test]
    fn test_media_data_get_data_file_not_found() {
        let dir = temp_dir();
        // Don't create any file — the path won't exist.
        let media = MediaData::new("no_such_file.png".to_string(), "image/png".to_string());
        let result = media.get_data(&dir);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Failed to resolve media path"),
            "expected file-not-found error, got: {err}"
        );
    }

    // ======== ContentPart tests ========

    #[test]
    fn test_content_part_text_serde_roundtrip() -> anyhow::Result<()> {
        let part = ContentPart::Text("hello world".to_string());
        let json = serde_json::to_string(&part)?;

        // With #[serde(untagged)], Text serializes as a bare string.
        assert_eq!(json, r#""hello world""#);

        let deserialized: ContentPart = serde_json::from_str(&json)?;
        match deserialized {
            ContentPart::Text(s) => assert_eq!(s, "hello world"),
            _ => panic!("Expected Text variant"),
        }

        Ok(())
    }

    #[test]
    fn test_content_part_media_serde_roundtrip() -> anyhow::Result<()> {
        let media_data = MediaData::new("uuid.png".to_string(), "image/png".to_string());
        let part = ContentPart::Media(media_data);
        let json_str = serde_json::to_string(&part)?;

        let value: serde_json::Value = serde_json::from_str(&json_str)?;
        let obj = value.as_object().expect("expected object");
        assert_eq!(obj["relative_path"], "uuid.png");
        assert_eq!(obj["media_type"], "image/png");

        let deserialized: ContentPart = serde_json::from_str(&json_str)?;
        match deserialized {
            ContentPart::Media(media) => {
                assert_eq!(media.relative_path(), "uuid.png");
                assert_eq!(media.media_type(), "image/png");
            }
            ContentPart::Text(_) => panic!("Expected Media variant, got Text"),
        }

        Ok(())
    }

    // ======== LLMMessage backward compat tests ========

    #[test]
    fn test_llm_message_user_old_format_deserialization() -> anyhow::Result<()> {
        // Old format: `{"User": "hello world"}`
        let json = r#"{"User": "hello world"}"#;
        let msg: LLMMessage = serde_json::from_str(json)?;

        match msg {
            LLMMessage::User(parts) => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    ContentPart::Text(s) => assert_eq!(s, "hello world"),
                    _ => panic!("Expected Text content part"),
                }
            }
            _ => panic!("Expected User variant"),
        }

        Ok(())
    }

    #[test]
    fn test_llm_message_user_new_format_roundtrip() -> anyhow::Result<()> {
        let media = MediaData::new("img.png".to_string(), "image/png".to_string());
        let msg = LLMMessage::User(vec![
            ContentPart::Text("look at this: ".to_string()),
            ContentPart::Media(media),
        ]);

        let json = serde_json::to_string(&msg)?;
        let deserialized: LLMMessage = serde_json::from_str(&json)?;

        match deserialized {
            LLMMessage::User(parts) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    ContentPart::Text(s) => assert_eq!(s, "look at this: "),
                    _ => panic!("Expected Text as first part"),
                }
                match &parts[1] {
                    ContentPart::Media(media) => {
                        assert_eq!(media.relative_path(), "img.png");
                        assert_eq!(media.media_type(), "image/png");
                    }
                    ContentPart::Text(_) => panic!("Expected Media as second part"),
                }
            }
            _ => panic!("Expected User variant"),
        }

        Ok(())
    }

    // ======== Image processing tests ========

    #[test]
    fn test_process_image_small_image_unchanged() -> anyhow::Result<()> {
        // 100x100 RGB — small enough that no resize should happen.
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(100, 100, |x, y| Rgb([x as u8, y as u8, 128u8]));

        let mut buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut buf);
        img.write_with_encoder(encoder)?;

        let result = process_image(&buf)?;
        let (processed_bytes, media_type, ext) = result;

        // RGB with no alpha → JPEG output
        assert_eq!(media_type, "image/jpeg");
        assert_eq!(ext, "jpg");

        // Decode the output and verify dimensions preserved.
        let decoded = image::load_from_memory(&processed_bytes)?;
        let (w, h) = decoded.dimensions();
        assert_eq!(w, 100);
        assert_eq!(h, 100);

        Ok(())
    }

    #[test]
    fn test_process_image_oversized_scaled_down() -> anyhow::Result<()> {
        // 3000x2000 — should be scaled to fit 2000x2000 (longest side → 2000).
        // Scale factor = 2000 / 3000 = 2/3 → result: 2000x1333.
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(3000, 2000, |x, y| Rgb([x as u8, y as u8, 128u8]));

        let mut buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut buf);
        img.write_with_encoder(encoder)?;

        let (processed_bytes, _media_type, _ext) = process_image(&buf)?;

        let decoded = image::load_from_memory(&processed_bytes)?;
        let (w, h) = decoded.dimensions();
        assert_eq!(w, 2000);
        assert_eq!(h, 1333);

        Ok(())
    }

    #[test]
    fn test_process_image_with_alpha_becomes_png() -> anyhow::Result<()> {
        // RGBA image — should produce PNG output.
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_fn(50, 50, |x, y| Rgba([x as u8, y as u8, 128u8, 200u8]));

        let mut buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut buf);
        img.write_with_encoder(encoder)?;

        let (processed_bytes, media_type, ext) = process_image(&buf)?;

        assert_eq!(media_type, "image/png");
        assert_eq!(ext, "png");

        // Should be decodable as PNG (not JPEG).
        let decoded = image::load_from_memory(&processed_bytes)?;
        let (w, h) = decoded.dimensions();
        assert_eq!(w, 50);
        assert_eq!(h, 50);

        Ok(())
    }

    #[test]
    fn test_process_image_no_alpha_becomes_jpeg() -> anyhow::Result<()> {
        // Large enough RGB image to stay above any size threshold logic
        // (process_image for RGB without alpha always outputs JPEG).
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(200, 200, |x, y| {
            // Use varied colours so compression doesn't make it trivially tiny.
            Rgb([
                (x.wrapping_mul(7)) as u8,
                (y.wrapping_mul(13)) as u8,
                ((x ^ y).wrapping_mul(3)) as u8,
            ])
        });

        let mut buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut buf);
        img.write_with_encoder(encoder)?;

        let (processed_bytes, media_type, ext) = process_image(&buf)?;

        assert_eq!(media_type, "image/jpeg");
        assert_eq!(ext, "jpg");

        // Should be decodable.
        let decoded = image::load_from_memory(&processed_bytes)?;
        let (w, h) = decoded.dimensions();
        assert_eq!(w, 200);
        assert_eq!(h, 200);

        Ok(())
    }
}
