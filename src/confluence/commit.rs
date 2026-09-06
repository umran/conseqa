//! The serializable commit gate: request/rejection shapes, patch
//! application with requirement adoption, and draft-local checks.
//!
//! Draft commits do not require full model validity (§8.1): the gate
//! verifies typed mutation shape, authorization, ID uniqueness,
//! reference resolution, and OCC validity. Structural validation and
//! verification are asynchronous analysis over assembled models.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::spec::{
    Effect, ExecutionSemantics, Id, Input, MessageSelector, Model, Operation, OperationConcurrency,
    OperationStep, Revision, Schema, StateMachineSubject, TransactionStep, TransitionSideEffect,
    TypeRef, ValueSource,
};

use super::fingerprint::SemanticHash;
use super::graph_query::GraphQuery;
use super::patch::{Mutation, PatchId, RequirementSubmission, SpecPatch};
use super::symbol::{RequirementFamily, SymbolKey};
use super::task::{TaskId, TaskState};
use super::workspace::{
    DraftOperation, ProposalStatus, PromptObligationStatus, RequirementOrigin,
    RequirementProposal, RequirementRef, WorkspaceState,
};

/// One commit submission. The read-set is server-owned and never part
/// of the request (§27).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitRequest {
    pub task: TaskId,
    pub patch_id: PatchId,
    pub base_revision: Revision,
    pub patch: SpecPatch,

    /// Makes accidental duplicate submission idempotent: a repeated
    /// identical submission returns the previously committed result.
    pub client_nonce: Uuid,
}

/// A successful commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitReceipt {
    pub revision: Revision,

    /// Whether this receipt replays a previously committed submission
    /// recognized by its client nonce.
    pub replayed: bool,
}

/// Why a commit was refused. `is_stale_context` distinguishes
/// rejections an agent must not try to fix in its own session — the
/// harness restarts the task against a fresh snapshot (§33).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommitRejection {
    #[error("the task is unknown to this engine")]
    TaskUnknown,

    #[error("the task is {state}, not running")]
    TaskNotRunning { state: TaskState },

    #[error("the task was invalidated; a replacement task must be created")]
    TaskInvalidated,

    #[error("base revision {} does not match the task's snapshot revision {}", submitted.0, pinned.0)]
    BaseRevisionMismatch {
        pinned: Revision,
        submitted: Revision,
    },

    #[error("observed {symbol} changed since it was read")]
    ReadConflict {
        symbol: SymbolKey,
        observed: SemanticHash,
        current: Option<SemanticHash>,
    },

    #[error("write target {symbol} changed since the task's snapshot")]
    WriteConflict { symbol: SymbolKey },

    #[error("an observed query's result changed since it was run")]
    PhantomConflict { query: GraphQuery },

    #[error("an observed search's result changed since it was run")]
    SearchConflict,

    #[error("the patch references {symbol}, which the task never observed")]
    UnobservedDependency { symbol: SymbolKey },

    #[error("the task's write scope does not authorize writing {attempted}")]
    WriteScopeViolation { attempted: SymbolKey },

    #[error("the patch fails draft validation")]
    DraftValidationFailed { diagnostics: Vec<DraftDiagnostic> },
}

impl CommitRejection {
    /// Whether the failure means the task's context is stale. A
    /// stale-context rejection is terminal for the session: the agent
    /// must not retry, and the harness restarts the task against a
    /// fresh snapshot (§2.6, §33).
    pub fn is_stale_context(&self) -> bool {
        matches!(
            self,
            Self::TaskInvalidated
                | Self::BaseRevisionMismatch { .. }
                | Self::ReadConflict { .. }
                | Self::WriteConflict { .. }
                | Self::PhantomConflict { .. }
                | Self::SearchConflict
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftDiagnostic {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<SymbolKey>,

    pub message: String,
}

impl DraftDiagnostic {
    fn new(subject: Option<SymbolKey>, message: impl Into<String>) -> Self {
        Self {
            subject,
            message: message.into(),
        }
    }
}

/// One accepted commit, as persisted. Never chain-of-thought — only
/// operational metadata (§80).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitRecord {
    pub revision: Revision,
    pub parent: Revision,

    pub task: TaskId,
    pub patch_id: PatchId,
    pub client_nonce: Uuid,

    pub changed_symbols: Vec<SymbolKey>,

    pub timestamp_unix_ms: u64,

    pub backend: Option<AgentBackendMetadata>,
}

/// Which agent backend produced a commit, for the run manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBackendMetadata {
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// Applies a patch to a candidate workspace, mutation by mutation in
/// order, recording requirement proposals and adopting qualifying ones
/// per run policy. Returns every diagnostic; a non-empty result means
/// the candidate must be discarded.
pub fn apply_patch(workspace: &mut WorkspaceState, patch: &SpecPatch) -> Vec<DraftDiagnostic> {
    let mut diagnostics = Vec::new();

    for mutation in &patch.mutations {
        apply_mutation(workspace, mutation, &mut diagnostics);
    }

    if diagnostics.is_empty() {
        diagnostics.extend(check_patch(workspace, patch));
    }

    diagnostics
}

fn apply_mutation(
    workspace: &mut WorkspaceState,
    mutation: &Mutation,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    match mutation {
        Mutation::PutService { id, value } => {
            workspace.services.insert(id.clone(), value.clone());
        }

        Mutation::PutSchema { id, value } => {
            workspace.schemas.insert(id.clone(), value.clone());
        }

        Mutation::PutDataModel { id, value } => {
            workspace.data_models.insert(id.clone(), value.clone());
        }

        Mutation::PutTopic { id, value } => {
            workspace.topics.insert(id.clone(), value.clone());
        }

        Mutation::PutStateMachine { id, value } => {
            workspace.state_machines.insert(id.clone(), value.clone());
        }

        Mutation::PutOperationInterface { operation, value } => {
            match workspace.operations.get_mut(operation) {
                Some(draft) => {
                    draft.service = value.service.clone();
                    draft.description = value.description.clone();
                    draft.inputs = value.inputs.clone();
                    draft.recompute_stage();
                }

                None => {
                    workspace
                        .operations
                        .insert(operation.clone(), DraftOperation::planned(value.clone()));
                }
            }
        }

        Mutation::ReplaceOperationProgram { operation, program } => {
            match workspace.operations.get_mut(operation) {
                Some(draft) => {
                    draft.program = Some(program.clone());
                    draft.recompute_stage();
                }

                None => diagnostics.push(DraftDiagnostic::new(
                    Some(SymbolKey::Operation(operation.clone())),
                    format!("operation {operation} is not declared; plan its interface first"),
                )),
            }
        }

        Mutation::ReplaceOperationExecution {
            operation,
            execution,
        } => match workspace.operations.get_mut(operation) {
            Some(draft) => {
                draft.execution = Some(execution.clone());
                draft.recompute_stage();
            }

            None => diagnostics.push(DraftDiagnostic::new(
                Some(SymbolKey::Operation(operation.clone())),
                format!("operation {operation} is not declared; plan its interface first"),
            )),
        },

        Mutation::ReplaceOperationRequirements {
            operation,
            requirements,
        } => match workspace.operations.get_mut(operation) {
            Some(draft) => {
                draft.requirements = requirements.clone();
                draft.recompute_stage();
            }

            None => diagnostics.push(DraftDiagnostic::new(
                Some(SymbolKey::Operation(operation.clone())),
                format!("operation {operation} is not declared; plan its interface first"),
            )),
        },

        Mutation::ProposeRequirements {
            operation,
            proposals,
        } => {
            apply_proposals(workspace, operation, proposals, diagnostics);
        }

        Mutation::PutPromptObligation { id, value } => {
            workspace
                .prompt_obligations
                .insert(id.clone(), value.clone());
        }

        Mutation::DeleteTopLevel { symbol } => {
            let removed = match symbol {
                SymbolKey::Service(id) => workspace.services.remove(id).is_some(),
                SymbolKey::Schema(id) => workspace.schemas.remove(id).is_some(),
                SymbolKey::DataModel(id) => workspace.data_models.remove(id).is_some(),
                SymbolKey::Topic(id) => workspace.topics.remove(id).is_some(),
                SymbolKey::StateMachine(id) => workspace.state_machines.remove(id).is_some(),
                SymbolKey::Operation(id) => workspace.operations.remove(id).is_some(),
                SymbolKey::PromptObligation(id) => {
                    workspace.prompt_obligations.remove(id).is_some()
                }

                other => {
                    diagnostics.push(DraftDiagnostic::new(
                        Some(other.clone()),
                        format!("{other} is not a top-level symbol and cannot be deleted"),
                    ));

                    return;
                }
            };

            if !removed {
                diagnostics.push(DraftDiagnostic::new(
                    Some(symbol.clone()),
                    format!("{symbol} does not exist"),
                ));
            }
        }
    }
}

/// Records each submission as a proposal and adopts qualifying ones
/// per the run's adoption policy (§70): explicit prompt obligations
/// always adopt, strongly implied ones under strict policy,
/// recommendations only when the run opts in. Adoption of an
/// explicit-prompt proposal maps its obligation.
fn apply_proposals(
    workspace: &mut WorkspaceState,
    operation: &Id,
    proposals: &[RequirementSubmission],
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    if !workspace.operations.contains_key(operation) {
        diagnostics.push(DraftDiagnostic::new(
            Some(SymbolKey::Operation(operation.clone())),
            format!("operation {operation} is not declared; plan its interface first"),
        ));

        return;
    }

    for submission in proposals {
        if let RequirementOrigin::ExplicitPrompt { obligation } = &submission.origin
            && !workspace.prompt_obligations.contains_key(obligation)
        {
            diagnostics.push(DraftDiagnostic::new(
                Some(SymbolKey::PromptObligation(obligation.clone())),
                format!("prompt obligation {obligation} does not exist"),
            ));

            continue;
        }

        let policy = workspace.run_meta.policy;
        let draft = workspace
            .operations
            .get_mut(operation)
            .expect("checked above");

        let family = submission.requirement.family();
        let fingerprint = requirement_fingerprint(&submission.requirement);

        let existing = declared_fingerprints(draft, family)
            .into_iter()
            .position(|declared| declared == fingerprint);

        let status = if let Some(index) = existing {
            ProposalStatus::Duplicate {
                reference: RequirementRef {
                    operation: operation.clone(),
                    family,
                    index,
                },
            }
        } else {
            let adopt = match &submission.origin {
                RequirementOrigin::ExplicitPrompt { .. } => true,
                RequirementOrigin::StronglyImplied { .. } => policy.strict_requirements,
                RequirementOrigin::Recommended { .. } => policy.adopt_recommended,
            };

            if adopt {
                let index = adopt_requirement(draft, &submission.requirement);

                let reference = RequirementRef {
                    operation: operation.clone(),
                    family,
                    index,
                };

                if let RequirementOrigin::ExplicitPrompt { obligation } = &submission.origin
                    && let Some(obligation) = workspace.prompt_obligations.get_mut(obligation)
                {
                    match &mut obligation.status {
                        PromptObligationStatus::Unmapped => {
                            obligation.status = PromptObligationStatus::Mapped {
                                requirements: vec![reference.clone()],
                            };
                        }

                        PromptObligationStatus::Mapped { requirements } => {
                            requirements.push(reference.clone());
                        }

                        // An unsupported or waived obligation keeps its
                        // status; the proposal is still recorded.
                        _ => {}
                    }
                }

                ProposalStatus::Adopted { reference }
            } else {
                ProposalStatus::Advisory
            }
        };

        workspace.requirement_proposals.push(RequirementProposal {
            operation: operation.clone(),
            requirement: submission.requirement.clone(),
            origin: submission.origin.clone(),
            status,
        });
    }

    if let Some(draft) = workspace.operations.get_mut(operation) {
        draft.recompute_stage();
    }
}

fn requirement_fingerprint(requirement: &super::workspace::ProposedRequirement) -> SemanticHash {
    use super::workspace::ProposedRequirement;

    match requirement {
        ProposedRequirement::Serialization(requirement) => SemanticHash::of(requirement),
        ProposedRequirement::Ordering(requirement) => SemanticHash::of(requirement),
        ProposedRequirement::Idempotency(requirement) => SemanticHash::of(requirement),
        ProposedRequirement::Recoverability(requirement) => SemanticHash::of(requirement),
    }
}

fn declared_fingerprints(draft: &DraftOperation, family: RequirementFamily) -> Vec<SemanticHash> {
    match family {
        RequirementFamily::Serialization => draft
            .requirements
            .serialization
            .iter()
            .map(SemanticHash::of)
            .collect(),

        RequirementFamily::Ordering => draft
            .requirements
            .ordering
            .iter()
            .map(SemanticHash::of)
            .collect(),

        RequirementFamily::Idempotency | RequirementFamily::ResultReplay => draft
            .requirements
            .idempotency
            .iter()
            .map(SemanticHash::of)
            .collect(),

        RequirementFamily::Recoverability => draft
            .requirements
            .recoverability
            .iter()
            .map(SemanticHash::of)
            .collect(),
    }
}

fn adopt_requirement(
    draft: &mut DraftOperation,
    requirement: &super::workspace::ProposedRequirement,
) -> usize {
    use super::workspace::ProposedRequirement;

    match requirement {
        ProposedRequirement::Serialization(requirement) => {
            draft.requirements.serialization.push(requirement.clone());
            draft.requirements.serialization.len() - 1
        }

        ProposedRequirement::Ordering(requirement) => {
            draft.requirements.ordering.push(requirement.clone());
            draft.requirements.ordering.len() - 1
        }

        ProposedRequirement::Idempotency(requirement) => {
            draft.requirements.idempotency.push(requirement.clone());
            draft.requirements.idempotency.len() - 1
        }

        ProposedRequirement::Recoverability(requirement) => {
            draft.requirements.recoverability.push(requirement.clone());
            draft.requirements.recoverability.len() - 1
        }
    }
}

/// Draft-local checks over the mutated slices of an applied candidate:
/// ID uniqueness and reference resolution, with precise diagnostics.
/// Deliberately shallower than the validator — full structural
/// coherence is analysis, not the commit gate.
fn check_patch(candidate: &WorkspaceState, patch: &SpecPatch) -> Vec<DraftDiagnostic> {
    let mut diagnostics = Vec::new();

    for mutation in &patch.mutations {
        match mutation {
            Mutation::PutSchema { id, value } => {
                check_schema(candidate, id, value, &mut diagnostics);
            }

            Mutation::PutDataModel { id, value } => {
                for (object_id, object) in &value.objects {
                    require_schema(candidate, &object.schema, &mut diagnostics, || {
                        format!("object {object_id} of data model {id}")
                    });
                }
            }

            Mutation::PutTopic { id, value } => {
                for message in &value.messages {
                    require_schema(candidate, message, &mut diagnostics, || {
                        format!("topic {id}")
                    });
                }
            }

            Mutation::PutStateMachine { id, value } => {
                check_state_machine(candidate, id, value, &mut diagnostics);
            }

            Mutation::PutOperationInterface { operation, value } => {
                if !candidate.services.contains_key(&value.service) {
                    diagnostics.push(DraftDiagnostic::new(
                        Some(SymbolKey::Service(value.service.clone())),
                        format!(
                            "operation {operation} names service {}, which is not declared",
                            value.service
                        ),
                    ));
                }

                for (input_id, input) in &value.inputs {
                    check_input(candidate, operation, input_id, input, &mut diagnostics);
                }
            }

            Mutation::ReplaceOperationProgram { operation, program } => {
                check_program(candidate, operation, program, &mut diagnostics);
            }

            Mutation::ReplaceOperationRequirements { operation, .. }
            | Mutation::ProposeRequirements { operation, .. } => {
                check_requirement_roots(candidate, operation, &mut diagnostics);
            }

            _ => {}
        }
    }

    // For any program written by this patch, run the validator's own
    // reference-resolution and definite-availability passes over a probe
    // of the shared symbols plus that one operation, so a dangling effect
    // intent, result, or transaction binding — or a value that is not
    // definitely available — is rejected here and fixed in-session,
    // rather than committing and only failing whole-model validation
    // asynchronously (§8.1). Reusing the validator keeps the gate from
    // drifting from the authority; the operation-local error filter makes
    // it sound over a model that omits sibling operations.
    let programs_written: std::collections::BTreeSet<&Id> = patch
        .mutations
        .iter()
        .filter_map(|mutation| match mutation {
            Mutation::ReplaceOperationProgram { operation, .. } => Some(operation),
            _ => None,
        })
        .collect();

    for operation in programs_written {
        let Some(model) = probe_model(candidate, operation) else {
            continue;
        };

        for diagnostic in crate::analyzer::validation::program_local_diagnostics(&model, operation) {
            diagnostics.push(DraftDiagnostic::new(
                Some(SymbolKey::OperationProgram(operation.clone())),
                diagnostic.message,
            ));
        }
    }

    diagnostics
}

/// A minimal, deliberately partial `Model` for draft-time validation of
/// one operation's program: every committed shared symbol plus the one
/// operation, whose program was just written. Sibling operations are
/// omitted — the draft state is not assemblable mid-fanout — so only the
/// operation-local diagnostics of [`program_local_diagnostics`] are
/// sound over it. Execution facts, which the reference and dataflow
/// passes never read, are stubbed when the draft has none yet.
fn probe_model(candidate: &WorkspaceState, operation: &Id) -> Option<Model> {
    let draft = candidate.operations.get(operation)?;
    let program = draft.program.clone()?;

    let assembled = Operation {
        service: draft.service.clone(),
        description: draft.description.clone(),
        inputs: draft.inputs.clone(),
        program,
        requirements: draft.requirements.clone(),
        execution: draft.execution.clone().unwrap_or(ExecutionSemantics {
            concurrency: OperationConcurrency::Unspecified,
        }),
    };

    let mut operations = std::collections::BTreeMap::new();
    operations.insert(operation.clone(), assembled);

    Some(Model {
        revision: candidate.revision,
        services: candidate.services.clone(),
        schemas: candidate.schemas.clone(),
        data_models: candidate.data_models.clone(),
        topics: candidate.topics.clone(),
        state_machines: candidate.state_machines.clone(),
        operations,
    })
}

fn check_schema(
    candidate: &WorkspaceState,
    id: &Id,
    schema: &Schema,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    match schema {
        Schema::Canonical(canonical) => {
            for (field, declaration) in &canonical.fields {
                let mut referenced = Vec::new();

                collect_type_refs(&declaration.ty, &mut referenced);

                for target in referenced {
                    require_schema(candidate, target, diagnostics, || {
                        format!("field {field} of schema {id}")
                    });
                }
            }
        }

        Schema::Fragment(fragment) => {
            require_schema(candidate, &fragment.source, diagnostics, || {
                format!("fragment {id}")
            });
        }
    }
}

fn check_state_machine(
    candidate: &WorkspaceState,
    id: &Id,
    machine: &crate::spec::StateMachine,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    if !machine.states.contains(&machine.initial) {
        diagnostics.push(DraftDiagnostic::new(
            Some(SymbolKey::StateMachine(id.clone())),
            format!(
                "initial state {} is not among the machine's states",
                machine.initial
            ),
        ));
    }

    let StateMachineSubject::Object { object, .. } = &machine.subject;

    let declared_somewhere = candidate
        .data_models
        .values()
        .any(|data_model| data_model.objects.contains_key(object));

    if !declared_somewhere {
        diagnostics.push(DraftDiagnostic::new(
            Some(SymbolKey::StateMachine(id.clone())),
            format!("subject object {object} is not declared by any data model"),
        ));
    }

    for (transition_id, transition) in &machine.transitions {
        for state in transition.from.iter().chain([&transition.to]) {
            if !machine.states.contains(state) {
                diagnostics.push(DraftDiagnostic::new(
                    Some(SymbolKey::Transition {
                        machine: id.clone(),
                        transition: transition_id.clone(),
                    }),
                    format!("transition {transition_id} names undeclared state {state}"),
                ));
            }
        }

        for side_effect in transition.side_effects.values() {
            match side_effect {
                TransitionSideEffect::Publication(publication) => {
                    require_topic(candidate, &publication.topic, diagnostics, || {
                        format!("transition {transition_id} of {id}")
                    });

                    require_schema(candidate, &publication.schema, diagnostics, || {
                        format!("transition {transition_id} of {id}")
                    });
                }

                TransitionSideEffect::Request(request) => {
                    require_request_target(
                        candidate,
                        &request.target.operation,
                        &request.target.input,
                        diagnostics,
                        || format!("transition {transition_id} of {id}"),
                    );

                    require_schema(candidate, &request.schema, diagnostics, || {
                        format!("transition {transition_id} of {id}")
                    });
                }
            }
        }
    }
}

fn check_input(
    candidate: &WorkspaceState,
    operation: &Id,
    input_id: &Id,
    input: &Input,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    match input {
        Input::Request(request) => {
            for schema in [
                &request.schema,
                &request.result.ok,
                &request.result.err.schema,
            ] {
                require_schema(candidate, schema, diagnostics, || {
                    format!("input {input_id} of {operation}")
                });
            }
        }

        Input::Subscription(subscription) => {
            let topic = candidate.topics.get(&subscription.topic);

            if topic.is_none() {
                diagnostics.push(DraftDiagnostic::new(
                    Some(SymbolKey::Topic(subscription.topic.clone())),
                    format!(
                        "input {input_id} of {operation} subscribes to {}, which is not declared",
                        subscription.topic
                    ),
                ));
            }

            if let (MessageSelector::Only(schemas), Some(topic)) = (&subscription.messages, topic) {
                for schema in schemas {
                    if !topic.messages.contains(schema) {
                        diagnostics.push(DraftDiagnostic::new(
                            Some(SymbolKey::Schema(schema.clone())),
                            format!(
                                "input {input_id} of {operation} selects {schema}, which {} does not carry",
                                subscription.topic
                            ),
                        ));
                    }
                }
            }
        }
    }
}

fn check_program(
    candidate: &WorkspaceState,
    operation: &Id,
    program: &crate::spec::OperationBlock,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    let Some(draft) = candidate.operations.get(operation) else {
        // Application already diagnosed the missing operation.
        return;
    };

    let mut transaction_ids = Vec::new();
    let mut effect_ids = Vec::new();
    let mut binding_ids = Vec::new();

    let check_effect = |site: &Id, effect: &Effect, diagnostics: &mut Vec<DraftDiagnostic>| {
        match effect {
            Effect::Publication(publication) => {
                require_topic(candidate, &publication.topic, diagnostics, || {
                    format!("effect {site} of {operation}")
                });

                require_schema(candidate, &publication.schema, diagnostics, || {
                    format!("effect {site} of {operation}")
                });
            }

            Effect::Request(request) => {
                require_request_target(
                    candidate,
                    &request.target.operation,
                    &request.target.input,
                    diagnostics,
                    || format!("effect {site} of {operation}"),
                );

                require_schema(candidate, &request.schema, diagnostics, || {
                    format!("effect {site} of {operation}")
                });
            }

            Effect::External(external) => {
                if let Some(result) = &external.result {
                    for schema in [&result.ok, &result.err.schema] {
                        require_schema(candidate, schema, diagnostics, || {
                            format!("effect {site} of {operation}")
                        });
                    }
                }
            }
        }
    };

    for (location, step) in program.steps_with_locations() {
        match step {
            OperationStep::Transaction(transaction) => {
                transaction_ids.push(transaction.id.clone());

                if let Some(data_model_id) = &transaction.data_model {
                    let data_model = candidate.data_models.get(data_model_id);

                    if data_model.is_none() {
                        diagnostics.push(DraftDiagnostic::new(
                            Some(SymbolKey::DataModel(data_model_id.clone())),
                            format!(
                                "transaction {} of {operation} names data model {data_model_id}, which is not declared",
                                transaction.id
                            ),
                        ));
                    }

                    for inner in &transaction.steps {
                        let object = match inner {
                            TransactionStep::Read(read) => Some(&read.target.object),
                            TransactionStep::Write(write) => Some(&write.target.object),
                            TransactionStep::Insert(insert) => Some(&insert.object),
                            TransactionStep::Delete(delete) => Some(&delete.target.object),
                            TransactionStep::Lock(lock) => Some(&lock.target.object),
                            TransactionStep::Transition(transition) => {
                                Some(&transition.subject.object)
                            }
                            _ => None,
                        };

                        if let (Some(object), Some(data_model)) = (object, data_model)
                            && !data_model.objects.contains_key(object)
                        {
                            diagnostics.push(DraftDiagnostic::new(
                                Some(SymbolKey::DataObject {
                                    data_model: data_model_id.clone(),
                                    object: object.clone(),
                                }),
                                format!(
                                    "transaction {} of {operation} accesses object {object}, which {data_model_id} does not declare",
                                    transaction.id
                                ),
                            ));
                        }
                    }
                }

                for inner in &transaction.steps {
                    match inner {
                        TransactionStep::Transition(transition) => {
                            match candidate.state_machines.get(&transition.machine) {
                                None => diagnostics.push(DraftDiagnostic::new(
                                    Some(SymbolKey::StateMachine(transition.machine.clone())),
                                    format!(
                                        "transaction {} of {operation} applies machine {}, which is not declared",
                                        transaction.id, transition.machine
                                    ),
                                )),

                                Some(machine) => match machine
                                    .transitions
                                    .get(&transition.transition)
                                {
                                    None => diagnostics.push(DraftDiagnostic::new(
                                        Some(SymbolKey::Transition {
                                            machine: transition.machine.clone(),
                                            transition: transition.transition.clone(),
                                        }),
                                        format!(
                                            "transaction {} of {operation} applies transition {}, which {} does not declare",
                                            transaction.id,
                                            transition.transition,
                                            transition.machine
                                        ),
                                    )),

                                    Some(declared) => {
                                        let declared_effects: Vec<&Id> =
                                            declared.side_effects.keys().collect();
                                        let supplied: Vec<&Id> =
                                            transition.effect_intents.keys().collect();

                                        if declared_effects != supplied {
                                            diagnostics.push(DraftDiagnostic::new(
                                                Some(SymbolKey::Transition {
                                                    machine: transition.machine.clone(),
                                                    transition: transition.transition.clone(),
                                                }),
                                                format!(
                                                    "transaction {} of {operation} supplies intents for [{}], but transition {} declares side effects [{}]",
                                                    transaction.id,
                                                    supplied
                                                        .iter()
                                                        .map(|id| id.to_string())
                                                        .collect::<Vec<_>>()
                                                        .join(", "),
                                                    transition.transition,
                                                    declared_effects
                                                        .iter()
                                                        .map(|id| id.to_string())
                                                        .collect::<Vec<_>>()
                                                        .join(", "),
                                                ),
                                            ));
                                        }

                                        for intent in transition.effect_intents.values() {
                                            binding_ids.push(intent.bind.clone());
                                        }
                                    }
                                },
                            }
                        }

                        TransactionStep::EstablishEffectIntent(establish) => {
                            effect_ids.push(establish.effect_id.clone());
                            binding_ids.push(establish.bind.clone());
                            check_effect(&establish.effect_id, &establish.effect, diagnostics);
                        }

                        TransactionStep::EstablishTransactionOutput(establish) => {
                            binding_ids.push(establish.bind.clone());

                            require_schema(candidate, &establish.schema, diagnostics, || {
                                format!("output {} of {operation}", establish.bind)
                            });
                        }

                        _ => {}
                    }

                    for root in inner.roots() {
                        if let ValueSource::Input(input) = &root.source
                            && !draft.inputs.contains_key(input)
                        {
                            diagnostics.push(DraftDiagnostic::new(
                                Some(SymbolKey::Input {
                                    operation: operation.clone(),
                                    input: input.clone(),
                                }),
                                format!(
                                    "step {location} of {operation} references input {input}, which the operation does not declare"
                                ),
                            ));
                        }
                    }
                }
            }

            OperationStep::ExecuteEffect(execute) => {
                effect_ids.push(execute.effect_id.clone());

                if let Some(bind) = &execute.bind {
                    binding_ids.push(bind.clone());
                }

                check_effect(&execute.effect_id, &execute.effect, diagnostics);
            }

            OperationStep::ExecuteEffectIntent(execute) => {
                if let Some(bind) = &execute.bind {
                    binding_ids.push(bind.clone());
                }
            }

            OperationStep::Return(returned) => {
                if !draft.inputs.contains_key(&returned.request) {
                    diagnostics.push(DraftDiagnostic::new(
                        Some(SymbolKey::Input {
                            operation: operation.clone(),
                            input: returned.request.clone(),
                        }),
                        format!(
                            "step {location} of {operation} returns for input {}, which the operation does not declare",
                            returned.request
                        ),
                    ));
                }
            }

            _ => {}
        }

        let step_roots: Vec<&crate::spec::ValueRef> = match step {
            OperationStep::ExecuteEffect(execute) => {
                let mut roots = execute.values.roots();

                roots.extend(execute.effect.roots());

                roots
            }

            OperationStep::Return(returned) => returned.outcome.values().roots(),

            OperationStep::Branch(branch) => branch.condition.roots(),

            _ => Vec::new(),
        };

        for root in step_roots {
            if let ValueSource::Input(input) = &root.source
                && !draft.inputs.contains_key(input)
            {
                diagnostics.push(DraftDiagnostic::new(
                    Some(SymbolKey::Input {
                        operation: operation.clone(),
                        input: input.clone(),
                    }),
                    format!(
                        "step {location} of {operation} references input {input}, which the operation does not declare"
                    ),
                ));
            }
        }
    }

    for (label, ids) in [
        ("transaction", transaction_ids),
        ("effect", effect_ids),
        ("binding", binding_ids),
    ] {
        let mut seen = std::collections::BTreeSet::new();

        for id in ids {
            if !seen.insert(id.clone()) {
                diagnostics.push(DraftDiagnostic::new(
                    Some(SymbolKey::Operation(operation.clone())),
                    format!("duplicate {label} id {id} in the program of {operation}"),
                ));
            }
        }
    }
}

fn check_requirement_roots(
    candidate: &WorkspaceState,
    operation: &Id,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    let Some(draft) = candidate.operations.get(operation) else {
        return;
    };

    let mut roots: Vec<&crate::spec::ValueRef> = Vec::new();

    for requirement in &draft.requirements.serialization {
        roots.push(&requirement.key);
    }

    for requirement in &draft.requirements.ordering {
        roots.push(&requirement.key);
    }

    for requirement in &draft.requirements.idempotency {
        roots.extend(requirement.key.components.iter());
    }

    for requirement in &draft.requirements.recoverability {
        roots.extend(requirement.key.components.iter());
    }

    for input in super::patch::input_roots(&roots) {
        if !draft.inputs.contains_key(&input) {
            diagnostics.push(DraftDiagnostic::new(
                Some(SymbolKey::Input {
                    operation: operation.clone(),
                    input: input.clone(),
                }),
                format!(
                    "a requirement of {operation} is keyed from input {input}, which the operation does not declare"
                ),
            ));
        }
    }
}

fn require_schema(
    candidate: &WorkspaceState,
    schema: &Id,
    diagnostics: &mut Vec<DraftDiagnostic>,
    context: impl FnOnce() -> String,
) {
    if !candidate.schemas.contains_key(schema) {
        diagnostics.push(DraftDiagnostic::new(
            Some(SymbolKey::Schema(schema.clone())),
            format!("{} references schema {schema}, which is not declared", context()),
        ));
    }
}

fn require_topic(
    candidate: &WorkspaceState,
    topic: &Id,
    diagnostics: &mut Vec<DraftDiagnostic>,
    context: impl FnOnce() -> String,
) {
    if !candidate.topics.contains_key(topic) {
        diagnostics.push(DraftDiagnostic::new(
            Some(SymbolKey::Topic(topic.clone())),
            format!("{} references topic {topic}, which is not declared", context()),
        ));
    }
}

fn require_request_target(
    candidate: &WorkspaceState,
    operation: &Id,
    input: &Id,
    diagnostics: &mut Vec<DraftDiagnostic>,
    context: impl FnOnce() -> String,
) {
    match candidate.operations.get(operation) {
        None => diagnostics.push(DraftDiagnostic::new(
            Some(SymbolKey::Operation(operation.clone())),
            format!(
                "{} targets operation {operation}, which is not declared or planned",
                context()
            ),
        )),

        Some(target) => {
            if !target.inputs.contains_key(input) {
                diagnostics.push(DraftDiagnostic::new(
                    Some(SymbolKey::Input {
                        operation: operation.clone(),
                        input: input.clone(),
                    }),
                    format!(
                        "{} targets input {input} of {operation}, which its interface does not declare",
                        context()
                    ),
                ));
            }
        }
    }
}

fn collect_type_refs<'a>(ty: &'a TypeRef, out: &mut Vec<&'a Id>) {
    match ty {
        TypeRef::Scalar(_) => {}
        TypeRef::Schema(id) => out.push(id),
        TypeRef::List(inner) => collect_type_refs(inner, out),
    }
}
