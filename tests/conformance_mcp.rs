//! The conformance surfaces of the design MCP server, and the review
//! workflow (§14.3, §14.5, and §15.1 of the conformance and
//! implementation guidance revision; T4 and T5 of the delivery loop
//! orchestrator design).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use conseqa::{
    analyzer::basis::TextualKey,
    conformance::{
        document::ReviewResult,
        record::{Party, PartyKind, RecordContent, Records, Verdict},
    },
    design::{
        CreateTask, DesignEngine, ProjectStore, RunId, RunMetadata, TaskBudget, TaskKind,
        WorkspaceState, WriteScope,
        mcp::{self, CredentialRole, Credentials},
    },
    harness::{
        InvocationBudget,
        backends::ClaudeCliBackend,
        review::{ReviewBackend, ReviewRun, Reviewer},
    },
    spec::Id,
};

fn fixture_workspace() -> WorkspaceState {
    let source =
        std::fs::read_to_string("tests/fixtures/flash_checkout.yaml").expect("fixture exists");

    let model = conseqa::parser::yaml::parse(&source).expect("fixture parses");

    WorkspaceState::from_model(&model, RunMetadata::new(RunId("conformance".to_string())))
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");

    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const CANCEL: &str = "transaction operation.cancel_order/tx.cancel_order";

/// A checkout of the flash checkout project, run by Ana, with the
/// cancellation's code bound.
fn checkout() -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("conseqa-conformance-mcp-{}", uuid::Uuid::new_v4()));

    std::fs::create_dir_all(dir.join("src")).expect("the repository");

    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.name", "Ana Tester"]);

    drop(ProjectStore::create(&dir, &fixture_workspace()).expect("the project"));

    std::fs::write(
        dir.join("src/cancel.rs"),
        "// conseqa:begin transaction operation.cancel_order/tx.cancel_order\n\
         pub fn cancel(db: &mut Db, order_id: Uuid) -> Result<()> {\n\
         \x20   let order = db.one(\"SELECT * FROM orders WHERE order_id = $1\", order_id)?;\n\
         \x20   db.execute(\"UPDATE orders SET status = 'cancelled', version = version + 1\n\
         \x20               WHERE order_id = $1 AND version = $2\", (order_id, order.version))\n\
         }\n\
         // conseqa:end transaction operation.cancel_order/tx.cancel_order\n",
    )
    .expect("the code");

    dir
}

fn agent(name: &str, job: &str) -> Party {
    Party {
        kind: PartyKind::Agent,
        name: name.to_string(),
        job: Some(job.to_string()),
    }
}

fn person(name: &str) -> Party {
    Party {
        kind: PartyKind::Person,
        name: name.to_string(),
        job: None,
    }
}

fn attested_by(dir: &Path) -> Party {
    match Records::of_project(dir)
        .read(&CANCEL.parse::<TextualKey>().unwrap())
        .unwrap()
        .expect("the cancellation has a record")
        .content
    {
        RecordContent::Read(record) => record.attested.by,
        other => panic!("the record is set aside: {other:?}"),
    }
}

fn reviews(dir: &Path) -> Vec<(Party, Verdict)> {
    match Records::of_project(dir)
        .read(&CANCEL.parse::<TextualKey>().unwrap())
        .unwrap()
        .expect("the cancellation has a record")
        .content
    {
        RecordContent::Read(record) => record
            .reviews
            .into_iter()
            .map(|review| (review.by, review.verdict))
            .collect(),
        other => panic!("the record is set aside: {other:?}"),
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
    init: Value,
}

impl McpClient {
    async fn connect(url: &str, token: &str) -> Self {
        let mut client = Self {
            http: reqwest::Client::new(),
            url: url.to_string(),
            token: token.to_string(),
            session: None,
            next_id: 1,
            init: Value::Null,
        };

        let response = client
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

        client.init = response["result"].clone();

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

    /// Calls a tool: the raw response, then every content block's text
    /// and whether it is an error.
    async fn call(&mut self, name: &str, arguments: Value) -> (Value, Vec<String>, bool) {
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
            .unwrap_or_default();

        let is_error = result["isError"].as_bool().unwrap_or(false);

        (response, texts, is_error)
    }
}

/// A single-project server serving the role tokens `credentials` holds,
/// as `conseqa-design-mcp serve` does.
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

/// The facts the cancellation must attest, and its packet's read set,
/// from its work item.
async fn work_item(client: &mut McpClient) -> (Vec<Value>, Value) {
    let (_, texts, is_error) = client.call("work_item", json!({ "key": CANCEL })).await;

    assert!(!is_error, "{texts:?}");
    assert_eq!(texts.len(), 2, "the packet in markdown, then as JSON");
    assert!(texts[0].starts_with(&format!("# {CANCEL}")), "{}", texts[0]);

    let packet: Value = serde_json::from_str(&texts[1]).expect("the item document");

    assert_eq!(packet["document"], json!("item"));

    let facts = packet["required"]
        .as_array()
        .expect("required facts")
        .iter()
        .filter(|required| required["class"] == json!("attestable"))
        .map(|required| {
            json!({
                "fact": required["fact"],
                "evidence": [{ "path": "src/cancel.rs", "lines": [2, 6] }],
            })
        })
        .collect();

    (facts, packet["read_set"].clone())
}

#[tokio::test]
async fn implementers_reviewers_and_the_coordinator_are_each_served_their_tools() {
    let dir = checkout();
    let (engine, credentials, server, url) = serve(&dir).await;

    let implementer = credentials
        .issue(
            &engine,
            CredentialRole::Implementer,
            agent("claude/claude-opus-5-5", "job-1"),
        )
        .expect("an implementer token");

    let reviewer = credentials
        .issue(&engine, CredentialRole::Reviewer, agent("codex", "job-2"))
        .expect("a reviewer token");

    let coordinator = engine
        .create_session(WriteScope::of([conseqa::design::WriteGrant::All]), "ui")
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

    // (token, attest, record_review, submit_patch, build_design, brief)
    let roles = [
        (
            &implementer,
            true,
            false,
            false,
            false,
            "attest with the facts",
        ),
        (
            &reviewer,
            false,
            true,
            false,
            false,
            "call\nrecord_review once",
        ),
        (&coordinator, true, true, true, true, "The authoring loop"),
        (&worker, false, false, true, false, "submit_patch"),
    ];

    for (token, attest, record_review, submit_patch, build_design, brief) in roles {
        let mut client = McpClient::connect(&url, token).await;

        let instructions = client.init["instructions"]
            .as_str()
            .expect("a brief")
            .replace(" \\\n", " ");

        assert!(
            instructions
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains(&brief.split_whitespace().collect::<Vec<_>>().join(" ")),
            "{instructions}"
        );

        let names = client.tools().await;

        // Every role reads conformance.
        for read in [
            "conformance_status",
            "implementation_plan",
            "work_item",
            "dsl_guide",
            "read_symbol",
        ] {
            assert!(names.iter().any(|name| name == read), "{read}: {names:?}");
        }

        for (tool, served) in [
            ("attest", attest),
            ("record_review", record_review),
            ("submit_patch", submit_patch),
            ("build_design", build_design),
        ] {
            assert_eq!(
                names.iter().any(|name| name == tool),
                served,
                "`{tool}` for {brief:?}: {names:?}"
            );
        }
    }

    // A tool outside the role cannot be called either.
    let mut client = McpClient::connect(&url, &reviewer).await;
    let (response, _, _) = client
        .call(
            "attest",
            json!({ "declaration": CANCEL, "structural": false }),
        )
        .await;

    assert!(response.get("result").is_none(), "{response}");

    server.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn attests_and_reviews_are_stamped_by_the_credential_never_the_caller() {
    let dir = checkout();
    let (engine, credentials, server, url) = serve(&dir).await;

    let implementer_party = agent("claude/claude-opus-5-5", "job-1");
    let reviewer_party = agent("codex", "job-2");

    let implementer = credentials
        .issue(
            &engine,
            CredentialRole::Implementer,
            implementer_party.clone(),
        )
        .expect("an implementer token");

    let reviewer = credentials
        .issue(&engine, CredentialRole::Reviewer, reviewer_party.clone())
        .expect("a reviewer token");

    // The implementer reads its work item, and attests with its read
    // set. An attest naming who attests is refused before it runs.
    let mut client = McpClient::connect(&url, &implementer).await;
    let (facts, read_set) = work_item(&mut client).await;

    assert!(!facts.is_empty());

    let (_, texts, is_error) = client
        .call(
            "attest",
            json!({
                "declaration": CANCEL,
                "facts": facts,
                "structural": false,
                "by": { "kind": "person", "name": "someone else" },
            }),
        )
        .await;

    assert!(is_error);
    assert!(texts[0].contains("unknown field `by`"), "{texts:?}");

    let (_, texts, is_error) = client
        .call(
            "attest",
            json!({
                "declaration": CANCEL,
                "facts": facts,
                "structural": false,
                "read_set": read_set,
            }),
        )
        .await;

    assert!(!is_error, "{texts:?}");

    let document: Value = serde_json::from_str(&texts[0]).expect("the attest document");

    assert_eq!(document["document"], json!("attest"));
    assert_eq!(document["outcome"], json!("written"));
    assert_eq!(attested_by(&dir), implementer_party);

    // A refusal comes back by name, as an error result.
    let (_, texts, is_error) = client
        .call(
            "attest",
            json!({ "declaration": "transaction operation.nope/tx.nope", "structural": false }),
        )
        .await;

    assert!(is_error);

    let document: Value = serde_json::from_str(&texts[0]).expect("the attest document");

    assert_eq!(document["outcome"], json!("refused"));
    assert_eq!(document["refusal"], json!("unknown_declaration"));

    // The reviewer's review is stamped with its credential.
    let mut client = McpClient::connect(&url, &reviewer).await;

    let (_, texts, is_error) = client
        .call(
            "record_review",
            json!({ "declaration": CANCEL, "verdict": "confirmed" }),
        )
        .await;

    assert!(!is_error, "{texts:?}");

    let document: Value = serde_json::from_str(&texts[0]).expect("the review document");

    assert_eq!(document["document"], json!("review"));
    assert_eq!(document["reviews"][0]["outcome"], json!("recorded"));
    assert_eq!(reviews(&dir), [(reviewer_party, Verdict::Confirmed)]);

    // The coordinator's are stamped with the person running the
    // checkout.
    let coordinator = engine
        .create_session(WriteScope::of([conseqa::design::WriteGrant::All]), "ui")
        .expect("a session")
        .token
        .0;

    let mut client = McpClient::connect(&url, &coordinator).await;
    let (facts, _) = work_item(&mut client).await;

    let (_, texts, is_error) = client
        .call(
            "attest",
            json!({ "declaration": CANCEL, "facts": facts, "structural": true }),
        )
        .await;

    assert!(!is_error, "{texts:?}");
    assert_eq!(attested_by(&dir), person("Ana Tester"));

    // Every role reads the status and the plan, a summary first.
    let (_, texts, is_error) = client.call("conformance_status", json!({})).await;

    assert!(!is_error, "{texts:?}");
    assert!(texts[0].starts_with("declarations"), "{}", texts[0]);
    assert_eq!(
        serde_json::from_str::<Value>(&texts[1]).unwrap()["document"],
        json!("status")
    );

    let (_, texts, is_error) = client.call("implementation_plan", json!({})).await;

    assert!(!is_error, "{texts:?}");
    assert_eq!(
        serde_json::from_str::<Value>(&texts[1]).unwrap()["document"],
        json!("plan")
    );

    server.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_review_server_serves_its_reviewers_and_nothing_of_the_design() {
    let dir = checkout();

    // An attested record to review, written while no engine runs.
    {
        let (engine, credentials, server, url) = serve(&dir).await;

        let implementer = credentials
            .issue(
                &engine,
                CredentialRole::Implementer,
                agent("claude", "job-1"),
            )
            .expect("an implementer token");

        let mut client = McpClient::connect(&url, &implementer).await;
        let (facts, _) = work_item(&mut client).await;

        let (_, texts, is_error) = client
            .call(
                "attest",
                json!({ "declaration": CANCEL, "facts": facts, "structural": false }),
            )
            .await;

        assert!(!is_error, "{texts:?}");

        server.shutdown().await;
    }

    let credentials = Credentials::default();
    let party = agent("codex", "review-1");
    let token = credentials.issue_detached(CredentialRole::Reviewer, party.clone());

    let server = mcp::serve_review(
        dir.clone(),
        credentials.clone(),
        "127.0.0.1:0".parse().expect("addr"),
    )
    .await
    .expect("the review server binds");

    let url = format!("http://{}/mcp", server.local_addr);

    let mut client = McpClient::connect(&url, &token).await;
    let mut names = client.tools().await;

    names.sort();

    assert_eq!(
        names,
        [
            "conformance_status",
            "dsl_guide",
            "explain_realization",
            "implementation_plan",
            "realization_status",
            "record_review",
            "work_item"
        ]
    );

    let (_, texts, is_error) = client
        .call(
            "record_review",
            json!({ "declaration": CANCEL, "verdict": "confirmed" }),
        )
        .await;

    assert!(!is_error, "{texts:?}");
    assert_eq!(reviews(&dir), [(party, Verdict::Confirmed)]);
    assert_eq!(credentials.recorded(&token).len(), 1);

    // A bearer it never minted reads nothing.
    let mut stranger = McpClient::connect(&url, "not-a-token").await;
    let (response, _, _) = stranger.call("conformance_status", json!({})).await;

    assert!(response.get("result").is_none(), "{response}");

    server.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

fn fake(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fake_agents")
        .join(name)
        .display()
        .to_string()
}

/// Attests the cancellation through a server that then stops, as an
/// implementer on Codex would before its record is reviewed.
async fn attest_the_cancellation(dir: &Path) {
    let (engine, credentials, server, url) = serve(dir).await;

    let implementer = credentials
        .issue(
            &engine,
            CredentialRole::Implementer,
            agent("codex", "job-1"),
        )
        .expect("an implementer token");

    let mut client = McpClient::connect(&url, &implementer).await;
    let (facts, _) = work_item(&mut client).await;

    let (_, texts, is_error) = client
        .call(
            "attest",
            json!({ "declaration": CANCEL, "facts": facts, "structural": false }),
        )
        .await;

    assert!(!is_error, "{texts:?}");

    server.shutdown().await;
}

#[tokio::test]
async fn the_review_workflow_gives_each_record_a_fresh_agent_and_reports_what_it_recorded() {
    let dir = checkout();
    attest_the_cancellation(&dir).await;

    let run = ReviewRun {
        keys: Vec::new(),
        max_agents: 2,
        budget: InvocationBudget::default(),
    };

    // An agent that reports its review: the record now holds it,
    // stamped with the backend and the job it ran in.
    let reviewer = Reviewer {
        backend: ReviewBackend::Claude(
            ClaudeCliBackend::new().with_program(fake("claude_review.sh")),
        ),
        name: "claude".to_string(),
    };

    let outcomes = conseqa::harness::review::review(&dir, &reviewer, &run)
        .await
        .expect("the run");

    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(outcomes[0].declaration.to_string(), CANCEL);
    assert!(
        matches!(outcomes[0].result, ReviewResult::Recorded { .. }),
        "{outcomes:?}"
    );

    let by = outcomes[0].by.clone().expect("a stamped reviewer");

    assert_eq!(by.kind, PartyKind::Agent);
    assert_eq!(by.name, "claude");
    assert!(by.job.as_deref().unwrap().starts_with("review-"));
    assert_eq!(reviews(&dir), [(by, Verdict::Confirmed)]);

    // Confirmed by another party than its attester, the record needs no
    // review, so a run with no keys has nothing to do.
    assert_eq!(
        conseqa::harness::review::review(&dir, &reviewer, &run)
            .await
            .expect("the run"),
        []
    );

    // An agent that ends without recording a review is reported so.
    let silent = Reviewer {
        backend: ReviewBackend::Claude(ClaudeCliBackend::new().with_program(fake("claude_ok.sh"))),
        name: "claude".to_string(),
    };

    let outcomes = conseqa::harness::review::review(
        &dir,
        &silent,
        &ReviewRun {
            keys: vec![CANCEL.parse().unwrap()],
            ..run.clone()
        },
    )
    .await
    .expect("the run");

    assert!(
        matches!(&outcomes[0].result, ReviewResult::NotReported { message } if message.contains("without recording")),
        "{outcomes:?}"
    );

    // A declaration with no record is refused before any agent starts.
    let outcomes = conseqa::harness::review::review(
        &dir,
        &silent,
        &ReviewRun {
            keys: vec![
                "transaction operation.apply_payment/tx.apply_payment"
                    .parse()
                    .unwrap(),
            ],
            ..run
        },
    )
    .await
    .expect("the run");

    assert!(
        matches!(&outcomes[0].result, ReviewResult::Refused { .. }),
        "{outcomes:?}"
    );
    assert_eq!(outcomes[0].by, None);

    let _ = std::fs::remove_dir_all(&dir);
}
