use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use llm_rs::conversation::{Message, MessageEndStatus};
use llm_rs::llm::{ChatOptions, LLM, LLMEvent, LLMMessage, ModelInfo};
use llm_rs::tool::Tool;
use tokio::net::UnixListener;
use tokio_stream::Stream;

use crate::bootstrap::send_socket_message;
use crate::protocol::{ClientKind, ClientMessage, RuntimeOwnerKind, ServerMessage};
use crate::server::{
    ClientLeaseTracker, EventWriterActivityGuard, Server, ServerRuntimeOptions,
    WebIdleShutdownPolicy, WorkActivityTracker, assistant_activity_key, close_stale_running_items,
    tool_activity_key, validate_owner_shutdown_token,
};
use crate::session::SessionMode;

fn test_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/r")
}

fn temp_dir() -> PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

#[derive(Clone)]
struct MockLlm {
    registered_tools: Arc<parking_lot::Mutex<Vec<String>>>,
}

impl MockLlm {
    fn new() -> Self {
        Self {
            registered_tools: Arc::new(parking_lot::Mutex::new(Vec::new())),
        }
    }

    fn with_registered_tools(registered_tools: Arc<parking_lot::Mutex<Vec<String>>>) -> Self {
        Self { registered_tools }
    }
}

impl LLM for MockLlm {
    fn register_tools(&mut self, tools: Vec<Arc<Tool>>) {
        *self.registered_tools.lock() = tools.iter().map(|tool| tool.name.clone()).collect();
    }

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

    fn available_models(&self) -> Vec<ModelInfo> {
        Vec::new()
    }
}

fn test_server(socket_path: PathBuf, session_dir: PathBuf, owner_token: &str) -> Server {
    Server::new_with_runtime_options(
        socket_path,
        session_dir.join("display.jsonl"),
        session_dir.join("status.txt"),
        session_dir.join("usage.txt"),
        session_dir.clone(),
        session_dir.join("conversation.json"),
        Box::new(MockLlm::new()),
        "mock-model".to_string(),
        ChatOptions::default(),
        10,
        false,
        None,
        None,
        ServerRuntimeOptions {
            owner_kind: RuntimeOwnerKind::Serve,
            session_mode: SessionMode::Normal,
            owner_shutdown_token: owner_token.to_string(),
            lease_timeout: Duration::from_secs(60),
        },
    )
}

#[test]
fn client_lease_register_heartbeat_detach_and_prune() {
    let tracker = ClientLeaseTracker::new(Duration::from_secs(60));
    let now = Instant::now();

    let cli_id = tracker.register(ClientKind::Cli, Some("attach".to_string()), now);
    let web_id = tracker.register(ClientKind::Web, None, now);

    assert_eq!(tracker.active_count(now), 2);
    let snapshot = tracker.snapshot();
    assert_eq!(snapshot.len(), 2);
    assert!(snapshot.iter().any(|record| {
        record.client_kind == ClientKind::Cli && record.client_label.as_deref() == Some("attach")
    }));
    assert!(
        snapshot.iter().any(|record| {
            record.client_kind == ClientKind::Web && record.client_label.is_none()
        })
    );

    let renewed_at = now + Duration::from_secs(30);
    assert!(tracker.heartbeat(&cli_id, renewed_at));
    assert!(!tracker.heartbeat("missing-client", renewed_at));

    let prune_at = now + Duration::from_secs(61);
    assert!(!tracker.heartbeat(&web_id, prune_at));
    assert_eq!(tracker.active_count(prune_at), 1);

    assert!(tracker.detach(&cli_id));
    assert!(!tracker.detach(&cli_id));
    assert_eq!(tracker.active_count(prune_at), 0);
    assert!(!tracker.detach(&web_id));
}

#[test]
fn work_activity_tracker_tracks_assistant_lifecycle() {
    let tracker = WorkActivityTracker::new();
    assert!(!tracker.has_active_work());

    tracker.assistant_started("conversation-1");
    assert!(tracker.has_active_work());

    tracker.assistant_finished("conversation-1");
    assert!(!tracker.has_active_work());
}

#[test]
fn work_activity_tracker_tracks_tool_lifecycle_and_duplicate_start() {
    let tracker = WorkActivityTracker::new();

    tracker.tool_started("tool-1");
    assert!(tracker.has_active_work());

    tracker.tool_started("tool-1");
    tracker.tool_finished("tool-1");
    assert!(!tracker.has_active_work());
}

#[test]
fn work_activity_tracker_tracks_subagent_turns_and_continuation() {
    let tracker = WorkActivityTracker::new();

    tracker.subagent_started("subagent-1");
    assert!(tracker.has_active_work());

    tracker.subagent_finished_or_idle("subagent-1");
    assert!(!tracker.has_active_work());

    tracker.subagent_started("subagent-1");
    assert!(tracker.has_active_work());

    tracker.subagent_finished_or_idle("subagent-1");
    assert!(!tracker.has_active_work());
}

#[test]
fn work_activity_tracker_requires_all_simultaneous_work_to_finish() {
    let tracker = WorkActivityTracker::new();

    tracker.assistant_started("conversation-1");
    tracker.tool_started("tool-1");
    tracker.subagent_started("subagent-1");
    assert!(tracker.has_active_work());

    tracker.assistant_finished("conversation-1");
    assert!(tracker.has_active_work());

    tracker.tool_finished("tool-1");
    assert!(tracker.has_active_work());

    tracker.subagent_finished_or_idle("subagent-1");
    assert!(!tracker.has_active_work());
}

#[test]
fn event_writer_activity_guard_cleans_unfinished_work_on_drop() {
    let tracker = Arc::new(WorkActivityTracker::new());

    {
        let mut guard = EventWriterActivityGuard::new(Arc::clone(&tracker));
        guard.assistant_started("conversation-1");
        guard.tool_started("tool-1");
        guard.subagent_started("subagent-1");
        assert!(tracker.has_active_work());
    }

    assert!(!tracker.has_active_work());
}

#[test]
fn event_writer_activity_guard_keeps_work_finished_before_drop_inactive() {
    let tracker = Arc::new(WorkActivityTracker::new());

    {
        let mut guard = EventWriterActivityGuard::new(Arc::clone(&tracker));
        guard.assistant_started("conversation-1");
        guard.tool_started("tool-1");
        guard.subagent_started("subagent-1");
        guard.assistant_finished("conversation-1");
        guard.tool_finished("tool-1");
        guard.subagent_finished_or_idle("subagent-1");
        assert!(!tracker.has_active_work());
    }

    assert!(!tracker.has_active_work());
}

#[test]
fn event_writer_assistant_activity_key_is_stable_across_message_events() {
    let tracker = Arc::new(WorkActivityTracker::new());
    let dir = temp_dir();

    let mut guard = EventWriterActivityGuard::new(Arc::clone(&tracker));
    guard.assistant_started(assistant_activity_key(&dir));
    assert!(tracker.has_active_work());

    guard.assistant_finished(&assistant_activity_key(&dir));
    assert!(!tracker.has_active_work());
}

#[test]
fn event_writer_tool_activity_keys_are_namespaced_by_session_dir() {
    let tracker = Arc::new(WorkActivityTracker::new());
    let root_dir = temp_dir();
    let nested_dir = root_dir.join("subagent-child");

    let mut root_guard = EventWriterActivityGuard::new(Arc::clone(&tracker));
    let mut nested_guard = EventWriterActivityGuard::new(Arc::clone(&tracker));

    root_guard.tool_started(tool_activity_key(&root_dir, "tool-1"));
    nested_guard.tool_started(tool_activity_key(&nested_dir, "tool-1"));
    assert!(tracker.has_active_work());

    root_guard.tool_finished(&tool_activity_key(&root_dir, "tool-1"));
    assert!(tracker.has_active_work());

    nested_guard.tool_finished(&tool_activity_key(&nested_dir, "tool-1"));
    assert!(!tracker.has_active_work());
}
#[test]
fn web_idle_shutdown_policy_active_lease_never_shuts_down() {
    let mut policy = WebIdleShutdownPolicy::new(Duration::from_secs(60));
    let now = Instant::now();

    assert!(!policy.should_shutdown_after_prune(1, false, false, now));
    assert!(!policy.should_shutdown_after_prune(1, true, false, now + Duration::from_secs(120)));
}

#[test]
fn web_idle_shutdown_policy_zero_leases_no_work_uses_grace() {
    let mut policy = WebIdleShutdownPolicy::new(Duration::from_secs(60));
    let now = Instant::now();

    assert!(!policy.should_shutdown_after_prune(0, false, false, now));
    assert!(!policy.should_shutdown_after_prune(0, false, false, now + Duration::from_secs(59)));
    assert!(policy.should_shutdown_after_prune(0, false, false, now + Duration::from_secs(60)));
}

#[test]
fn web_idle_shutdown_policy_resets_grace_when_lease_becomes_active() {
    let mut policy = WebIdleShutdownPolicy::new(Duration::from_secs(60));
    let now = Instant::now();

    assert!(!policy.should_shutdown_after_prune(0, false, false, now));
    assert!(!policy.should_shutdown_after_prune(1, false, false, now + Duration::from_secs(30)));
    assert!(!policy.should_shutdown_after_prune(0, false, false, now + Duration::from_secs(61)));
    assert!(policy.should_shutdown_after_prune(0, false, false, now + Duration::from_secs(121)));
}

#[test]
fn web_idle_shutdown_policy_last_lease_expiry_no_work_shuts_down_immediately() {
    let mut policy = WebIdleShutdownPolicy::new(Duration::from_secs(60));
    let now = Instant::now();

    assert!(!policy.should_shutdown_after_prune(1, false, false, now));
    assert!(policy.should_shutdown_after_prune(0, true, false, now + Duration::from_secs(60)));
}

#[test]
fn web_idle_shutdown_policy_last_lease_expiry_with_work_waits_until_inactive() {
    let mut policy = WebIdleShutdownPolicy::new(Duration::from_secs(60));
    let now = Instant::now();

    assert!(!policy.should_shutdown_after_prune(1, false, false, now));
    assert!(!policy.should_shutdown_after_prune(0, true, true, now + Duration::from_secs(60)));
    assert!(!policy.should_shutdown_after_prune(0, false, true, now + Duration::from_secs(120)));
    assert!(policy.should_shutdown_after_prune(0, false, false, now + Duration::from_secs(121)));
}

#[test]
fn web_idle_shutdown_policy_explicit_empty_with_work_does_not_advance_grace() {
    let mut policy = WebIdleShutdownPolicy::new(Duration::from_secs(60));
    let now = Instant::now();

    assert!(!policy.should_shutdown_after_prune(0, false, true, now));
    assert!(!policy.should_shutdown_after_prune(0, false, true, now + Duration::from_secs(120)));
    assert!(!policy.should_shutdown_after_prune(0, false, false, now + Duration::from_secs(121)));
    assert!(!policy.should_shutdown_after_prune(0, false, false, now + Duration::from_secs(180)));
    assert!(policy.should_shutdown_after_prune(0, false, false, now + Duration::from_secs(181)));
}

#[test]
fn web_idle_shutdown_policy_work_resets_existing_explicit_empty_grace() {
    let mut policy = WebIdleShutdownPolicy::new(Duration::from_secs(60));
    let now = Instant::now();

    assert!(!policy.should_shutdown_after_prune(0, false, false, now));
    assert!(!policy.should_shutdown_after_prune(0, false, true, now + Duration::from_secs(30)));
    assert!(!policy.should_shutdown_after_prune(0, false, false, now + Duration::from_secs(120)));
    assert!(policy.should_shutdown_after_prune(0, false, false, now + Duration::from_secs(180)));
}

#[test]
fn shutdown_authorization_requires_owner_token_match() {
    assert!(validate_owner_shutdown_token("owner-token", "owner-token"));
    assert!(!validate_owner_shutdown_token(
        "attached-client",
        "owner-token"
    ));
    assert!(!validate_owner_shutdown_token("", "owner-token"));
    assert!(!validate_owner_shutdown_token("owner-token", ""));
}

#[test]
fn server_runtime_options_default_to_normal_session_mode() {
    assert_eq!(
        ServerRuntimeOptions::default().session_mode,
        SessionMode::Normal
    );
}

#[tokio::test]
async fn web_only_runtime_reports_mode_and_registers_only_web_tools() -> anyhow::Result<()> {
    let dir = temp_dir();
    let socket_path = dir.join("s");
    let owner_token = "owner-token";
    let registered_tools = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let server = Server::new_with_runtime_options(
        socket_path.clone(),
        dir.join("display.jsonl"),
        dir.join("status.txt"),
        dir.join("usage.txt"),
        dir.clone(),
        dir.join("conversation.json"),
        Box::new(MockLlm::with_registered_tools(Arc::clone(
            &registered_tools,
        ))),
        "mock-model".to_string(),
        ChatOptions::default(),
        10,
        false,
        None,
        None,
        ServerRuntimeOptions {
            owner_kind: RuntimeOwnerKind::Serve,
            session_mode: SessionMode::WebOnly,
            owner_shutdown_token: owner_token.to_string(),
            lease_timeout: Duration::from_secs(60),
        },
    );
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(server.run(Some(ready_tx)));

    ready_rx.await??;

    let response =
        send_socket_message(socket_path.clone(), &ClientMessage::GetSessionRuntimeInfo).await?;
    let Some(ServerMessage::SessionRuntimeInfo(info)) = response else {
        anyhow::bail!("expected runtime info response");
    };
    assert_eq!(info.session_mode, SessionMode::WebOnly);
    assert_eq!(
        registered_tools.lock().clone(),
        vec![
            "current_time".to_string(),
            "web_fetch".to_string(),
            "web_search".to_string(),
            "subagent".to_string(),
            "continue_subagent".to_string(),
        ]
    );
    assert!(!dir.join("lsp-hint.txt").exists());

    let response = send_socket_message(
        socket_path,
        &ClientMessage::AuthorizedShutdown {
            owner_token: owner_token.to_string(),
        },
    )
    .await?;
    assert!(matches!(response, Some(ServerMessage::Ack)));
    handle.await??;
    Ok(())
}

#[tokio::test]
async fn server_run_removes_stale_socket_inside_startup_path_before_bind() -> anyhow::Result<()> {
    let dir = temp_dir();
    let socket_path = dir.join("s");
    let stale_listener = UnixListener::bind(&socket_path)?;
    drop(stale_listener);
    assert!(socket_path.exists());

    let owner_token = "owner-token";
    let server = test_server(socket_path.clone(), dir, owner_token);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(server.run(Some(ready_tx)));

    ready_rx.await??;
    let response = send_socket_message(
        socket_path.clone(),
        &ClientMessage::AuthorizedShutdown {
            owner_token: owner_token.to_string(),
        },
    )
    .await?;
    assert!(matches!(response, Some(ServerMessage::Ack)));

    handle.await??;
    assert!(!socket_path.exists());
    Ok(())
}
#[tokio::test]
async fn close_stale_running_tool_call_writes_cancelled_status() -> anyhow::Result<()> {
    let dir = temp_dir();
    let tool_call_id = "tool-1";
    let start = Message::ToolMessageStart {
        msg_id: 1,
        tool_call_id: tool_call_id.to_string(),
        created_at: 0,
        tool_name: "bash".to_string(),
        tool_args: "{}".to_string(),
    };
    let line = serde_json::to_string(&start)?;
    tokio::fs::write(dir.join("display.jsonl"), format!("{line}\n")).await?;
    tokio::fs::write(
        dir.join(format!("tool-call-{tool_call_id}.jsonl")),
        format!("{line}\n"),
    )
    .await?;

    close_stale_running_items(&dir).await?;

    let status =
        tokio::fs::read_to_string(dir.join(format!("tool-call-{tool_call_id}-status.txt"))).await?;
    assert_eq!(status, "Cancelled");

    let display = tokio::fs::read_to_string(dir.join("display.jsonl")).await?;
    let last_line = display
        .lines()
        .last()
        .expect("display should contain synthetic end event");
    let end: Message = serde_json::from_str(last_line)?;
    assert!(matches!(
        end,
        Message::ToolMessageEnd {
            end_status: MessageEndStatus::Cancelled,
            ..
        }
    ));

    Ok(())
}
