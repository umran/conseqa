//! The MCP tool surface (§42–§53 of the confluence spec).
//!
//! One shared Streamable HTTP server; every request authenticates with
//! a per-task bearer capability that resolves to the task's pinned
//! snapshot, read tracker, and write scope. The tool set is
//! deliberately small: tracked reads, canonical queries, one mutation
//! tool (`submit_patch`), dependency requests, and status.
//!
//! DSL payloads — symbol keys, graph queries, patches — travel in
//! their canonical serialized JSON forms; `dsl_reference` documents
//! the shapes compactly for agents.

use std::net::SocketAddr;
use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo},
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::spec::{Id, Revision};

use super::commit::{CommitRejection, CommitRequest};
use super::engine::{ConfluenceEngine, EngineError, OperationReadMode};
use super::graph_query::GraphQuery;
use super::patch::{PatchId, SpecPatch};
use super::read_set::SearchSpec;
use super::symbol::SymbolKey;
use super::task::TaskId;
use super::workspace::EvidenceRef;

/// The invariants every architecture agent operates under (§89),
/// served as the MCP server's instructions.
const INSTRUCTIONS: &str = "\
Conseqa shared architecture state. Invariants:
1. Shared Conseqa state is available only through these tools.
2. Do not infer that your snapshot is current after invalidation.
3. Only submit changes through submit_patch.
4. Do not modify symbols outside your write scope.
5. If another symbol must change, use dependency_request.
6. Requirements are obligations, not guarantees.
7. Do not weaken or remove a requirement to make verification pass.
8. Prefer unknown/unresolved over inventing an unsupported guarantee.
9. Finish by committing one patch, filing a dependency request, or \
reporting unresolved.
Call dsl_reference for the JSON shapes of symbols, queries, and \
patches.";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadSymbolParams {
    /// The symbol key in its canonical serialized form, e.g.
    /// {"kind":"schema","value":"schema.Order"} or
    /// {"kind":"transaction","value":{"operation":"operation.checkout",
    /// "transaction":"tx.charge"}}. See dsl_reference.
    pub symbol: serde_json::Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchSymbolsParams {
    /// Restrict to one symbol kind: service, schema, data_model,
    /// data_object, topic, state_machine, transition, operation,
    /// operation_interface, operation_program, operation_requirements,
    /// operation_execution, input, transaction, effect_site, binding,
    /// requirement, operation_summary, prompt_obligation.
    #[serde(default)]
    pub kind: Option<String>,

    /// Substring matched against the symbol's display form.
    #[serde(default)]
    pub prefix: Option<String>,

    /// Restrict operations to one service id.
    #[serde(default)]
    pub service: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadOperationParams {
    /// The operation id.
    pub operation: String,

    /// Which slice: interface, program, requirements, proof_summary,
    /// or full. Prefer interface and proof_summary for dependencies;
    /// full only when truly needed.
    pub mode: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphQueryParams {
    /// The canonical query, e.g. {"kind":"callers","operation":
    /// "operation.checkout"} or {"kind":"writers","data_model":
    /// "data.checkout","object":"object.order","field":"status"}.
    /// See dsl_reference.
    pub query: serde_json::Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RequirementReportParams {
    /// Restrict to one operation id.
    #[serde(default)]
    pub operation: Option<String>,

    /// Restrict to one requirement family: serialization, ordering,
    /// idempotency, result_replay, recoverability.
    #[serde(default)]
    pub family: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SubmitPatchParams {
    /// The typed patch: {"mutations":[...]}. See dsl_reference for
    /// mutation shapes.
    pub patch: serde_json::Value,

    /// The revision your reasoning is based on. Defaults to your
    /// task's pinned snapshot revision.
    #[serde(default)]
    pub base_revision: Option<u64>,

    /// Idempotency nonce (UUID). Reuse the same nonce when retrying a
    /// submission whose response was lost; a fresh one is generated
    /// when omitted and echoed back.
    #[serde(default)]
    pub client_nonce: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DependencyRequestParams {
    /// The symbol that must change, in canonical serialized form.
    pub target: serde_json::Value,

    /// The concrete change being requested.
    pub requested_change: String,

    /// Why the change is needed.
    pub reason: String,

    /// Evidence pointers (prompt spans, report obligation ids).
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Clone)]
pub struct ConseqaMcp {
    engine: ConfluenceEngine,
}

impl ConseqaMcp {
    pub fn new(engine: ConfluenceEngine) -> Self {
        Self { engine }
    }

    /// Resolves the request's bearer capability to its task (§43).
    fn authenticate(&self, context: &RequestContext<RoleServer>) -> Result<TaskId, McpError> {
        let parts = context
            .extensions
            .get::<http::request::Parts>()
            .ok_or_else(|| {
                McpError::invalid_request("the transport did not carry HTTP request parts", None)
            })?;

        let token = parts
            .headers
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or_else(|| {
                McpError::invalid_request(
                    "missing task capability: send `Authorization: Bearer <task-token>`",
                    None,
                )
            })?;

        self.engine.resolve_token(token).ok_or_else(|| {
            McpError::invalid_request(
                "the task capability is not recognized; the task may have ended",
                None,
            )
        })
    }
}

fn json_result(value: serde_json::Value) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(&value)
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;

    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

fn json_error(value: serde_json::Value) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(&value)
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;

    Ok(CallToolResult::error(vec![ContentBlock::text(text)]))
}

/// Engine failures surface as tool errors the model can read; they
/// carry their own do-not-retry guidance where relevant.
fn engine_error(error: EngineError) -> Result<CallToolResult, McpError> {
    json_error(serde_json::json!({ "error": error.to_string() }))
}

fn parse<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
    what: &str,
) -> Result<T, McpError> {
    serde_json::from_value(value).map_err(|error| {
        McpError::invalid_params(
            format!("{what} does not parse: {error}; call dsl_reference for the expected shape"),
            None,
        )
    })
}

fn id_from(text: &str) -> Id {
    Id(text.to_string())
}

#[tool_router]
impl ConseqaMcp {
    #[tool(
        description = "Your task: objective, kind, pinned snapshot revision, write scope, \
                       prompt evidence, and completion condition. Call this first."
    )]
    async fn task_context(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let task = self.authenticate(&context)?;

        match self.engine.task_context(task) {
            Ok(view) => json_result(serde_json::to_value(view).expect("context serializes")),
            Err(error) => engine_error(error),
        }
    }

    #[tool(
        description = "Your task's current state: running, invalidated, committed, or \
                       cancelled. If invalidated, stop — the harness restarts the task."
    )]
    async fn task_status(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let task = self.authenticate(&context)?;

        match self.engine.task_status(task) {
            Ok(state) => json_result(serde_json::json!({ "state": state.to_string() })),
            Err(error) => engine_error(error),
        }
    }

    #[tool(
        description = "Read one shared architecture symbol from your pinned snapshot. The \
                       read is tracked: committing depends on it still holding."
    )]
    async fn read_symbol(
        &self,
        params: Parameters<ReadSymbolParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let task = self.authenticate(&context)?;
        let key: SymbolKey = parse(params.0.symbol, "symbol")?;

        match self.engine.read_symbol(task, &key) {
            Ok(view) => json_result(serde_json::to_value(view).expect("view serializes")),
            Err(error) => engine_error(error),
        }
    }

    #[tool(
        description = "Search shared symbols by kind, substring, or service — for \
                       navigation. Relying on the returned set is tracked like a query."
    )]
    async fn search_symbols(
        &self,
        params: Parameters<SearchSymbolsParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let task = self.authenticate(&context)?;
        let params = params.0;

        let kind = match params.kind {
            None => None,
            Some(kind) => Some(parse(serde_json::Value::String(kind), "kind")?),
        };

        let spec = SearchSpec {
            kind,
            prefix: params.prefix,
            service: params.service.map(|service| id_from(&service)),
        };

        match self.engine.search_symbols(task, &spec) {
            Ok(rows) => json_result(serde_json::json!({
                "symbols": rows,
            })),
            Err(error) => engine_error(error),
        }
    }

    #[tool(
        description = "Read one slice of an operation: interface, program, requirements, \
                       proof_summary, or full. Prefer interface/proof_summary for \
                       dependencies. Reads are tracked."
    )]
    async fn read_operation(
        &self,
        params: Parameters<ReadOperationParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let task = self.authenticate(&context)?;
        let params = params.0;

        let mode: OperationReadMode =
            parse(serde_json::Value::String(params.mode), "mode")?;

        match self
            .engine
            .read_operation(task, &id_from(&params.operation), mode)
        {
            Ok(view) => json_result(serde_json::to_value(view).expect("view serializes")),
            Err(error) => engine_error(error),
        }
    }

    #[tool(
        description = "Run one canonical semantic graph query — callers, callees, readers, \
                       writers, publishers, consumers, transition_users, references_to, \
                       impacted_by, operation_neighborhood, provenance_roots — against your \
                       pinned snapshot. The result set is tracked: a changed answer at \
                       commit time is a conflict."
    )]
    async fn graph_query(
        &self,
        params: Parameters<GraphQueryParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let task = self.authenticate(&context)?;
        let query: GraphQuery = parse(params.0.query, "query")?;

        match self.engine.graph_query(task, &query) {
            Ok(result) => json_result(serde_json::json!({
                "rows": result.rows,
            })),
            Err(error) => engine_error(error),
        }
    }

    #[tool(
        description = "Analyzer verdicts for declared requirements: proven or unproven, \
                       with structured proofs and obstacles. The central input for repair."
    )]
    async fn requirement_report(
        &self,
        params: Parameters<RequirementReportParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let task = self.authenticate(&context)?;
        let params = params.0;

        match self.engine.requirement_report(
            task,
            params.operation.as_deref().map(id_from),
            params.family.as_deref(),
        ) {
            Ok(report) => json_result(report),
            Err(error) => engine_error(error),
        }
    }

    #[tool(
        description = "Submit your typed patch through the serializable commit gate. The \
                       gate revalidates everything you observed; a stale-context rejection \
                       means stop — do not retry in this session, the harness restarts the \
                       task. Scope, unobserved-dependency, and draft-validation rejections \
                       are fixable here."
    )]
    async fn submit_patch(
        &self,
        params: Parameters<SubmitPatchParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let task = self.authenticate(&context)?;
        let params = params.0;

        let patch: SpecPatch = parse(params.patch, "patch")?;

        let base_revision = match params.base_revision {
            Some(revision) => Revision(revision),
            None => match self.engine.task_context(task) {
                Ok(view) => view.snapshot_revision,
                Err(error) => return engine_error(error),
            },
        };

        let client_nonce = match params.client_nonce {
            None => Uuid::new_v4(),
            Some(text) => Uuid::parse_str(&text).map_err(|error| {
                McpError::invalid_params(format!("client_nonce is not a UUID: {error}"), None)
            })?,
        };

        let request = CommitRequest {
            task,
            patch_id: PatchId::fresh(),
            base_revision,
            patch,
            client_nonce,
        };

        match self.engine.submit(request).await {
            Err(error) => engine_error(error),

            Ok(Ok(receipt)) => json_result(serde_json::json!({
                "committed": true,
                "revision": receipt.revision.0,
                "replayed": receipt.replayed,
                "client_nonce": client_nonce.to_string(),
            })),

            Ok(Err(rejection)) => {
                let guidance = if rejection.is_stale_context() {
                    "Your context is stale. Do not try to fix this in this session; \
                     the harness will restart the task against a fresh snapshot."
                } else {
                    "This is fixable in this session: adjust the patch (or read the \
                     missing symbols) and submit again with a fresh nonce."
                };

                json_error(serde_json::json!({
                    "committed": false,
                    "rejection": rejection_body(&rejection),
                    "message": rejection.to_string(),
                    "stale_context": rejection.is_stale_context(),
                    "guidance": guidance,
                }))
            }
        }
    }

    #[tool(
        description = "Request a change to a symbol outside your write scope — a schema \
                       field, a callee contract, a topic, a transition. The scheduler \
                       routes it to the owning authority. Never edit outside your scope."
    )]
    async fn dependency_request(
        &self,
        params: Parameters<DependencyRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let task = self.authenticate(&context)?;
        let params = params.0;

        let target: SymbolKey = parse(params.target, "target")?;

        match self.engine.dependency_request(
            task,
            target,
            params.requested_change,
            params.reason,
            params.evidence.into_iter().map(EvidenceRef).collect(),
        ) {
            Ok(id) => json_result(serde_json::json!({
                "dependency_request": id.to_string(),
                "status": "recorded",
            })),
            Err(error) => engine_error(error),
        }
    }

    #[tool(
        description = "Compact reference for the JSON shapes these tools exchange: symbol \
                       keys, graph queries, and patch mutations."
    )]
    async fn dsl_reference(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(
            DSL_REFERENCE.to_string(),
        )]))
    }
}

fn rejection_body(rejection: &CommitRejection) -> serde_json::Value {
    serde_json::to_value(rejection).unwrap_or_else(|_| {
        serde_json::json!({ "kind": "unserializable_rejection" })
    })
}

#[tool_handler]
impl ServerHandler for ConseqaMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());

        info.instructions = Some(INSTRUCTIONS.to_string());

        info
    }
}

/// A running MCP server bound to localhost.
pub struct McpServer {
    pub local_addr: SocketAddr,
    shutdown: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl McpServer {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.handle.await;
    }
}

/// The axum router exposing `/mcp`, for embedding alongside other
/// routes (the standalone daemon adds its admin surface).
pub fn router(engine: ConfluenceEngine) -> axum::Router {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    let mcp = ConseqaMcp::new(engine);

    let mut config = StreamableHttpServerConfig::default();

    // Plain JSON terminal responses; tools here never stream.
    config.json_response = true;

    let service = StreamableHttpService::new(
        move || Ok(mcp.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    axum::Router::new().nest_service("/mcp", service)
}

/// Serves the MCP endpoint on `bind` (use port 0 for an ephemeral
/// port; the handle reports the bound address).
pub async fn serve(engine: ConfluenceEngine, bind: SocketAddr) -> std::io::Result<McpServer> {
    serve_router(router(engine), bind).await
}

/// Serves an already composed router (the daemon's admin surface rides
/// along).
pub async fn serve_router(
    router: axum::Router,
    bind: SocketAddr,
) -> std::io::Result<McpServer> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let local_addr = listener.local_addr()?;

    let (shutdown, on_shutdown) = tokio::sync::oneshot::channel::<()>();

    let handle = tokio::spawn(async move {
        let serving = axum::serve(listener, router).with_graceful_shutdown(async {
            let _ = on_shutdown.await;
        });

        if let Err(error) = serving.await {
            tracing::error!("mcp server exited with error: {error}");
        }
    });

    Ok(McpServer {
        local_addr,
        shutdown,
        handle,
    })
}

const DSL_REFERENCE: &str = r#"Conseqa confluence JSON shapes (canonical serialized forms).

SYMBOL KEYS — {"kind": K, "value": V}:
  {"kind":"service","value":"service.checkout"}
  {"kind":"schema","value":"schema.Order"}
  {"kind":"data_model","value":"data.checkout"}
  {"kind":"data_object","value":{"data_model":"data.checkout","object":"object.order"}}
  {"kind":"topic","value":"topic.order_events"}
  {"kind":"state_machine","value":"machine.order_lifecycle"}
  {"kind":"transition","value":{"machine":"machine.x","transition":"transition.y"}}
  {"kind":"operation","value":"operation.checkout"}
  {"kind":"operation_interface","value":"operation.checkout"}
  {"kind":"operation_program","value":"operation.checkout"}
  {"kind":"operation_requirements","value":"operation.checkout"}
  {"kind":"operation_execution","value":"operation.checkout"}
  {"kind":"input","value":{"operation":"operation.x","input":"input.x.request"}}
  {"kind":"transaction","value":{"operation":"operation.x","transaction":"tx.y"}}
  {"kind":"effect_site","value":{"operation":"operation.x","effect":"effect.y"}}
  {"kind":"binding","value":{"operation":"operation.x","binding":"intent.y"}}
  {"kind":"operation_summary","value":"operation.checkout"}
  {"kind":"prompt_obligation","value":"obl.no-double-charge"}

GRAPH QUERIES — {"kind": K, ...}:
  {"kind":"callers","operation":"operation.x"}
  {"kind":"callees","operation":"operation.x"}
  {"kind":"readers","data_model":"data.x","object":"object.y","field":"status"}   (field optional)
  {"kind":"writers","data_model":"data.x","object":"object.y","field":"status"}   (field optional)
  {"kind":"publishers","topic":"topic.x"}
  {"kind":"consumers","topic":"topic.x"}
  {"kind":"transition_users","machine":"machine.x","transition":"transition.y"}
  {"kind":"references_to","symbol":<symbol key>}
  {"kind":"impacted_by","symbol":<symbol key>,"depth":2}
  {"kind":"operation_neighborhood","operation":"operation.x","depth":2}
  {"kind":"provenance_roots","operation":"operation.x","binding":"output.y"}

PATCH — {"mutations": [<mutation>, ...]}; each mutation {"kind": K, ...}:
  {"kind":"put_service","id":"service.x","value":{"kind":"backend"}}
  {"kind":"put_schema","id":"schema.X","value":<schema declaration>}
  {"kind":"put_data_model","id":"data.x","value":{"objects":{...}}}
  {"kind":"put_topic","id":"topic.x","value":{"messages":[...],"ordering":...,"message_identity":...}}
  {"kind":"put_state_machine","id":"machine.x","value":{...}}
  {"kind":"put_operation_interface","operation":"operation.x",
   "value":{"service":"service.x","description":"...","inputs":{...}}}
  {"kind":"replace_operation_program","operation":"operation.x","program":{"steps":[...]}}
  {"kind":"replace_operation_execution","operation":"operation.x",
   "execution":{"concurrency":{"kind":"unbounded"}}}
  {"kind":"replace_operation_requirements","operation":"operation.x","requirements":{...}}
  {"kind":"propose_requirements","operation":"operation.x","proposals":[
     {"requirement":{"family":"idempotency","requirement":{"key":{"components":[...]},
       "result":"replay_consistent"}},
      "origin":{"kind":"explicit_prompt","obligation":"obl.x"}}]}
    (origins: explicit_prompt {obligation}; strongly_implied {rationale, evidence};
     recommended {rationale, evidence})
  {"kind":"put_prompt_obligation","id":"obl.x","value":{"source_span":"...",
   "normalized_intent":"...","targets":["operation.x"],"status":{"kind":"unmapped"}}}
  {"kind":"delete_top_level","symbol":<symbol key>}   (coordinator-only)

Schema, topic, state-machine, input, and program declarations use the
Conseqa model YAML structure, as JSON. Value references:
  {"source":"input:input.x.request","path":"order_id"}
Derivations: {"kind":"unspecified"} or {"kind":"deterministic","from":[<value ref>...]}.
"#;
