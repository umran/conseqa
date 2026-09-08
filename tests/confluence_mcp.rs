//! MCP surface acceptance (§107 phase 3): two independent MCP clients
//! read pinned snapshots and commit safely over Streamable HTTP, with
//! per-task bearer capabilities; a stale client's submission is
//! rejected with restart guidance.

use conseqa::confluence::{
    ConfluenceEngine, CreateTask, RunId, RunMetadata, TaskBudget, TaskHandle, TaskKind,
    WorkspaceState, WriteScope, mcp,
};
use conseqa::spec::Id;

fn fixture_workspace() -> WorkspaceState {
    let source =
        std::fs::read_to_string("tests/fixtures/flash_checkout.yaml").expect("fixture exists");

    let model = conseqa::parser::yaml::parse(&source).expect("fixture parses");

    WorkspaceState::from_model(&model, RunMetadata::new(RunId("mcp-test".to_string())))
}

fn task(engine: &ConfluenceEngine, operation: &str) -> TaskHandle {
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

/// A minimal Streamable HTTP MCP client speaking raw JSON-RPC, so the
/// test exercises the wire protocol rather than a shared SDK.
struct McpClient {
    http: reqwest::Client,
    url: String,
    token: String,
    session: Option<String>,
    next_id: u64,
}

impl McpClient {
    async fn connect(url: &str, token: &str) -> Self {
        let mut client = Self {
            http: reqwest::Client::new(),
            url: url.to_string(),
            token: token.to_string(),
            session: None,
            next_id: 1,
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

        serde_json::from_str(&text).unwrap_or_else(|error| {
            panic!("response is not JSON ({error}): {content_type} {text}")
        })
    }

    /// Calls one tool and returns (parsed JSON payload, is_error).
    async fn call(&mut self, name: &str, arguments: serde_json::Value) -> (serde_json::Value, bool) {
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
}

fn execution_patch(operation: &str, bound: u32) -> serde_json::Value {
    serde_json::json!({
        "mutations": [{
            "kind": "replace_operation_execution",
            "operation": operation,
            "execution": {"concurrency": {"kind": "bounded", "value": bound}},
        }],
    })
}

#[tokio::test]
async fn shared_skeleton_put_service_commits_over_mcp() {
    // The exact path the live decomposer agent takes: an empty
    // workspace, a shared-skeleton task, and one put_service mutation
    // submitted through MCP. Existing tests only exercised
    // replace_operation_execution, so this path was untested.
    let engine = ConfluenceEngine::in_memory(WorkspaceState::empty(RunMetadata::new(RunId(
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
    // active project switches, and each keeps its own model. This does
    // not depend on any MCP session id — real clients (Claude Code) run
    // the transport statelessly — so the client never sends one.
    let dir = std::env::temp_dir().join(format!("conseqa-multi-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("data dir");

    let manager = Arc::new(conseqa::confluence::WorkspaceManager::new(&dir));
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
    let (early, is_error) = client.call("task_context", serde_json::json!({})).await;
    assert!(is_error);
    assert!(
        early["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no project is open"),
        "{early}"
    );

    // Create "checkout", grounded by its prompt, and commit a service.
    let (created, is_error) = client
        .call(
            "create_project",
            serde_json::json!({"project": "checkout", "prompt": "A checkout system."}),
        )
        .await;
    assert!(!is_error, "{created}");
    assert_eq!(created["created"], true);

    let (context, _) = client.call("task_context", serde_json::json!({})).await;
    assert_eq!(context["prompt_evidence"][0]["excerpt"], "A checkout system.");

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

    // Switch to a fresh "billing" project: it is empty — checkout's
    // service is not visible.
    let (_opened, is_error) = client
        .call("open_project", serde_json::json!({"project": "billing"}))
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

    // Switch back to checkout: its committed service is still there,
    // proving projects persist independently.
    client
        .call("open_project", serde_json::json!({"project": "checkout"}))
        .await;

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

    // list_projects sees both.
    let (list, _) = client.call("list_projects", serde_json::json!({})).await;
    let names: Vec<&str> = list["projects"]
        .as_array()
        .expect("projects")
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert!(names.contains(&"checkout"), "{list}");
    assert!(names.contains(&"billing"), "{list}");

    // A wrong API key is refused.
    let mut intruder = McpClient::connect(&url, "wrong-key").await;
    let refused = intruder
        .rpc(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 500,
            "method": "tools/call",
            "params": {"name": "list_projects", "arguments": {}},
        }))
        .await;
    assert!(
        refused.get("error").is_some(),
        "a wrong API key is refused: {refused}"
    );

    server.shutdown().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn request_design_invokes_the_injected_launcher() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // A stand-in launcher records how many times the tool triggered it,
    // proving the MCP tool reaches the injected orchestrator without
    // confluence depending on the harness.
    struct MockLauncher {
        calls: Arc<AtomicUsize>,
    }

    impl mcp::DesignLauncher for MockLauncher {
        fn launch(
            &self,
            _engine: ConfluenceEngine,
            _objective: Option<String>,
        ) -> Result<serde_json::Value, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);

            Ok(serde_json::json!({ "launched": true, "backend": "mock" }))
        }
    }

    let engine = ConfluenceEngine::in_memory(WorkspaceState::empty(RunMetadata::new(RunId(
        "design".to_string(),
    ))))
    .expect("engine starts");

    let calls = Arc::new(AtomicUsize::new(0));
    let launcher = Arc::new(MockLauncher {
        calls: Arc::clone(&calls),
    });

    let router = mcp::router_with_launcher(engine.clone(), launcher);

    let server = mcp::serve_router(router, "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let handle = engine
        .create_session(WriteScope::shared_skeleton(), "ui")
        .expect("session");

    let mut client = McpClient::connect(&url, &handle.token.0).await;

    let (result, is_error) = client.call("request_design", serde_json::json!({})).await;

    assert!(!is_error, "{result}");
    assert_eq!(result["launched"], true);
    assert_eq!(result["backend"], "mock");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    server.shutdown().await;
}

#[tokio::test]
async fn request_design_reports_when_no_launcher_is_configured() {
    // A plain confluence server (no orchestration backend) reports the
    // feature is unavailable rather than erroring at the protocol level.
    let engine = ConfluenceEngine::in_memory(WorkspaceState::empty(RunMetadata::new(RunId(
        "no-design".to_string(),
    ))))
    .expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let handle = engine
        .create_session(WriteScope::shared_skeleton(), "ui")
        .expect("session");

    let mut client = McpClient::connect(&url, &handle.token.0).await;

    let (result, is_error) = client.call("request_design", serde_json::json!({})).await;

    assert!(is_error);
    assert_eq!(result["launched"], false);
    assert!(
        result["error"]
            .as_str()
            .unwrap_or_default()
            .contains("not available"),
        "{result}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn an_interactive_session_commits_repeatedly_under_one_bearer_token() {
    // The UI demo shape: one interactive session token, driven over MCP,
    // committing several patches in a row. The token rolls to a fresh
    // task after each commit, so the client's bearer value never changes.
    let engine = ConfluenceEngine::in_memory(WorkspaceState::empty(RunMetadata::new(RunId(
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

        // task_context still works under the same token, now pointing at
        // the rolled successor pinned to the new head.
        let (context, is_error) = client.call("task_context", serde_json::json!({})).await;

        assert!(!is_error);
        assert_eq!(context["state"], "running");
        assert_eq!(context["snapshot_revision"], index + 1);
    }

    assert_eq!(engine.head_revision().0, 3);

    server.shutdown().await;
}

#[tokio::test]
async fn a_malformed_patch_returns_actionable_feedback_not_a_protocol_error() {
    // The live decomposer read a malformed submit_patch response as a
    // "transport issue" because the handler returned a JSON-RPC error.
    // A bad argument must come back as a readable tool result instead.
    let engine = ConfluenceEngine::in_memory(WorkspaceState::empty(RunMetadata::new(RunId(
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
            text.contains("dsl_reference"),
            "the feedback points at dsl_reference: {text}"
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

    assert!(!is_error, "a stringified patch was not accepted: {committed}");
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
    let engine = ConfluenceEngine::in_memory(fixture_workspace()).expect("engine starts");

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

    // A commits an execution change for its own operation — the
    // interface B read stays untouched, so B survives.
    let (committed, is_error) = client_a
        .call(
            "submit_patch",
            serde_json::json!({"patch": execution_patch("operation.create_order", 2)}),
        )
        .await;

    assert!(!is_error, "{committed}");
    assert_eq!(committed["committed"], true);
    assert_eq!(committed["revision"], 2);

    // B still commits cleanly: nothing it observed changed.
    let (committed, is_error) = client_b
        .call(
            "submit_patch",
            serde_json::json!({"patch": execution_patch("operation.transfer_stock", 3)}),
        )
        .await;

    assert!(!is_error, "{committed}");
    assert_eq!(committed["committed"], true);
    assert_eq!(committed["revision"], 3);

    server.shutdown().await;
}

#[tokio::test]
async fn a_stale_client_is_rejected_with_restart_guidance() {
    let engine = ConfluenceEngine::in_memory(fixture_workspace()).expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("bind addr"))
        .await
        .expect("the mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let a = task(&engine, "operation.transfer_stock");
    let b = task(&engine, "operation.create_order");

    let mut client_a = McpClient::connect(&url, &a.token.0).await;
    let mut client_b = McpClient::connect(&url, &b.token.0).await;

    // A reads the execution facts B is about to replace.
    let (_, is_error) = client_a
        .call(
            "read_symbol",
            serde_json::json!({"symbol": {
                "kind": "operation_execution",
                "value": "operation.create_order",
            }}),
        )
        .await;

    assert!(!is_error);

    let (committed, is_error) = client_b
        .call(
            "submit_patch",
            serde_json::json!({"patch": execution_patch("operation.create_order", 2)}),
        )
        .await;

    assert!(!is_error, "{committed}");

    // A's task is now invalidated; even reads say so.
    let (status, _) = client_a.call("task_status", serde_json::json!({})).await;

    assert_eq!(status["state"], "invalidated");

    let (rejection, is_error) = client_a
        .call(
            "submit_patch",
            serde_json::json!({"patch": execution_patch("operation.transfer_stock", 4)}),
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
            serde_json::json!({"patch": execution_patch("operation.transfer_stock", 4)}),
        )
        .await;

    assert!(!is_error, "{committed}");
    assert_eq!(committed["committed"], true);

    server.shutdown().await;
}

#[tokio::test]
async fn an_unknown_capability_is_refused() {
    let engine = ConfluenceEngine::in_memory(fixture_workspace()).expect("engine starts");

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
    let engine = ConfluenceEngine::in_memory(fixture_workspace()).expect("engine starts");

    let server = mcp::serve(engine.clone(), "127.0.0.1:0".parse().expect("bind addr"))
        .await
        .expect("the mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);
    let handle = task(&engine, "operation.create_order");
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

    let report = std::fs::read_to_string(dir.join("verification-report.json"))
        .expect("report written");

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

    std::fs::remove_dir_all(&dir).ok();
    server.shutdown().await;
}

/// Fanning out over an incomplete skeleton makes one mistake
/// simultaneously in every worker, so the server refuses rather than
/// trusting the agent to remember. The refusal is deterministic — the
/// same judgment the commit gate applies — and spec_status reports the
/// same gaps so the agent can check before it calls.
#[tokio::test]
async fn request_design_refuses_a_skeleton_that_is_not_ready() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingLauncher {
        calls: Arc<AtomicUsize>,
    }

    impl mcp::DesignLauncher for CountingLauncher {
        fn launch(
            &self,
            _engine: ConfluenceEngine,
            _objective: Option<String>,
        ) -> Result<serde_json::Value, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);

            Ok(serde_json::json!({"launched": true}))
        }
    }

    // A schema an operation's request input names is gone: the
    // interface no longer resolves, though every operation still has a
    // program the workers would otherwise be asked to repair.
    let mut workspace = fixture_workspace();

    workspace
        .schemas
        .remove(&Id("schema.CreateOrderRequest".to_string()));

    let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");

    let calls = Arc::new(AtomicUsize::new(0));
    let launcher = Arc::new(CountingLauncher {
        calls: Arc::clone(&calls),
    });

    let router = mcp::router_with_launcher(engine.clone(), launcher);

    let server = mcp::serve_router(router, "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let handle = engine
        .create_session(WriteScope::shared_skeleton(), "ui")
        .expect("session");

    let mut client = McpClient::connect(&url, &handle.token.0).await;

    let (refused, is_error) = client.call("request_design", serde_json::json!({})).await;

    assert!(is_error, "a premature fanout is refused: {refused}");
    assert_eq!(refused["launched"], false, "{refused}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no worker is dispatched: {refused}"
    );

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

    assert_eq!(status["skeleton"]["ready_to_fan_out"], false, "{status}");

    server.shutdown().await;
}

/// The complement: a coherent skeleton launches, and the result names
/// the operations receiving a worker so a short list — the signature of
/// handing the skeleton over half-built — is visible immediately.
#[tokio::test]
async fn a_ready_skeleton_launches_and_names_its_operations() {
    use std::sync::Arc;

    struct OkLauncher;

    impl mcp::DesignLauncher for OkLauncher {
        fn launch(
            &self,
            _engine: ConfluenceEngine,
            _objective: Option<String>,
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({"launched": true}))
        }
    }

    // The intact fixture, with one operation's program dropped: the
    // skeleton resolves, and exactly that operation needs a worker.
    let mut workspace = fixture_workspace();

    workspace
        .operations
        .get_mut(&Id("operation.create_order".to_string()))
        .expect("the fixture declares create_order")
        .program = None;

    let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");

    let router = mcp::router_with_launcher(engine.clone(), Arc::new(OkLauncher));

    let server = mcp::serve_router(router, "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let handle = engine
        .create_session(WriteScope::shared_skeleton(), "ui")
        .expect("session");

    let mut client = McpClient::connect(&url, &handle.token.0).await;

    let (status, _) = client.call("spec_status", serde_json::json!({})).await;

    assert_eq!(status["skeleton"]["ready_to_fan_out"], true, "{status}");

    let (launched, is_error) = client.call("request_design", serde_json::json!({})).await;

    assert!(!is_error, "{launched}");
    assert_eq!(
        launched["operations"],
        serde_json::json!(["operation.create_order"]),
        "the run names the operation it parallelizes: {launched}"
    );

    server.shutdown().await;
}

/// The fanout hand-off an interactive agent depends on: its objective
/// reaches the orchestrator, and while workers are committing,
/// spec_status reports the run rather than a verdict read off a head
/// that is still moving — so the agent polls instead of synthesizing
/// operations serially or patching into a live run.
#[tokio::test]
async fn spec_status_reports_a_running_design_and_passes_the_objective() {
    use std::sync::Arc;
    use std::sync::Mutex;

    struct MockLauncher {
        objectives: Arc<Mutex<Vec<Option<String>>>>,
    }

    impl mcp::DesignLauncher for MockLauncher {
        fn launch(
            &self,
            _engine: ConfluenceEngine,
            objective: Option<String>,
        ) -> Result<serde_json::Value, String> {
            self.objectives.lock().expect("lock").push(objective);

            Ok(serde_json::json!({"launched": true}))
        }

        fn status(&self) -> Option<serde_json::Value> {
            // A run in flight, as the daemon launcher reports one.
            Some(serde_json::json!({"running": true, "started_at_revision": 0}))
        }
    }

    let engine = ConfluenceEngine::in_memory(fixture_workspace()).expect("engine starts");

    let objectives = Arc::new(Mutex::new(Vec::new()));
    let launcher = Arc::new(MockLauncher {
        objectives: Arc::clone(&objectives),
    });

    let router = mcp::router_with_launcher(engine.clone(), launcher);

    let server = mcp::serve_router(router, "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let handle = engine
        .create_session(WriteScope::shared_skeleton(), "ui")
        .expect("session");

    let mut client = McpClient::connect(&url, &handle.token.0).await;

    let (launched, is_error) = client
        .call(
            "request_design",
            serde_json::json!({"objective": "prioritize the checkout path"}),
        )
        .await;

    assert!(!is_error, "{launched}");
    assert_eq!(
        objectives.lock().expect("lock").as_slice(),
        [Some("prioritize the checkout path".to_string())],
        "the caller's objective reaches the orchestrator"
    );

    let (status, is_error) = client.call("spec_status", serde_json::json!({})).await;

    assert!(!is_error, "{status}");
    assert_eq!(status["design"]["running"], true, "{status}");
    assert_eq!(
        status["analysis"]["state"], "design_running",
        "a moving head reports the run, not a verdict: {status}"
    );

    server.shutdown().await;
}
