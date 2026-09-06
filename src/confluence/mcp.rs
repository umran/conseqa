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
use super::task::{TaskId, TaskState};
use super::workspace::EvidenceRef;
use super::workspace_manager::WorkspaceManager;

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
pub struct RequestDesignParams {
    /// Optional extra guidance for the worker agents, layered on top of
    /// the run's prompt.
    #[serde(default)]
    pub objective: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OpenProjectParams {
    /// The project name (letters, digits, `.`, `_`, `-`).
    pub project: String,

    /// Seed a newly created project with this natural-language intent.
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateProjectParams {
    /// The new project's name (letters, digits, `.`, `_`, `-`).
    pub project: String,

    /// Natural-language description of the system to build.
    #[serde(default)]
    pub prompt: Option<String>,
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

/// Launches the concurrent multi-agent design workflow against one
/// project's engine. Implemented by the orchestrating binary (which
/// depends on the `harness` module) and injected here, so the MCP
/// server can trigger a fanout without `confluence` depending on
/// `harness`.
pub trait DesignLauncher: Send + Sync {
    /// Starts the workflow in the background against `engine` and
    /// returns a description of the launched run, or a reason it could
    /// not start (for example, one is already running).
    fn launch(
        &self,
        engine: ConfluenceEngine,
        objective: Option<String>,
    ) -> Result<serde_json::Value, String>;
}

/// One interactive UI connection's project state, keyed by its MCP
/// transport session id. Established by `open_project` / `create_project`
/// and reused across the connection's later tool calls.
#[derive(Clone)]
struct ProjectSession {
    engine: ConfluenceEngine,
    /// The interactive session token in that engine (never sent to the
    /// client; the client authenticates with the stable API key). Rolls
    /// to a successor internally on each commit while the string stays
    /// stable.
    token: String,
}

/// Multi-project hosting: a stable API key authenticates UI clients,
/// and each connection selects a project through the project tools.
#[derive(Clone)]
struct MultiProject {
    manager: Arc<WorkspaceManager>,
    api_key: String,
    sessions: Arc<parking_lot::RwLock<rustc_hash::FxHashMap<String, ProjectSession>>>,
}

/// How the MCP server resolves credentials and finds engines.
#[derive(Clone)]
enum Backing {
    /// One fixed engine; the bearer is a per-task capability (§43). The
    /// embedded and single-project daemon paths.
    Single(ConfluenceEngine),

    /// Many projects behind a stable API key; the UI selects a project
    /// per connection, and worker capability tokens resolve against the
    /// open engines.
    Multi(MultiProject),
}

/// What one authenticated architecture request resolves to.
struct Resolved {
    engine: ConfluenceEngine,
    task: TaskId,
}

/// Why a request could not be resolved to an authorized task.
enum ResolveError {
    /// Malformed transport or missing/incorrect credential.
    Unauthorized(String),

    /// Authenticated, but no project is open on this connection.
    NoProject,
}

#[derive(Clone)]
pub struct ConseqaMcp {
    backing: Backing,
    launcher: Option<Arc<dyn DesignLauncher>>,
}

impl ConseqaMcp {
    pub fn new(engine: ConfluenceEngine) -> Self {
        Self {
            backing: Backing::Single(engine),
            launcher: None,
        }
    }

    /// A single-project server that can also launch the concurrent
    /// design workflow.
    pub fn with_launcher(engine: ConfluenceEngine, launcher: Arc<dyn DesignLauncher>) -> Self {
        Self {
            backing: Backing::Single(engine),
            launcher: Some(launcher),
        }
    }

    /// A multi-project server: a stable API key authenticates UI
    /// clients, which select projects through the project tools.
    pub fn multi(
        manager: Arc<WorkspaceManager>,
        api_key: impl Into<String>,
        launcher: Option<Arc<dyn DesignLauncher>>,
    ) -> Self {
        Self {
            backing: Backing::Multi(MultiProject {
                manager,
                api_key: api_key.into(),
                sessions: Arc::new(parking_lot::RwLock::new(rustc_hash::FxHashMap::default())),
            }),
            launcher,
        }
    }

    /// The bearer credential and MCP transport session id of a request.
    fn credentials(
        context: &RequestContext<RoleServer>,
    ) -> Result<(String, Option<String>), ResolveError> {
        let parts = context.extensions.get::<http::request::Parts>().ok_or_else(|| {
            ResolveError::Unauthorized("the transport did not carry HTTP request parts".to_string())
        })?;

        let bearer = parts
            .headers
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or_else(|| {
                ResolveError::Unauthorized(
                    "missing credential: send `Authorization: Bearer <token>`".to_string(),
                )
            })?
            .to_string();

        let session = parts
            .headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);

        Ok((bearer, session))
    }

    /// Resolves a request to an authorized `(engine, task)` (§43).
    fn resolve(&self, context: &RequestContext<RoleServer>) -> Result<Resolved, ResolveError> {
        let (bearer, mcp_session) = Self::credentials(context)?;

        match &self.backing {
            Backing::Single(engine) => {
                let task = engine.resolve_token(&bearer).ok_or_else(|| {
                    ResolveError::Unauthorized(
                        "the task capability is not recognized; the task may have ended"
                            .to_string(),
                    )
                })?;

                Ok(Resolved {
                    engine: engine.clone(),
                    task,
                })
            }

            Backing::Multi(multi) => {
                if bearer == multi.api_key {
                    // Interactive UI client: route by connection.
                    let session_id = mcp_session.ok_or(ResolveError::NoProject)?;

                    let session = multi
                        .sessions
                        .read()
                        .get(&session_id)
                        .cloned()
                        .ok_or(ResolveError::NoProject)?;

                    let task = session.engine.resolve_token(&session.token).ok_or_else(|| {
                        ResolveError::Unauthorized(
                            "the project session has ended; open a project again".to_string(),
                        )
                    })?;

                    Ok(Resolved {
                        engine: session.engine,
                        task,
                    })
                } else {
                    // Worker agent: its capability token resolves in
                    // whichever open project engine minted it.
                    for engine in multi.manager.open_engines() {
                        if let Some(task) = engine.resolve_token(&bearer) {
                            return Ok(Resolved { engine, task });
                        }
                    }

                    Err(ResolveError::Unauthorized(
                        "the credential is not recognized".to_string(),
                    ))
                }
            }
        }
    }

    /// Authenticates a UI client for the project tools (which select a
    /// project rather than operate within one), returning its MCP
    /// session id.
    fn authenticate_client(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<(MultiProject, String), McpError> {
        let (bearer, mcp_session) = Self::credentials(context).map_err(resolve_to_mcp_error)?;

        let Backing::Multi(multi) = &self.backing else {
            return Err(McpError::invalid_request(
                "project selection is only available on a multi-project server",
                None,
            ));
        };

        if bearer != multi.api_key {
            return Err(McpError::invalid_request(
                "the API key is not recognized",
                None,
            ));
        }

        let session_id = mcp_session.ok_or_else(|| {
            McpError::invalid_request("the transport did not carry an MCP session id", None)
        })?;

        Ok((multi.clone(), session_id))
    }

    /// Opens or creates a project for this connection and starts its
    /// interactive session.
    fn open_project_session(
        multi: &MultiProject,
        mcp_session: &str,
        project: &str,
        prompt: Option<String>,
    ) -> Result<(ConfluenceEngine, super::workspace_manager::ProjectInfo), String> {
        let engine = multi
            .manager
            .open_or_create(project, prompt)
            .map_err(|error| error.to_string())?;

        let handle = engine
            .create_session(
                super::task::WriteScope::of([super::task::WriteGrant::All]),
                format!("interactive session for project {project}"),
            )
            .map_err(|error| error.to_string())?;

        multi.sessions.write().insert(
            mcp_session.to_string(),
            ProjectSession {
                engine: engine.clone(),
                token: handle.token.0,
            },
        );

        let info = super::workspace_manager::ProjectInfo {
            name: project.to_string(),
            open: true,
            revision: Some(engine.head_revision()),
        };

        Ok((engine, info))
    }
}

/// Converts an internal resolve error into the tool-facing MCP error.
fn resolve_to_mcp_error(error: ResolveError) -> McpError {
    match error {
        ResolveError::Unauthorized(message) => McpError::invalid_request(message, None),
        ResolveError::NoProject => McpError::invalid_request(
            "no project is open on this connection; call open_project or create_project first",
            None,
        ),
    }
}

/// The tool-result form of a "no project open" state, so architecture
/// tools guide the agent instead of failing at the protocol level.
fn no_project_result() -> Result<CallToolResult, McpError> {
    json_error(serde_json::json!({
        "error": "no project is open on this connection",
        "guidance": "Call list_projects to see existing projects, open_project to select \
                     one, or create_project to start a new one. Then use the architecture \
                     tools.",
    }))
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

/// Parses one tool argument, returning an in-band tool-result error on
/// failure rather than a JSON-RPC protocol error. A protocol error is
/// what an MCP client surfaces as a "transport" failure — opaque to the
/// model — so a malformed argument must come back as a readable tool
/// result the agent can correct, pointing at `dsl_reference`.
///
/// A structured argument is accepted whether it arrives as a JSON
/// object or as a JSON-encoded string: some models serialize nested
/// arguments as strings when the parameter's schema is permissive, and
/// rejecting that is needless friction.
///
/// The `Err` arm is an already-formed tool result; handlers return it
/// with `?` short-circuiting turned into an explicit `return`.
fn parse<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
    what: &str,
) -> Result<T, Result<CallToolResult, McpError>> {
    let value = coerce_stringified_json(value);

    serde_json::from_value(value).map_err(|error| {
        json_error(serde_json::json!({
            "error": format!("{what} did not parse: {error}"),
            "guidance": format!(
                "Pass `{what}` as a JSON object, not a JSON string. Call dsl_reference \
                 for the exact shapes, then try again."
            ),
        }))
    })
}

/// If a value is a string that itself holds JSON, returns the parsed
/// JSON; otherwise returns the value unchanged. Non-JSON strings pass
/// through so the typed deserialize can report a readable error.
fn coerce_stringified_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
        }
        other => other,
    }
}

/// Unwraps a [`parse`] result, returning the tool-result error from the
/// enclosing handler on failure.
macro_rules! parse_arg {
    ($value:expr, $what:expr) => {
        match parse($value, $what) {
            Ok(value) => value,
            Err(result) => return result,
        }
    };
}

/// Resolves a request to `(engine, task)`, returning a guiding
/// tool-result when no project is open and the MCP error otherwise.
macro_rules! resolved {
    ($self:expr, $context:expr) => {
        match $self.resolve(&$context) {
            Ok(resolved) => resolved,
            Err(ResolveError::NoProject) => return no_project_result(),
            Err(error) => return Err(resolve_to_mcp_error(error)),
        }
    };
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
        let resolved = resolved!(self, context);
        let (engine, task) = (resolved.engine, resolved.task);

        match engine.task_context(task) {
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
        let resolved = resolved!(self, context);
        let (engine, task) = (resolved.engine, resolved.task);

        match engine.task_status(task) {
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
        let resolved = resolved!(self, context);
        let (engine, task) = (resolved.engine, resolved.task);
        let key: SymbolKey = parse_arg!(params.0.symbol, "symbol");

        match engine.read_symbol(task, &key) {
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
        let resolved = resolved!(self, context);
        let (engine, task) = (resolved.engine, resolved.task);
        let params = params.0;

        let kind = match params.kind {
            None => None,
            Some(kind) => Some(parse_arg!(serde_json::Value::String(kind), "kind")),
        };

        let spec = SearchSpec {
            kind,
            prefix: params.prefix,
            service: params.service.map(|service| id_from(&service)),
        };

        match engine.search_symbols(task, &spec) {
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
        let resolved = resolved!(self, context);
        let (engine, task) = (resolved.engine, resolved.task);
        let params = params.0;

        let mode: OperationReadMode =
            parse_arg!(serde_json::Value::String(params.mode), "mode");

        match engine.read_operation(task, &id_from(&params.operation), mode) {
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
        let resolved = resolved!(self, context);
        let (engine, task) = (resolved.engine, resolved.task);
        let query: GraphQuery = parse_arg!(params.0.query, "query");

        match engine.graph_query(task, &query) {
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
        let resolved = resolved!(self, context);
        let (engine, task) = (resolved.engine, resolved.task);
        let params = params.0;

        match engine.requirement_report(
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
        let resolved = resolved!(self, context);
        let (engine, task) = (resolved.engine, resolved.task);
        let params = params.0;

        let patch: SpecPatch = parse_arg!(params.patch, "patch");

        if patch.mutations.is_empty() {
            return json_error(serde_json::json!({
                "committed": false,
                "error": "empty patch",
                "guidance": "A patch must contain at least one mutation. Call dsl_reference \
                             for mutation shapes and submit a non-empty patch, or finish \
                             the task another way if there is nothing to change.",
            }));
        }

        let base_revision = match params.base_revision {
            Some(revision) => Revision(revision),
            None => match engine.task_context(task) {
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

        match engine.submit(request).await {
            Err(error) => engine_error(error),

            Ok(Ok(receipt)) => json_result(serde_json::json!({
                "committed": true,
                "revision": receipt.revision.0,
                "replayed": receipt.replayed,
                "client_nonce": client_nonce.to_string(),
            })),

            // An already-committed task is success, not a failure to
            // fix: a task commits exactly one patch, then it is done.
            Ok(Err(CommitRejection::TaskNotRunning {
                state: TaskState::Committed,
            })) => json_result(serde_json::json!({
                "committed": true,
                "already_committed": true,
                "message": "This task already committed its one patch; it is complete. \
                            Stop here — do not submit again.",
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
        let resolved = resolved!(self, context);
        let (engine, task) = (resolved.engine, resolved.task);
        let params = params.0;

        let target: SymbolKey = parse_arg!(params.target, "target");

        match engine.dependency_request(
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
        description = "Launch the concurrent multi-agent design workflow against this shared \
                       model: independent coding-agent sessions fan out to synthesize and \
                       repair operations in parallel, each committing through the same gate. \
                       Use this to build out or complete an architecture faster than editing \
                       one operation at a time. Returns immediately; the workers run in the \
                       background — watch the head advance (task_context) and read the growing \
                       model with the read tools. One workflow runs at a time."
    )]
    async fn request_design(
        &self,
        params: Parameters<RequestDesignParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // Any valid session establishes that the caller is an
        // authorized client of this engine.
        let resolved = resolved!(self, context);
        let engine = resolved.engine;

        let Some(launcher) = &self.launcher else {
            return json_error(serde_json::json!({
                "launched": false,
                "error": "concurrent design is not available on this server",
                "guidance": "This confluence server has no orchestration backend. Run the \
                             design workflow with the conseqa-harness CLI instead.",
            }));
        };

        match launcher.launch(engine, params.0.objective) {
            Ok(value) => json_result(value),
            Err(error) => json_error(serde_json::json!({
                "launched": false,
                "error": error,
            })),
        }
    }

    #[tool(
        description = "List the projects this server hosts, each an isolated architecture \
                       model with its own history. Use open_project to select one for this \
                       connection, or create_project to start a new one."
    )]
    async fn list_projects(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (multi, _session) = self.authenticate_client(&context)?;

        let projects: Vec<serde_json::Value> = multi
            .manager
            .list()
            .into_iter()
            .map(|project| {
                serde_json::json!({
                    "name": project.name,
                    "open": project.open,
                    "revision": project.revision.map(|revision| revision.0),
                })
            })
            .collect();

        json_result(serde_json::json!({ "projects": projects }))
    }

    #[tool(
        description = "Select an existing project for this connection. Every subsequent \
                       architecture tool call operates on it. Creates the project if it does \
                       not exist. Pass an optional prompt to seed a new project's intent."
    )]
    async fn open_project(
        &self,
        params: Parameters<OpenProjectParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (multi, session_id) = self.authenticate_client(&context)?;
        let params = params.0;

        match Self::open_project_session(&multi, &session_id, &params.project, params.prompt) {
            Ok((_engine, info)) => json_result(serde_json::json!({
                "project": info.name,
                "opened": true,
                "revision": info.revision.map(|revision| revision.0),
                "note": "This connection is now working on this project. Use task_context and \
                         the read tools to explore it, submit_patch to change it, and \
                         request_design to fan out concurrent agents.",
            })),

            Err(error) => json_error(serde_json::json!({
                "opened": false,
                "error": error,
            })),
        }
    }

    #[tool(
        description = "Create a new isolated project and select it for this connection, \
                       seeded with an optional natural-language prompt describing the system \
                       to build. Errors if a project of that name already exists."
    )]
    async fn create_project(
        &self,
        params: Parameters<CreateProjectParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (multi, session_id) = self.authenticate_client(&context)?;
        let params = params.0;

        if multi
            .manager
            .list()
            .into_iter()
            .any(|project| project.name == params.project)
        {
            return json_error(serde_json::json!({
                "created": false,
                "error": format!("a project named `{}` already exists", params.project),
                "guidance": "Use open_project to work on it, or choose a different name.",
            }));
        }

        match Self::open_project_session(&multi, &session_id, &params.project, params.prompt) {
            Ok((_engine, info)) => json_result(serde_json::json!({
                "project": info.name,
                "created": true,
                "note": "New project created and selected for this connection. Its prompt is \
                         in task_context. Build it with submit_patch, or call request_design \
                         to fan out concurrent agents.",
            })),

            Err(error) => json_error(serde_json::json!({
                "created": false,
                "error": error,
            })),
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
    build_router(ConseqaMcp::new(engine))
}

/// The `/mcp` router with a design launcher wired in, so `request_design`
/// can start the concurrent workflow.
pub fn router_with_launcher(
    engine: ConfluenceEngine,
    launcher: Arc<dyn DesignLauncher>,
) -> axum::Router {
    build_router(ConseqaMcp::with_launcher(engine, launcher))
}

/// The `/mcp` router for a multi-project server: a stable API key
/// authenticates UI clients, which select projects through the project
/// tools, and an optional design launcher powers `request_design`.
pub fn router_multi(
    manager: Arc<WorkspaceManager>,
    api_key: impl Into<String>,
    launcher: Option<Arc<dyn DesignLauncher>>,
) -> axum::Router {
    build_router(ConseqaMcp::multi(manager, api_key, launcher))
}

fn build_router(mcp: ConseqaMcp) -> axum::Router {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

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
