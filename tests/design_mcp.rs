//! MCP surface acceptance (§107 phase 3): two independent MCP clients
//! read pinned snapshots and commit safely over Streamable HTTP, with
//! per-task bearer capabilities; a stale client's submission is
//! rejected with restart guidance.

use conseqa::design::{
    CreateTask, DesignEngine, RunId, RunMetadata, TaskBudget, TaskHandle, TaskKind, WorkspaceState,
    WriteScope, mcp,
};
use conseqa::spec::Id;

#[cfg(feature = "system-one")]
mod common;

fn fixture_workspace() -> WorkspaceState {
    let source =
        std::fs::read_to_string("tests/fixtures/flash_checkout.yaml").expect("fixture exists");

    let model = conseqa::parser::yaml::parse(&source).expect("fixture parses");

    WorkspaceState::from_model(&model, RunMetadata::new(RunId("mcp-test".to_string())))
}

fn task(engine: &DesignEngine, operation: &str) -> TaskHandle {
    engine
        .create_task(CreateTask {
            kind: TaskKind::OperationSynthesis,
            objective: format!("synthesize {operation}"),
            write_scope: WriteScope::operation_synthesis(Id(operation.to_string())),
            prompt_evidence: Vec::new(),
            budget: TaskBudget::default(),
        })
        .expect("task is created")
}

/// An interactive session with the full authoring grant: the
/// coordinator's credential on a single-project server.
fn session(engine: &DesignEngine) -> TaskHandle {
    engine
        .create_session(WriteScope::of([conseqa::design::WriteGrant::All]), "ui")
        .expect("session is created")
}

/// A minimal Streamable HTTP MCP client speaking raw JSON-RPC, so the
/// test exercises the wire protocol rather than a shared SDK.
struct McpClient {
    http: reqwest::Client,
    url: String,
    token: String,
    session: Option<String>,
    next_id: u64,

    /// The initialize result, kept so tests can read the server's
    /// declared instructions.
    init: serde_json::Value,
}

impl McpClient {
    async fn connect(url: &str, token: &str) -> Self {
        let mut client = Self {
            http: reqwest::Client::new(),
            url: url.to_string(),
            token: token.to_string(),
            session: None,
            next_id: 1,
            init: serde_json::Value::Null,
        };

        let response = client
            .rpc(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "conseqa-test", "version": "0"},
                },
            }))
            .await;

        assert!(
            response.get("result").is_some(),
            "initialize succeeds: {response}"
        );

        client.init = response["result"].clone();

        client
            .notify(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
            }))
            .await;

        client
    }

    async fn post(&mut self, body: serde_json::Value) -> reqwest::Response {
        let mut request = self
            .http
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.token))
            .json(&body);

        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session.clone());
        }

        let response = request.send().await.expect("the server responds");

        if let Some(session) = response.headers().get("mcp-session-id") {
            self.session = Some(session.to_str().expect("ascii session id").to_string());
        }

        response
    }

    async fn notify(&mut self, body: serde_json::Value) {
        let response = self.post(body).await;

        assert!(
            response.status().is_success(),
            "notification accepted: {}",
            response.status()
        );
    }

    /// Sends one request and decodes the JSON-RPC response, whether it
    /// arrives as plain JSON or as an SSE stream.
    async fn rpc(&mut self, body: serde_json::Value) -> serde_json::Value {
        let response = self.post(body).await;

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();

        let text = response.text().await.expect("body reads");

        if content_type.starts_with("text/event-stream") {
            for line in text.lines() {
                if let Some(data) = line.strip_prefix("data:")
                    && let Ok(message) = serde_json::from_str::<serde_json::Value>(data.trim())
                    && message.get("id").is_some()
                {
                    return message;
                }
            }

            panic!("no JSON-RPC response in SSE stream: {text}");
        }

        serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("response is not JSON ({error}): {content_type} {text}"))
    }

    /// Calls one tool and returns (parsed JSON payload, is_error).
    async fn call(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> (serde_json::Value, bool) {
        let id = self.next_id;

        self.next_id += 1;

        let response = self
            .rpc(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }))
            .await;

        let result = response
            .get("result")
            .unwrap_or_else(|| panic!("tool call has a result: {response}"));

        let is_error = result
            .get("isError")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("tool returned text content: {result}"));

        let payload = serde_json::from_str(text)
            .unwrap_or_else(|_| serde_json::Value::String(text.to_string()));

        (payload, is_error)
    }

    /// Calls a tool and returns every content block's text, for a tool
    /// that answers with more than one.
    async fn call_texts(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> (Vec<String>, bool) {
        let id = self.next_id;

        self.next_id += 1;

        let response = self
            .rpc(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }))
            .await;

        let result = &response["result"];

        let texts = result["content"]
            .as_array()
            .unwrap_or_else(|| panic!("tool returned content: {response}"))
            .iter()
            .map(|block| block["text"].as_str().expect("text content").to_string())
            .collect();

        (texts, result["isError"].as_bool().unwrap_or(false))
    }
}

/// A small, always-valid, distinguishable write to one operation's
/// program: a marker transaction that touches nothing, then a
/// terminal. These tests are about the MCP commit path, not about
/// program content.
fn program_patch(operation: &str, marker: u32) -> serde_json::Value {
    let short = operation.strip_prefix("operation.").unwrap_or(operation);

    serde_json::json!({
        "mutations": [{
            "kind": "replace_operation_program",
            "operation": operation,
            "program": {"steps": [
                {
                    "kind": "transaction",
                    "transaction": {
                        "id": format!("tx.{short}.probe{marker}"),
                        "data_model": null,
                        "isolation": "read_committed",
                        "idempotency": {"kind": "not_deduplicated"},
                        "steps": [],
                    },
                },
                {"kind": "complete"},
            ]},
        }],
    })
}

#[tokio::test]
async fn shared_skeleton_put_service_commits_over_mcp() {
    // The exact path the live decomposer agent takes: an empty
    // workspace, a shared-skeleton task, and one put_service mutation
    // submitted through MCP. Existing tests only exercised
    // operation-scoped writes, so this path was untested.
    let engine = DesignEngine::in_memory(WorkspaceState::empty(RunMetadata::new(RunId(
        "skeleton".to_string(),
    ))))
    .expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("bind addr"))
        .await
        .expect("the mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let handle = engine
        .create_task(CreateTask {
            kind: TaskKind::Decompose,
            objective: "create a service".to_string(),
            write_scope: WriteScope::shared_skeleton(),
            prompt_evidence: Vec::new(),
            budget: TaskBudget::default(),
        })
        .expect("task is created");

    let mut client = McpClient::connect(&url, &handle.token.0).await;

    // Raw response first, so a JSON-RPC error (what the live agent read
    // as a "transport issue") is visible rather than swallowed.
    let raw = client
        .rpc(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 100,
            "method": "tools/call",
            "params": {
                "name": "submit_patch",
                "arguments": {
                    "patch": {
                        "mutations": [
                            {"kind": "put_service", "id": "service.api",
                             "value": {"kind": "backend"}}
                        ]
                    }
                },
            },
        }))
        .await;

    assert!(
        raw.get("result").is_some(),
        "submit_patch returned a JSON-RPC error, not a tool result: {raw}"
    );

    // The single put_service committed and advanced the head.
    let result = &raw["result"];
    assert_eq!(result["isError"], false, "put_service was rejected: {raw}");

    let head = engine.head_snapshot();
    assert_eq!(head.revision.0, 1);
    assert!(
        head.workspace
            .services
            .contains_key(&Id("service.api".to_string()))
    );

    // A task commits exactly one patch. A second submission on the same
    // task reports completion clearly, not a contradictory "adjust and
    // retry" — the confusion the live agent hit.
    let (again, _) = client
        .call(
            "submit_patch",
            serde_json::json!({
                "patch": {
                    "mutations": [
                        {"kind": "put_service", "id": "service.other",
                         "value": {"kind": "worker"}}
                    ]
                }
            }),
        )
        .await;

    assert_eq!(again["already_committed"], true, "{again}");

    server.shutdown().await;
}

#[tokio::test]
async fn a_multi_project_server_isolates_projects_behind_one_api_key() {
    use std::sync::Arc;

    // One global server, a stable API key. Projects are isolated: the
    // active project switches, and each keeps its own model in its own
    // repository. This does not depend on any MCP session id — real
    // clients (Claude Code) run the transport statelessly — so the
    // client never sends one.
    let root = std::env::temp_dir().join(format!("conseqa-multi-{}", uuid::Uuid::new_v4()));
    let checkout = root.join("checkout");
    let billing = root.join("billing");

    std::fs::create_dir_all(&checkout).expect("checkout repository");
    std::fs::create_dir_all(&billing).expect("billing repository");

    let manager = Arc::new(conseqa::design::WorkspaceManager::new());
    let api_key = "test-api-key-xyz".to_string();

    let server = mcp::serve_router(
        mcp::router_multi(Arc::clone(&manager), api_key.clone(), None),
        "127.0.0.1:0".parse().expect("addr"),
    )
    .await
    .expect("server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let mut client = McpClient::connect(&url, &api_key).await;

    // Before any project is active, architecture tools guide rather than
    // fail at the protocol level.
    let (early, is_error) = client.call("spec_status", serde_json::json!({})).await;
    assert!(is_error);
    assert!(
        early["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no project is open"),
        "{early}"
    );

    // A directory holding no project is not one.
    let (missing, is_error) = client
        .call("open_project", serde_json::json!({"path": checkout}))
        .await;
    assert!(is_error, "{missing}");

    // Create "checkout" and commit a service.
    let (created, is_error) = client
        .call("create_project", serde_json::json!({"path": checkout}))
        .await;
    assert!(!is_error, "{created}");
    assert_eq!(created["created"], true);
    assert_eq!(created["project"], "checkout", "named after its directory");

    // What to design is a build's input, never project state.
    assert!(
        !std::fs::read_to_string(checkout.join(".conseqa/project.yaml"))
            .expect("project.yaml")
            .contains("prompt"),
    );

    let (committed, is_error) = client
        .call(
            "submit_patch",
            serde_json::json!({"patch": {"mutations": [
                {"kind": "put_service", "id": "service.checkout", "value": {"kind": "backend"}}
            ]}}),
        )
        .await;
    assert!(!is_error, "{committed}");
    assert_eq!(committed["committed"], true);

    assert!(
        checkout
            .join(".conseqa/spec/services/service.checkout.yaml")
            .is_file(),
        "the commit is in checkout's own files"
    );

    // Switch to a fresh "billing" project: it is empty — checkout's
    // service is not visible.
    let (_created, is_error) = client
        .call("create_project", serde_json::json!({"path": billing}))
        .await;
    assert!(!is_error);

    let (search, _) = client
        .call("search_symbols", serde_json::json!({"kind": "service"}))
        .await;
    assert_eq!(
        search["symbols"].as_array().map(|a| a.len()),
        Some(0),
        "billing is isolated from checkout: {search}"
    );

    // Switch back to checkout, by a directory inside it: its committed
    // service is still there, proving projects persist independently.
    let inside = checkout.join("src");
    std::fs::create_dir_all(&inside).expect("a directory inside checkout");

    let (opened, is_error) = client
        .call("open_project", serde_json::json!({"path": inside}))
        .await;
    assert!(!is_error, "{opened}");
    assert_eq!(opened["project"], "checkout");

    let (search, _) = client
        .call("search_symbols", serde_json::json!({"kind": "service"}))
        .await;
    let services: Vec<&str> = search["symbols"]
        .as_array()
        .expect("symbols")
        .iter()
        .filter_map(|s| s["value"].as_str())
        .collect();
    assert!(
        services.contains(&"service.checkout"),
        "checkout kept its service: {search}"
    );

    // A wrong API key is refused.
    let mut intruder = McpClient::connect(&url, "wrong-key").await;
    let refused = intruder
        .rpc(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 500,
            "method": "tools/call",
            "params": {"name": "open_project", "arguments": {"path": checkout}},
        }))
        .await;
    assert!(
        refused.get("error").is_some(),
        "a wrong API key is refused: {refused}"
    );

    server.shutdown().await;
    std::fs::remove_dir_all(&root).ok();
}

/// A stand-in builder: records each build's prompt and answers with a
/// fixed outcome, proving the tool reaches the injected builder without
/// `design` depending on the harness.
struct MockBuilder {
    prompts: std::sync::Arc<std::sync::Mutex<Vec<Option<String>>>>,
    outcome: mcp::BuildOutcome,
}

impl MockBuilder {
    fn answering(outcome: mcp::BuildOutcome) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            prompts: Default::default(),
            outcome,
        })
    }

    fn prompts(&self) -> Vec<Option<String>> {
        self.prompts.lock().expect("lock").clone()
    }
}

impl mcp::DesignBuilder for MockBuilder {
    fn build(
        &self,
        engine: DesignEngine,
        prompt: Option<String>,
    ) -> Result<futures::future::BoxFuture<'static, mcp::DesignBuild>, String> {
        self.prompts.lock().expect("lock").push(prompt);

        let build = mcp::DesignBuild {
            started_at_revision: engine.head_revision(),
            elapsed: std::time::Duration::from_millis(3),
            outcome: self.outcome.clone(),
        };

        Ok(Box::pin(async move { build }))
    }
}

/// Serves `engine` with `builder` and connects an interactive session.
async fn session_with_builder(
    engine: &DesignEngine,
    builder: std::sync::Arc<MockBuilder>,
) -> (mcp::McpServer, McpClient) {
    let router = mcp::router_with_builder(engine.clone(), builder);

    let server = mcp::serve_router(router, "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let handle = engine
        .create_session(WriteScope::shared_skeleton(), "ui")
        .expect("session");

    let client = McpClient::connect(&url, &handle.token.0).await;

    (server, client)
}

/// The tool reaches the injected builder with the caller's prompt, and
/// answers with the outcome, what was handed back, and the checker's
/// verdict — so no status call is needed after a build.
#[tokio::test]
async fn build_design_reaches_the_injected_builder_and_reports_its_outcome() {
    let engine = DesignEngine::in_memory(fixture_workspace()).expect("engine starts");

    let builder = MockBuilder::answering(mcp::BuildOutcome::HandedBack {
        hand_offs: vec![serde_json::json!({
            "kind": "requirement_discovery",
            "objective": "Discover the correctness requirements of operation.create_order.",
        })],
    });

    let (server, mut client) = session_with_builder(&engine, std::sync::Arc::clone(&builder)).await;

    let (built, is_error) = client
        .call(
            "build_design",
            serde_json::json!({"prompt": "A flash-sale checkout."}),
        )
        .await;

    assert!(!is_error, "{built}");
    assert_eq!(
        builder.prompts(),
        vec![Some("A flash-sale checkout.".to_string())]
    );
    assert_eq!(built["status"], "handed_back", "{built}");
    assert_eq!(
        built["hand_offs"][0]["kind"], "requirement_discovery",
        "{built}"
    );
    assert_eq!(built["elapsed_ms"], 3, "{built}");
    assert_eq!(built["analysis"]["state"], "validated", "{built}");
    assert!(
        built["guidance"]
            .as_str()
            .is_some_and(|guidance| guidance.contains("later phases have not run")),
        "{built}"
    );

    server.shutdown().await;
}

/// An empty project gives a build nothing to build, and a server
/// without a builder says so in the result rather than at the protocol
/// level.
#[tokio::test]
async fn build_design_refuses_an_empty_project_and_a_server_without_a_builder() {
    let empty = DesignEngine::in_memory(WorkspaceState::empty(RunMetadata::new(RunId(
        "design".to_string(),
    ))))
    .expect("engine starts");

    let builder = MockBuilder::answering(mcp::BuildOutcome::Success);

    let (server, mut client) = session_with_builder(&empty, std::sync::Arc::clone(&builder)).await;

    let (refused, is_error) = client
        .call(
            "build_design",
            serde_json::json!({"prompt": "A ping service."}),
        )
        .await;

    assert!(is_error, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .is_some_and(|error| error.contains("no operation interfaces")),
        "{refused}"
    );
    assert!(builder.prompts().is_empty());

    server.shutdown().await;

    let engine = DesignEngine::in_memory(fixture_workspace()).expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let handle = engine
        .create_session(WriteScope::shared_skeleton(), "ui")
        .expect("session");

    let mut client = McpClient::connect(&url, &handle.token.0).await;

    let (result, is_error) = client.call("build_design", serde_json::json!({})).await;

    assert!(is_error, "{result}");
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|error| error.contains("no design builder")),
        "{result}"
    );

    server.shutdown().await;
}

/// The surface follows the credential. On one server, an interactive
/// session is the coordinator — the authoring brief, the build and
/// status tools, no task tools — and a task's token is a worker, with
/// the task contract and only its own tools.
#[tokio::test]
async fn a_session_and_a_task_are_each_served_their_own_role() {
    let engine = DesignEngine::in_memory(fixture_workspace()).expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let session = engine
        .create_session(WriteScope::shared_skeleton(), "ui")
        .expect("session");

    let worker = task(&engine, "operation.create_order");

    for (token, coordinator) in [(&session.token.0, true), (&worker.token.0, false)] {
        let mut client = McpClient::connect(&url, token).await;

        let instructions = client.init["instructions"]
            .as_str()
            .expect("the server declares instructions")
            .to_string();

        assert_eq!(
            instructions.contains("The authoring loop"),
            coordinator,
            "{instructions}"
        );

        let listed = client
            .rpc(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 900,
                "method": "tools/list",
                "params": {},
            }))
            .await;

        let names: Vec<String> = listed["result"]["tools"]
            .as_array()
            .expect("tools are listed")
            .iter()
            .map(|tool| tool["name"].as_str().expect("named").to_string())
            .collect();

        for shared in [
            "read_symbol",
            "submit_patch",
            "requirement_report",
            "diff_spec",
            "dsl_guide",
        ] {
            assert!(names.iter().any(|name| name == shared), "{names:?}");
        }

        for (tool, served) in [
            ("build_design", coordinator),
            ("spec_status", coordinator),
            ("export_spec", coordinator),
            ("task_context", !coordinator),
            ("dependency_request", !coordinator),
            // A single-project server selects no project.
            ("open_project", false),
            ("request_design", false),
            ("await_design", false),
            ("dsl_reference", false),
        ] {
            assert_eq!(
                names.iter().any(|name| name == tool),
                served,
                "`{tool}` for coordinator={coordinator}: {names:?}"
            );
        }

        // A tool outside the caller's role cannot be called either.
        let foreign = if coordinator {
            "task_context"
        } else {
            "spec_status"
        };

        let refused = client
            .rpc(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 901,
                "method": "tools/call",
                "params": {"name": foreign, "arguments": {}},
            }))
            .await;

        assert!(refused.get("result").is_none(), "{refused}");
    }

    server.shutdown().await;
}

#[tokio::test]
async fn an_interactive_session_commits_repeatedly_under_one_bearer_token() {
    // The UI demo shape: one interactive session token, driven over MCP,
    // committing several patches in a row. The token rolls to a fresh
    // task after each commit, so the client's bearer value never changes.
    let engine = DesignEngine::in_memory(WorkspaceState::empty(RunMetadata::new(RunId(
        "interactive".to_string(),
    ))))
    .expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("bind addr"))
        .await
        .expect("the mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let handle = engine
        .create_session(WriteScope::shared_skeleton(), "ui demo")
        .expect("session is created");

    let mut client = McpClient::connect(&url, &handle.token.0).await;

    for index in 0..3 {
        let (committed, is_error) = client
            .call(
                "submit_patch",
                serde_json::json!({
                    "patch": {"mutations": [
                        {"kind": "put_service", "id": format!("service.s{index}"),
                         "value": {"kind": "backend"}}
                    ]}
                }),
            )
            .await;

        assert!(!is_error, "commit {index} rejected: {committed}");
        assert_eq!(committed["committed"], true, "{committed}");
        assert_eq!(committed["revision"], index + 1);

        // The same token now resolves to the rolled successor, pinned
        // to the new head.
        let (status, is_error) = client.call("spec_status", serde_json::json!({})).await;

        assert!(!is_error, "{status}");
        assert_eq!(status["revision"], index + 1);
        assert_eq!(
            engine
                .task_context(engine.resolve_token(&handle.token.0).expect("resolves"))
                .expect("context")
                .snapshot_revision
                .0,
            index + 1
        );
    }

    assert_eq!(engine.head_revision().0, 3);

    server.shutdown().await;
}

#[tokio::test]
async fn a_malformed_patch_returns_actionable_feedback_not_a_protocol_error() {
    // The live decomposer read a malformed submit_patch response as a
    // "transport issue" because the handler returned a JSON-RPC error.
    // A bad argument must come back as a readable tool result instead.
    let engine = DesignEngine::in_memory(WorkspaceState::empty(RunMetadata::new(RunId(
        "malformed".to_string(),
    ))))
    .expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("bind addr"))
        .await
        .expect("the mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let handle = engine
        .create_task(CreateTask {
            kind: TaskKind::Decompose,
            objective: "create a service".to_string(),
            write_scope: WriteScope::shared_skeleton(),
            prompt_evidence: Vec::new(),
            budget: TaskBudget::default(),
        })
        .expect("task is created");

    let mut client = McpClient::connect(&url, &handle.token.0).await;

    // Non-JSON garbage and an unknown mutation kind are argument errors
    // the agent must be able to read and correct — never protocol
    // errors.
    for bad in [
        serde_json::json!({"patch": "this is not json at all"}),
        serde_json::json!({"patch": {"mutations": [{"kind": "make_service"}]}}),
    ] {
        let raw = client
            .rpc(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 200,
                "method": "tools/call",
                "params": {"name": "submit_patch", "arguments": bad},
            }))
            .await;

        // A tool result (with isError), never a JSON-RPC protocol error.
        assert!(
            raw.get("result").is_some() && raw.get("error").is_none(),
            "a malformed patch produced a protocol error, not a tool result: {raw}"
        );

        let text = raw["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");

        assert!(
            text.contains("JSON shapes"),
            "the feedback points at the guide's shapes: {text}"
        );
    }

    // The exact failure the live agent hit: the patch passed as a
    // JSON-encoded string rather than an object. The handler coerces
    // it and commits, so the agent's stringified argument succeeds.
    let (committed, is_error) = client
        .call(
            "submit_patch",
            serde_json::json!({
                "patch": "{\"mutations\":[{\"kind\":\"put_service\",\
                          \"id\":\"service.api\",\"value\":{\"kind\":\"backend\"}}]}"
            }),
        )
        .await;

    assert!(
        !is_error,
        "a stringified patch was not accepted: {committed}"
    );
    assert_eq!(committed["committed"], true);

    assert!(
        engine
            .head_snapshot()
            .workspace
            .services
            .contains_key(&Id("service.api".to_string()))
    );

    server.shutdown().await;
}

#[tokio::test]
async fn two_mcp_clients_read_pinned_snapshots_and_commit_safely() {
    let engine = DesignEngine::in_memory(fixture_workspace()).expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("bind addr"))
        .await
        .expect("the mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let a = task(&engine, "operation.create_order");
    let b = task(&engine, "operation.transfer_stock");

    let mut client_a = McpClient::connect(&url, &a.token.0).await;
    let mut client_b = McpClient::connect(&url, &b.token.0).await;

    // Each client sees its own task context.
    let (context, is_error) = client_a.call("task_context", serde_json::json!({})).await;

    assert!(!is_error);
    assert_eq!(context["objective"], "synthesize operation.create_order");
    assert_eq!(context["snapshot_revision"], 1);

    // B reads a symbol A is about to leave untouched, and one it is
    // about to change.
    let (_, is_error) = client_b
        .call(
            "read_symbol",
            serde_json::json!({"symbol": {"kind": "schema", "value": "schema.StockRecord"}}),
        )
        .await;

    assert!(!is_error);

    let (_, is_error) = client_b
        .call(
            "read_operation",
            serde_json::json!({"operation": "operation.create_order", "mode": "interface"}),
        )
        .await;

    assert!(!is_error);

    // A commits a program change for its own operation — the interface
    // B read stays untouched, so B survives.
    let (committed, is_error) = client_a
        .call(
            "submit_patch",
            serde_json::json!({"patch": program_patch("operation.create_order", 2)}),
        )
        .await;

    assert!(!is_error, "{committed}");
    assert_eq!(committed["committed"], true);
    assert_eq!(committed["revision"], 2);

    // B still commits cleanly: nothing it observed changed.
    let (committed, is_error) = client_b
        .call(
            "submit_patch",
            serde_json::json!({"patch": program_patch("operation.transfer_stock", 3)}),
        )
        .await;

    assert!(!is_error, "{committed}");
    assert_eq!(committed["committed"], true);
    assert_eq!(committed["revision"], 3);

    server.shutdown().await;
}

#[tokio::test]
async fn a_stale_client_is_rejected_with_restart_guidance() {
    let engine = DesignEngine::in_memory(fixture_workspace()).expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("bind addr"))
        .await
        .expect("the mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let a = task(&engine, "operation.transfer_stock");
    let b = task(&engine, "operation.create_order");

    let mut client_a = McpClient::connect(&url, &a.token.0).await;
    let mut client_b = McpClient::connect(&url, &b.token.0).await;

    // A reads the program B is about to replace.
    let (_, is_error) = client_a
        .call(
            "read_symbol",
            serde_json::json!({"symbol": {
                "kind": "operation_program",
                "value": "operation.create_order",
            }}),
        )
        .await;

    assert!(!is_error);

    let (committed, is_error) = client_b
        .call(
            "submit_patch",
            serde_json::json!({"patch": program_patch("operation.create_order", 2)}),
        )
        .await;

    assert!(!is_error, "{committed}");

    // A's task is now invalidated; even reads say so.
    let (status, _) = client_a.call("task_status", serde_json::json!({})).await;

    assert_eq!(status["state"], "invalidated");

    let (rejection, is_error) = client_a
        .call(
            "submit_patch",
            serde_json::json!({"patch": program_patch("operation.transfer_stock", 4)}),
        )
        .await;

    assert!(is_error);
    assert_eq!(rejection["committed"], false);
    assert_eq!(rejection["stale_context"], true);

    assert!(
        rejection["guidance"]
            .as_str()
            .expect("guidance is text")
            .contains("Do not try to fix this in this session"),
        "{rejection}"
    );

    // A replacement task in a fresh session commits cleanly.
    let replacement = task(&engine, "operation.transfer_stock");
    let mut replacement_client = McpClient::connect(&url, &replacement.token.0).await;

    let (committed, is_error) = replacement_client
        .call(
            "submit_patch",
            serde_json::json!({"patch": program_patch("operation.transfer_stock", 4)}),
        )
        .await;

    assert!(!is_error, "{committed}");
    assert_eq!(committed["committed"], true);

    server.shutdown().await;
}

#[tokio::test]
async fn an_unknown_capability_is_refused() {
    let engine = DesignEngine::in_memory(fixture_workspace()).expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("bind addr"))
        .await
        .expect("the mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let mut intruder = McpClient::connect(&url, "not-a-real-token").await;

    let response = intruder
        .rpc(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": {"name": "task_context", "arguments": {}},
        }))
        .await;

    let error_text = serde_json::to_string(&response).expect("serializes");

    assert!(
        response.get("error").is_some() && error_text.contains("not recognized"),
        "unauthorized calls are refused: {response}"
    );

    server.shutdown().await;
}

/// The interactive authoring loop's feedback and delivery surface:
/// spec_status reports the checker's verdict on the head, export_spec
/// writes the canonical YAML, the verification report, and the
/// self-contained visualization, and dsl_guide serves the semantics —
/// so an agent never needs the crate's source to author or deliver.
#[tokio::test]
async fn status_export_and_guide_serve_the_authoring_loop() {
    let engine = DesignEngine::in_memory(fixture_workspace()).expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("bind addr"))
        .await
        .expect("the mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);
    let handle = session(&engine);
    let mut client = McpClient::connect(&url, &handle.token.0).await;

    // The status call reports the fixture's real standing: a complete,
    // validated model with declared obligations, some of them open.
    let (status, is_error) = client.call("spec_status", serde_json::json!({})).await;

    assert!(!is_error, "spec_status succeeds: {status}");
    assert_eq!(status["inventory"]["operations"], 6, "{status}");
    assert_eq!(
        status["inventory"]["operations_without_programs"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "{status}"
    );
    assert_eq!(status["analysis"]["state"], "validated", "{status}");

    let total = status["analysis"]["obligations"]["total"]
        .as_u64()
        .expect("total");
    let proven = status["analysis"]["obligations"]["proven"]
        .as_u64()
        .expect("proven");
    let open = status["analysis"]["obligations"]["open"]
        .as_array()
        .expect("open")
        .len() as u64;

    assert!(total > 0, "the fixture declares obligations: {status}");
    assert_eq!(proven + open, total, "{status}");

    // Export writes all three artifacts; the YAML round-trips through
    // the standalone parser and the visualization is self-contained.
    let dir = std::env::temp_dir().join(format!("conseqa-export-{}", uuid::Uuid::new_v4()));

    let (exported, is_error) = client
        .call(
            "export_spec",
            serde_json::json!({"dir": dir.display().to_string()}),
        )
        .await;

    assert!(!is_error, "export_spec succeeds: {exported}");
    assert_eq!(exported["exported"], true, "{exported}");
    assert_eq!(
        exported["artifacts"].as_array().map(Vec::len),
        Some(3),
        "{exported}"
    );

    let yaml = std::fs::read_to_string(dir.join("conseqa.yaml")).expect("yaml written");
    let model = conseqa::parser::yaml::parse(&yaml).expect("exported model parses");

    assert!(conseqa::analyzer::validate(&model).is_empty());

    let report =
        std::fs::read_to_string(dir.join("verification-report.json")).expect("report written");

    assert!(report.contains("obligations"), "{report}");

    let html = std::fs::read_to_string(dir.join("spec.html")).expect("visualization written");

    assert!(html.contains("window.CONSEQA"), "the page data is injected");
    assert!(
        html.contains("operation.create_order"),
        "the model is embedded"
    );

    // The guide answers a semantics question without any project state.
    let (guide, is_error) = client
        .call("dsl_guide", serde_json::json!({"topic": "effect intents"}))
        .await;

    assert!(!is_error);
    assert!(
        guide
            .as_str()
            .is_some_and(|text| text.contains("EstablishEffectIntent")),
        "{guide}"
    );

    // Several sections come back in one call, in the order asked.
    let (guides, is_error) = client
        .call(
            "dsl_guide",
            serde_json::json!({"topics": ["effect intents", "Locks"]}),
        )
        .await;

    assert!(!is_error);

    let text = guides.as_str().expect("text");
    let intents = text.find("EstablishEffectIntent").expect("first section");
    let locks = text.find("## 21. Locks").expect("second section");

    assert!(intents < locks, "{text}");

    std::fs::remove_dir_all(&dir).ok();
    server.shutdown().await;
}

/// Building over an incomplete skeleton would make one mistake in every
/// operation at once, so the server refuses rather than trusting the
/// agent to remember. The refusal is deterministic — the same judgment
/// the commit gate applies — and spec_status reports the same gaps so
/// the agent can check before it calls.
#[tokio::test]
async fn build_design_refuses_a_skeleton_that_is_not_ready() {
    // A schema an operation's request input names is gone: the
    // interface no longer resolves.
    let mut workspace = fixture_workspace();

    workspace
        .schemas
        .remove(&Id("schema.CreateOrderRequest".to_string()));

    let engine = DesignEngine::in_memory(workspace).expect("engine starts");

    let builder = MockBuilder::answering(mcp::BuildOutcome::Success);

    let (server, mut client) = session_with_builder(&engine, std::sync::Arc::clone(&builder)).await;

    let (refused, is_error) = client.call("build_design", serde_json::json!({})).await;

    assert!(is_error, "a premature build is refused: {refused}");
    assert!(builder.prompts().is_empty(), "nothing is built: {refused}");

    let gaps = refused["skeleton_gaps"]
        .as_array()
        .expect("the refusal names the gaps");

    assert!(
        gaps.iter().any(|gap| gap["message"]
            .as_str()
            .is_some_and(|message| message.contains("schema.CreateOrderRequest"))),
        "the missing schema is named: {refused}"
    );

    // The agent can see the same verdict before it ever calls.
    let (status, _) = client.call("spec_status", serde_json::json!({})).await;

    assert_eq!(status["skeleton"]["ready"], false, "{status}");

    server.shutdown().await;
}

/// With no decider configured, every builder still runs and settles
/// what code alone can: every sketch compiles, the default topology is
/// declared, and nothing that needs no judgment is handed back.
#[cfg(feature = "system-one")]
#[tokio::test]
async fn a_build_with_no_decider_settles_what_code_alone_can() {
    use clap::Parser;

    #[derive(Parser)]
    struct Flags {
        #[command(flatten)]
        decider: conseqa::harness::executors::cli::DeciderArgs,
    }

    let source = std::fs::read_to_string("tests/fixtures/shop.yaml").expect("fixture exists");
    let mut model = conseqa::parser::yaml::parse(&source).expect("fixture parses");

    model.runtime = None;

    let mut run_meta = RunMetadata::new(RunId("build-test".to_string()));

    // Implied requirements need a judgment, so adopt only explicit ones.
    run_meta.policy.strict_requirements = false;

    let mut workspace = WorkspaceState::from_model(&model, run_meta);

    for (operation, sketch) in common::sketches::shop_sketches() {
        let draft = workspace
            .operations
            .get_mut(&Id(operation.to_string()))
            .expect("the operation");

        draft.program = None;
        draft.sketch = Some(serde_json::from_value(sketch).expect("the sketch parses"));
    }

    let engine = DesignEngine::in_memory(workspace).expect("engine starts");

    let builders =
        conseqa::harness::executors::cli::every_builder(&Flags::parse_from(["test"]).decider)
            .expect("no decider is valid settings");

    let router = mcp::router_with_builder(
        engine.clone(),
        std::sync::Arc::new(conseqa::harness::InProcessBuilder::new(builders)),
    );

    let server = mcp::serve_router(router, "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let handle = engine
        .create_session(WriteScope::of([conseqa::design::WriteGrant::All]), "ui")
        .expect("session");

    let mut client = McpClient::connect(&url, &handle.token.0).await;

    let (built, is_error) = client
        .call(
            "build_design",
            serde_json::json!({"prompt": "A small shop backend."}),
        )
        .await;

    assert!(!is_error, "{built}");
    assert!(built.get("hand_offs").is_none(), "{built}");

    let head = engine.head_snapshot();

    assert!(
        head.workspace
            .operations
            .values()
            .all(|draft| draft.program.is_some()),
        "every sketch compiled: {built}"
    );
    assert!(
        !head.workspace.runtime.is_empty(),
        "the default topology is declared"
    );
    assert_eq!(built["revision"], head.revision.0, "{built}");

    // The session was moved to the build's head: a patch commits.
    let (committed, is_error) = client
        .call(
            "submit_patch",
            serde_json::json!({"patch": {"mutations": [
                {"kind": "put_service", "id": "service.after_build", "value": {"kind": "backend"}}
            ]}}),
        )
        .await;

    assert!(!is_error, "{committed}");

    server.shutdown().await;
}

/// Part V of the external-boundary revision: the server states the DSL
/// contract it speaks at the handshake and on every served surface —
/// and never refuses a connection over it; refusal lives at the patch
/// boundary, where a stale authored claim actually meets the server.
#[tokio::test]
async fn the_server_and_its_artifacts_declare_the_dsl_contract_version() {
    let engine = DesignEngine::in_memory(fixture_workspace()).expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("bind addr"))
        .await
        .expect("the mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);
    let handle = session(&engine);
    let mut client = McpClient::connect(&url, &handle.token.0).await;

    // The handshake: instructions lead with the contract version; the
    // Implementation stays the build version — the axes are distinct.
    let instructions = client.init["instructions"]
        .as_str()
        .expect("the server declares instructions");

    assert!(
        instructions.contains("DSL contract version 6"),
        "{instructions}"
    );

    assert_eq!(
        client.init["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION"),
        "{}",
        client.init
    );

    // Machine-readable before any project state: the guide, its
    // semantics and its shapes.
    let (toc, is_error) = client.call("dsl_guide", serde_json::json!({})).await;

    assert!(!is_error);
    assert!(
        toc.as_str()
            .is_some_and(|text| text.contains("DSL contract version 6")),
        "{toc}"
    );

    let (shapes, is_error) = client
        .call("dsl_guide", serde_json::json!({"topics": ["JSON shapes"]}))
        .await;

    assert!(!is_error);
    assert!(
        shapes
            .as_str()
            .is_some_and(|text| text.starts_with("## JSON shapes — symbol keys")
                && text.contains("put_operation_interface")),
        "{shapes}"
    );

    // Machine-readable with a project open: status and report.
    let (status, is_error) = client.call("spec_status", serde_json::json!({})).await;

    assert!(!is_error, "{status}");
    assert_eq!(status["dsl"], 6, "{status}");

    let (report, is_error) = client
        .call("requirement_report", serde_json::json!({}))
        .await;

    assert!(!is_error, "{report}");
    assert_eq!(report["dsl"], 6, "{report}");

    // Every documented family filters. One that names nothing is an
    // error naming the accepted values — never an empty report, which
    // would read as "nothing is open". The v3 names have no alias.
    for family in [
        "transaction_serializability",
        "transaction_ordering",
        "idempotency",
        "result_replay",
        "recoverability",
    ] {
        let (report, is_error) = client
            .call(
                "requirement_report",
                serde_json::json!({ "family": family }),
            )
            .await;

        assert!(!is_error, "{family}: {report}");
    }

    let (refused, is_error) = client
        .call(
            "requirement_report",
            serde_json::json!({ "family": "serialization" }),
        )
        .await;

    assert!(is_error, "{refused}");
    assert_eq!(refused["error"], "unknown_requirement_family", "{refused}");
    assert_eq!(refused["family"], "serialization", "{refused}");
    assert_eq!(
        refused["accepted"][0], "transaction_serializability",
        "{refused}"
    );

    // The export leads with the stamp, and the stamped document
    // round-trips through the standalone two-phase parser.
    let dir = std::env::temp_dir().join(format!("conseqa-dsl-export-{}", uuid::Uuid::new_v4()));

    let (exported, is_error) = client
        .call(
            "export_spec",
            serde_json::json!({"dir": dir.display().to_string()}),
        )
        .await;

    assert!(!is_error, "{exported}");

    let yaml = std::fs::read_to_string(dir.join("conseqa.yaml")).expect("yaml written");

    assert!(
        yaml.starts_with("dsl: 6\n"),
        "{}",
        &yaml[..40.min(yaml.len())]
    );
    assert!(conseqa::parser::yaml::parse(&yaml).is_ok());

    // The patch boundary refuses a stale authored claim by name,
    // before shape validation.
    let (refused, is_error) = client
        .call(
            "submit_patch",
            serde_json::json!({
                "dsl": 2,
                "patch": program_patch("operation.create_order", 7),
            }),
        )
        .await;

    assert!(is_error, "{refused}");
    assert_eq!(refused["error"], "dsl_version_mismatch", "{refused}");
    assert!(
        refused["guidance"]
            .as_str()
            .is_some_and(|text| text.contains("declares dsl 2")),
        "{refused}"
    );

    // dsl 5 meant something else by a transaction program: refused too.
    let (refused, is_error) = client
        .call(
            "submit_patch",
            serde_json::json!({
                "dsl": 5,
                "patch": program_patch("operation.create_order", 7),
            }),
        )
        .await;

    assert!(is_error, "{refused}");
    assert_eq!(refused["error"], "dsl_version_mismatch", "{refused}");

    // A matching declaration commits.
    let (committed, is_error) = client
        .call(
            "submit_patch",
            serde_json::json!({
                "dsl": 6,
                "patch": program_patch("operation.create_order", 7),
            }),
        )
        .await;

    assert!(!is_error, "{committed}");
    assert_eq!(committed["committed"], true, "{committed}");

    std::fs::remove_dir_all(&dir).ok();
    server.shutdown().await;
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Conseqa Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=master",
        ])
        .args(args)
        .output()
        .expect("git runs");

    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `diff_spec` (§16 of the spec change analysis revision): served to
/// every role, it compares the project's current files, as the engine
/// adopted them, with a commit.
#[tokio::test]
async fn diff_spec_compares_the_current_files_with_a_commit_for_every_role() {
    let dir = std::env::temp_dir().join(format!("conseqa-diff-spec-{}", uuid::Uuid::new_v4()));

    std::fs::create_dir_all(&dir).expect("the repository");
    git(&dir, &["init", "-q"]);

    drop(conseqa::design::ProjectStore::create(&dir, &fixture_workspace()).expect("the project"));

    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "the project"]);

    let engine = DesignEngine::open_project(&dir).expect("the project opens");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let coordinator = session(&engine);
    let worker = task(&engine, "operation.apply_payment");

    for token in [&coordinator.token.0, &worker.token.0] {
        let mut client = McpClient::connect(&url, token).await;

        let (texts, is_error) = client.call_texts("diff_spec", serde_json::json!({})).await;

        assert!(!is_error, "{texts:?}");
        assert_eq!(texts.len(), 2, "a summary, then the document");
        assert!(texts[0].contains("no semantic change"), "{}", texts[0]);

        let document: serde_json::Value = serde_json::from_str(&texts[1]).expect("the document");

        assert_eq!(document["format"], 1);
        assert_eq!(document["head"]["source"]["working_tree"], true);
        assert_eq!(document["obligations"]["status"], "available");
    }

    // An edit to the files is adopted before the call, and diffed.
    let file = dir.join(".conseqa/spec/operations/operation.apply_payment.yaml");
    let text = std::fs::read_to_string(&file).expect("the operation file");

    std::fs::write(
        &file,
        text.replacen("isolation: read_committed", "isolation: serializable", 1),
    )
    .expect("the edit");

    let mut client = McpClient::connect(&url, &coordinator.token.0).await;

    let (texts, is_error) = client
        .call_texts("diff_spec", serde_json::json!({ "merge_base": true }))
        .await;

    assert!(!is_error, "{texts:?}");
    assert!(texts[0].contains("1 modified"), "{}", texts[0]);

    let document: serde_json::Value = serde_json::from_str(&texts[1]).expect("the document");

    assert_eq!(document["base"]["source"]["merge_base"], true);
    assert_eq!(
        document["obligations"]["counts"]["rebased"], 2,
        "{document}"
    );

    // A base that names no commit is refused in band.
    let (refused, is_error) = client
        .call("diff_spec", serde_json::json!({ "base": "no-such-branch" }))
        .await;

    assert!(is_error, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap_or_default()
            .contains("names no commit"),
        "{refused}"
    );

    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
