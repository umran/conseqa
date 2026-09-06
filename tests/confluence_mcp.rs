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
