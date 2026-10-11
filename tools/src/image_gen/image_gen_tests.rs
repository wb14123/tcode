#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::Result;
    use base64::Engine;
    use image::{ImageBuffer, Rgb};
    use llm_rs::image::{ImageRequest, ImageResponse};
    use llm_rs::llm::{ChatOptions, LLM, LLMEvent, LLMMessage, ModelInfo};
    use llm_rs::media::{ContentPart, MediaData};
    use llm_rs::permission::{
        PermissionKey, PermissionManager, PermissionScope, ScopedPermissionManager,
    };
    use llm_rs::tool::{CancellationToken, MediaSink, SessionDir, Tool, ToolContext};
    use tokio_stream::{Stream, StreamExt};

    // ── TestDir RAII ──────────────────────────────────────────────────────

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            Self::for_module("image_gen")
        }

        fn for_module(module: &str) -> Self {
            let root =
                Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../target/test-tmp/{module}"));
            std::fs::create_dir_all(&root).expect("failed to create test root");
            let dir = root.join(uuid::Uuid::new_v4().to_string());
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("failed to create test dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // ── Fake provider ─────────────────────────────────────────────────────

    struct FakeState {
        supports: bool,
        images: Vec<String>,
        output_format: Option<String>,
        error: Option<String>,
        hang: bool,
        received: Vec<ImageRequest>,
    }

    #[derive(Clone)]
    struct FakeLlm {
        state: Arc<Mutex<FakeState>>,
    }

    impl FakeLlm {
        fn responding(images: Vec<String>, output_format: Option<&str>) -> Self {
            Self::with(FakeState {
                supports: true,
                images,
                output_format: output_format.map(str::to_string),
                error: None,
                hang: false,
                received: Vec::new(),
            })
        }

        fn with(state: FakeState) -> Self {
            Self {
                state: Arc::new(Mutex::new(state)),
            }
        }

        fn requests(&self) -> Vec<ImageRequest> {
            self.state
                .lock()
                .expect("state mutex poisoned")
                .received
                .clone()
        }
    }

    impl LLM for FakeLlm {
        fn register_tools(&mut self, _tools: Vec<Arc<Tool>>) {}

        fn chat(
            &self,
            _model: &str,
            _msgs: &[LLMMessage],
            _options: &ChatOptions,
        ) -> Pin<Box<dyn Stream<Item = LLMEvent> + Send>> {
            Box::pin(tokio_stream::empty())
        }

        fn clone_box(&self) -> Box<dyn LLM> {
            Box::new(self.clone())
        }

        fn set_media_dir(&mut self, _dir: Option<PathBuf>) {}

        fn available_models(&self) -> Vec<ModelInfo> {
            vec![]
        }

        fn supports_image_generation(&self) -> bool {
            self.state.lock().expect("state mutex poisoned").supports
        }

        fn generate_image(
            &self,
            request: ImageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<ImageResponse, String>> + Send>> {
            let state = Arc::clone(&self.state);
            Box::pin(async move {
                let (images, output_format, error, hang) = {
                    let mut guard = state.lock().expect("state mutex poisoned");
                    guard.received.push(request);
                    (
                        guard.images.clone(),
                        guard.output_format.clone(),
                        guard.error.clone(),
                        guard.hang,
                    )
                };
                if hang {
                    std::future::pending::<()>().await;
                }
                if let Some(error) = error {
                    return Err(error);
                }
                Ok(ImageResponse {
                    images,
                    output_format,
                })
            })
        }
    }

    // ── Fake media sink ───────────────────────────────────────────────────

    #[derive(Debug, Clone, PartialEq)]
    enum SinkEvent {
        Started(String),
        Output(String),
        Failed(String),
    }

    #[derive(Default)]
    struct FakeSink {
        events: Mutex<Vec<SinkEvent>>,
    }

    impl FakeSink {
        fn events(&self) -> Vec<SinkEvent> {
            self.events.lock().expect("sink mutex poisoned").clone()
        }
    }

    impl MediaSink for FakeSink {
        fn media_started(&self) -> String {
            let id = uuid::Uuid::new_v4().to_string();
            self.events
                .lock()
                .expect("sink mutex poisoned")
                .push(SinkEvent::Started(id.clone()));
            id
        }

        fn media_output(&self, media_id: &str, _media: MediaData) {
            self.events
                .lock()
                .expect("sink mutex poisoned")
                .push(SinkEvent::Output(media_id.to_string()));
        }

        fn media_failed(&self, media_id: &str) {
            self.events
                .lock()
                .expect("sink mutex poisoned")
                .push(SinkEvent::Failed(media_id.to_string()));
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    struct CallParams {
        prompt: String,
        referenced_image_paths: Option<Vec<String>>,
        n: Option<u32>,
        size: Option<String>,
        quality: Option<String>,
        transparent_background: Option<bool>,
        output_format: Option<String>,
        timeout_seconds: Option<u64>,
    }

    impl CallParams {
        fn new(prompt: &str) -> Self {
            Self {
                prompt: prompt.to_string(),
                referenced_image_paths: None,
                n: None,
                size: None,
                quality: None,
                transparent_background: None,
                output_format: None,
                timeout_seconds: None,
            }
        }

        fn stream(self, ctx: ToolContext) -> impl Stream<Item = Result<ContentPart, String>> {
            crate::image_gen::generate_image(
                ctx,
                self.prompt,
                self.referenced_image_paths,
                self.n,
                self.size,
                self.quality,
                self.transparent_background,
                self.output_format,
                self.timeout_seconds,
            )
        }
    }

    struct Collected {
        text: String,
        media_parts: usize,
        error: Option<String>,
    }

    async fn collect(stream: impl Stream<Item = Result<ContentPart, String>>) -> Collected {
        tokio::pin!(stream);
        let mut out = Collected {
            text: String::new(),
            media_parts: 0,
            error: None,
        };
        while let Some(item) = stream.next().await {
            match item {
                Ok(ContentPart::Text(text)) => out.text.push_str(&text),
                Ok(ContentPart::Media(_)) => out.media_parts += 1,
                Err(error) => out.error = Some(error),
            }
        }
        out
    }

    fn build_ctx(
        dir: &Path,
        llm: Arc<dyn LLM>,
        sink: Option<Arc<dyn MediaSink>>,
    ) -> (ToolContext, Arc<PermissionManager>) {
        let pm = Arc::new(PermissionManager::new(dir.join("permissions.json")));
        let scoped = ScopedPermissionManager::new(
            "generate_image",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );
        let ctx = ToolContext {
            cancel_token: CancellationToken::new(),
            permission: scoped,
            container_config: None,
            session_dir: Some(SessionDir::new(dir.to_path_buf())),
            supports_media: false,
            llm: Some(llm),
            model: None,
            media_sink: sink,
        };
        (ctx, pm)
    }

    fn grant_read(pm: &PermissionManager, path: &Path) -> Result<PermissionKey> {
        let canonical = path.canonicalize()?;
        let key = PermissionKey {
            tool: "file_read".to_string(),
            key: "path".to_string(),
            value: tcode_encoding::path_to_str(&canonical)?.to_string(),
        };
        pm.add_permission(key.clone(), PermissionScope::Session)?;
        Ok(key)
    }

    fn make_png_bytes() -> Vec<u8> {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(32, 32, |x, y| Rgb([x as u8, y as u8, 64u8]));
        let mut buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut buf);
        img.write_with_encoder(encoder)
            .expect("failed to encode test PNG");
        buf
    }

    fn encode(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn media_files(media_dir: &Path) -> Result<Vec<PathBuf>> {
        if !media_dir.exists() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        for entry in std::fs::read_dir(media_dir)? {
            files.push(entry?.path());
        }
        files.sort();
        Ok(files)
    }

    fn file_mode(path: &Path) -> Result<u32> {
        Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn generation_saves_returned_bytes_unprocessed_with_matching_extension() -> Result<()> {
        let png = make_png_bytes();
        let fake = FakeLlm::responding(vec![encode(&png)], Some("png"));
        let fixture = TestDir::new();
        let sink = Arc::new(FakeSink::default());
        let (ctx, _pm) = build_ctx(
            fixture.path(),
            Arc::new(fake.clone()),
            Some(sink.clone() as Arc<dyn MediaSink>),
        );

        let mut call = CallParams::new("a red panda");
        // Ask for webp; the backend returns PNG bytes and echoes "png".
        call.output_format = Some("webp".to_string());
        let collected = collect(call.stream(ctx)).await;

        assert!(
            collected.error.is_none(),
            "unexpected error: {:?}",
            collected.error
        );
        assert_eq!(
            collected.media_parts, 0,
            "tool output must not contain media parts"
        );

        let media_dir = fixture.path().join("media");
        let files = media_files(&media_dir)?;
        assert_eq!(files.len(), 1, "expected exactly one saved image");

        let saved = &files[0];
        assert!(
            saved.to_str().expect("utf8 path").ends_with(".png"),
            "extension must come from the returned bytes, got {}",
            saved.display()
        );
        let saved_bytes = std::fs::read(saved)?;
        assert_eq!(
            saved_bytes, png,
            "saved bytes must equal the decoded payload"
        );
        assert_eq!(file_mode(saved)?, 0o600, "saved image must be mode 0600");
        assert_eq!(file_mode(&media_dir)?, 0o700, "media dir must be mode 0700");

        assert!(
            collected
                .text
                .contains(media_dir.to_str().expect("utf8 path")),
            "output must contain the absolute path, got: {}",
            collected.text
        );
        assert!(
            collected.text.contains("(image/png)"),
            "output must report the returned media type, got: {}",
            collected.text
        );

        let events = sink.events();
        assert_eq!(
            events.len(),
            2,
            "expected one started/output pair: {events:?}"
        );
        match (&events[0], &events[1]) {
            (SinkEvent::Started(start), SinkEvent::Output(out)) => assert_eq!(start, out),
            other => panic!("expected Started then Output, got {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn several_images_yield_several_files_and_media_pairs() -> Result<()> {
        let png = encode(&make_png_bytes());
        let fake = FakeLlm::responding(vec![png.clone(), png], None);
        let fixture = TestDir::new();
        let sink = Arc::new(FakeSink::default());
        let (ctx, _pm) = build_ctx(
            fixture.path(),
            Arc::new(fake.clone()),
            Some(sink.clone() as Arc<dyn MediaSink>),
        );

        let collected = collect(CallParams::new("two cats").stream(ctx)).await;
        assert!(
            collected.error.is_none(),
            "unexpected error: {:?}",
            collected.error
        );

        let files = media_files(&fixture.path().join("media"))?;
        assert_eq!(files.len(), 2, "expected two saved images");
        assert_eq!(
            collected.text.matches("[Image: ").count(),
            2,
            "expected two image lines, got: {}",
            collected.text
        );
        assert!(
            collected.text.contains("Generated 2 images"),
            "expected plural hint, got: {}",
            collected.text
        );

        let events = sink.events();
        assert_eq!(
            events.len(),
            4,
            "expected two started/output pairs: {events:?}"
        );
        for pair in events.chunks(2) {
            match (&pair[0], &pair[1]) {
                (SinkEvent::Started(start), SinkEvent::Output(out)) => assert_eq!(start, out),
                other => panic!("expected Started then Output, got {other:?}"),
            }
        }
        let started: Vec<&String> = events
            .iter()
            .filter_map(|event| match event {
                SinkEvent::Started(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(started.len(), 2);
        assert_ne!(
            started[0], started[1],
            "each image must have its own media id"
        );
        Ok(())
    }

    #[tokio::test]
    async fn n_and_output_format_omitted_when_unset() -> Result<()> {
        let fake = FakeLlm::responding(vec![encode(&make_png_bytes())], None);
        let fixture = TestDir::new();
        let (ctx, _pm) = build_ctx(fixture.path(), Arc::new(fake.clone()), None);

        let collected = collect(CallParams::new("a dog").stream(ctx)).await;
        assert!(
            collected.error.is_none(),
            "unexpected error: {:?}",
            collected.error
        );

        let requests = fake.requests();
        assert_eq!(requests.len(), 1);
        match &requests[0] {
            ImageRequest::Generate(params) => {
                assert_eq!(params.n, None, "n must be omitted when unset");
                assert_eq!(
                    params.output_format, None,
                    "output_format must be omitted when unset"
                );
                assert_eq!(params.background, "opaque");
            }
            other => panic!("expected a generate request, got {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn transparent_background_maps_to_background() -> Result<()> {
        let fake = FakeLlm::responding(vec![encode(&make_png_bytes())], None);
        let fixture = TestDir::new();
        let (ctx, _pm) = build_ctx(fixture.path(), Arc::new(fake.clone()), None);

        let mut transparent = CallParams::new("transparent logo");
        transparent.transparent_background = Some(true);
        let collected = collect(transparent.stream(ctx)).await;
        assert!(collected.error.is_none(), "{:?}", collected.error);

        let (ctx, _pm) = build_ctx(fixture.path(), Arc::new(fake.clone()), None);
        let collected = collect(CallParams::new("opaque logo").stream(ctx)).await;
        assert!(collected.error.is_none(), "{:?}", collected.error);

        let requests = fake.requests();
        let backgrounds: Vec<&str> = requests
            .iter()
            .map(|request| match request {
                ImageRequest::Generate(params) => params.background.as_str(),
                ImageRequest::Edit { params, .. } => params.background.as_str(),
            })
            .collect();
        assert_eq!(backgrounds, vec!["transparent", "opaque"]);
        Ok(())
    }

    #[tokio::test]
    async fn edit_request_carries_input_images() -> Result<()> {
        let fixture = TestDir::new();
        let session_root = fixture.path().to_path_buf();
        let input = session_root.join("input.png");
        std::fs::write(&input, make_png_bytes())?;

        let fake = FakeLlm::responding(vec![encode(&make_png_bytes())], None);
        let sink = Arc::new(FakeSink::default());
        let (ctx, pm) = build_ctx(
            &session_root,
            Arc::new(fake.clone()),
            Some(sink.clone() as Arc<dyn MediaSink>),
        );
        grant_read(&pm, &session_root)?;

        let mut call = CallParams::new("add a hat");
        call.referenced_image_paths = Some(vec![input.to_str().expect("utf8 path").to_string()]);
        let collected = collect(call.stream(ctx)).await;
        assert!(
            collected.error.is_none(),
            "unexpected error: {:?}",
            collected.error
        );

        let requests = fake.requests();
        assert_eq!(requests.len(), 1);
        match &requests[0] {
            ImageRequest::Edit { images, .. } => {
                assert_eq!(images.len(), 1);
                assert_eq!(images[0].media_type, "image/png");
                assert!(!images[0].data.is_empty());
            }
            other => panic!("expected an edit request, got {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn edit_input_outside_file_root_is_denied_without_api_call() -> Result<()> {
        let fixture = TestDir::new();
        let session_root = fixture.path().join("session");
        let outside = fixture.path().join("outside");
        std::fs::create_dir_all(&session_root)?;
        std::fs::create_dir_all(&outside)?;
        let outside_file = outside.join("secret.png");
        std::fs::write(&outside_file, make_png_bytes())?;

        let fake = FakeLlm::responding(vec![encode(&make_png_bytes())], None);
        let sink = Arc::new(FakeSink::default());
        let (ctx, pm) = build_ctx(
            &session_root,
            Arc::new(fake.clone()),
            Some(sink.clone() as Arc<dyn MediaSink>),
        );
        pm.set_file_root(Some(session_root.clone()))?;

        let mut call = CallParams::new("edit it");
        call.referenced_image_paths =
            Some(vec![outside_file.to_str().expect("utf8 path").to_string()]);
        let collected = collect(call.stream(ctx)).await;

        let error = collected.error.expect("expected a permission error");
        assert!(
            error.contains("web sessions can only access files inside the session directory"),
            "expected the confinement message, got: {error}"
        );
        assert!(fake.requests().is_empty(), "no API call must be made");
        assert!(sink.events().is_empty(), "no media event must be emitted");
        assert_eq!(media_files(&session_root.join("media"))?.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn at_most_five_image_inputs_enforced() -> Result<()> {
        let fixture = TestDir::new();
        let fake = FakeLlm::responding(vec![encode(&make_png_bytes())], None);
        let (ctx, _pm) = build_ctx(fixture.path(), Arc::new(fake.clone()), None);

        let mut call = CallParams::new("too many");
        call.referenced_image_paths = Some(
            (0..6)
                .map(|index| {
                    fixture
                        .path()
                        .join(format!("img{index}.png"))
                        .to_str()
                        .expect("utf8 path")
                        .to_string()
                })
                .collect(),
        );
        let collected = collect(call.stream(ctx)).await;

        let error = collected.error.expect("expected an error");
        assert!(
            error.contains("at most 5"),
            "expected the path-count limit message, got: {error}"
        );
        assert!(fake.requests().is_empty(), "no API call must be made");
        Ok(())
    }

    #[tokio::test]
    async fn hanging_provider_times_out_with_failure_event() -> Result<()> {
        let fake = FakeLlm::with(FakeState {
            supports: true,
            images: Vec::new(),
            output_format: None,
            error: None,
            hang: true,
            received: Vec::new(),
        });
        let fixture = TestDir::new();
        let sink = Arc::new(FakeSink::default());
        let (ctx, _pm) = build_ctx(
            fixture.path(),
            Arc::new(fake.clone()),
            Some(sink.clone() as Arc<dyn MediaSink>),
        );

        let mut call = CallParams::new("will hang");
        call.timeout_seconds = Some(1);
        let collected = collect(call.stream(ctx)).await;

        let error = collected.error.expect("expected a timeout error");
        assert!(
            error.contains("timed out") && error.contains('1'),
            "expected a timeout reporting the configured seconds, got: {error}"
        );

        let events = sink.events();
        assert_eq!(
            events.len(),
            2,
            "expected one started/failed pair: {events:?}"
        );
        match (&events[0], &events[1]) {
            (SinkEvent::Started(start), SinkEvent::Failed(failed)) => assert_eq!(start, failed),
            other => panic!("expected Started then Failed, got {other:?}"),
        }
        assert_eq!(media_files(&fixture.path().join("media"))?.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn timeout_default_is_used_when_unset() -> Result<()> {
        // A provider that returns immediately must not be affected by the default
        // timeout. This also exercises the `DEFAULT_TIMEOUT_SECS` fallback path.
        assert_eq!(crate::image_gen::DEFAULT_TIMEOUT_SECS, 300);
        let fake = FakeLlm::responding(vec![encode(&make_png_bytes())], None);
        let fixture = TestDir::new();
        let (ctx, _pm) = build_ctx(fixture.path(), Arc::new(fake.clone()), None);
        let collected = tokio::time::timeout(
            Duration::from_secs(5),
            collect(CallParams::new("quick").stream(ctx)),
        )
        .await?;
        assert!(
            collected.error.is_none(),
            "unexpected error: {:?}",
            collected.error
        );
        Ok(())
    }
}
