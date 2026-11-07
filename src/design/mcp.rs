//! The MCP tool surface (§42–§53 of the harness spec).
//!
//! One server, two roles, each served its own brief and only its own
//! tools. The coordinator — the stdio client, the holder of the API
//! key, or an interactive session's token — designs the system: it
//! selects projects, authors through `submit_patch`, builds in process
//! with `build_design`, reads the checker's verdict, and exports. A
//! worker — a scoped task's agent, holding that task's bearer
//! capability — reads its pinned snapshot, commits through the same
//! gate, and files dependency requests. Both read through tracked
//! reads and canonical queries.
//!
//! DSL payloads — symbol keys, graph queries, patches — travel in
//! their canonical serialized JSON forms; `dsl_guide` documents the
//! shapes under its `JSON shapes` topics.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext, wrapper::Parameters},
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
        InitializeRequestParams, InitializeResult, ListToolsResult, PaginatedRequestParams,
        ServerCapabilities, ServerInfo, Tool,
    },
    schemars,
    service::RequestContext,
    tool, tool_router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::spec::{Id, Revision};

use super::analysis::AnalysisState;
use super::change::{
    self,
    side::{Side, SideName, Source},
};
use super::commit::{CommitRejection, CommitRequest};
use super::engine::{DesignEngine, EngineError, OperationReadMode};
use super::graph_query::GraphQuery;
use super::patch::{PatchId, SpecPatch};
use super::read_set::SearchSpec;
use super::symbol::SymbolKey;
use super::task::{TaskId, TaskState};
use super::workspace::EvidenceRef;
use super::workspace_manager::WorkspaceManager;

/// The invariants every worker operates under (§89), served as its
/// brief: one scoped task, one patch, restart on invalidation.
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
Call dsl_guide for the DSL's semantics by topic, and for the JSON \
shapes of symbols, queries, and patches under its `JSON shapes` topics.";

/// The brief served to the coordinator: the authoring loop, stated up
/// front, so an agent asked to design a system with Conseqa knows
/// exactly how to work instead of reverse-engineering the crate.
const COORDINATOR_INSTRUCTIONS: &str = "\
Conseqa architecture authoring. You design a system as a Conseqa model \
— services, schemas, data models, topics, state machines, and \
operations with explicit causal programs — and a deterministic checker \
validates it and proves or refuses its correctness obligations. Model \
state lives in the project's `.conseqa/` directory, committed with its \
code: those files are the source of truth, and what changes them \
outside this server — a hand edit, a checkout, a pull — is picked up \
on your next call. Change the model through submit_patch, which \
validates every change; edit the files directly only when the user \
asks you to.

The authoring loop:
1. open_project selects the model you are building: pass the \
repository's absolute path (or any directory inside it). A server \
started inside a project opens it on its own. create_project starts a \
new one in a directory, normally the repository root.
2. Learn the DSL from this server, not from source code: dsl_guide \
explains the semantics by topic, and its `JSON shapes` topics give the \
exact shapes — symbol keys, graph queries, patch mutations, sketch \
steps, program steps, and a worked program example. Pass every topic \
you need as `topics` in ONE call rather than one call per topic.
3. Author the shared skeleton yourself with submit_patch: services, \
schemas, data models (outboxes included), topics, state machines, and \
one interface per planned operation (its id, service, inputs, and \
request, subscription, or outbox contracts). Give every interface a \
`sketch` — the operation's business actions as typed steps (find, \
update, create, delete, transition, advance, fence, enqueue, publish, \
call, request, when, reject, parallel, answer, race, start; see the \
sketch shapes) — whenever its work fits that vocabulary: a sketched \
program is compiled in code, in milliseconds. Many small typed patches \
are normal. The commit gate rejects a structurally broken patch with \
precise diagnostics; fix it and resubmit in the same session.
4. Once that skeleton is complete for the whole system, call \
build_design, passing the user's request as `prompt` — the project \
records no prompt, so each build is told what to build. It runs the \
whole design in this server, one step after another: it compiles every \
sketch, declares the default runtime topology, discovers requirements, \
verifies, and repairs what its remedy catalogue can prove. It returns \
within the call with the checker's verdict, and your session is moved \
to the new head.
5. When code could not settle something, the build stops at that phase \
with status `handed_back` and lists it under `hand_offs`, each with its \
objective and what the builder established — a sketch that does not \
compile, a program the sketch vocabulary cannot express, an unsure \
requirement, a repair outside the catalogue. Those are yours: resolve \
them with submit_patch, then call build_design again. Repeat until it \
returns status `success`.
6. spec_status is your feedback loop while drafting: assembly gaps, \
validation errors, or the verification verdict with exactly which \
obligations are proven and which are not; requirement_report gives an \
operation's open obligations in full. Fix what they name narrowly, \
yourself, and build again. Before you finish, diff_spec shows what your \
work changed relative to a commit (`merge_base` for the branch it \
began from): every obligation it regressed, removed, or rebased, and \
the declarations that explain each.

An unproven transaction serializability or ordering obligation \
carries `remedy: application`, always: the fix is in the L0 model. \
Those two families are proven only from transaction primitives — \
declared isolation, shared and exclusive locks, atomic guarded \
mutations (compare_and_set, transitions and cursor advances comparing \
an observed version), ordered cursors, fences — over the \
model-wide conflict closure of the transaction, never from runtime \
topology, so no L1 edit can discharge one. No obligation currently \
carries `remedy: runtime`, and the replay families carry no remedy at \
all. The L1 runtime topology — transport grouping and ordering, \
subscription delivery and dispatch, outbox partitioning, ordering, \
and dispatch, execution pools and their member concurrency, request \
routers, storage layouts — describes placement, transport, grouping, \
precedence, and capacity; it provides no serializability or ordering \
guarantee, and only the replay families consume its delivery facts. \
build_design declares its weakest defaults once the programs exist; \
author a runtime declaration yourself only where the prompt states \
delivery, concurrency, ordering, or batching. Never \
invent topology to make a proof pass; leaving a requirement unproven \
is a legitimate outcome.
7. export_spec delivers the result: the canonical YAML, the \
verification report, and a self-contained interactive HTML \
visualization, written to a directory you choose — show these to the \
user.

Write an operation program yourself only when its work does not fit \
the sketch vocabulary, or when build_design hands it back. To have \
coding agents build the design instead, the user runs `conseqa design` \
in the repository. Invariants: requirements are obligations, not \
guarantees; never weaken or remove a requirement to make verification \
pass; prefer unknown/unspecified over inventing a guarantee the design \
does not support. Correctness verdicts come only from the checker.";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadSymbolParams {
    /// The symbol key in its canonical serialized form, e.g.
    /// {"kind":"schema","value":"schema.Order"} or
    /// {"kind":"transaction","value":{"operation":"operation.checkout",
    /// "transaction":"tx.charge"}}. See dsl_guide's `JSON shapes`.
    pub symbol: serde_json::Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchSymbolsParams {
    /// Restrict to one symbol kind: service, schema, data_model,
    /// data_object, outbox, topic, state_machine, transition, operation,
    /// operation_interface, operation_program, operation_requirements,
    /// topic_runtime, subscription_runtime, outbox_runtime,
    /// execution_pool, router, storage_layout, input, transaction,
    /// effect_site, binding, requirement, operation_summary,
    /// prompt_obligation.
    #[serde(default)]
    pub kind: Option<String>,

    /// Substring matched against the symbol's display form.
    #[serde(default)]
    pub prefix: Option<String>,

    /// Restrict operations to one service id.
    #[serde(default)]
    pub service: Option<String>,

    /// Restrict prompt obligations to those targeting one operation
    /// id. Prefer it to listing every obligation: mapping another
    /// operation's obligation then cannot invalidate this session.
    #[serde(default)]
    pub targets: Option<String>,
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
    /// See dsl_guide's `JSON shapes`.
    pub query: serde_json::Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RequirementReportParams {
    /// Restrict to one operation id.
    #[serde(default)]
    pub operation: Option<String>,

    /// Restrict to one requirement family: transaction_serializability,
    /// transaction_ordering, idempotency, result_replay, recoverability.
    /// Any other value is an error, never an empty report.
    #[serde(default)]
    pub family: Option<String>,

    /// Also list the proven obligations, each with its whole proof.
    /// Off by default: they are counted, and a repair reads only what
    /// is open.
    #[serde(default)]
    pub include_proven: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DiffSpecParams {
    /// The base: a git commit expression (a branch, a tag, `HEAD~2`, a
    /// hash). `HEAD` when omitted.
    #[serde(default)]
    pub base: Option<String>,

    /// Compare against the merge base of `base` and the project's
    /// current files, which count as `HEAD`: what this work changed,
    /// not what the base branch changed since it began.
    #[serde(default)]
    pub merge_base: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SubmitPatchParams {
    /// The DSL contract version this patch is authored against.
    /// Optional: omitted means the server's current version, since an
    /// agent learns the DSL from this server's live guide. Present
    /// and mismatched, the patch is refused by name before shape
    /// validation.
    #[serde(default)]
    pub dsl: Option<u64>,

    /// The typed patch: {"mutations":[...]}. See dsl_guide's `JSON
    /// shapes` for mutation shapes.
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
pub struct BuildDesignParams {
    /// What to design, in the user's words. Pass it on every build: the
    /// project records no prompt — the correctness obligations
    /// discovery extracts from it are what last — and discovery judges
    /// each explicit obligation against it.
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OpenProjectParams {
    /// The project's directory, or any directory inside it, as an
    /// absolute path. Omit to reopen the active project at its current
    /// head — or, with none active, the project this server was started
    /// in.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateProjectParams {
    /// The directory to create the project in, as an absolute path —
    /// normally the repository root. Its state goes in a `.conseqa/`
    /// directory there, to be committed with the code.
    pub path: String,

    /// A short name for the project; the directory's name when omitted.
    #[serde(default)]
    pub name: Option<String>,
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

/// Builds one project's design in process, one task at a time and
/// with no agent sessions. Implemented by the harness (which depends on
/// `design`) and injected here, so the MCP server can build without
/// `design` depending on `harness`.
pub trait DesignBuilder: Send + Sync {
    /// Starts a build of `engine`'s project for `prompt`, returning
    /// what it did once it is done, or why it could not start (another
    /// build is running).
    fn build(
        &self,
        engine: DesignEngine,
        prompt: Option<String>,
    ) -> Result<futures::future::BoxFuture<'static, DesignBuild>, String>;
}

/// What one build did.
#[derive(Debug, Clone)]
pub struct DesignBuild {
    pub started_at_revision: Revision,
    pub elapsed: Duration,
    pub outcome: BuildOutcome,
}

/// How a build ended.
#[derive(Debug, Clone)]
pub enum BuildOutcome {
    /// Every phase ran, every adopted obligation is proven, and every
    /// explicit prompt obligation is mapped.
    Success,

    /// Every phase ran, and what is named is left.
    Incomplete {
        reason: String,
        unresolved: Vec<String>,
    },

    /// The build stopped at a phase boundary to hand what code could
    /// not settle back to its caller; later phases have not run.
    HandedBack { hand_offs: Vec<serde_json::Value> },

    /// The workflow itself failed.
    Failed { error: String },
}

impl BuildOutcome {
    fn label(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Incomplete { .. } => "incomplete",
            Self::HandedBack { .. } => "handed_back",
            Self::Failed { .. } => "failed",
        }
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
    engine: DesignEngine,
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

    /// The directory the server runs in, when its project is the one a
    /// client means without naming one: a stdio server started inside a
    /// repository.
    home: Option<PathBuf>,
}

impl ProjectHost {
    fn new(manager: Arc<WorkspaceManager>, home: Option<PathBuf>) -> Self {
        Self {
            manager,
            active: Arc::new(parking_lot::RwLock::new(None)),
            home,
        }
    }
}

/// How the MCP server resolves credentials and finds engines.
#[derive(Clone)]
enum Backing {
    /// One fixed engine; the bearer is a per-task capability (§43). The
    /// embedded and single-project daemon paths.
    Single(DesignEngine),

    /// Many projects over HTTP behind a stable API key; the client
    /// selects the active project through the project tools.
    Multi { host: ProjectHost, api_key: String },

    /// Many projects over stdio: the single local client is inherently
    /// trusted (no bearer to send), so no API key is required. Used by
    /// the `stdio` command for the Claude Desktop / Code UI, which
    /// accepts a command-based (stdio) MCP server without TLS.
    Local(ProjectHost),
}

/// What one authenticated architecture request resolves to.
struct Resolved {
    engine: DesignEngine,
    task: TaskId,
}

/// Why a request could not be resolved to an authorized task.
enum ResolveError {
    /// Malformed transport or missing/incorrect credential.
    Unauthorized(String),

    /// Authenticated, but no project is open on this connection.
    NoProject,

    /// The project cannot be read: its files no longer load, or
    /// another process holds it.
    Project(String),
}

/// Who is calling, which decides the brief and the tools served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// The author driving the design: the stdio client, the holder of
    /// the API key, or an interactive session's token.
    Coordinator,

    /// A scoped task's agent, holding that task's capability.
    Worker,
}

/// Each role's tools, built once: a router generates every tool's
/// schema, and rebuilding one per list or call is waste.
struct Routers {
    worker: ToolRouter<ConseqaMcp>,

    /// The coordinator of a multi-project server, which selects its
    /// projects.
    coordinator: ToolRouter<ConseqaMcp>,

    /// The coordinator of a single-project server: an interactive
    /// session, with no project to select.
    session: ToolRouter<ConseqaMcp>,

    /// Every tool, for looking one up by name.
    all: ToolRouter<ConseqaMcp>,
}

static ROUTERS: LazyLock<Routers> = LazyLock::new(|| Routers {
    worker: ConseqaMcp::shared_tools() + ConseqaMcp::worker_tools(),
    coordinator: ConseqaMcp::shared_tools()
        + ConseqaMcp::coordinator_tools()
        + ConseqaMcp::project_tools(),
    session: ConseqaMcp::shared_tools() + ConseqaMcp::coordinator_tools(),
    all: ConseqaMcp::shared_tools()
        + ConseqaMcp::worker_tools()
        + ConseqaMcp::coordinator_tools()
        + ConseqaMcp::project_tools(),
});

#[derive(Clone)]
pub struct ConseqaMcp {
    backing: Backing,
    builder: Option<Arc<dyn DesignBuilder>>,
}

impl ConseqaMcp {
    pub fn new(engine: DesignEngine) -> Self {
        Self {
            backing: Backing::Single(engine),
            builder: None,
        }
    }

    /// A single-project server whose sessions can also build the
    /// design.
    pub fn with_builder(engine: DesignEngine, builder: Arc<dyn DesignBuilder>) -> Self {
        Self {
            backing: Backing::Single(engine),
            builder: Some(builder),
        }
    }

    /// A multi-project HTTP server: a stable API key authenticates
    /// clients, which select projects through the project tools.
    pub fn multi(
        manager: Arc<WorkspaceManager>,
        api_key: impl Into<String>,
        builder: Option<Arc<dyn DesignBuilder>>,
    ) -> Self {
        Self {
            backing: Backing::Multi {
                host: ProjectHost::new(manager, None),
                api_key: api_key.into(),
            },
            builder,
        }
    }

    /// A multi-project stdio server: no API key, since the single local
    /// client is inherently trusted. For the Claude Desktop / Code UI.
    /// `home` is the directory it runs in: the project there, if any,
    /// opens on the first call that needs one.
    pub fn local(
        manager: Arc<WorkspaceManager>,
        builder: Option<Arc<dyn DesignBuilder>>,
        home: Option<PathBuf>,
    ) -> Self {
        Self {
            backing: Backing::Local(ProjectHost::new(manager, home)),
            builder,
        }
    }

    /// The role of a request's caller. A single-project server serves
    /// its workers and its interactive sessions alike, so there the
    /// bearer decides; every other server's caller is its coordinator.
    fn role(&self, context: &RequestContext<RoleServer>) -> Role {
        match &self.backing {
            Backing::Single(engine) => match Self::bearer(context)
                .ok()
                .and_then(|bearer| engine.resolve_token(&bearer))
            {
                Some(task) if engine.is_session(task) => Role::Coordinator,
                _ => Role::Worker,
            },

            Backing::Multi { .. } | Backing::Local(_) => Role::Coordinator,
        }
    }

    /// The tools `role` is served on this server.
    fn tools(&self, role: Role) -> &'static ToolRouter<Self> {
        match (role, &self.backing) {
            (Role::Worker, _) => &ROUTERS.worker,
            (Role::Coordinator, Backing::Single(_)) => &ROUTERS.session,
            (Role::Coordinator, _) => &ROUTERS.coordinator,
        }
    }

    /// The server's description for `role`. Both briefs lead with the
    /// DSL contract version, so an agent learns which contract it
    /// authors against before it drafts anything; `Implementation`
    /// stays the build version — the axes are distinct.
    fn info(&self, role: Role) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());

        // This build, honestly: the default is rmcp's own build env,
        // which reports the library.
        let mut implementation = rmcp::model::Implementation::default();
        implementation.name = env!("CARGO_PKG_NAME").to_string();
        implementation.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info = implementation;

        info.instructions = Some(format!(
            "This server speaks Conseqa DSL contract version {}; author against it, \
             and learn it through dsl_guide.\n\n{}",
            crate::spec::DSL_VERSION,
            match role {
                Role::Worker => WORKER_INSTRUCTIONS,
                Role::Coordinator => COORDINATOR_INSTRUCTIONS,
            }
        ));

        info
    }

    /// The bearer credential of a request.
    fn bearer(context: &RequestContext<RoleServer>) -> Result<String, ResolveError> {
        let parts = context
            .extensions
            .get::<http::request::Parts>()
            .ok_or_else(|| {
                ResolveError::Unauthorized(
                    "the transport did not carry HTTP request parts".to_string(),
                )
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

                sync(engine)?;

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
                if &Self::bearer(context)? != api_key {
                    return Err(ResolveError::Unauthorized(
                        "the API key is not recognized".to_string(),
                    ));
                }

                host.active_resolved()
            }

            // stdio: the local client is trusted; no bearer to check.
            Backing::Local(host) => host.active_resolved(),
        }
    }

    /// The project host for the project-selection tools, authenticated
    /// as appropriate for the backing.
    fn project_host(&self, context: &RequestContext<RoleServer>) -> Result<ProjectHost, McpError> {
        match &self.backing {
            Backing::Multi { host, api_key } => {
                let bearer = Self::bearer(context).map_err(resolve_to_mcp_error)?;

                if &bearer != api_key {
                    return Err(McpError::invalid_request(
                        "the API key is not recognized",
                        None,
                    ));
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
    /// Resolves the active project to `(engine, task)`, first opening
    /// the server's own project when none is active, and adopting any
    /// change made to the files outside the server.
    fn active_resolved(&self) -> Result<Resolved, ResolveError> {
        if self.active.read().is_none()
            && let Some(home) = &self.home
            && super::store::ProjectStore::discover(home).is_some()
        {
            let engine = self
                .manager
                .open(home)
                .map_err(|error| ResolveError::Project(error.to_string()))?;

            self.activate(engine).map_err(ResolveError::Project)?;
        }

        let session = self.active.read().clone().ok_or(ResolveError::NoProject)?;

        // Before the token resolves: adopting a change moves the
        // session's token to a successor at the new head.
        sync(&session.engine)?;

        let task = session
            .engine
            .resolve_token(&session.token)
            .ok_or_else(|| {
                ResolveError::Unauthorized(
                    "the project session has ended; open a project again".to_string(),
                )
            })?;

        Ok(Resolved {
            engine: session.engine,
            task,
        })
    }

    /// Makes `engine`'s project the active one, under a fresh session
    /// at its current head.
    fn activate(
        &self,
        engine: DesignEngine,
    ) -> Result<super::workspace_manager::ProjectInfo, String> {
        engine.sync_with_disk().map_err(|error| error.to_string())?;

        let path = engine.project_dir().unwrap_or_default();

        let handle = engine
            .create_session(
                super::task::WriteScope::of([super::task::WriteGrant::All]),
                format!("interactive session for the project at {}", path.display()),
            )
            .map_err(|error| error.to_string())?;

        *self.active.write() = Some(ProjectSession {
            engine: engine.clone(),
            token: handle.token.0,
        });

        Ok(super::workspace_manager::ProjectInfo::of(path, &engine))
    }

    /// The project `open_project` means: the one at `path`, else the
    /// active one, else the server's own.
    fn project_at(&self, path: Option<&str>) -> Result<DesignEngine, String> {
        let opened = match (path, self.active.read().clone(), &self.home) {
            (Some(path), _, _) => self.manager.open(Path::new(path)),
            (None, Some(session), _) => return Ok(session.engine),
            (None, None, Some(home)) => self.manager.open(home),
            (None, None, None) => {
                return Err("no project is active and none was named; pass the \
                            repository's absolute path"
                    .to_string());
            }
        };

        opened.map_err(|error| error.to_string())
    }
}

/// Adopts what changed a project's files outside the server, before a
/// request reads or writes it.
fn sync(engine: &DesignEngine) -> Result<(), ResolveError> {
    engine
        .sync_with_disk()
        .map(|_| ())
        .map_err(|error| ResolveError::Project(error.to_string()))
}

/// Converts an internal resolve error into the tool-facing MCP error.
fn resolve_to_mcp_error(error: ResolveError) -> McpError {
    match error {
        ResolveError::Unauthorized(message) => McpError::invalid_request(message, None),
        ResolveError::NoProject => McpError::invalid_request(
            "no project is open on this connection; call open_project or create_project first",
            None,
        ),
        ResolveError::Project(message) => McpError::invalid_request(message, None),
    }
}

/// The tool-result form of a "no project open" state, so architecture
/// tools guide the agent instead of failing at the protocol level.
fn no_project_result() -> Result<CallToolResult, McpError> {
    json_error(serde_json::json!({
        "error": "no project is open on this connection",
        "guidance": "Call open_project with the repository's absolute path to select its \
                     project (the `.conseqa/` directory), or create_project to start one \
                     there. Then use the architecture tools.",
    }))
}

/// The tool-result form of a project that cannot be read.
fn project_error_result(message: String) -> Result<CallToolResult, McpError> {
    json_error(serde_json::json!({
        "error": message,
        "guidance": "Nothing is read from or committed to the project until this is \
                     resolved. A file under `.conseqa/` that no longer loads is never \
                     written over: fix or revert the file named. A project open in \
                     another conseqa process is that process's until it exits. Then call \
                     again.",
    }))
}

/// A tool's answer, as compact JSON: every byte of it is read into the
/// caller's context, and indentation is most of a deep program's size.
fn json_result(value: serde_json::Value) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string(&value)
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;

    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

fn json_error(value: serde_json::Value) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string(&value)
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;

    Ok(CallToolResult::error(vec![ContentBlock::text(text)]))
}

/// Engine failures surface as tool errors the model can read; they
/// carry their own do-not-retry guidance where relevant.
fn engine_error(error: EngineError) -> Result<CallToolResult, McpError> {
    match error {
        // Structured, so a caller can correct itself from the accepted
        // values without parsing prose.
        EngineError::UnknownRequirementFamily { family, accepted } => {
            json_error(serde_json::json!({
                "error": "unknown_requirement_family",
                "family": family,
                "accepted": accepted,
            }))
        }

        other => json_error(serde_json::json!({ "error": other.to_string() })),
    }
}

/// Parses one tool argument, returning an in-band tool-result error on
/// failure rather than a JSON-RPC protocol error. A protocol error is
/// what an MCP client surfaces as a "transport" failure — opaque to the
/// model — so a malformed argument must come back as a readable tool
/// result the agent can correct, pointing at the guide's shapes.
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
                "Pass `{what}` as a JSON object, not a JSON string. Call dsl_guide with \
                 the topic `JSON shapes` for the exact shapes, then try again."
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
            Err(ResolveError::Project(message)) => return project_error_result(message),
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

    /// Several topics at once, answered in one response in the order
    /// given. Prefer this to one call per section.
    #[serde(default)]
    pub topics: Vec<String>,
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

/// The tools of both roles: tracked reads, canonical queries, the one
/// mutation tool, and the guide.
#[tool_router(router = shared_tools)]
impl ConseqaMcp {
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
            targets: params.targets.map(|operation| id_from(&operation)),
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

        let mode: OperationReadMode = parse_arg!(serde_json::Value::String(params.mode), "mode");

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
        description = "The checker's open obligations: each unproven declared requirement \
                       with its structured obstacles and the evidence naming the missing \
                       fact, for one operation or family or the whole model. The central \
                       input for repair. Proven obligations are counted, not listed, unless \
                       include_proven asks for their proofs."
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
            params.include_proven,
        ) {
            Ok(report) => json_result(report),
            Err(error) => engine_error(error),
        }
    }

    #[tool(
        description = "What the project's current files change relative to a base commit \
                       (default HEAD; merge_base for what this work changed since it \
                       branched): the declarations added, removed, and modified; every \
                       obligation that changed, matched by key and classed regressed, \
                       removed, rebased, improved, unproven, added, or held, with the changed \
                       declarations explaining it; and the facts the proofs newly rely on or \
                       no longer rely on. A short summary, then the diff document (JSON). \
                       Read-only."
    )]
    async fn diff_spec(
        &self,
        params: Parameters<DiffSpecParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let resolved = resolved!(self, context);
        let engine = resolved.engine;
        let params = params.0;

        let Some(project) = engine.project_dir() else {
            return json_error(serde_json::json!({
                "error": "the model lives in no project directory, so it has no history to \
                          diff against",
            }));
        };

        // The head is the engine's head, which the files were adopted
        // into before this call: the project's current files, read
        // whole rather than while another call may be writing them.
        let head = Side {
            name: SideName::Head,
            source: Source::WorkingTree,
            commit: None,
            merge_base: false,
            workspace: Some(engine.head_snapshot().workspace.as_ref().clone()),
        };

        let base = params.base.as_deref().unwrap_or("HEAD");

        let base = if params.merge_base {
            change::side::read_merge_base(&project, base, &Source::WorkingTree)
        } else {
            change::side::read(&project, SideName::Base, &Source::Commit(base.to_string()))
        };

        let base = match base {
            Ok(base) => base,
            Err(error) => {
                return json_error(serde_json::json!({ "error": error.to_string() }));
            }
        };

        let document = change::diff::diff(&base, &head);

        let json = serde_json::to_string(&document)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;

        Ok(CallToolResult::success(vec![
            ContentBlock::text(change::render::summary(&document)),
            ContentBlock::text(json),
        ]))
    }

    #[tool(
        description = "Submit your typed patch through the serializable commit gate. The \
                       gate revalidates everything you observed and rejects with precise \
                       diagnostics; the rejection's guidance says whether to fix it here."
    )]
    async fn submit_patch(
        &self,
        params: Parameters<SubmitPatchParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let resolved = resolved!(self, context);
        let (engine, task) = (resolved.engine, resolved.task);
        let params = params.0;

        // A stale authored claim meets the server here, not at the
        // connection: refuse a declared version mismatch by name,
        // before shape validation turns it into serde noise.
        if let Some(declared) = params.dsl
            && !crate::spec::DSL_READS
                .iter()
                .any(|readable| readable.0 == declared)
        {
            return json_error(serde_json::json!({
                "committed": false,
                "error": "dsl_version_mismatch",
                "declared": declared,
                "server": crate::spec::DSL_VERSION,
                "guidance": format!(
                    "This patch declares dsl {declared}, but this server speaks dsl {}; \
                     the contract changed and patches are not migrated automatically. \
                     Re-learn the DSL with dsl_guide, then re-author the patch.",
                    crate::spec::DSL_VERSION
                ),
            }));
        }

        let patch: SpecPatch = parse_arg!(params.patch, "patch");

        if patch.mutations.is_empty() {
            return json_error(serde_json::json!({
                "committed": false,
                "error": "empty patch",
                "guidance": "A patch must contain at least one mutation. Call dsl_guide \
                             with the topic `JSON shapes` for mutation shapes and submit a \
                             non-empty patch, or finish another way if there is nothing to \
                             change.",
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
                let guidance = match (rejection.is_stale_context(), self.role(&context)) {
                    (true, Role::Worker) => {
                        "Your context is stale. Do not try to fix this in this session; \
                         the harness will restart the task against a fresh snapshot."
                    }
                    (true, Role::Coordinator) => {
                        "Your session's snapshot is behind the head. Reopen the project \
                         (open_project with no path) to move it there, re-read what the \
                         rejection names, and submit again with a fresh nonce."
                    }
                    (false, _) => {
                        "This is fixable in this session: adjust the patch (or read the \
                         missing symbols) and submit again with a fresh nonce."
                    }
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
        description = "The Conseqa DSL, by topic: what services, schemas, data models, \
                       topics, state machines, operations, programs, transactions, effects, \
                       value references, and requirements mean and how they compose — and, \
                       under the `JSON shapes` topics, the exact shapes these tools exchange: \
                       symbol keys, graph queries, patch mutations, sketch steps, program \
                       steps, a worked program example. Call with no topic for the table of \
                       contents, then pass every section you need as topics in one call (a \
                       section name or a few words of it). Use this instead of reading \
                       Conseqa's source code."
    )]
    async fn dsl_guide(
        &self,
        params: Parameters<DslGuideParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;

        let mut topics: Vec<&str> = params.topic.iter().map(String::as_str).collect();
        topics.extend(params.topics.iter().map(String::as_str));

        let text = match topics.as_slice() {
            [] => guide_toc(),
            [topic] => guide_lookup(topic),
            topics => topics
                .iter()
                .map(|topic| guide_lookup(topic))
                .collect::<Vec<_>>()
                .join("\n\n---\n\n"),
        };

        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}

/// A worker's own tools: its task, and asks outside its scope.
#[tool_router(router = worker_tools)]
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
                       cancelled. A task that is invalidated must stop — the harness \
                       restarts it."
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
}

/// The coordinator's tools: where the model stands, the in-process
/// build, and delivery.
#[tool_router(router = coordinator_tools)]
impl ConseqaMcp {
    #[tool(
        description = "Where the active model stands right now: revision, an inventory of \
                       its symbols, which operations still lack programs, whether the \
                       skeleton is ready to build, and the checker's verdict — assembly gaps \
                       while drafting, validation errors, or the verification result with \
                       every open obligation. Call it after committing to steer your next \
                       step."
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
            .filter(|(_, draft)| draft.program.is_none())
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

        let analysis = analysis_json(&settled_analysis(&engine, head.revision).await);

        let skeleton_gaps = super::commit::skeleton_diagnostics(workspace);

        json_result(serde_json::json!({
            "dsl": crate::spec::DSL_VERSION,
            "revision": head.revision.0,
            "skeleton": {
                "ready": skeleton_gaps.is_empty(),
                "gaps": skeleton_gaps,
            },
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
        }))
    }

    #[tool(
        description = "Build the design in this server, in one call: compiles every \
                       sketched operation still missing a program, declares the default \
                       runtime topology, discovers requirements, verifies, and repairs what \
                       the remedy catalogue can prove, one step after another. Pass the \
                       user's request as `prompt`. Status success: done. Incomplete: every \
                       phase ran, and `unresolved` names what is left. Handed_back: the \
                       build stopped at the first phase whose work code could not settle, \
                       listed under `hand_offs` with what each builder established — resolve \
                       those with submit_patch and build again. Every result carries the \
                       checker's verdict as `analysis`, and your session is moved to the new \
                       head. Author the whole skeleton first, with a sketch on every \
                       interface."
    )]
    async fn build_design(
        &self,
        params: Parameters<BuildDesignParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let resolved = resolved!(self, context);
        let (engine, session) = (resolved.engine, resolved.task);

        let Some(builder) = &self.builder else {
            return json_error(serde_json::json!({
                "error": "this server has no design builder",
                "guidance": "This server embeds the design MCP surface without a builder; \
                             every `conseqa-design-mcp` command (serve, global, stdio) has \
                             one.",
            }));
        };

        let head = engine.head_snapshot();

        if head.workspace.operations.is_empty() {
            return json_error(serde_json::json!({
                "error": "no operation interfaces are declared",
                "guidance": "Author the skeleton first — services, schemas, data models, \
                             topics, machines, and one sketched interface per operation — \
                             then call build_design.",
            }));
        }

        let gaps = super::commit::skeleton_diagnostics(&head.workspace);

        if !gaps.is_empty() {
            return json_error(serde_json::json!({
                "error": "the skeleton has unresolved references",
                "skeleton_gaps": gaps,
                "guidance": "Resolve each gap with submit_patch, then call build_design \
                             again.",
            }));
        }

        let build = match builder.build(engine.clone(), params.0.prompt) {
            Ok(build) => build.await,
            Err(error) => return json_error(serde_json::json!({ "error": error })),
        };

        // The build moved the head; without moving the session there
        // the caller's next patch would be refused as stale.
        let refreshed = engine.refresh_session(session);

        let revision = engine.head_revision();

        let mut body = serde_json::json!({
            "status": build.outcome.label(),
            "revision": revision.0,
            "started_at_revision": build.started_at_revision.0,
            "elapsed_ms": u64::try_from(build.elapsed.as_millis()).unwrap_or(u64::MAX),
        });

        let failed = matches!(build.outcome, BuildOutcome::Failed { .. });

        let guidance = match build.outcome {
            BuildOutcome::Success => {
                "Every phase ran and every obligation the run adopted is proven. \
                 export_spec delivers the design."
            }

            BuildOutcome::Incomplete { reason, unresolved } => {
                body["reason"] = serde_json::json!(reason);
                body["unresolved"] = serde_json::json!(unresolved);

                "Every phase ran; `unresolved` and `analysis` name what is left. Fix what \
                 they name with submit_patch and build again — or leave an obligation the \
                 design cannot support unproven, which is a legitimate outcome."
            }

            BuildOutcome::HandedBack { hand_offs } => {
                body["hand_offs"] = serde_json::json!(hand_offs);

                "The build stopped at the first phase that handed work back, so later \
                 phases have not run. Resolve each hand-off with submit_patch — fix a \
                 sketch that does not compile, write a program the vocabulary cannot \
                 express, declare or decline an unsure requirement, repair an unproven \
                 transaction — then call build_design again."
            }

            BuildOutcome::Failed { error } => {
                body["error"] = serde_json::json!(error);

                "The build aborted. The model keeps every commit it made; call \
                 build_design again to resume from the current head."
            }
        };

        body["analysis"] = analysis_json(&settled_analysis(&engine, revision).await);
        body["guidance"] = serde_json::json!(guidance);

        if let Err(error) = refreshed {
            body["session"] = serde_json::json!(format!(
                "your session could not be moved to the head ({error}); call open_project \
                 before patching"
            ));
        }

        if failed {
            json_error(body)
        } else {
            json_result(body)
        }
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
                    "guidance": "Give every operation a program — \
                                 spec_status shows the same gaps — then export again.",
                }));
            }
        };

        let analysis = settled_analysis(&engine, head.revision).await;

        let report = match &analysis {
            AnalysisState::Ready(analysis) => Some(&analysis.obligations),
            _ => None,
        };

        let title = params.render.unwrap_or(true).then(|| {
            params
                .title
                .clone()
                .unwrap_or_else(|| head.workspace.run_meta.run.0.clone())
        });

        match super::export::write_artifacts(
            Path::new(&params.dir),
            &model,
            report,
            title.as_deref(),
        ) {
            Ok(artifacts) => json_result(serde_json::json!({
                "exported": true,
                "revision": head.revision.0,
                "artifacts": artifacts,
                "analysis": analysis_json(&analysis),
                "note": "spec.html is self-contained — open it in a browser or hand it to \
                         the user. conseqa.yaml is the canonical model; `conseqa check` \
                         verifies the project from the command line. The report is written only once the analysis has \
                         settled.",
            })),

            Err(error) => json_error(serde_json::json!({
                "exported": false,
                "error": error.message,
                "artifacts": error.written,
            })),
        }
    }
}

/// Project selection, on a server that hosts several.
#[tool_router(router = project_tools)]
impl ConseqaMcp {
    #[tool(
        description = "Select a project as the active one; every subsequent architecture \
                       tool call operates on it. A project is the `.conseqa/` directory of a \
                       repository, committed with its code: pass the repository's absolute \
                       path, or any directory inside it. Omit the path to reopen the active \
                       project at its current head or, with none active, the project this \
                       server was started in."
    )]
    async fn open_project(
        &self,
        params: Parameters<OpenProjectParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let host = self.project_host(&context)?;

        let opened = host
            .project_at(params.0.path.as_deref())
            .and_then(|engine| host.activate(engine));

        match opened {
            Ok(info) => json_result(serde_json::json!({
                "project": info.name,
                "path": info.path,
                "opened": true,
                "revision": info.revision.0,
                "note": "This is now the active project. See where it stands with \
                         spec_status, learn the DSL with dsl_guide, author the skeleton \
                         with submit_patch — a sketch on every interface — then call \
                         build_design, and export_spec to deliver. Every accepted patch \
                         is written to the project's `.conseqa/` files; remind the user \
                         to commit them.",
            })),

            Err(error) => json_error(serde_json::json!({
                "opened": false,
                "error": error,
                "guidance": "Pass the absolute path of a repository holding a \
                             `.conseqa/project.yaml`, or call create_project to start one.",
            })),
        }
    }

    #[tool(
        description = "Create a new project in a directory — normally the repository \
                       root — and make it active. Its state is a `.conseqa/` directory \
                       there, to be committed with the code. Errors if the directory, or \
                       one above it, already holds a project. What to design is not part of \
                       the project: pass the user's request as `prompt` to build_design."
    )]
    async fn create_project(
        &self,
        params: Parameters<CreateProjectParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let host = self.project_host(&context)?;
        let params = params.0;

        let created = host
            .manager
            .create(Path::new(&params.path), params.name)
            .map_err(|error| error.to_string())
            .and_then(|engine| host.activate(engine));

        match created {
            Ok(info) => json_result(serde_json::json!({
                "project": info.name,
                "path": info.path,
                "created": true,
                "note": "New project created and now active; its files are in `.conseqa/` \
                         under the path above, and the user should commit them. Learn the \
                         DSL with dsl_guide, then author the skeleton with submit_patch — \
                         services, schemas, data models, topics, machines, and one \
                         sketched interface per planned operation — and call \
                         build_design, which compiles, verifies and repairs in one call \
                         and hands back only what code cannot settle. Export with \
                         export_spec.",
            })),

            Err(error) => json_error(serde_json::json!({
                "created": false,
                "error": error,
                "guidance": "Use open_project for a directory that already holds a project.",
            })),
        }
    }
}

/// Waits a bounded moment for the head revision's analysis, so a status
/// call right after a commit reports the verdict rather than
/// "analyzing".
async fn settled_analysis(engine: &DesignEngine, revision: Revision) -> AnalysisState {
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
                         before the validator can run.",
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
                        serde_json::to_value(obligation)
                            .unwrap_or_else(|_| serde_json::Value::String(obligation.id.clone())),
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
fn rejection_body(rejection: &CommitRejection) -> serde_json::Value {
    serde_json::to_value(rejection)
        .unwrap_or_else(|_| serde_json::json!({ "kind": "unserializable_rejection" }))
}

impl ServerHandler for ConseqaMcp {
    /// The description of the server's usual caller: workers on a
    /// single-project server, its coordinator on every other.
    fn get_info(&self) -> ServerInfo {
        self.info(match &self.backing {
            Backing::Single(_) => Role::Worker,
            Backing::Multi { .. } | Backing::Local(_) => Role::Coordinator,
        })
    }

    /// The handshake, answered with the caller's own brief: rmcp's asks
    /// `get_info`, which cannot see who is calling, so it runs here over
    /// the caller's description instead.
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        let brief = Brief(self.info(self.role(&context)));

        brief.initialize(request, context).await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: self.tools(self.role(&context)).list_all(),
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let tools = self.tools(self.role(&context));

        tools
            .call(ToolCallContext::new(self, request, context))
            .await
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        ROUTERS.all.get(name).cloned()
    }
}

/// A server that is only a description, so rmcp's own handshake can
/// run over a caller's brief.
struct Brief(ServerInfo);

impl ServerHandler for Brief {
    fn get_info(&self) -> ServerInfo {
        self.0.clone()
    }
}

/// Serves a multi-project MCP server over stdio for `manager`, until
/// the client disconnects. Nothing may be written to stdout by the rest
/// of the process — stdout is the MCP channel — so route logs to stderr.
///
/// This is the transport Claude Desktop / Code accepts for a local
/// server without TLS: the app spawns the command and speaks MCP over
/// its stdin/stdout, so there is no `https` URL to satisfy. `home` is
/// the directory the app started it in; the project there, if any, is
/// the one a client means without naming one.
pub async fn serve_stdio(
    manager: Arc<WorkspaceManager>,
    builder: Option<Arc<dyn DesignBuilder>>,
    home: Option<PathBuf>,
) -> std::io::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};

    let mcp = ConseqaMcp::local(manager, builder, home);

    let service = mcp.serve(stdio()).await.map_err(std::io::Error::other)?;

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
pub fn router(engine: DesignEngine) -> axum::Router {
    build_router(ConseqaMcp::new(engine))
}

/// The `/mcp` router with a design builder wired in, so an interactive
/// session's `build_design` can build.
pub fn router_with_builder(engine: DesignEngine, builder: Arc<dyn DesignBuilder>) -> axum::Router {
    build_router(ConseqaMcp::with_builder(engine, builder))
}

/// The `/mcp` router for a multi-project server: a stable API key
/// authenticates clients, which select projects through the project
/// tools, and an optional design builder powers `build_design`.
pub fn router_multi(
    manager: Arc<WorkspaceManager>,
    api_key: impl Into<String>,
    builder: Option<Arc<dyn DesignBuilder>>,
) -> axum::Router {
    build_router(ConseqaMcp::multi(manager, api_key, builder))
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
pub async fn serve(engine: DesignEngine, bind: SocketAddr) -> std::io::Result<McpServer> {
    serve_router(router(engine), bind).await
}

/// Serves an already composed router (the daemon's admin surface rides
/// along).
pub async fn serve_router(router: axum::Router, bind: SocketAddr) -> std::io::Result<McpServer> {
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

/// The JSON shapes the tools exchange, in their canonical serialized
/// forms, as guide sections: each `##` header is a topic.
const DSL_REFERENCE: &str = r#"## JSON shapes — symbol keys

Every symbol key is {"kind": K, "value": V}:
  {"kind":"service","value":"service.checkout"}
  {"kind":"schema","value":"schema.Order"}
  {"kind":"data_model","value":"data.checkout"}
  {"kind":"data_object","value":{"data_model":"data.checkout","object":"object.order"}}
  {"kind":"outbox","value":{"data_model":"data.checkout","outbox":"outbox.order_events"}}
  {"kind":"topic","value":"topic.order_events"}
  {"kind":"state_machine","value":"machine.order_lifecycle"}
  {"kind":"transition","value":{"machine":"machine.x","transition":"transition.y"}}
  {"kind":"operation","value":"operation.checkout"}
  {"kind":"operation_interface","value":"operation.checkout"}
  {"kind":"operation_program","value":"operation.checkout"}
  {"kind":"operation_requirements","value":"operation.checkout"}
  {"kind":"topic_runtime","value":"topic.order_events"}
  {"kind":"subscription_runtime","value":{"operation":"operation.x","input":"input.x.events"}}
  {"kind":"outbox_runtime","value":{"operation":"operation.x","input":"input.x.outbox"}}
  {"kind":"execution_pool","value":"pool.order_workers"}
  {"kind":"router","value":"router.checkout"}
  {"kind":"storage_layout","value":"layout.order"}
  {"kind":"input","value":{"operation":"operation.x","input":"input.x.request"}}
  {"kind":"transaction","value":{"operation":"operation.x","transaction":"tx.y"}}
  {"kind":"effect_site","value":{"operation":"operation.x","effect":"effect.y"}}
  {"kind":"binding","value":{"operation":"operation.x","binding":"intent.y"}}
  {"kind":"operation_summary","value":"operation.checkout"}
  {"kind":"prompt_obligation","value":"obl.no-double-charge"}

## JSON shapes — graph queries

Every query is {"kind": K, ...}:
  {"kind":"callers","operation":"operation.x"}
  {"kind":"callees","operation":"operation.x"}
  {"kind":"readers","data_model":"data.x","object":"object.y","field":"status"}   (field optional)
  {"kind":"writers","data_model":"data.x","object":"object.y","field":"status"}   (field optional)
  {"kind":"publishers","topic":"topic.x"}      (also answers for an outbox id)
  {"kind":"consumers","topic":"topic.x"}       (also answers for an outbox id)
  {"kind":"transition_users","machine":"machine.x","transition":"transition.y"}
  {"kind":"references_to","symbol":<symbol key>}
  {"kind":"impacted_by","symbol":<symbol key>,"depth":2}
  {"kind":"operation_neighborhood","operation":"operation.x","depth":2}
  {"kind":"provenance_roots","operation":"operation.x","binding":"output.y"}

## JSON shapes — skeleton patches

A patch is {"mutations": [<mutation>, ...]}; each mutation is
{"kind": K, ...}. The shared skeleton's mutations:
  {"kind":"put_service","id":"service.x","value":{"kind":"backend"}}
  {"kind":"put_schema","id":"schema.X","value":<schema declaration>}
  {"kind":"put_data_model","id":"data.x","value":{"objects":{...},"outboxes":{...}}}
    (objects: {"object.x":{"schema":"schema.X","identity":[["id"]],
     "version":{"field":["version"]}}}. version optional: the object's
     application concurrency token, a non-optional int outside the
     identity, managed by the persistence protocol — insert establishes
     it and every committed mutation of the live instance publishes a
     newer one by itself, with no step of its own. No update,
     compare_and_set, or upsert may assign it; comparing it is how a
     mutation is conditioned on the version a preceding read observed.
     outboxes optional: {"outbox.x":{"messages":["schema.X"],
     "message_identity":{"kind":"keyed","mapping":{"schema.X":[["event_id"]]}}}}.
     An outbox is a typed transactional message collection of the data
     model: a transaction on the model may mutate objects AND admit
     messages to its outboxes in one atomic commit. Written only by a
     transaction's write_outbox step or a transition's outbox effect;
     consumed by exactly one outbox input, whose operation is its
     exclusive logical consumer.)
  {"kind":"put_topic","id":"topic.x","value":{"messages":[...],"message_identity":...}}
  {"kind":"put_state_machine","id":"machine.x","value":{...}}
    (a transition: {"from":["state.a"],"to":"state.b","side_effects":{...},
     "effects":{"effect.x.paid":{"kind":"outbox_write","outbox":"outbox.x",
                                 "schema":"schema.X","idempotency_key_propagation":[]}}}.
     A transition is an explicit commit guard: applied when the subject's
     state is not in `from`, it REJECTS the containing transaction. Its
     `effects` (optional) are outbox messages admitted atomically with the
     transition applying — the only kind is outbox_write; the outbox must
     belong to the data model owning the subject object and admit the
     schema. The applying transaction step supplies each message's
     derivation under the same key.)
  {"kind":"put_prompt_obligation","id":"obl.x","value":{"source_span":"...",
   "normalized_intent":"...","targets":["operation.x"],"status":{"kind":"unmapped"}}}
  {"kind":"delete_top_level","symbol":<symbol key>}   (coordinator-only)

Schema, topic, state-machine, input, and program declarations use the
Conseqa model YAML structure, as JSON.

## JSON shapes — operation interfaces and sketches

A patch mutation, and the input shape it uses for an outbox:
  {"kind":"put_operation_interface","operation":"operation.x",
   "value":{"service":"service.x","description":"...","inputs":{...},
            "sketch":{"steps":[
              {"kind":"find","as":"order","record":"object.order",
               "by":{"order_id":"input.order_id"}},
              {"kind":"transition","record":"order","transition":"transition.order.pay",
               "otherwise":"not_payable"},
              {"kind":"find","as":"product","record":"object.product",
               "by":{"product_id":"order.product_id"}},
              {"kind":"update","record":"product","set":["stock"],
               "from":["product.stock","order.quantity"]},
              {"kind":"create","record":"object.payment",
               "from":["input.request_id","input.order_id","input.amount"]}]}}}
    (sketch — optional, and the fastest way to get a program: what the
     operation does as typed business actions, in order, compiled into
     the program in code with no worker session. Every construct of the
     program language is reachable from a sketch. Steps:
       find       {as, record, by: {field: value-or-literal}, all: true to
                   select every match instead of one instance by its whole
                   identity, lock: "shared"|"exclusive", lock_order:
                   [{field, direction: "ascending"|"descending"}],
                   isolation: <this transaction's>};
       update     {record: <a find's as>, set: [fields], from: values};
       create     {record: <data object>, from: values, isolation};
       upsert     {record: <data object>, by: {identity field: value},
                   from: values, set: [fields], update_from: values,
                   isolation} — one atomic insert-or-update of the instance
                   `by` identifies; it reads nothing and protects no read;
       delete     {record: <a find's as>};
       transition {record, transition, otherwise: <declared error>,
                   already_ok: true, effects_from: [values] for its
                   declared side effects and outbox writes};
       advance    {record, field, to, rule: "successor"|"monotonic_after"
                   (omit to let the prompt decide), otherwise};
       fence      {record, field, token, otherwise};
       enqueue    {outbox, schema, from, key};
       publish    {topic, schema, from, key, durable, detached} — after
                  the commit when records change first, directly
                  otherwise; durable arranges it in a transaction anyway;
                  detached does not wait for it;
       call       {name, as, identity: [values], duplicates: unspecified |
                   distinguishable | identical_per_identity |
                   side_effect_free, result_replay: unspecified | unstable
                   | replay_stable, result: {ok: <schema>, errors: {class:
                   <schema> | {schema, disposition: terminal|retryable}}},
                   from, on_ok, on_error, durable, detached};
       request    {operation, input, as, retry: unspecified | never |
                   may_repeat, from, key, on_ok, on_error, durable,
                   detached} — another operation's request input;
       when       {if, then, otherwise} — if: {"equals": [value,
                  value-or-literal]}, {"present": value}, {"not": c},
                  {"all": [c]}, {"any": [c]}, {"unspecified": "<a rule the
                  model states no fact about, in words>"};
       reject     {error, from} — end a request with a declared error;
       abandon    {} — end a message's attempt without completing it,
                  leaving the message for another attempt (subscription
                  and outbox operations only);
       parallel   {steps} — start all, wait for all; a member's `as` binds
                  its answer for an `answer` step;
       answer     {of: <a parallel member's as>, on_ok, on_error};
       race       {steps, as, on_ok, on_error} — act on the first answer;
       start      {step} — start an effect and never wait for it.
     Values ("from") are a list of references, or a map {target field:
     reference} whose fields are checked against the target's schema (an
     update's `set` may then be omitted). References are "input.<field>",
     "<input id>.<field>" when the operation has several inputs,
     "<find's as>.<field>", or "<as>.<field>" of an answer — inside its
     arms, or after it, in which case later steps run in its ok arm and
     each error arm must end the operation. Literals are strings,
     integers or booleans. Sketch-level: "returns": [values] for the ok
     result; "returns_for": <request input> when there are several;
     "isolation" for every transaction (each defaults to read_committed;
     repair strengthens where a proof needs it); "commit_key": [values],
     or [] for no deduplication (the trigger's identity by default);
     "on_rejected": [steps] a refused transaction runs instead of
     completing (a request's must reject).
     Consecutive record steps on one data model are one transaction; a
     step on another, an effect or a when starts the next; a durable
     effect with no record before it gets a transaction of its own.
     Records found earlier are carried forward through the transaction's
     output — change one only in the transaction that found it. The gate
     compiles the sketch when you write it and rejects one that does not
     compile, saying why. Compiled programs condition the first change of
     a found versioned record on the version its find observed (an update
     becomes a compare_and_set, a transition or advance gains a compare),
     hold everything else that changes under a strict lock, and carry
     keyed commits, key propagation into messages and requests, and the
     inspect-then-decide shape a guarded transition needs; a record only
     read gets no guard. Requirements are added after, by discovery.)
An outbox input:
  {"kind":"outbox","outbox":"outbox.x"}
    (exactly ONE outbox input in the model may reference a given
     outbox: its operation is the outbox's exclusive logical consumer
     of every admitted schema — there is no message selection and no
     acknowledgement field. Consumption is intrinsic: a committed
     message stays durably pending, re-driven until an attempt reaches
     successful logical completion, and overlapping attempts for one
     message are possible — so the consumer needs idempotency. Fan-out
     to several consumers belongs to a topic, not an outbox. One
     committed message = one logical invocation; no batch payload
     exists at L0.)

## JSON shapes — program and requirement patches

Patch mutations:
  {"kind":"replace_operation_program","operation":"operation.x","program":{"steps":[...]}}
  {"kind":"replace_operation_requirements","operation":"operation.x","requirements":{...}}
    (operation requirements are idempotency and recoverability only;
     serializability and ordering are declared on the transaction they
     constrain, inside the program — see the transaction step below.)
  {"kind":"propose_requirements","operation":"operation.x","proposals":[
     {"requirement":{"family":"idempotency","requirement":{"key":{"components":[...]},
       "result":"replay_consistent"}},
      "origin":{"kind":"explicit_prompt","obligation":"obl.x"}},
     {"requirement":{"family":"transaction_serializability",
       "requirement":{"transaction":"tx.x","requirement":{"key":<value ref>}}},
      "origin":{"kind":"strongly_implied","rationale":"...","evidence":[]}},
     {"requirement":{"family":"transaction_ordering",
       "requirement":{"transaction":"tx.x",
                      "requirement":{"key":<value ref>,"position":<value ref>}}},
      "origin":{"kind":"recommended","rationale":"...","evidence":[]}}]}
    (families: transaction_serializability | transaction_ordering — each
     names an inline transaction of the operation's program, which must
     exist — | idempotency | recoverability.
     origins: explicit_prompt {obligation}; strongly_implied {rationale, evidence};
     recommended {rationale, evidence})

## JSON shapes — L1 runtime topology patches

Patch mutations, all shared-skeleton writes; L1 is optional:
  {"kind":"put_topic_runtime","topic":"topic.x",
   "value":{"grouping":{"schema.Event":[["order_id"]]},"ordering":"within_group"}}
    (grouping: a per-schema key mapping, or omitted for no grouping;
     ordering: "none" | "global" | "within_group", or omitted.
     Omitting `ordering` declares nothing and leaves the scope open;
     writing "none" is an explicit negative and claims the scope.
     Grouping and ordering are INDEPENDENT facts sharing one declaration
     scope. A topic declaring either supplies both to every subscription
     of it, and no subscription of that topic may declare its own; a topic
     declaring neither leaves each subscription to declare its own pair.
     Never both scopes — there is no inheritance and no override.
     `within_group` requires a keyed grouping at the same scope.)
  {"kind":"put_execution_pool","id":"pool.x",
   "value":{"member_concurrency":{"kind":"bounded","value":1}}}
    (member_concurrency kinds: unspecified | unbounded | bounded{value}.
     A capacity fact an external scenario reads; bounded(1) proves no
     transaction property — a member running one invocation at a time
     still overlaps every other member, its own replacement, and a
     stale incarnation of itself.)
  {"kind":"put_router","id":"router.x",
   "value":{"boundary":{"operation":"operation.x","input":"input.x.request"},
            "pool":"pool.x",
            "routing":{"key":["order_id"],
                       "member_assignment":{"kind":"consistent_hash"}}}}
    (member_assignment: consistent_hash | round_robin — placement facts
     only. Omit "routing" entirely to assign the boundary to a pool and
     declare no member affinity; that is not the same as round_robin,
     which states that affinity is known NOT to exist. Neither proves
     serializability or ordering: no L1 fact is commit-order
     evidence.)
  {"kind":"put_subscription_runtime","operation":"operation.x","input":"input.x.events",
   "value":{"delivery":"at_least_once",
            "dispatch":{"pool":"pool.x",
                        "routing":{"key":"grouping_key",
                                   "member_assignment":{"kind":"consistent_hash"}}}}}
    (delivery: unspecified | at_most_once | at_least_once.
     Add "grouping" and "ordering" here — as a pair, same shapes as on a
     topic runtime — only when the subscribed topic declares neither.
     routing key: grouping_key, which routes by the effective grouping
     domain and so requires a keyed grouping in effect at one scope.)
  {"kind":"put_outbox_runtime","operation":"operation.x","input":"input.x.outbox",
   "value":{"partitioning":{"kind":"keyed",
                            "mapping":{"schema.Event":[["tenant_id"]]}},
            "ordering":"partition",
            "dispatch":{"pool":"pool.x",
                        "routing":{"key":"partition_key",
                                   "member_assignment":{"kind":"consistent_hash"}},
                        "batching":{"ordering":"preserved"}}}}
    (the target input must be kind outbox. There is NO delivery field:
     durable re-drive until successful consumption is intrinsic to the
     outbox, and stale attempts may overlap fresh ones — which is why
     the consumer needs idempotency. partitioning: {"kind":"none"} or
     keyed with a per-schema mapping covering every schema the outbox
     admits — the outbox's ONE grouping concept. ordering: "none" |
     "global" | "partition"; "partition" requires keyed partitioning.
     routing is optional, mirroring subscription dispatch: omit it to
     declare no member-affinity fact; key "partition_key" routes by the
     declared partition domain and requires keyed partitioning. batching
     is optional; when present its "ordering" is explicit: "preserved" |
     "unspecified". Batching is an opaque L1 realization over
     per-message logical invocations; like every other L1 fact it
     proves no transaction property.)
  {"kind":"put_storage_layout","id":"layout.x",
   "value":{"object":{"data_model":"data.x","object":"object.y"},
            "partition_key":["channel_id","bucket"]}}



## JSON shapes — transaction steps

A transaction step declares and executes one inline transaction:
  {"kind":"transaction",
   "transaction":{"id":"tx.x","data_model":"data.x","isolation":"read_committed",
                  "idempotency":{"kind":"deduplicated_by","key":{"components":[...]}},
                  "requirements":{"serializability":[{"key":<value ref>}],
                                  "ordering":[{"key":<value ref>,"position":<value ref>}]},
                  "steps":[...]},
   "rejected":{"steps":[...]}}
    (requirements optional. SerializableBy(key): the transaction's
     executions, with every transaction they may conflict with, commit in
     a history equivalent to some serial order. OrderedBy(key, position):
     within each key domain, executions take effect in position order —
     the position must be a non-optional int, decimal, or timestamp and
     must be persisted by an advance_cursor or fence step. Both key and
     position must be available at transaction entry: an input, a prior
     transaction output, or a bound result — never a transaction_read of
     this transaction. Proven from the model-wide conflict closure by
     serializable isolation, strict S/X locks, an atomic conditional
     mutation (its own comparison, or one of a read's observed state or
     version), a read-only transaction's single observation, or a shared
     ordered cursor; never from runtime topology.
     rejected: REQUIRED iff the body contains a commit guard — a
     compare_and_set, transition, advance_cursor, or fence — and
     forbidden otherwise. On rejection nothing commits, no artifact or
     outbox admission exists, and control enters the block; if it falls
     through, control rejoins after the step with the transaction's
     artifacts unavailable. Interruption (crash, deadlock, timeout) never
     enters rejected and is never an Err.)
Transaction steps beyond read/update/insert/delete/lock/transition/
establish_effect_intent/establish_transaction_output/write_outbox:
  {"kind":"update","target":<object selector>,"fields":[["balance"]],
   "values":<derivation>}
    (an unconditional mutation. It never rejects because something read
     earlier changed: it is not a stale-read guard. On a versioned object
     it publishes a newer version by itself.)
  {"kind":"compare_and_set","target":<object selector pinning the whole
   identity>,
   "compare":[{"field":["version"],
               "expected":{"source":"transaction_read:read.x","path":"version"}}],
   "fields":[["balance"]],"values":<derivation>}
    (an ATOMIC conditional update of one identified instance — like
     UPDATE ... WHERE id = ? AND version = ?: the comparison and the
     mutation are one storage operation, whose write protection is then
     held to commit. Rejects the transaction when the instance does not
     exist or any comparison is false. compare: one or more equality
     conditions, each field at most once; expected is a value reference
     or a literal. A comparison whose expected value is exactly
     transaction_read:<bind>.<the same field> of an EARLIER read of the
     SAME instance is an observed-state comparison: comparing the
     object's version covers every field that read observed (every
     committed mutation of the live instance publishes a newer version);
     comparing fields directly covers those fields. That is what lets a
     read-then-write prove serializable. Any other comparison — an input,
     a literal — is valid behaviour but no such evidence. Use it where the
     transaction relies on what it read; a blind update stays an update.)
  {"kind":"upsert","target":<object selector pinning the whole identity>,
   "insert_values":<derivation>,"update_fields":[["balance"]],
   "update_values":<derivation>}
    (atomic insert-or-update arbitrated on the object's identity: absent,
     inserted; present, update_fields set. Never rejects, never names the
     version or an identity field in update_fields, and protects no
     earlier read — not even of the instance it upserts.)
  A transition, advance_cursor, or fence may carry "compare":[...] too:
  the comparisons are conjoined with its own condition in the same atomic
  statement. A fence holds no write protection when its token equals the
  fence, so a fence's comparison guards no read; put the comparison on a
  compare_and_set, transition, or cursor advance of the instance.
  There is no validate_version or bump_version: a read the transaction
  relies on and never mutates is protected by a real mechanism — a lock
  before the read, serializable isolation, or a guarded mutation of what
  it read — or the obligation is refused.
  {"kind":"advance_cursor","target":<object selector>,"field":["seq"],
   "incoming":<value ref>,"rule":"successor"}
    (rule: successor — commits only when incoming = stored + 1 — or
     monotonic_after — commits only when incoming > stored — then sets the
     cursor atomically. The field is a non-optional int (successor) or
     int/decimal/timestamp (monotonic_after); no update may name
     it. The route for OrderedBy when incoming is the position.)
  {"kind":"fence","target":<object selector>,"field":["generation"],
   "token":<value ref>}
    (token < fence rejects; = leaves it; > advances it atomically. A
     lower authority generation cannot commit after a higher one is
     accepted; equal tokens order nothing.)
  {"kind":"lock","target":<object selector>,"mode":"exclusive",
   "order":{"kind":"unspecified"}}
    (mode: shared | exclusive; held to termination; protects only
     accesses AFTER it. S/X locking proves serializable commit order and
     says nothing about deadlock.)
  {"kind":"transition","machine":"machine.x","transition":"transition.y",
   "subject":<object selector>,"effect_intents":{...},
   "effects":{"effect.x.paid":{"values":<derivation>}}}
    (effects: one derivation per outbox effect the transition declares,
     admitted iff the transition applies. Rejects the transaction when
     the subject's state is not in the transition's `from`.)
A transactional outbox write, legal ONLY as a transaction step:
  {"kind":"write_outbox","effect_id":"effect.x.outbox",
   "effect":{"outbox":"outbox.x","schema":"schema.X",
             "idempotency_key_propagation":[]},
   "values":<derivation>}
    (the destination outbox must belong to the transaction's data_model;
     admission is atomic with the commit; the step binds no result. The
     kind is rejected under execute_effect, execute_effect_async, and
     establish_effect_intent.)

## JSON shapes — results, effects, values, and branches

Result contracts name their error classes:
  {"ok":"schema.Ok",
   "errors":{"conflict":{"schema":"schema.Conflict","disposition":"retryable"},
             "declined":{"schema":"schema.Declined","disposition":"terminal"}}}
    (errors optional; each class keeps its own disposition: unspecified |
     terminal | retryable. A bare schema id declares disposition
     unspecified. Interruption is never an Err.)
  {"kind":"return","request":"input.x.request",
   "outcome":{"kind":"err","error":"conflict","values":<derivation>}}
    (the class must exist in the request's contract; ok outcomes carry
     no class.)
  {"kind":"match_result","result":"result.x","ok":{"steps":[...]},
   "errors":{"conflict":{"steps":[...]},"declined":{"steps":[...]}}}
    (arms are exhaustive: exactly one per declared error class. Inside an
     error arm, effect_result_err:<result> resolves to that class's
     schema.)
  {"kind":"abandon"}
    (a terminal for a subscription or outbox operation: the attempt ends
     without completing, so the message stays unacknowledged or pending
     and is delivered again. It rolls nothing back; every step before it
     is re-done by the next attempt, so it must be retry-safe.
PASSING A RETRYABLE ERROR UP keeps every proof: a request returns an
error class its own contract declares retryable — exempt from result
replay — and a consumer abandons. Do it from a match arm that does
nothing else: on that arm the decision then adds no work, whatever the
ok arm does. A target's retryable error is never a stable result, so
turning it into a terminal error, or doing work on its arm, leaves the
operation's replay unproven.)
An external effect declares three orthogonal boundary facts:
  {"kind":"external","name":"provider.op",
   "identity":{"kind":"keyed","key":{"components":[<value ref>...]}},
   "idempotency":"identical_per_identity",
   "result_replay":"replay_stable",
   "result":{"ok":"schema.Ok",
             "errors":{"declined":{"schema":"schema.Err","disposition":"terminal"}}}}
    (identity: unspecified | keyed — what makes applications one logical
     interaction. idempotency: unspecified | distinguishable |
     identical_per_identity | side_effect_free — duplicate-side-effect
     behaviour; identical_per_identity requires keyed identity.
     result_replay: unspecified | unstable | replay_stable — terminal-
     result behaviour; unstable and replay_stable require a result
     contract, replay_stable also a keyed identity. The axes are
     independent: a side_effect_free boundary may be result-unstable
     (a presigner), and neither behavioural axis is inferred from the
     other.)
Value references:
  {"source":"input:input.x.request","path":"order_id"}
Derivations: {"kind":"unspecified"} or {"kind":"deterministic","from":[<value ref>...]}.

Branch conditions — deliberately small, structurally exposed:
  {"kind":"eq","value":<value ref>,"equals":<value ref or literal>}
  {"kind":"present","value":<value ref>}   (holds iff the path resolves;
     absent iff any optional segment is absent. eq never holds over an
     absent operand; presence is asked only through present. A present
     over a required path is vacuously true — redundant, warned, valid.)
  {"kind":"and","conditions":[...]}  {"kind":"not","condition":...}
  {"kind":"unspecified"}   (no fact; the decision never replays)

## JSON shapes — async program steps

Launch effects without waiting, then synchronize:
  {"kind":"execute_effect_async","handle":"async.x","effect_id":"effect.x",
   "effect":<effect>, "values":<derivation>}
    (same contract/derivation semantics as execute_effect; binds NO result,
     only the operation-local handle. start(x) precedes the next step;
     complete(x) is established only by a later join_all or race.)
  {"kind":"execute_effect_intent_async","intent":"intent.x","handle":"async.x"}
  {"kind":"join_all","handles":[{"handle":"async.a","bind":"result.a"},
                                {"handle":"async.b"}]}
    (waits for ALL; each entry may bind its effect's result — only there do
     async results become available. No order among the joined effects.)
  {"kind":"race","handles":["async.a","async.b"],"bind":"result.first"}
    (waits for the FIRST completion — not first success; losers are not
     cancelled and stay in the side-effect blast radius. bind requires every
     candidate to expose the same result contract; omit bind otherwise.
     At least two handles.)
"#;

/// The prose that introduces [`PROGRAM_EXAMPLE_JSON`] in the reference.
const PROGRAM_EXAMPLE_PREAMBLE: &str = "
A valid program (the body of a `replace_operation_program`
patch). The transaction step nests its declaration under `transaction`; it
carries no `rejected` block because nothing in the body can reject. An
effect intent must be established before it is executed, by the same
binding id: executing an intent no transaction established is the
`unknown effect intent` error the commit gate rejects. A transaction output
is bound once and then consumed by the `return`.
";

/// A complete, valid operation program, the reference's last section, so
/// an agent has a concrete template for the establish/execute-intent and
/// output-binding wiring it most often gets wrong. Kept as pure JSON and
/// checked by a test (`the_worked_example_is_a_valid_program`), so the
/// reference cannot drift into an invalid shape.
const PROGRAM_EXAMPLE_JSON: &str = r#"{"steps":[
  {"kind":"transaction",
   "transaction":{"id":"tx.example","data_model":null,
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
     ]}},
  {"kind":"execute_effect_intent","intent":"intent.example.notify"},
  {"kind":"return","request":"input.example.request",
   "outcome":{"kind":"ok","values":{"kind":"deterministic",
              "from":[{"source":"transaction_output:output.example","path":"id"}]}}}
]}"#;

/// The canonical DSL semantics document, embedded so `dsl_guide` can
/// teach an agent the language from the server itself — instead of the
/// agent reverse-engineering the crate's source, which it cannot see
/// from its own project anyway.
const DSL_SEMANTICS: &str = include_str!("../../docs/live/CONSEQA_DSL_SEMANTICS.md");

/// The reference, its worked example included.
static REFERENCE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{DSL_REFERENCE}\n## JSON shapes — worked program example\n{PROGRAM_EXAMPLE_PREAMBLE}\n\
         ```json\n{PROGRAM_EXAMPLE_JSON}\n```\n"
    )
});

/// Every section the guide serves: the semantics document's, then the
/// reference's.
fn guide_sections() -> Vec<(&'static str, &'static str)> {
    let mut sections = sections_of(DSL_SEMANTICS);

    sections.extend(sections_of(REFERENCE.as_str()));

    sections
}

/// The `##` sections of a document: header line and full body,
/// subsections included. Fence-aware, so a `##` inside a code block
/// never starts a section.
fn sections_of(doc: &'static str) -> Vec<(&'static str, &'static str)> {
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

/// The guide's table of contents: every section and subsection header
/// of the semantics, then the reference's sections, with the usage
/// hint.
fn guide_toc() -> String {
    let mut toc = format!(
        "The Conseqa DSL guide, DSL contract version {}. Call dsl_guide again with \
         topics — section names or a few words of them — to read those sections in full, \
         every one you need in one call.\n\nSections:\n",
        crate::spec::DSL_VERSION,
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

    toc.push_str(
        "\nThe exact JSON shapes these tools exchange (the topic `JSON shapes` reads them \
         all):\n",
    );

    for (header, _) in sections_of(REFERENCE.as_str()) {
        if let Some(header) = header.strip_prefix("## ") {
            toc.push_str(&format!("- {header}\n"));
        }
    }

    toc
}

/// Sections whose header — or any subsection header within — contains
/// the query, case-insensitively. An unmatched query falls back to the
/// contents, and one matching several sections too long to serve
/// together to their headers, so the caller can narrow; one section
/// is always served whole, however long.
fn guide_lookup(topic: &str) -> String {
    let query = topic.trim().to_lowercase();

    if query.is_empty() {
        return guide_toc();
    }

    let matched: Vec<(&str, &str)> = guide_sections()
        .into_iter()
        .filter(|(header, body)| {
            header.to_lowercase().contains(&query)
                || body
                    .lines()
                    .any(|line| line.starts_with("### ") && line.to_lowercase().contains(&query))
        })
        .collect();

    if matched.is_empty() {
        return format!("No section matches `{topic}`.\n\n{}", guide_toc());
    }

    let total: usize = matched.iter().map(|(_, body)| body.len()).sum();

    if matched.len() > 1 && total > 40_000 {
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

    use crate::spec::{Id, Model, Operation, OperationBlock, Revision};

    /// The worked example in the guide's shapes must be a genuinely valid
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
        };

        let mut operations = BTreeMap::new();
        operations.insert(operation_id.clone(), operation);

        let model = Model {
            dsl: crate::spec::DSL_VERSION,
            revision: Revision(1),
            services: BTreeMap::new(),
            schemas: BTreeMap::new(),
            data_models: BTreeMap::new(),
            topics: BTreeMap::new(),
            state_machines: BTreeMap::new(),
            operations,
            runtime: None,
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

    // The guidance states the contract as the provers implement it: a
    // transaction obligation's remedy is always an application edit,
    // and no agent is told to expect — or to route on — a `runtime` one.
    #[test]
    fn the_coordinator_instructions_promise_no_runtime_remedy() {
        let instructions = super::COORDINATOR_INSTRUCTIONS;

        assert!(instructions.contains("carries `remedy: application`, always"));
        assert!(instructions.contains("No obligation currently carries `remedy: runtime`"));
        assert!(!instructions.contains("`runtime` means"));
    }

    #[test]
    fn the_guide_toc_lists_the_semantics_sections() {
        let toc = super::guide_toc();

        for expected in [
            "Interpretation model",
            "Operations",
            "Effect intents",
            "Value references",
            "Operation requirements",
            "L1 — runtime topology and realization semantics",
        ] {
            assert!(toc.contains(expected), "toc lacks `{expected}`:\n{toc}");
        }
    }

    #[test]
    fn a_topic_returns_its_full_section() {
        let section = super::guide_lookup("effect intents");

        assert!(
            section.contains("EstablishEffectIntent") && section.contains("ExecuteEffectIntent"),
            "the effect-intents section should explain both sides of the wiring:\n{section}"
        );
    }

    #[test]
    fn a_subsection_topic_returns_its_enclosing_section() {
        let section = super::guide_lookup("SubscriptionRuntime");

        assert!(
            section.contains("L1 — runtime topology")
                && section.contains("ExecutionPool")
                && section.contains("MemberAssignment"),
            "a `###` header match should return its whole enclosing section:\n{section}"
        );
    }

    /// A section longer than the over-broad limit is still one section,
    /// and is served whole rather than "narrowed" to itself forever.
    #[test]
    fn a_section_longer_than_the_limit_is_served_whole() {
        let section = super::guide_lookup("16. The operation program");

        assert!(section.len() > 40_000, "{} bytes", section.len());
        assert!(
            section.starts_with("## 16. The operation program"),
            "{}",
            &section[..200.min(section.len())]
        );
    }

    /// The shapes are guide sections: listed in the contents, served by
    /// name, and all at once under `JSON shapes`.
    #[test]
    fn the_json_shapes_are_guide_topics() {
        let toc = super::guide_toc();

        for topic in [
            "JSON shapes — symbol keys",
            "JSON shapes — skeleton patches",
            "JSON shapes — operation interfaces and sketches",
            "JSON shapes — worked program example",
        ] {
            assert!(toc.contains(topic), "toc lacks `{topic}`:\n{toc}");
        }

        let sketches = super::guide_lookup("interfaces and sketches");

        assert!(sketches.contains("put_operation_interface"), "{sketches}");
        assert!(!sketches.contains("put_topic_runtime"), "{sketches}");

        let every = super::guide_lookup("JSON shapes");

        for shape in [
            "\"kind\":\"callers\"",
            "put_service",
            "replace_operation_program",
            "put_router",
            "compare_and_set",
            "match_result",
            "join_all",
            super::PROGRAM_EXAMPLE_JSON,
        ] {
            assert!(every.contains(shape), "`JSON shapes` lacks {shape}");
        }
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
