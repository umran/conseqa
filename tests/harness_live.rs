//! Opt-in live smoke tests (§106 of the confluence spec).
//!
//! These launch a real coding agent against a local confluence server
//! and incur model cost, so they run only when explicitly enabled:
//!
//! ```text
//! CONSEQA_TEST_CLAUDE=1 cargo test --features confluence --test harness_live
//! CONSEQA_TEST_CODEX=1  cargo test --features confluence --test harness_live
//! ```
//!
//! Kept small and outside default CI. The agent is given one trivial
//! decomposition task and the test asserts only that a real session
//! connected to the MCP endpoint and reached a terminal confluence
//! state — never a specific architecture, which depends on the model.

use std::sync::Arc;
use std::time::Duration;

use conseqa::confluence::{
    BundleSpec, ConfluenceEngine, RunId, RunMetadata, WorkspaceState, WriteScope, mcp,
};
use conseqa::harness::backend::InvocationBudget;
use conseqa::harness::backends::{ClaudeCliBackend, CodexCliBackend};
use conseqa::harness::{
    AgentBackend, LogicalTask, Scheduler, SchedulerPolicy, Supervisor,
};
use conseqa::confluence::TaskKind;

fn enabled(var: &str) -> bool {
    std::env::var(var).map(|value| value == "1").unwrap_or(false)
}

async fn run_live(backend: Arc<dyn AgentBackend>, label: &str) {
    // Route the supervisor's tool-call and session tracing to the test
    // output so `--nocapture` shows what the live agent actually did.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "conseqa=debug".into()),
        )
        .with_test_writer()
        .try_init();

    let engine = ConfluenceEngine::in_memory(WorkspaceState::empty(RunMetadata::new(RunId(
        format!("live-{label}"),
    ))))
    .expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("mcp server binds");

    let mcp_url = format!("http://{}/mcp", server.local_addr);

    let work_dir = std::env::temp_dir().join(format!("conseqa-live-{}", uuid::Uuid::new_v4()));

    let supervisor = Supervisor::new(
        engine.clone(),
        backend,
        mcp_url,
        None,
        work_dir.clone(),
    );

    let scheduler = Scheduler::new(
        engine.clone(),
        supervisor,
        SchedulerPolicy {
            max_attempts: 1,
            invocation_budget: InvocationBudget {
                max_wall_time_secs: Some(150),
                max_turns: Some(20),
            },
            ..Default::default()
        },
    );

    // A concrete task with a definite engine-side effect: creating one
    // service through submit_patch. The assertion below proves a real
    // agent process reached through MCP into the confluence engine and
    // committed — not merely that a subprocess started.
    let run = scheduler
        .run(&LogicalTask {
            kind: TaskKind::Decompose,
            objective: "Create exactly one service in the shared architecture. Call \
                        dsl_reference to see the mutation shape, then call submit_patch \
                        with a single put_service mutation for a backend service named \
                        `service.api`. Do not create anything else. Finish once the \
                        patch commits."
                .to_string(),
            write_scope: WriteScope::shared_skeleton(),
            bundle: BundleSpec::default(),
            prompt_evidence: Vec::new(),
            interactive: false,
        })
        .await
        .expect("the live task runs");

    let head = engine.head_revision();

    println!(
        "live {label}: task {} ended {}, head at revision {}",
        run.task, run.final_state, head.0
    );

    assert!(!run.attempts.is_empty(), "at least one session ran");

    let attempt = &run.attempts[0];
    assert!(
        attempt.agent_exit.session.is_some(),
        "a real session id was reported: {:?}",
        attempt.agent_exit
    );

    // The definitive end-to-end proof: the agent reached the confluence
    // engine over MCP and committed, advancing the head past the empty
    // initial revision, and its service is in the workspace.
    assert!(
        head.0 > 0,
        "the live agent did not commit; head is still at revision 0 \
         (agent exit: {:?})",
        attempt.agent_exit
    );

    let committed = engine.head_snapshot();
    assert!(
        committed
            .workspace
            .services
            .contains_key(&conseqa::spec::Id("service.api".to_string())),
        "expected service.api in the committed workspace, found: {:?}",
        committed.workspace.services.keys().collect::<Vec<_>>()
    );

    server.shutdown().await;

    std::fs::remove_dir_all(&work_dir).ok();
}

#[tokio::test]
async fn claude_connects_and_reaches_a_terminal_state() {
    if !enabled("CONSEQA_TEST_CLAUDE") {
        eprintln!("skipping: set CONSEQA_TEST_CLAUDE=1 to run the live Claude smoke test");
        return;
    }

    let backend = Arc::new(ClaudeCliBackend::new());

    tokio::time::timeout(Duration::from_secs(180), run_live(backend, "claude"))
        .await
        .expect("the live Claude session finishes within the timeout");
}

#[tokio::test]
async fn codex_connects_and_reaches_a_terminal_state() {
    if !enabled("CONSEQA_TEST_CODEX") {
        eprintln!("skipping: set CONSEQA_TEST_CODEX=1 to run the live Codex smoke test");
        return;
    }

    let backend = Arc::new(CodexCliBackend::new());

    tokio::time::timeout(Duration::from_secs(180), run_live(backend, "codex"))
        .await
        .expect("the live Codex session finishes within the timeout");
}

