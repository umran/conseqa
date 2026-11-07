//! The conformance tools (§15.1 of the conformance revision), and the
//! implementer and reviewer credentials that call them (T4 and T5 of
//! the delivery loop orchestrator design).
//!
//! The read tools join every role's tools: the status, the work list,
//! and a work item. The write tools are the roles' own: `attest` an
//! implementer's and the coordinator's, `record_review` a reviewer's
//! and the coordinator's. Neither role is served `submit_patch` or
//! `build_design`, since implementing and reviewing change code and
//! records, never the spec.
//!
//! Who attests or reviews is never the caller's to say. An implementer's
//! or a reviewer's token carries the party its writes are stamped with,
//! minted with it; the coordinator's surfaces stamp the person running
//! the project's checkout, by git's `user.name` (§14.5).
//!
//! On an engine, the tools read the spec at the engine's head, which
//! adopts outside edits before every call. On a review server, which
//! has no engine, they read the project's files. Records are written
//! outside the spec either way, so a write never contends with the
//! commit gate.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, RoleServer,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars,
    service::RequestContext,
    tool, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::analyzer::basis::TextualKey;
use crate::conformance::attest::{self, AttestError, AttestInput};
use crate::conformance::document::{self, AttestOutcome, ReviewOutcome, ReviewResult};
use crate::conformance::packet;
use crate::conformance::plan::{self, Scope};
use crate::conformance::record::{Party, PartyKind};
use crate::conformance::review::{self, ReviewError, ReviewInput, Reviewed};
use crate::conformance::state::Derivation;
use crate::design::TaskToken;
use crate::design::engine::{DesignEngine, EngineError};
use crate::design::store::ProjectStore;
use crate::design::task::WriteScope;
use crate::design::workspace::WorkspaceState;

use super::{
    Backing, ConseqaMcp, ResolveError, json_error, no_project_result, project_error_result,
    resolve_to_mcp_error,
};

/// The roles a minted token can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialRole {
    /// Reads, and attests.
    Implementer,

    /// Reads, and records reviews.
    Reviewer,
}

impl fmt::Display for CredentialRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Implementer => "implementer",
            Self::Reviewer => "reviewer",
        })
    }
}

/// An implementer's or a reviewer's credential: its role, and the party
/// its writes are stamped with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub role: CredentialRole,
    pub by: Party,
}

/// The implementer and reviewer tokens a server serves, shared between
/// the server and whatever mints them: a daemon's admin surface, or
/// `conseqa conform review`.
#[derive(Debug, Clone, Default)]
pub struct Credentials {
    tokens: Arc<parking_lot::RwLock<HashMap<String, Credential>>>,

    /// The reviews each token recorded, in order: what `conseqa conform
    /// review` reports of the agents it started.
    recorded: Arc<parking_lot::Mutex<HashMap<String, Vec<Reviewed>>>>,
}

impl Credentials {
    /// Mints a token on a single-project server: a read-only session of
    /// `engine`, so that the read tools serve it as they serve any
    /// session, carrying its role and `by`.
    pub fn issue(
        &self,
        engine: &DesignEngine,
        role: CredentialRole,
        by: Party,
    ) -> Result<String, EngineError> {
        let handle = engine.create_session(WriteScope::of([]), format!("{role}: {}", by.name))?;
        let token = handle.token.0;

        self.tokens
            .write()
            .insert(token.clone(), Credential { role, by });

        Ok(token)
    }

    /// Mints a token for a server with no engine: a review server's.
    pub fn issue_detached(&self, role: CredentialRole, by: Party) -> String {
        let token = TaskToken::generate().0;

        self.tokens
            .write()
            .insert(token.clone(), Credential { role, by });

        token
    }

    /// The credential a token names.
    pub fn get(&self, token: &str) -> Option<Credential> {
        self.tokens.read().get(token).cloned()
    }

    /// The reviews a token recorded, in order.
    pub fn recorded(&self, token: &str) -> Vec<Reviewed> {
        self.recorded.lock().get(token).cloned().unwrap_or_default()
    }

    fn note(&self, token: &str, reviewed: Reviewed) {
        self.recorded
            .lock()
            .entry(token.to_string())
            .or_default()
            .push(reviewed);
    }

    /// Whether a token may be minted for `by`: an agent or a person,
    /// named, since a checker's reviews are its catalogue entry's
    /// (Part VI §13).
    pub fn admissible(by: &Party) -> Result<(), String> {
        if by.kind == PartyKind::Checker {
            return Err("a token is minted for an agent or a person, not a checker".to_string());
        }

        if by.name.trim().is_empty() {
            return Err("`by.name` is empty".to_string());
        }

        if by.job.as_deref().is_some_and(|job| job.trim().is_empty()) {
            return Err("`by.job` is empty".to_string());
        }

        Ok(())
    }
}

/// What a conformance tool reads, and whom it stamps.
struct Call {
    project_dir: PathBuf,
    workspace: WorkspaceState,
    credential: Option<Credential>,
}

impl Call {
    /// Who a write is stamped with: the token's party, else the person
    /// running the checkout.
    fn by(&self) -> Result<Party, String> {
        match &self.credential {
            Some(credential) => Ok(credential.by.clone()),
            None => Party::person(&self.project_dir).map_err(|error| error.to_string()),
        }
    }
}

/// Resolves a conformance tool's call, or returns the result that says
/// why it cannot be served.
macro_rules! call {
    ($self:expr, $context:expr) => {
        match $self.conformance_call(&$context) {
            Ok(call) => call,
            Err(result) => return result,
        }
    };
}

impl ConseqaMcp {
    fn conformance_call(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<Call, Result<CallToolResult, McpError>> {
        let credential = self.credential(context);

        match &self.backing {
            Backing::Review { project_dir } => {
                if credential.is_none() {
                    return Err(Err(McpError::invalid_request(
                        "the reviewer credential is not recognized",
                        None,
                    )));
                }

                let read = ProjectStore::read(project_dir)
                    .map_err(|error| project_error_result(error.to_string()))?;

                Ok(Call {
                    project_dir: project_dir.clone(),
                    workspace: read.workspace,
                    credential,
                })
            }

            Backing::Single(_) | Backing::Multi { .. } | Backing::Local(_) => {
                let resolved = match self.resolve(context) {
                    Ok(resolved) => resolved,
                    Err(ResolveError::NoProject) => return Err(no_project_result()),
                    Err(ResolveError::Project(message)) => {
                        return Err(project_error_result(message));
                    }
                    Err(error) => return Err(Err(resolve_to_mcp_error(error))),
                };

                let engine = resolved.engine;

                let Some(project_dir) = engine.project_dir() else {
                    return Err(json_error(json!({
                        "error": "the model lives in no project directory, so it has no code \
                                  to conform",
                    })));
                };

                Ok(Call {
                    project_dir,
                    workspace: engine.head_snapshot().workspace.as_ref().clone(),
                    credential,
                })
            }
        }
    }
}

/// A derivation, or the error result saying why there is none.
fn derive<'a>(call: &'a Call) -> Result<Derivation<'a>, Result<CallToolResult, McpError>> {
    Derivation::of(&call.project_dir, &call.workspace)
        .map_err(|error| json_error(json!({ "error": error.to_string() })))
}

/// The result holding a text rendering, then a document.
fn rendered(text: String, json: String) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![
        ContentBlock::text(text),
        ContentBlock::text(json),
    ]))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImplementationPlanParams {
    /// Scope the list to what the spec's changes since this commit owe:
    /// a git commit expression (a branch, a tag, `HEAD~2`, a hash).
    #[serde(default)]
    pub base: Option<String>,

    /// With `base`, compare against the merge base of `base` and the
    /// project's current files.
    #[serde(default)]
    pub merge_base: bool,

    /// Scope the list to what a range of spec commits owes, `<a>..<b>`,
    /// instead: on a trunk carrying other changes' specs, one change's
    /// own commits.
    #[serde(default)]
    pub spec_range: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkItemParams {
    /// A bindable declaration's textual key, e.g.
    /// "transaction operation.apply_payment/tx.apply_payment", or an
    /// orphaned key.
    pub key: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttestParams {
    /// The declaration's textual key.
    pub declaration: String,

    /// Each attested fact: {"fact": <its canonical form, as the packet
    /// gives it>, "evidence": [{"path": "<file>", "lines": [<first>,
    /// <last>]}, ...]}. Each span lies strictly between a region's
    /// markers.
    #[serde(default)]
    pub facts: Vec<Value>,

    /// That the declaration's regions implement its residual and do no
    /// modeled work beyond it.
    pub structural: bool,

    /// Completeness claims, on a data object, topic, or outbox:
    /// {"scope": "writers" | "publishers", "patterns": [<regex>, ...],
    /// "exceptions": [{"path": "<file>", "reason": "<why>"}]}.
    #[serde(default)]
    pub completeness: Vec<Value>,

    /// The `read_set` of the packet the work was done against, as the
    /// packet gives it.
    #[serde(default)]
    pub read_set: Option<Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordReviewParams {
    /// The declaration's textual key.
    pub declaration: String,

    /// The verdict on the record as a whole: "confirmed" or "disputed".
    pub verdict: String,

    /// Verdicts on single things the record carries, each overriding
    /// the overall verdict for its target: {"fact": <index>} or
    /// {"completeness": <index>} or {"structural": true}, with
    /// "verdict" and an optional "note".
    #[serde(default)]
    pub items: Vec<Value>,

    /// The `record` digest of the read set you reviewed against, as the
    /// packet gives it; the review is refused if the record changed
    /// since.
    #[serde(default)]
    pub record: Option<String>,
}

/// The read tools, served to every role.
#[tool_router(router = conformance_reads, vis = "pub(super)")]
impl ConseqaMcp {
    #[tool(
        description = "The project's conformance: how many bindable declarations are \
                       current and what conditions the rest have, and, for every proven \
                       obligation, whether the code conforms and which declarations block \
                       it. A summary, then the status document as JSON."
    )]
    async fn conformance_status(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let call = call!(self, context);

        let derivation = match derive(&call) {
            Ok(derivation) => derivation,
            Err(result) => return result,
        };

        rendered(
            derivation.state.summary(),
            document::status(&derivation.state).to_json(),
        )
    }

    #[tool(
        description = "The ordered work list: one item per declaration whose code does not \
                       conform yet, and one per orphaned key, by priority, then dependency, \
                       each with what it depends on and the files it will likely touch. \
                       `base` (with `merge_base`) or `spec_range` scopes it to what a \
                       spec change owes. The list in text, then the plan document as JSON."
    )]
    async fn implementation_plan(
        &self,
        params: Parameters<ImplementationPlanParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        let call = call!(self, context);

        let derivation = match derive(&call) {
            Ok(derivation) => derivation,
            Err(result) => return result,
        };

        let scope = match (&params.base, &params.spec_range) {
            (None, None) => None,

            (Some(_), Some(_)) => {
                return json_error(json!({
                    "error": "`base` and `spec_range` are alternatives: give one",
                }));
            }

            (Some(base), None) => {
                // The head is the spec the tool read: the project's
                // current files.
                match Scope::since(&call.project_dir, base, params.merge_base, &call.workspace) {
                    Ok(scope) => Some(scope),
                    Err(message) => return json_error(json!({ "error": message })),
                }
            }

            (None, Some(range)) => {
                if params.merge_base {
                    return json_error(json!({
                        "error": "`merge_base` goes with `base`, not with `spec_range`",
                    }));
                }

                match Scope::range(&call.project_dir, range) {
                    Ok(scope) => Some(scope),
                    Err(message) => return json_error(json!({ "error": message })),
                }
            }
        };

        let work = plan::plan(&derivation);

        let work = match &scope {
            Some(scope) => work.scoped(scope, &derivation),
            None => work,
        };

        rendered(
            work.render(),
            document::plan(&work.items, scope.as_ref()).to_json(),
        )
    }

    #[tool(
        description = "The work item for one declaration, by its textual key: what it \
                       declares and what changed since its record, its conditions, the facts \
                       its code must provide with the obligations each one carries and what \
                       deleting it would cost, its claims, where its code and its \
                       neighbours' code are, precedents, guide topics, the checklist, open \
                       disputes, the last refused attest, and the read set to attest with. \
                       Any bindable declaration or orphaned key has one. The packet in \
                       markdown, then as JSON."
    )]
    async fn work_item(
        &self,
        params: Parameters<WorkItemParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        let call = call!(self, context);

        let key: TextualKey = match params.key.parse() {
            Ok(key) => key,
            Err(error) => {
                return json_error(json!({
                    "error": format!("`{}` is not a textual key: {error}", params.key),
                }));
            }
        };

        let derivation = match derive(&call) {
            Ok(derivation) => derivation,
            Err(result) => return result,
        };

        match packet::packet(&derivation, &key) {
            Some(packet) => rendered(packet.render(), document::item(&packet).to_json()),

            None => json_error(json!({
                "error": format!(
                    "`{key}` has no work item: it is neither a bindable declaration of the spec \
                     nor an orphaned key"
                ),
            })),
        }
    }
}

/// Attest: an implementer's and the coordinator's.
#[tool_router(router = attest_tools, vis = "pub(super)")]
impl ConseqaMcp {
    #[tool(
        description = "Record what a declaration's code was checked to provide: the facts \
                       its regions provide, each with evidence spans, the structural claim, \
                       any completeness claims, and the packet's read set. The tool computes \
                       the regions, digests, and residual itself, and stamps who attests \
                       from your credential. A refusal names the clause that failed; an \
                       attest the record already holds succeeds without writing. The attest \
                       document as JSON."
    )]
    async fn attest(
        &self,
        params: Parameters<AttestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        let call = call!(self, context);

        let input: AttestInput = match serde_json::from_value(json!({
            "declaration": params.declaration,
            "facts": params.facts,
            "structural": params.structural,
            "completeness": params.completeness,
            "read_set": params.read_set,
        })) {
            Ok(input) => input,
            Err(error) => {
                return json_error(json!({
                    "error": format!("the attest does not parse: {error}"),
                }));
            }
        };

        let by = match call.by() {
            Ok(by) => by,
            Err(message) => return json_error(json!({ "error": message })),
        };

        match attest::attest(&call.project_dir, &call.workspace, &input, by) {
            Ok(attested) => Ok(CallToolResult::success(vec![ContentBlock::text(
                document::attest(AttestOutcome::of(Ok(&attested))).to_json(),
            )])),

            Err(AttestError::Refused(refusal)) => {
                Ok(CallToolResult::error(vec![ContentBlock::text(
                    document::attest(AttestOutcome::of(Err(&refusal))).to_json(),
                )]))
            }

            Err(AttestError::Failed(error)) => json_error(json!({ "error": error.to_string() })),
        }
    }
}

/// Recording a review: a reviewer's and the coordinator's.
#[tool_router(router = review_tools, vis = "pub(super)")]
impl ConseqaMcp {
    #[tool(
        description = "Record your review of one declaration's record against its code: a \
                       verdict on the record as a whole, and per fact and claim where you \
                       can give one, each with a note. The tool records the region digests \
                       you reviewed and stamps who reviewed from your credential; your \
                       earlier review at these digests is replaced. A refusal names why the \
                       record cannot be reviewed as it stands. The review document as JSON."
    )]
    async fn record_review(
        &self,
        params: Parameters<RecordReviewParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        let call = call!(self, context);

        let input: ReviewInput = match serde_json::from_value(json!({
            "declaration": params.declaration,
            "verdict": params.verdict,
            "items": params.items,
            "record": params.record,
        })) {
            Ok(input) => input,
            Err(error) => {
                return json_error(json!({
                    "error": format!("the review does not parse: {error}"),
                }));
            }
        };

        let by = match call.by() {
            Ok(by) => by,
            Err(message) => return json_error(json!({ "error": message })),
        };

        let declaration = input.declaration.clone();

        let result =
            match review::record_review(&call.project_dir, &call.workspace, &input, by.clone()) {
                Ok(reviewed) => {
                    if call.credential.is_some()
                        && let Ok(token) = Self::bearer(&context)
                    {
                        self.credentials.note(&token, reviewed.clone());
                    }

                    ReviewResult::of(Ok(reviewed))
                }
                Err(ReviewError::Refused(refusal)) => ReviewResult::of(Err(*refusal)),
                Err(ReviewError::Failed(error)) => {
                    return json_error(json!({ "error": error.to_string() }));
                }
            };

        let refused = matches!(result, ReviewResult::Refused { .. });

        let json = document::review(vec![ReviewOutcome {
            declaration,
            by: Some(by),
            result,
        }])
        .to_json();

        Ok(if refused {
            CallToolResult::error(vec![ContentBlock::text(json)])
        } else {
            CallToolResult::success(vec![ContentBlock::text(json)])
        })
    }
}
