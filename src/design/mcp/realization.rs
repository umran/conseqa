//! The realization tools (§15.2 of the realization profiles revision).
//!
//! `realization_status` and `explain_realization` join every role's
//! tools, and a review server's. Design agents read them while
//! designing, which is the cheapest place to steer technology: an agent
//! asks what the stack provides before it reaches for an interactive
//! transaction on a DynamoDB stack. `choose_realization` is the
//! coordinator's alone, since choosing a technology is the architect's
//! decision.
//!
//! The tools read the spec where the caller reads it: a design task's
//! worker at the head its task pinned, every other role at the engine's
//! head, and a review server's agents from the project's files. The
//! realization profile is read from the project's files on every call.
//! A choice is written outside the spec, under its own lock, so a write
//! never contends with the commit gate.

use std::path::PathBuf;

use rmcp::{
    ErrorData as McpError, RoleServer,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars,
    service::RequestContext,
    tool, tool_router,
};
use serde::Deserialize;
use serde_json::json;

use crate::analyzer::basis::TextualKey;
use crate::analyzer::validate;
use crate::design::store::ProjectStore;
use crate::realization::choice::{
    self, ChooseError, ChooseRequest, Explanation, Profile, Refusal, Status,
};
use crate::realization::document::{self, ChoiceOutcome};
use crate::spec::Model;

use super::{
    Backing, ConseqaMcp, ResolveError, Role, json_error, no_project_result, project_error_result,
    resolve_to_mcp_error,
};

/// What a realization tool reads.
struct Call {
    project_dir: PathBuf,
    model: Model,
}

/// Resolves a realization tool's call, or returns the result that says
/// why it cannot be served.
macro_rules! call {
    ($self:expr, $context:expr) => {
        match $self.realization_call(&$context) {
            Ok(call) => call,
            Err(result) => return result,
        }
    };
}

impl ConseqaMcp {
    fn realization_call(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<Call, Result<CallToolResult, McpError>> {
        let (project_dir, workspace) = match &self.backing {
            Backing::Review { project_dir } => {
                if self.credential(context).is_none() {
                    return Err(Err(McpError::invalid_request(
                        "the reviewer credential is not recognized",
                        None,
                    )));
                }

                let read = ProjectStore::read(project_dir)
                    .map_err(|error| project_error_result(error.to_string()))?;

                (project_dir.clone(), read.workspace)
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
                        "error": "the model lives in no project directory, so it has no \
                                  realization profile",
                    })));
                };

                let snapshot = if self.role(context) == Role::Worker {
                    engine
                        .task_snapshot(resolved.task)
                        .map_err(|error| json_error(json!({ "error": error.to_string() })))?
                } else {
                    engine.head_snapshot()
                };

                (project_dir, snapshot.workspace.as_ref().clone())
            }
        };

        let model = workspace.assemble_model().map_err(|error| {
            json_error(json!({
                "error": format!("the spec has no needs to realize yet: {error}"),
            }))
        })?;

        if !validate(&model).is_empty() {
            return Err(json_error(json!({
                "error": "the model is invalid, so it has no needs to realize; \
                          requirement_report and the checker name what to fix",
            })));
        }

        Ok(Call { project_dir, model })
    }
}

/// The result holding a text rendering, then a document.
fn rendered(text: String, json: String) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![
        ContentBlock::text(text),
        ContentBlock::text(json),
    ]))
}

/// The tool error holding a refusal and why.
fn refused(refusal: &Refusal) -> Result<CallToolResult, McpError> {
    let mut value = serde_json::to_value(refusal)
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;

    value["message"] = json!(refusal.to_string());

    json_error(value)
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExplainRealizationParams {
    /// A realizable declaration's textual key, e.g. "data_model data.orders"
    /// or "topic_runtime topic.order_events".
    pub declaration: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChooseRealizationParams {
    /// The declaration's textual key.
    pub declaration: String,

    /// The catalogue entry: its id, for its current version, or its id
    /// at a version, e.g. "kafka.keyed-topic@1".
    pub entry: String,

    /// The entry's options to choose with it.
    #[serde(default)]
    pub options: Vec<String>,

    /// Why it was chosen, recorded in the choice file.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Reading what realizes the spec: every role's.
#[tool_router(router = realization_reads, vis = "pub(super)")]
impl ConseqaMcp {
    #[tool(
        description = "Which technologies realize the spec on the project's stack: each \
                       realizable declaration's needs, derived from the spec in Conseqa's \
                       capability vocabulary with the transaction or field each comes from, \
                       and its state — chosen, automatic, ambiguous, invalid_choice, \
                       infeasible, or unneeded — with the catalogue entry that realizes it. \
                       Read it before relying on a primitive the stack may not provide. A \
                       short summary, then the status document (JSON). Read-only."
    )]
    async fn realization_status(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let call = call!(self, context);

        match Status::derive(&call.project_dir, &call.model) {
            Ok(status) => rendered(status.render(), document::status(&status).to_json()),
            Err(error) => json_error(json!({ "error": error.to_string() })),
        }
    }

    #[tool(
        description = "Every candidate for one realizable declaration: what each catalogue \
                       entry provides, the needs it misses and why it cannot provide them, \
                       the option sets it is valid with, and the kind's entries outside the \
                       stack. A data object is realized by its data model's store. A report, \
                       then the explain document (JSON). Read-only."
    )]
    async fn explain_realization(
        &self,
        params: Parameters<ExplainRealizationParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        let call = call!(self, context);

        let declaration: TextualKey = match params.declaration.parse() {
            Ok(declaration) => declaration,
            Err(error) => {
                return json_error(json!({
                    "error": format!(
                        "`{}` is not a textual key: {error}",
                        params.declaration
                    ),
                }));
            }
        };

        let profile = match Profile::read(&call.project_dir) {
            Ok(Some(profile)) => profile,
            Ok(None) => return refused(&Refusal::NoStack),
            Err(error) => return json_error(json!({ "error": error.to_string() })),
        };

        match Explanation::of(&profile, &call.model, &declaration) {
            Ok(explanation) => rendered(
                explanation.render(),
                document::explain(&explanation).to_json(),
            ),
            Err(refusal) => refused(&refusal),
        }
    }
}

/// Choosing a realization: the coordinator's.
#[tool_router(router = realization_writes, vis = "pub(super)")]
impl ConseqaMcp {
    #[tool(
        description = "Choose the catalogue entry that realizes a declaration, recorded in its \
                       choice file beside the spec: the entry's id or id@version, the options \
                       to choose with it, and why. Refused, naming what it lacks, when the \
                       entry is not a valid realization of the declaration's needs on the \
                       stack. An identical choice changes nothing. The choice document (JSON)."
    )]
    async fn choose_realization(
        &self,
        params: Parameters<ChooseRealizationParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        let call = call!(self, context);

        let declaration: TextualKey = match params.declaration.parse() {
            Ok(declaration) => declaration,
            Err(error) => {
                return json_error(json!({
                    "error": format!(
                        "`{}` is not a textual key: {error}",
                        params.declaration
                    ),
                }));
            }
        };

        let request = ChooseRequest {
            declaration,
            entry: params.entry,
            options: params.options,
            reason: params.reason,
        };

        match choice::choose(&call.project_dir, &call.model, &request) {
            Ok(chosen) => Ok(CallToolResult::success(vec![ContentBlock::text(
                document::choice(ChoiceOutcome::of(Ok(&chosen))).to_json(),
            )])),

            Err(ChooseError::Refused(refusal)) => {
                Ok(CallToolResult::error(vec![ContentBlock::text(
                    document::choice(ChoiceOutcome::of(Err(&refusal))).to_json(),
                )]))
            }

            Err(ChooseError::Failed(error)) => json_error(json!({ "error": error.to_string() })),
        }
    }
}
