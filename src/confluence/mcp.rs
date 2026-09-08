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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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

use super::analysis::AnalysisState;
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
/// The contract served to harness worker agents (`Single` backing):
/// one scoped task, one patch, restart on invalidation.
const WORKER_INSTRUCTIONS: &str = "\
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
Call dsl_guide for DSL semantics by topic, and dsl_reference for the \
JSON shapes of symbols, queries, and patches.";

/// The brief served to interactive clients (`Local` stdio and `Multi`
/// http backings): the authoring loop, stated up front, so an agent
/// asked to design a system with Conseqa knows exactly how to work
/// instead of reverse-engineering the crate.
const INTERACTIVE_INSTRUCTIONS: &str = "\
Conseqa architecture authoring. You design a system as a Conseqa model \
— services, schemas, data models, topics, state machines, and \
operations with explicit causal programs — and a deterministic checker \
validates it and proves or refuses its correctness obligations. Model \
state lives in this server, never in files you edit.

The authoring loop:
1. create_project or open_project selects the model you are building.
2. Learn the DSL from this server, not from source code: dsl_guide \
explains the semantics by topic; dsl_reference gives the exact JSON \
shapes plus a worked program example.
3. Author the shared skeleton yourself with submit_patch: services, \
schemas, data models, topics, state machines, and one interface per \
planned operation (its id, service, inputs, and request or \
subscription contracts). Many small typed patches are normal. The \
commit gate rejects a structurally broken patch with precise \
diagnostics; fix it and resubmit in the same session.
4. Then hand the operation programs to request_design. It runs one \
coding agent per operation still missing a program, concurrently, each \
committing through the same gate. This is the intended division of \
labor: you establish the interfaces callers reason against, the \
fanout writes the program bodies in parallel. Do not synthesize \
operation programs one at a time yourself when the system has several \
— that is what the fanout is for, and it is far slower without it.
5. While a run is active, do not submit patches: poll spec_status, \
whose design block reports running and then the finished run's report. \
When it finishes, call open_project again to refresh your session to \
the new head.
6. spec_status is your feedback loop throughout: assembly gaps while \
drafting, validation errors, or the verification verdict with exactly \
which obligations are proven and which are not. Fix what it names — \
narrowly, yourself, or with another request_design pass.
7. export_spec delivers the result: the canonical YAML, the \
verification report, and a self-contained interactive HTML \
visualization, written to a directory you choose — show these to the \
user.

Author an operation program yourself only for a one-off change, or \
when a single operation remains. Invariants: requirements are \
obligations, not guarantees; never weaken or remove a requirement to \
make verification pass; prefer unknown/unspecified over inventing a \
guarantee the design does not support. Correctness verdicts come only \
from the checker.";

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

    /// Whether a run is in flight and the last one's report, for
    /// `spec_status` to surface. `None` when the orchestrator tracks no
    /// such state.
    fn status(&self) -> Option<serde_json::Value> {
        None
    }
}

/// The active project the API key is working on. Established by
/// `open_project` / `create_project` and used by later tool calls.
///
/// It is a single server-wide slot rather than per-connection state,
/// because the Streamable HTTP transport can run statelessly — real
/// clients (Claude Code) do not carry a stable session id across calls,
/// so there is nothing per-connection to key on. For one human working
/// one project at a time this is exactly right; concurrent isolation of
/// two simultaneous UI connections is intentionally not offered here.
#[derive(Clone)]
struct ProjectSession {
    engine: ConfluenceEngine,
    /// The interactive session token in that engine (never sent to the
    /// client; the client authenticates with the stable API key). Rolls
    /// to a successor internally on each commit while the string stays
    /// stable.
    token: String,
}

/// Shared project hosting: a manager of many project engines and the
/// single active project. The active-project slot is server-wide (see
/// [`ProjectSession`]).
#[derive(Clone)]
struct ProjectHost {
    manager: Arc<WorkspaceManager>,
    active: Arc<parking_lot::RwLock<Option<ProjectSession>>>,
}

impl ProjectHost {
    fn new(manager: Arc<WorkspaceManager>) -> Self {
        Self {
            manager,
            active: Arc::new(parking_lot::RwLock::new(None)),
        }
    }
}

/// How the MCP server resolves credentials and finds engines.
#[derive(Clone)]
enum Backing {
    /// One fixed engine; the bearer is a per-task capability (§43). The
    /// embedded and single-project daemon paths.
    Single(ConfluenceEngine),

    /// Many projects over HTTP behind a stable API key; the UI selects
    /// the active project through the project tools, and worker
    /// capability tokens resolve against the open engines.
    Multi { host: ProjectHost, api_key: String },

    /// Many projects over stdio: the single local client is inherently
    /// trusted (no bearer to send), so no API key is required. Used by
    /// the `stdio` command for the Claude Desktop / Code UI, which
    /// accepts a command-based (stdio) MCP server without TLS.
    Local(ProjectHost),
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

    /// A multi-project HTTP server: a stable API key authenticates UI
    /// clients, which select projects through the project tools.
    pub fn multi(
        manager: Arc<WorkspaceManager>,
        api_key: impl Into<String>,
        launcher: Option<Arc<dyn DesignLauncher>>,
    ) -> Self {
        Self {
            backing: Backing::Multi {
                host: ProjectHost::new(manager),
                api_key: api_key.into(),
            },
            launcher,
        }
    }

    /// A multi-project stdio server: no API key, since the single local
    /// client is inherently trusted. For the Claude Desktop / Code UI.
    pub fn local(
        manager: Arc<WorkspaceManager>,
        launcher: Option<Arc<dyn DesignLauncher>>,
    ) -> Self {
        Self {
            backing: Backing::Local(ProjectHost::new(manager)),
            launcher,
        }
    }

    /// The bearer credential of a request.
    fn bearer(context: &RequestContext<RoleServer>) -> Result<String, ResolveError> {
        let parts = context.extensions.get::<http::request::Parts>().ok_or_else(|| {
            ResolveError::Unauthorized("the transport did not carry HTTP request parts".to_string())
        })?;

        parts
            .headers
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::to_string)
            .ok_or_else(|| {
                ResolveError::Unauthorized(
                    "missing credential: send `Authorization: Bearer <token>`".to_string(),
                )
            })
    }

    /// Resolves a request to an authorized `(engine, task)` (§43).
    fn resolve(&self, context: &RequestContext<RoleServer>) -> Result<Resolved, ResolveError> {
        match &self.backing {
            Backing::Single(engine) => {
                let bearer = Self::bearer(context)?;

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

            Backing::Multi { host, api_key } => {
                let bearer = Self::bearer(context)?;

                if &bearer == api_key {
                    host.active_resolved()
                } else {
                    // Worker agent: its capability token resolves in
                    // whichever open project engine minted it.
                    for engine in host.manager.open_engines() {
                        if let Some(task) = engine.resolve_token(&bearer) {
                            return Ok(Resolved { engine, task });
                        }
                    }

                    Err(ResolveError::Unauthorized(
                        "the credential is not recognized".to_string(),
                    ))
                }
            }

            // stdio: the local client is trusted; no bearer to check.
            Backing::Local(host) => host.active_resolved(),
        }
    }

    /// The project host for the project-selection tools, authenticated
    /// as appropriate for the backing.
    fn project_host(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<ProjectHost, McpError> {
        match &self.backing {
            Backing::Multi { host, api_key } => {
                let bearer = Self::bearer(context).map_err(resolve_to_mcp_error)?;

                if &bearer != api_key {
                    return Err(McpError::invalid_request("the API key is not recognized", None));
                }

                Ok(host.clone())
            }

            Backing::Local(host) => Ok(host.clone()),

            Backing::Single(_) => Err(McpError::invalid_request(
                "project selection is only available on a multi-project server",
                None,
            )),
        }
    }
}

impl ProjectHost {
    /// Resolves the active project to `(engine, task)`.
    fn active_resolved(&self) -> Result<Resolved, ResolveError> {
        let session = self.active.read().clone().ok_or(ResolveError::NoProject)?;

        let task = session.engine.resolve_token(&session.token).ok_or_else(|| {
            ResolveError::Unauthorized(
                "the project session has ended; open a project again".to_string(),
            )
        })?;

        Ok(Resolved {
            engine: session.engine,
            task,
        })
    }

    /// Opens or creates a project and makes it the active one.
    fn activate(
        &self,
        project: &str,
        prompt: Option<String>,
    ) -> Result<super::workspace_manager::ProjectInfo, String> {
        let engine = self
            .manager
            .open_or_create(project, prompt)
            .map_err(|error| error.to_string())?;

        let handle = engine
            .create_session(
                super::task::WriteScope::of([super::task::WriteGrant::All]),
                format!("interactive session for project {project}"),
            )
            .map_err(|error| error.to_string())?;

        *self.active.write() = Some(ProjectSession {
            engine: engine.clone(),
            token: handle.token.0,
        });

        Ok(super::workspace_manager::ProjectInfo {
            name: project.to_string(),
            open: true,
            revision: Some(engine.head_revision()),
        })
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DslGuideParams {
    /// A topic from the guide's table of contents — a section name or a
    /// few words of it, e.g. "effect intents", "subscription",
    /// "idempotency keys", "value references". Omit for the table of
    /// contents.
    #[serde(default)]
    pub topic: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExportSpecParams {
    /// Directory to write the artifacts into, created if missing. Use
    /// an absolute path in the user's project so the files land where
    /// they expect them.
    pub dir: String,

    /// Also render the interactive HTML visualization (default true).
    #[serde(default)]
    pub render: Option<bool>,

    /// Page title for the visualization. Defaults to the project name.
    #[serde(default)]
    pub title: Option<String>,
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
                       cancelled. A harness worker whose task is invalidated must stop — \
                       the harness restarts it. An interactive session rolls forward on \
                       each commit and stays running."
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
        description = "Fan out concurrent coding agents to write the operation programs. \
                       This is the normal way to build a system with more than one \
                       operation, and is far faster than synthesizing them yourself one at \
                       a time. Author the skeleton first — services, schemas, data models, \
                       topics, machines, and an interface per planned operation — then call \
                       this: it skips decomposition when interfaces already exist and runs \
                       one agent per operation still missing a program, concurrently, each \
                       committing through the same gate. It also repairs operations the \
                       checker leaves unproven. Optionally pass an objective to steer the \
                       workers. Returns immediately; poll spec_status (its design block) \
                       rather than patching while it runs, then call open_project to refresh \
                       your session to the new head. One workflow runs at a time."
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
                       model with its own history. Use open_project to make one \
                       active, or create_project to start a new one."
    )]
    async fn list_projects(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let host = self.project_host(&context)?;

        let projects: Vec<serde_json::Value> = host
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
        description = "Select a project as the active one. Every subsequent architecture \
                       tool call operates on it. Creates the project if it does not exist. \
                       Pass an optional prompt to seed a new project's intent."
    )]
    async fn open_project(
        &self,
        params: Parameters<OpenProjectParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let host = self.project_host(&context)?;
        let params = params.0;

        match host.activate(&params.project, params.prompt) {
            Ok(info) => json_result(serde_json::json!({
                "project": info.name,
                "opened": true,
                "revision": info.revision.map(|revision| revision.0),
                "note": "This is now the active project. Explore it with task_context and \
                         the read tools, learn the DSL with dsl_guide, change it with \
                         submit_patch, check spec_status for the checker's verdict, and \
                         export_spec to deliver YAML, report, and visualization. \
                         request_design fans out concurrent agents. Opening another \
                         project switches the active one.",
            })),

            Err(error) => json_error(serde_json::json!({
                "opened": false,
                "error": error,
            })),
        }
    }

    #[tool(
        description = "Create a new isolated project and make it active, \
                       seeded with an optional natural-language prompt describing the system \
                       to build. Errors if a project of that name already exists."
    )]
    async fn create_project(
        &self,
        params: Parameters<CreateProjectParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let host = self.project_host(&context)?;
        let params = params.0;

        if host
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

        match host.activate(&params.project, params.prompt) {
            Ok(info) => json_result(serde_json::json!({
                "project": info.name,
                "created": true,
                "note": "New project created and now active. Its prompt is in task_context. \
                         Learn the DSL with dsl_guide and dsl_reference, then author the \
                         skeleton with submit_patch — services, schemas, data models, \
                         topics, machines, and one interface per planned operation — and \
                         call request_design to write the operation programs concurrently, \
                         one agent per operation. (request_design on an empty project \
                         decomposes as well, if you would rather hand it the whole build.) \
                         Check spec_status as you go, and export_spec to deliver.",
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
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "{DSL_REFERENCE}{PROGRAM_EXAMPLE_PREAMBLE}\n{PROGRAM_EXAMPLE_JSON}\n"
        ))]))
    }

    #[tool(
        description = "The Conseqa DSL semantics, by topic: what services, schemas, data \
                       models, topics, state machines, operations, programs, transactions, \
                       effects, effect intents, value references, and requirements mean and \
                       how they compose. Call with no topic for the table of contents, then \
                       with a topic — a section name or a few words of it — for the full \
                       section. Use this instead of reading Conseqa's source code."
    )]
    async fn dsl_guide(
        &self,
        params: Parameters<DslGuideParams>,
    ) -> Result<CallToolResult, McpError> {
        let text = match params.0.topic.as_deref() {
            None => guide_toc(),
            Some(topic) => guide_lookup(topic),
        };

        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(
        description = "Where the active model stands right now: revision, an inventory of \
                       its symbols, which operations still lack programs, and the checker's \
                       verdict — assembly gaps while drafting, validation errors, or the \
                       verification result with every open obligation. Call it after \
                       committing to steer your next step."
    )]
    async fn spec_status(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let resolved = resolved!(self, context);
        let engine = resolved.engine;

        let head = engine.head_snapshot();
        let workspace = &head.workspace;

        let unfinished: Vec<String> = workspace
            .operations
            .iter()
            .filter(|(_, draft)| draft.program.is_none() || draft.execution.is_none())
            .map(|(id, _)| id.to_string())
            .collect();

        let prompt_obligations: serde_json::Map<String, serde_json::Value> = workspace
            .prompt_obligations
            .iter()
            .map(|(id, obligation)| {
                (
                    id.0.clone(),
                    serde_json::Value::String(obligation_status_label(&obligation.status)),
                )
            })
            .collect();

        // A design run is reported before waiting on analysis: while
        // workers are committing, the head moves under us and the
        // caller's next step is to keep polling, not to read a verdict.
        let design = self.launcher.as_ref().and_then(|launcher| launcher.status());

        let running = design
            .as_ref()
            .and_then(|status| status.get("running"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let analysis = if running {
            serde_json::json!({
                "state": "design_running",
                "guidance": "Worker agents are committing to this model right now. Wait for \
                             the design run to finish before judging the verdict or \
                             submitting patches of your own.",
            })
        } else {
            analysis_json(&settled_analysis(&engine, head.revision).await)
        };

        let mut body = serde_json::json!({
            "revision": head.revision.0,
            "inventory": {
                "services": workspace.services.len(),
                "schemas": workspace.schemas.len(),
                "data_models": workspace.data_models.len(),
                "topics": workspace.topics.len(),
                "state_machines": workspace.state_machines.len(),
                "operations": workspace.operations.len(),
                "operations_without_programs": unfinished,
            },
            "prompt_obligations": prompt_obligations,
            "analysis": analysis,
        });

        if let Some(design) = design {
            body["design"] = design;

            if !running {
                body["note"] = serde_json::Value::String(
                    "If a design run just finished, call open_project again to refresh your \
                     session to the new head before reading or patching."
                        .to_string(),
                );
            }
        }

        json_result(body)
    }

    #[tool(
        description = "Export the active model for delivery: writes conseqa.yaml (the \
                       canonical model), verification-report.json (the checker's obligation \
                       report), and by default spec.html — a self-contained interactive \
                       visualization with the report overlaid — into the directory you \
                       name, returning absolute paths. Show spec.html to the user. Requires \
                       every operation to have a program; spec_status lists what is missing."
    )]
    async fn export_spec(
        &self,
        params: Parameters<ExportSpecParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let resolved = resolved!(self, context);
        let engine = resolved.engine;
        let params = params.0;

        let head = engine.head_snapshot();

        let model = match head.workspace.assemble_model() {
            Ok(model) => model,

            Err(error) => {
                return json_error(serde_json::json!({
                    "exported": false,
                    "error": "the model is not assemblable yet",
                    "assembly_gaps": error
                        .gaps
                        .iter()
                        .map(|gap| gap.to_string())
                        .collect::<Vec<_>>(),
                    "guidance": "Give every operation a program and execution facts — \
                                 spec_status shows the same gaps — then export again.",
                }));
            }
        };

        let dir = PathBuf::from(&params.dir);

        if let Err(error) = std::fs::create_dir_all(&dir) {
            return json_error(serde_json::json!({
                "exported": false,
                "error": format!("cannot create {}: {error}", dir.display()),
            }));
        }

        let dir = dir.canonicalize().unwrap_or(dir);
        let mut artifacts = Vec::new();

        let yaml = match serde_yaml::to_string(&model) {
            Ok(yaml) => yaml,
            Err(error) => {
                return json_error(serde_json::json!({
                    "exported": false,
                    "error": format!("cannot serialize the model: {error}"),
                }));
            }
        };

        if let Some(result) = write_artifact(&dir.join("conseqa.yaml"), &yaml, &mut artifacts) {
            return result;
        }

        let analysis = settled_analysis(&engine, head.revision).await;

        let report = match &analysis {
            AnalysisState::Ready(analysis) => Some(&analysis.obligations),
            _ => None,
        };

        if let Some(report) = report {
            let json = serde_json::to_string_pretty(report)
                .unwrap_or_else(|_| "{}".to_string());

            if let Some(result) = write_artifact(
                &dir.join("verification-report.json"),
                &format!("{json}\n"),
                &mut artifacts,
            ) {
                return result;
            }
        }

        if params.render.unwrap_or(true) {
            let title = params
                .title
                .clone()
                .unwrap_or_else(|| head.workspace.run_meta.run.0.clone());

            match crate::viz::render(&model, report, &title) {
                Ok(html) => {
                    if let Some(result) =
                        write_artifact(&dir.join("spec.html"), &html, &mut artifacts)
                    {
                        return result;
                    }
                }

                Err(error) => {
                    return json_error(serde_json::json!({
                        "exported": false,
                        "error": format!("cannot render the visualization: {error}"),
                        "artifacts": artifacts,
                    }));
                }
            }
        }

        json_result(serde_json::json!({
            "exported": true,
            "revision": head.revision.0,
            "artifacts": artifacts,
            "analysis": analysis_json(&analysis),
            "note": "spec.html is self-contained — open it in a browser or hand it to \
                     the user. conseqa.yaml is the canonical model, consumable by the \
                     `conseqa` checker and `conseqa-viz`.",
        }))
    }
}

/// Waits a bounded moment for the head revision's analysis, so a status
/// call right after a commit reports the verdict rather than
/// "analyzing".
async fn settled_analysis(engine: &ConfluenceEngine, revision: Revision) -> AnalysisState {
    let state = engine.analysis_state(revision);

    if state.is_terminal() {
        return state;
    }

    tokio::time::timeout(Duration::from_secs(15), engine.analysis_ready(revision))
        .await
        .unwrap_or_else(|_| engine.analysis_state(revision))
}

/// The checker's verdict in a shape an authoring agent can act on.
fn analysis_json(state: &AnalysisState) -> serde_json::Value {
    match state {
        AnalysisState::NotAssemblable { gaps } => serde_json::json!({
            "state": "draft",
            "assembly_gaps": gaps.iter().map(|gap| gap.to_string()).collect::<Vec<_>>(),
            "guidance": "The model is still a draft: the listed operations need programs \
                         and execution facts before the validator can run.",
        }),

        AnalysisState::ValidationFailed { errors } => serde_json::json!({
            "state": "invalid",
            "errors": errors,
            "guidance": "Fix the listed structural errors with submit_patch, then check \
                         spec_status again.",
        }),

        AnalysisState::Ready(analysis) => {
            let mut proven = 0usize;
            let mut open = Vec::new();

            for obligation in &analysis.obligations.obligations {
                if obligation.status == crate::analyzer::report::Status::Proven {
                    proven += 1;
                } else {
                    open.push(
                        serde_json::to_value(obligation).unwrap_or_else(|_| {
                            serde_json::Value::String(obligation.id.clone())
                        }),
                    );
                }
            }

            let total = analysis.obligations.obligations.len();

            serde_json::json!({
                "state": "validated",
                "obligations": {
                    "total": total,
                    "proven": proven,
                    "open": open,
                },
                "guidance": if total == 0 {
                    "The model validates and declares no requirements yet; discover and \
                     declare its correctness obligations, or export as is."
                } else if proven == total {
                    "The model validates and every obligation is proven."
                } else {
                    "The model validates; each open obligation's evidence names the \
                     missing fact. requirement_report gives per-operation detail."
                },
            })
        }

        other => serde_json::json!({
            "state": "analyzing",
            "detail": other.label(),
            "guidance": "Analysis is still running; check spec_status again shortly.",
        }),
    }
}

fn obligation_status_label(status: &super::workspace::PromptObligationStatus) -> String {
    use super::workspace::PromptObligationStatus;

    match status {
        PromptObligationStatus::Unmapped => "unmapped".to_string(),
        PromptObligationStatus::Mapped { requirements } => {
            format!("mapped ({} requirements)", requirements.len())
        }
        PromptObligationStatus::UnsupportedByCurrentDsl { reason } => {
            format!("unsupported: {reason}")
        }
        PromptObligationStatus::ExplicitlyWaivedByUser => "waived".to_string(),
    }
}

/// Writes one export artifact, recording its path; on failure returns
/// the error result to hand straight back to the caller.
fn write_artifact(
    path: &std::path::Path,
    contents: &str,
    artifacts: &mut Vec<String>,
) -> Option<Result<CallToolResult, McpError>> {
    match std::fs::write(path, contents) {
        Ok(()) => {
            artifacts.push(path.display().to_string());
            None
        }

        Err(error) => Some(json_error(serde_json::json!({
            "exported": false,
            "error": format!("cannot write {}: {error}", path.display()),
            "artifacts": artifacts,
        }))),
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

        // The worker daemon serves the task contract; interactive
        // backings (the stdio and multi-project servers) serve the
        // authoring loop.
        info.instructions = Some(
            match &self.backing {
                Backing::Single(_) => WORKER_INSTRUCTIONS,
                Backing::Multi { .. } | Backing::Local(_) => INTERACTIVE_INSTRUCTIONS,
            }
            .to_string(),
        );

        info
    }
}

/// Serves a multi-project MCP server over stdio for `manager`, until
/// the client disconnects. Nothing may be written to stdout by the rest
/// of the process — stdout is the MCP channel — so route logs to stderr.
///
/// This is the transport Claude Desktop / Code accepts for a local
/// server without TLS: the app spawns the command and speaks MCP over
/// its stdin/stdout, so there is no `https` URL to satisfy.
pub async fn serve_stdio(
    manager: Arc<WorkspaceManager>,
    launcher: Option<Arc<dyn DesignLauncher>>,
) -> std::io::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};

    let mcp = ConseqaMcp::local(manager, launcher);

    let service = mcp
        .serve(stdio())
        .await
        .map_err(std::io::Error::other)?;

    service.waiting().await.map_err(std::io::Error::other)?;

    Ok(())
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

/// The prose that introduces [`PROGRAM_EXAMPLE_JSON`] in the reference.
const PROGRAM_EXAMPLE_PREAMBLE: &str = "
WORKED EXAMPLE — a valid program (the body of a `replace_operation_program`
patch). An effect intent must be established before it is executed, by the
same binding id: executing an intent no transaction established is the
`unknown effect intent` error the commit gate rejects. A transaction output
is bound once and then consumed by the `return`.
";

/// A complete, valid operation program, appended to `dsl_reference` so an
/// agent has a concrete template for the establish/execute-intent and
/// output-binding wiring it most often gets wrong. Kept as pure JSON and
/// checked by a test (`the_worked_example_is_a_valid_program`), so the
/// reference cannot drift into an invalid shape.
const PROGRAM_EXAMPLE_JSON: &str = r#"{"steps":[
  {"kind":"transaction","id":"tx.example","data_model":null,
   "isolation":"read_committed","idempotency":{"kind":"not_deduplicated"},
   "steps":[
     {"kind":"establish_effect_intent",
      "bind":"intent.example.notify","effect_id":"effect.example.notify",
      "effect":{"kind":"publication","topic":"topic.example",
                "schema":"schema.Event","idempotency_key_propagation":[]},
      "values":{"kind":"deterministic",
                "from":[{"source":"input:input.example.request","path":"id"}]}},
     {"kind":"establish_transaction_output",
      "bind":"output.example","schema":"schema.Result",
      "values":{"kind":"deterministic",
                "from":[{"source":"input:input.example.request","path":"id"}]}}
   ]},
  {"kind":"execute_effect_intent","intent":"intent.example.notify"},
  {"kind":"return","request":"input.example.request",
   "outcome":{"kind":"ok","values":{"kind":"deterministic",
              "from":[{"source":"transaction_output:output.example","path":"id"}]}}}
]}"#;

/// The canonical DSL semantics document, embedded so `dsl_guide` can
/// teach an agent the language from the server itself — instead of the
/// agent reverse-engineering the crate's source, which it cannot see
/// from its own project anyway.
const DSL_SEMANTICS: &str = include_str!("../../CONSEQA_DSL_SEMANTICS.md");

/// The `##` sections of the semantics document: header line and full
/// body, subsections included. Fence-aware, so a `##` inside a code
/// block never starts a section.
fn guide_sections() -> Vec<(&'static str, &'static str)> {
    let doc = DSL_SEMANTICS;

    let mut starts: Vec<usize> = Vec::new();
    let mut in_fence = false;
    let mut offset = 0;

    for line in doc.split_inclusive('\n') {
        let trimmed = line.trim_end();

        if trimmed.starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence && trimmed.starts_with("## ") {
            starts.push(offset);
        }

        offset += line.len();
    }

    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(doc.len());
            let section = &doc[*start..end];
            let header = section.lines().next().unwrap_or_default();

            (header, section)
        })
        .collect()
}

/// The guide's table of contents: every section and subsection header,
/// with the usage hint.
fn guide_toc() -> String {
    let mut toc = String::from(
        "The Conseqa DSL semantics guide. Call dsl_guide again with a topic — a \
         section name or a few words of it — to read that section in full.\n\nSections:\n",
    );

    let mut in_fence = false;

    for line in DSL_SEMANTICS.lines() {
        if line.trim_end().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }

        if in_fence {
            continue;
        }

        if let Some(header) = line.strip_prefix("## ") {
            toc.push_str(&format!("- {header}\n"));
        } else if let Some(header) = line.strip_prefix("### ") {
            toc.push_str(&format!("    - {header}\n"));
        }
    }

    toc
}

/// Sections whose header — or any subsection header within — contains
/// the query, case-insensitively. An unmatched or over-broad query
/// falls back to headers so the caller can narrow.
fn guide_lookup(topic: &str) -> String {
    let query = topic.trim().to_lowercase();

    if query.is_empty() {
        return guide_toc();
    }

    let matched: Vec<(&str, &str)> = guide_sections()
        .into_iter()
        .filter(|(header, body)| {
            header.to_lowercase().contains(&query)
                || body.lines().any(|line| {
                    line.starts_with("### ") && line.to_lowercase().contains(&query)
                })
        })
        .collect();

    if matched.is_empty() {
        return format!("No section matches `{topic}`.\n\n{}", guide_toc());
    }

    let total: usize = matched.iter().map(|(_, body)| body.len()).sum();

    if total > 40_000 {
        let headers: Vec<&str> = matched.iter().map(|(header, _)| *header).collect();

        return format!(
            "`{topic}` matches several sections; narrow to one of:\n{}",
            headers.join("\n")
        );
    }

    matched
        .into_iter()
        .map(|(_, body)| body)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::spec::{
        ExecutionSemantics, Id, Model, Operation, OperationBlock, OperationConcurrency, Revision,
    };

    /// The worked example in `dsl_reference` must be a genuinely valid
    /// program: it parses as an operation program and passes the same
    /// program-local checks the commit gate applies, so the reference can
    /// never teach a shape the gate would reject.
    #[test]
    fn the_worked_example_is_a_valid_program() {
        let program: OperationBlock = serde_json::from_str(super::PROGRAM_EXAMPLE_JSON)
            .expect("the worked example parses as an operation program");

        let operation_id = Id("operation.example".to_string());

        let operation = Operation {
            service: Id("service.example".to_string()),
            description: None,
            inputs: BTreeMap::new(),
            program,
            requirements: Default::default(),
            execution: ExecutionSemantics {
                concurrency: OperationConcurrency::Unspecified,
            },
        };

        let mut operations = BTreeMap::new();
        operations.insert(operation_id.clone(), operation);

        let model = Model {
            revision: Revision(1),
            services: BTreeMap::new(),
            schemas: BTreeMap::new(),
            data_models: BTreeMap::new(),
            topics: BTreeMap::new(),
            state_machines: BTreeMap::new(),
            operations,
        };

        let diagnostics =
            crate::analyzer::validation::program_local_diagnostics(&model, &operation_id);

        assert!(
            diagnostics.is_empty(),
            "the worked example must be internally valid, but the program-local \
             check reported:\n{diagnostics:#?}"
        );
    }

    // The guide serves the embedded semantics document: the table of
    // contents lists every section, a topic returns its full section,
    // and an unmatched topic falls back to the listing — so an agent
    // can always navigate to the semantics it needs without reading
    // crate source.

    #[test]
    fn the_guide_toc_lists_the_semantics_sections() {
        let toc = super::guide_toc();

        for expected in [
            "Interpretation model",
            "Operations",
            "Effect intents",
            "Value references",
            "Operation requirements",
        ] {
            assert!(toc.contains(expected), "toc lacks `{expected}`:\n{toc}");
        }
    }

    #[test]
    fn a_topic_returns_its_full_section() {
        let section = super::guide_lookup("effect intents");

        assert!(
            section.contains("EstablishEffectIntent")
                && section.contains("ExecuteEffectIntent"),
            "the effect-intents section should explain both sides of the wiring:\n{section}"
        );
    }

    #[test]
    fn a_subsection_topic_returns_its_enclosing_section() {
        let section = super::guide_lookup("dispatch routing");

        assert!(
            section.contains("Dispatch routing"),
            "a `###` header match should return its section:\n{section}"
        );
    }

    #[test]
    fn an_unmatched_topic_falls_back_to_the_listing() {
        let fallback = super::guide_lookup("zzz-not-a-topic");

        assert!(
            fallback.contains("No section matches") && fallback.contains("Sections:"),
            "{fallback}"
        );
    }
}
