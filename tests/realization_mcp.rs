//! The realization tools of the design MCP server (§15.2 of the
//! realization profiles revision): who is served each, where each
//! reads the spec, and the coordinator's choice.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use conseqa::{
    conformance::record::{Party, PartyKind},
    design::{
        CreateTask, DesignEngine, ProjectStore, RunId, RunMetadata, TaskBudget, TaskKind,
        WorkspaceState, WriteGrant, WriteScope,
        mcp::{self, CredentialRole, Credentials},
    },
    spec::Id,
};

const CHOICE: &str = ".conseqa/realization/choices/topic_runtime/topic.order_events.yaml";

/// A project of the flash checkout, with a stack offering two brokers
/// for its topic runtime, and nothing that bounds a pool.
fn project() -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("conseqa-realization-mcp-{}", uuid::Uuid::new_v4()));

    std::fs::create_dir_all(&dir).expect("the repository");

    let source =
        std::fs::read_to_string("tests/fixtures/flash_checkout.yaml").expect("fixture exists");
    let model = conseqa::parser::yaml::parse(&source).expect("fixture parses");
    let workspace =
        WorkspaceState::from_model(&model, RunMetadata::new(RunId("realization".to_string())));

    drop(ProjectStore::create(&dir, &workspace).expect("the project"));

    stack(
        &dir,
        &["postgres@16", "kafka@3.7", "envoy@1.30", "sqs@2012.11.05"],
    );

    dir
}

fn stack(dir: &Path, technologies: &[&str]) {
    let path = dir.join(".conseqa/realization/stack.yaml");

    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!(
            "format: 1\ncatalogue: conseqa-standard@1\ntechnologies: [{}]\n",
            technologies.join(", ")
        ),
    )
    .unwrap();
}

fn agent(name: &str, job: &str) -> Party {
    Party {
        kind: PartyKind::Agent,
        name: name.to_string(),
        job: Some(job.to_string()),
    }
}

/// A minimal Streamable HTTP MCP client speaking raw JSON-RPC, as the
/// design MCP tests use.
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

        client
            .rpc(json!({
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

        let response = client
            .post(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;

        assert!(response.status().is_success());

        client
    }

    async fn post(&mut self, body: Value) -> reqwest::Response {
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

    /// Sends one request and decodes the JSON-RPC response, whether it
    /// arrives as plain JSON or as an SSE stream.
    async fn rpc(&mut self, body: Value) -> Value {
        let response = self.post(body).await;

        let streamed = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));

        let text = response.text().await.expect("body reads");

        if streamed {
            return text
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
                .find(|message| message.get("id").is_some())
                .unwrap_or_else(|| panic!("no JSON-RPC response in the stream: {text}"));
        }

        serde_json::from_str(&text).unwrap_or_else(|error| panic!("not JSON ({error}): {text}"))
    }

    async fn tools(&mut self) -> Vec<String> {
        let listed = self
            .rpc(json!({"jsonrpc": "2.0", "id": 900, "method": "tools/list", "params": {}}))
            .await;

        listed["result"]["tools"]
            .as_array()
            .expect("tools are listed")
            .iter()
            .map(|tool| tool["name"].as_str().expect("named").to_string())
            .collect()
    }

    /// Calls a tool: every content block's text, and whether it is an
    /// error.
    async fn call(&mut self, name: &str, arguments: Value) -> (Vec<String>, bool) {
        let id = self.next_id;

        self.next_id += 1;

        let response = self
            .rpc(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }))
            .await;

        let result = &response["result"];

        let texts = result["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| block["text"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_else(|| panic!("a tool result: {response}"));

        (texts, result["isError"].as_bool().unwrap_or(false))
    }

    /// The document a tool returned last among its blocks.
    async fn document(&mut self, name: &str, arguments: Value) -> (Value, bool) {
        let (texts, is_error) = self.call(name, arguments).await;

        let document = serde_json::from_str(texts.last().expect("a block"))
            .unwrap_or_else(|error| panic!("{error}: {texts:?}"));

        (document, is_error)
    }
}

/// A single-project server, as `conseqa-design-mcp serve` runs one.
async fn serve(dir: &Path) -> (DesignEngine, Credentials, mcp::McpServer, String) {
    let engine = DesignEngine::open_project(dir).expect("the project opens");
    let credentials = Credentials::default();

    let server = mcp::serve_router(
        mcp::router_with_credentials(engine.clone(), None, credentials.clone()),
        "127.0.0.1:0".parse().expect("addr"),
    )
    .await
    .expect("mcp server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    (engine, credentials, server, url)
}

fn state(document: &Value, declaration: &str) -> String {
    document["declarations"]
        .as_array()
        .expect("declarations")
        .iter()
        .find(|realized| realized["declaration"] == declaration)
        .unwrap_or_else(|| panic!("`{declaration}` in {document}"))["state"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn every_role_reads_what_realizes_the_spec_and_only_the_coordinator_chooses() {
    let dir = project();
    let (engine, credentials, server, url) = serve(&dir).await;

    let implementer = credentials
        .issue(
            &engine,
            CredentialRole::Implementer,
            agent("claude", "job-1"),
        )
        .expect("an implementer token");

    let reviewer = credentials
        .issue(&engine, CredentialRole::Reviewer, agent("codex", "job-2"))
        .expect("a reviewer token");

    let coordinator = engine
        .create_session(WriteScope::of([WriteGrant::All]), "ui")
        .expect("a session")
        .token
        .0;

    let worker = engine
        .create_task(CreateTask {
            kind: TaskKind::OperationSynthesis,
            objective: "synthesize operation.cancel_order".to_string(),
            write_scope: WriteScope::operation_synthesis(Id("operation.cancel_order".to_string())),
            prompt_evidence: Vec::new(),
            budget: TaskBudget::default(),
        })
        .expect("a task")
        .token
        .0;

    for (token, chooses) in [
        (&implementer, false),
        (&reviewer, false),
        (&coordinator, true),
        (&worker, false),
    ] {
        let mut client = McpClient::connect(&url, token).await;
        let names = client.tools().await;

        for read in ["realization_status", "explain_realization"] {
            assert!(names.iter().any(|name| name == read), "{read}: {names:?}");
        }

        assert_eq!(
            names.iter().any(|name| name == "choose_realization"),
            chooses,
            "{names:?}"
        );

        let (texts, is_error) = client.call("realization_status", json!({})).await;

        assert!(!is_error, "{texts:?}");
        assert_eq!(texts.len(), 2, "the summary, then the document");
        assert!(texts[0].starts_with("realization against conseqa-standard@1"));

        let document: Value = serde_json::from_str(&texts[1]).unwrap();

        assert_eq!(document["document"], "realization_status");
        assert_eq!(
            state(&document, "topic_runtime topic.order_events"),
            "ambiguous"
        );
    }

    // A tool outside the role cannot be called either.
    let mut client = McpClient::connect(&url, &implementer).await;
    let response = client
        .rpc(json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "choose_realization",
                "arguments": {
                    "declaration": "topic_runtime topic.order_events",
                    "entry": "sqs.fifo",
                },
            },
        }))
        .await;

    assert!(response.get("result").is_none(), "{response}");
    assert!(!dir.join(CHOICE).exists());

    server.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_coordinator_chooses_and_the_reads_explain() {
    let dir = project();
    let (engine, _, server, url) = serve(&dir).await;

    let coordinator = engine
        .create_session(WriteScope::of([WriteGrant::All]), "ui")
        .expect("a session")
        .token
        .0;

    let mut client = McpClient::connect(&url, &coordinator).await;

    let (texts, is_error) = client
        .call(
            "explain_realization",
            json!({ "declaration": "topic_runtime topic.order_events" }),
        )
        .await;

    assert!(!is_error, "{texts:?}");
    assert!(
        texts[0].contains("candidate sqs.standard@1 (standard)"),
        "{}",
        texts[0]
    );

    let explanation: Value = serde_json::from_str(&texts[1]).unwrap();

    assert_eq!(explanation["document"], "realization_explain");
    assert_eq!(explanation["state"], "ambiguous");

    let (refused, is_error) = client
        .document(
            "explain_realization",
            json!({ "declaration": "data_object data.checkout/object.order" }),
        )
        .await;

    assert!(is_error);
    assert_eq!(refused["refusal"], "not_realizable");
    assert_eq!(refused["realized_by"], "data_model data.checkout");

    let (refused, is_error) = client
        .document(
            "choose_realization",
            json!({
                "declaration": "topic_runtime topic.order_events",
                "entry": "sqs.standard",
            }),
        )
        .await;

    assert!(is_error);
    assert_eq!(refused["document"], "realization_choice");
    assert_eq!(refused["outcome"], "refused");
    assert_eq!(refused["problems"][0]["problem"], "uncovered");
    assert!(!dir.join(CHOICE).exists());

    let choose = json!({
        "declaration": "topic_runtime topic.order_events",
        "entry": "kafka.keyed-topic",
        "reason": "The order stream stays on Kafka.",
    });

    let (written, is_error) = client.document("choose_realization", choose.clone()).await;

    assert!(!is_error, "{written}");
    assert_eq!(written["outcome"], "written");
    assert_eq!(written["entry"], "kafka.keyed-topic@1");
    assert!(dir.join(CHOICE).exists());

    let (again, _) = client.document("choose_realization", choose).await;

    assert_eq!(again["outcome"], "unchanged");

    let (status, _) = client.document("realization_status", json!({})).await;

    assert_eq!(state(&status, "topic_runtime topic.order_events"), "chosen");

    // Without a stack nothing is realized, and nothing explained.
    std::fs::remove_dir_all(dir.join(".conseqa/realization")).unwrap();

    let (refused, is_error) = client
        .document(
            "explain_realization",
            json!({ "declaration": "data_model data.checkout" }),
        )
        .await;

    assert!(is_error);
    assert_eq!(refused["refusal"], "no_stack");

    let (status, is_error) = client.document("realization_status", json!({})).await;

    assert!(!is_error);
    assert_eq!(status["profile"], Value::Null);

    server.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_worker_reads_the_spec_its_task_pinned() {
    let dir = project();
    let (engine, _, server, url) = serve(&dir).await;

    let coordinator = engine
        .create_session(WriteScope::of([WriteGrant::All]), "ui")
        .expect("a session")
        .token
        .0;

    let worker = engine
        .create_task(CreateTask {
            kind: TaskKind::OperationSynthesis,
            objective: "synthesize operation.cancel_order".to_string(),
            write_scope: WriteScope::operation_synthesis(Id("operation.cancel_order".to_string())),
            prompt_evidence: Vec::new(),
            budget: TaskBudget::default(),
        })
        .expect("a task")
        .token
        .0;

    // An edit outside the server bounds the checkout API's pool, which
    // nothing in the stack can provide.
    let pool = dir.join(".conseqa/spec/runtime/execution_pools/pool.checkout_api.yaml");
    let text = std::fs::read_to_string(&pool).expect("the pool's file");

    assert!(text.contains("unbounded"), "{text}");

    std::fs::write(
        &pool,
        text.replace("kind: unbounded", "kind: bounded\n  value: 4"),
    )
    .unwrap();

    let mut client = McpClient::connect(&url, &coordinator).await;
    let (status, _) = client.document("realization_status", json!({})).await;

    assert_eq!(
        state(&status, "execution_pool pool.checkout_api"),
        "infeasible",
        "the head adopts the edit"
    );

    let mut client = McpClient::connect(&url, &worker).await;
    let (texts, is_error) = client.call("realization_status", json!({})).await;

    assert!(!is_error, "{texts:?}");

    let status: Value = serde_json::from_str(&texts[1]).unwrap();

    assert_eq!(
        state(&status, "execution_pool pool.checkout_api"),
        "unneeded",
        "the worker reads the head its task pinned"
    );

    server.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}
