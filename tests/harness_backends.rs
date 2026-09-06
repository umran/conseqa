//! Backend adapter tests (§105 of the confluence spec): process
//! startup, MCP configuration injection, stdout streaming, completion
//! parsing, cancellation, timeout, and non-zero exit — driven by fake
//! executables that emit the same event shape as the real CLIs, so no
//! live vendor CLI is required.

use std::path::PathBuf;
use std::time::Duration;

use conseqa::confluence::{TaskId, TaskKind};
use conseqa::harness::backend::{
    AgentBackend, AgentEvent, AgentExitStatus, AgentHandle, AgentInvocation, InvocationBudget,
};
use conseqa::harness::backends::{ClaudeCliBackend, CodexCliBackend};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn fake(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fake_agents")
        .join(name)
        .display()
        .to_string()
}

fn work_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("conseqa-backend-{}", uuid::Uuid::new_v4()));

    std::fs::create_dir_all(&dir).expect("scratch dir");

    dir
}

fn invocation(work_dir: PathBuf) -> AgentInvocation {
    AgentInvocation {
        task: TaskId::fresh(),
        kind: TaskKind::OperationSynthesis,
        prompt: "synthesize operation.create_order".to_string(),
        mcp_url: "http://127.0.0.1:43127/mcp".to_string(),
        task_token: "test-token-abc123".to_string(),
        repo: None,
        work_dir,
        budget: InvocationBudget::default(),
    }
}

fn handle(task: TaskId) -> (AgentHandle, CancellationToken) {
    let cancel = CancellationToken::new();

    (
        AgentHandle {
            task,
            cancel: cancel.clone(),
        },
        cancel,
    )
}

fn collect(mut rx: mpsc::UnboundedReceiver<AgentEvent>) -> tokio::task::JoinHandle<Vec<AgentEvent>> {
    tokio::spawn(async move {
        let mut events = Vec::new();

        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        events
    })
}

#[tokio::test]
async fn claude_backend_streams_events_injects_config_and_completes() {
    let backend = ClaudeCliBackend::new().with_program(fake("claude_ok.sh"));
    let dir = work_dir();
    let invocation = invocation(dir.clone());
    let task = invocation.task;

    let (handle, _cancel) = handle(task);
    let (tx, rx) = mpsc::unbounded_channel();
    let events = collect(rx);

    let exit = backend
        .run(invocation, handle, tx)
        .await
        .expect("the fake claude runs");

    assert_eq!(exit.status, AgentExitStatus::Completed);
    assert_eq!(exit.session.as_deref(), Some("fake-session-0001"));
    assert_eq!(exit.usage.turns, Some(3));
    assert_eq!(exit.usage.tokens, Some(150));
    assert_eq!(exit.usage.usd_cents, Some(1)); // 0.0123 usd -> 1 cent
    assert_eq!(exit.backend.name, "claude-cli");

    let events = events.await.expect("collector");

    // The session-started and tool-call events came through.
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::SessionStarted { session } if session.as_deref() == Some("fake-session-0001")
    )));

    let tool_calls: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCall { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();

    assert!(tool_calls.contains(&"mcp__conseqa__submit_patch"));

    // The MCP config file was written with the confluence server and
    // an env-var token placeholder — never the raw token (§57.1).
    let configs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("work dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
        .collect();

    assert_eq!(configs.len(), 1, "one MCP config was written");

    let config = std::fs::read_to_string(&configs[0]).expect("config readable");

    assert!(config.contains("http://127.0.0.1:43127/mcp"), "{config}");
    assert!(config.contains("${CONSEQA_TASK_TOKEN}"), "{config}");
    assert!(!config.contains("test-token-abc123"), "raw token leaked: {config}");

    // The fake echoed the token from its environment, proving env
    // injection reached the child.
    assert!(
        exit.final_message
            .as_deref()
            .unwrap_or_default()
            .contains("token=test-token-abc123"),
        "{:?}",
        exit.final_message
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn claude_backend_reports_semantic_error_on_zero_exit() {
    let backend = ClaudeCliBackend::new().with_program(fake("claude_error.sh"));
    let dir = work_dir();
    let invocation = invocation(dir.clone());
    let task = invocation.task;

    let (handle, _cancel) = handle(task);
    let (tx, _rx) = mpsc::unbounded_channel();

    let exit = backend.run(invocation, handle, tx).await.expect("runs");

    // is_error:true makes the session a failure even though the
    // process exited 0.
    assert!(matches!(exit.status, AgentExitStatus::Failed { .. }));

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn claude_backend_reports_non_zero_exit() {
    let backend = ClaudeCliBackend::new().with_program(fake("claude_nonzero.sh"));
    let dir = work_dir();
    let invocation = invocation(dir.clone());
    let task = invocation.task;

    let (handle, _cancel) = handle(task);
    let (tx, rx) = mpsc::unbounded_channel();
    let events = collect(rx);

    let exit = backend.run(invocation, handle, tx).await.expect("runs");

    assert_eq!(exit.status, AgentExitStatus::Failed { code: Some(7) });

    // Stderr was surfaced as a log event.
    let events = events.await.expect("collector");

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Log { message } if message.contains("simulated crash")
    )));

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn claude_backend_honors_cancellation() {
    let backend = ClaudeCliBackend::new().with_program(fake("claude_hang.sh"));
    let dir = work_dir();
    let invocation = invocation(dir.clone());
    let task = invocation.task;

    let (handle, cancel) = handle(task);
    let (tx, _rx) = mpsc::unbounded_channel();

    // Cancel shortly after launch.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    });

    let exit = tokio::time::timeout(Duration::from_secs(10), backend.run(invocation, handle, tx))
        .await
        .expect("the backend returns promptly after cancellation")
        .expect("runs");

    assert_eq!(exit.status, AgentExitStatus::Cancelled);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn claude_backend_times_out_a_hanging_session() {
    let backend = ClaudeCliBackend::new().with_program(fake("claude_hang.sh"));
    let dir = work_dir();

    let mut invocation = invocation(dir.clone());
    invocation.budget = InvocationBudget {
        max_wall_time_secs: Some(1),
        max_turns: None,
    };

    let task = invocation.task;
    let (handle, _cancel) = handle(task);
    let (tx, _rx) = mpsc::unbounded_channel();

    let exit = tokio::time::timeout(Duration::from_secs(10), backend.run(invocation, handle, tx))
        .await
        .expect("the backend returns after the timeout")
        .expect("runs");

    assert_eq!(exit.status, AgentExitStatus::TimedOut);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn codex_backend_streams_events_and_injects_overrides() {
    let backend = CodexCliBackend::new().with_program(fake("codex_ok.sh"));
    let dir = work_dir();
    let invocation = invocation(dir.clone());
    let task = invocation.task;

    let (handle, _cancel) = handle(task);
    let (tx, rx) = mpsc::unbounded_channel();
    let events = collect(rx);

    let exit = backend.run(invocation, handle, tx).await.expect("runs");

    assert_eq!(exit.status, AgentExitStatus::Completed);
    assert_eq!(exit.session.as_deref(), Some("codex-thread-1"));
    assert_eq!(exit.usage.tokens, Some(120));
    assert_eq!(exit.backend.name, "codex-cli");

    let events = events.await.expect("collector");

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCall { name } if name == "conseqa.task_context"
    )));

    // The config overrides carried the confluence URL and the env-var
    // token reference, and the env injected the real token.
    // The fake strips quotes from the echoed override values to keep
    // its event JSON valid, so assert on the quote-free forms.
    let message = exit.final_message.unwrap_or_default();

    assert!(
        message.contains("mcp_servers.conseqa.url=http://127.0.0.1:43127/mcp"),
        "{message}"
    );
    assert!(
        message.contains("mcp_servers.conseqa.bearer_token_env_var=CONSEQA_TASK_TOKEN"),
        "{message}"
    );
    assert!(message.contains("token=test-token-abc123"), "{message}");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn missing_executable_is_a_launch_error() {
    let backend = ClaudeCliBackend::new().with_program("/nonexistent/conseqa-fake-claude");
    let dir = work_dir();
    let invocation = invocation(dir.clone());
    let task = invocation.task;

    let (handle, _cancel) = handle(task);
    let (tx, _rx) = mpsc::unbounded_channel();

    let error = backend.run(invocation, handle, tx).await.expect_err("no such program");

    assert!(
        matches!(error, conseqa::harness::backend::AgentBackendError::Launch(_)),
        "{error:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
