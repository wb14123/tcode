use crate::image::{ImageInput, ImageParams, ImageRequest, ImageResponse};

fn body(request: &ImageRequest, model: &str) -> anyhow::Result<String> {
    request.to_json_body(model).map_err(anyhow::Error::msg)
}

fn parse(body: &str) -> anyhow::Result<ImageResponse> {
    ImageResponse::from_json(body).map_err(anyhow::Error::msg)
}

fn generate_params() -> ImageParams {
    ImageParams {
        prompt: "a red panda".to_string(),
        n: Some(2),
        size: Some("auto".to_string()),
        quality: Some("auto".to_string()),
        background: "opaque".to_string(),
        output_format: Some("webp".to_string()),
    }
}

#[test]
fn generate_body_matches_the_api_example() -> anyhow::Result<()> {
    let request = ImageRequest::Generate(generate_params());
    let body = body(&request, "gpt-image-2")?;
    assert_eq!(
        body,
        r#"{"prompt":"a red panda","background":"opaque","model":"gpt-image-2","quality":"auto","size":"auto","n":2,"output_format":"webp"}"#
    );
    Ok(())
}

#[test]
fn edit_body_matches_the_api_example() -> anyhow::Result<()> {
    let request = ImageRequest::Edit {
        params: ImageParams {
            prompt: "add a hat".to_string(),
            n: Some(1),
            size: Some("auto".to_string()),
            quality: Some("auto".to_string()),
            background: "transparent".to_string(),
            output_format: Some("png".to_string()),
        },
        images: vec![ImageInput {
            media_type: "image/png".to_string(),
            data: vec![1, 2, 3],
        }],
    };
    let body = body(&request, "gpt-image-2")?;
    assert_eq!(
        body,
        r#"{"images":[{"image_url":"data:image/png;base64,AQID"}],"prompt":"add a hat","background":"transparent","model":"gpt-image-2","quality":"auto","size":"auto","n":1,"output_format":"png"}"#
    );
    Ok(())
}

#[test]
fn n_and_output_format_are_omitted_when_none() -> anyhow::Result<()> {
    let request = ImageRequest::Generate(ImageParams {
        prompt: "a cat".to_string(),
        n: None,
        size: None,
        quality: None,
        background: "opaque".to_string(),
        output_format: None,
    });
    let body = body(&request, "gpt-image-2")?;
    assert_eq!(
        body,
        r#"{"prompt":"a cat","background":"opaque","model":"gpt-image-2","quality":"auto","size":"auto"}"#
    );
    Ok(())
}

#[test]
fn caller_supplied_size_and_quality_are_preserved() -> anyhow::Result<()> {
    let request = ImageRequest::Generate(ImageParams {
        prompt: "a dog".to_string(),
        n: None,
        size: Some("1024x1024".to_string()),
        quality: Some("high".to_string()),
        background: "transparent".to_string(),
        output_format: None,
    });
    let body = body(&request, "custom-model")?;
    let value: serde_json::Value = serde_json::from_str(&body)?;
    assert_eq!(value["model"], "custom-model");
    assert_eq!(value["size"], "1024x1024");
    assert_eq!(value["quality"], "high");
    assert_eq!(value["background"], "transparent");
    Ok(())
}

#[test]
fn media_type_background_and_quality_are_checked() -> anyhow::Result<()> {
    let input = ImageInput {
        media_type: "image/jpeg".to_string(),
        data: vec![1, 2, 3],
    };
    assert_eq!(input.to_data_uri(), "data:image/jpeg;base64,AQID");
    Ok(())
}

#[test]
fn response_parsing_reads_b64_and_output_format() -> anyhow::Result<()> {
    let response = parse(
        r#"{"created":1,"data":[{"b64_json":"AAAA","generation_id":"g1"}],"output_format":"png"}"#,
    )?;
    assert_eq!(response.images, ["AAAA"]);
    assert_eq!(response.output_format.as_deref(), Some("png"));
    Ok(())
}

#[test]
fn response_parsing_yields_one_image_per_data_entry() -> anyhow::Result<()> {
    let response = parse(r#"{"data":[{"b64_json":"AA"},{"b64_json":"BB"}]}"#)?;
    assert_eq!(response.images, ["AA", "BB"]);
    assert_eq!(response.output_format, None);
    Ok(())
}

#[test]
fn response_parsing_rejects_missing_data() {
    assert!(ImageResponse::from_json(r#"{"created":1}"#).is_err());
}

#[test]
fn response_parsing_rejects_missing_b64_json() {
    assert!(ImageResponse::from_json(r#"{"data":[{"generation_id":"g1"}]}"#).is_err());
}

#[test]
fn response_parsing_rejects_non_string_b64_json() {
    assert!(ImageResponse::from_json(r#"{"data":[{"b64_json":42}]}"#).is_err());
}

#[test]
fn response_parsing_allows_an_empty_data_array() -> anyhow::Result<()> {
    let response = parse(r#"{"data":[]}"#)?;
    assert!(response.images.is_empty());
    Ok(())
}
