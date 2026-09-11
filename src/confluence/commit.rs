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
    Effect, Id, Input, MessageSelector, Model, Operation, TopicRuntime,
    OperationStep, Revision, Schema, StateMachineSubject, TransactionStep, TransitionSideEffect,
    TypeRef, ValueSource,
};

use super::fingerprint::SemanticHash;
use super::graph_query::GraphQuery;
use super::patch::{Mutation, PatchId, RequirementSubmission, SpecPatch};
use super::symbol::{RequirementFamily, SymbolKey};
use super::task::{TaskId, TaskState};
use super::workspace::{
    DraftOperation, OperationInterfaceDraft, ProposalStatus, PromptObligationStatus,
    RequirementOrigin, RequirementProposal, RequirementRef, WorkspaceState,
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

/// Whether the committed skeleton — the shared symbols and the
/// operation interfaces — is coherent enough to synthesize programs
/// against, judged exactly as the commit gate judges a patch by
/// replaying the skeleton through the same checks.
///
/// This is the precondition for fanning out: every worker writes its
/// program against these declarations, so launching over a skeleton
/// with unresolved references sends all of them at once to build on
/// sand. An interface with no inputs counts as a gap — it has no
/// trigger, so it is a placeholder rather than a contract a worker
/// could implement.
pub fn skeleton_diagnostics(workspace: &WorkspaceState) -> Vec<DraftDiagnostic> {
    let mut mutations: Vec<Mutation> = Vec::new();

    for (id, value) in &workspace.schemas {
        mutations.push(Mutation::PutSchema {
            id: id.clone(),
            value: value.clone(),
        });
    }

    for (id, value) in &workspace.data_models {
        mutations.push(Mutation::PutDataModel {
            id: id.clone(),
            value: value.clone(),
        });
    }

    for (id, value) in &workspace.topics {
        mutations.push(Mutation::PutTopic {
            id: id.clone(),
            value: value.clone(),
        });
    }

    for (id, value) in &workspace.state_machines {
        mutations.push(Mutation::PutStateMachine {
            id: id.clone(),
            value: value.clone(),
        });
    }

    for (operation, draft) in &workspace.operations {
        mutations.push(Mutation::PutOperationInterface {
            operation: operation.clone(),
            value: OperationInterfaceDraft {
                service: draft.service.clone(),
                description: draft.description.clone(),
                inputs: draft.inputs.clone(),
            },
        });
    }

    // Whatever runtime topology already exists is checked here too.
    // L1 is normally authored after the fan-out, once verification has
    // said what it must discharge, so usually there is none yet. When
    // an interactive author has declared some early, whole-model
    // validation cannot reach it until every operation has a program —
    // so without this it would go unchecked across the whole fan-out
    // window, and a broken declaration is a mistake every concurrent
    // worker inherits at once.
    for (topic, value) in &workspace.runtime.topics {
        mutations.push(Mutation::PutTopicRuntime {
            topic: topic.clone(),
            value: value.clone(),
        });
    }

    for (id, value) in &workspace.runtime.execution_pools {
        mutations.push(Mutation::PutExecutionPool {
            id: id.clone(),
            value: value.clone(),
        });
    }

    for (operation, inputs) in &workspace.runtime.subscriptions {
        for (input, value) in inputs {
            mutations.push(Mutation::PutSubscriptionRuntime {
                operation: operation.clone(),
                input: input.clone(),
                value: value.clone(),
            });
        }
    }

    for (operation, inputs) in &workspace.runtime.outboxes {
        for (input, value) in inputs {
            mutations.push(Mutation::PutOutboxRuntime {
                operation: operation.clone(),
                input: input.clone(),
                value: value.clone(),
            });
        }
    }

    for (id, value) in &workspace.runtime.routers {
        mutations.push(Mutation::PutRouter {
            id: id.clone(),
            value: value.clone(),
        });
    }

    for (id, value) in &workspace.runtime.storage_layouts {
        mutations.push(Mutation::PutStorageLayout {
            id: id.clone(),
            value: value.clone(),
        });
    }

    let mut diagnostics = check_patch(workspace, &SpecPatch { mutations });

    for (operation, draft) in &workspace.operations {
        if draft.inputs.is_empty() {
            diagnostics.push(DraftDiagnostic::new(
                Some(SymbolKey::OperationInterface(operation.clone())),
                format!(
                    "operation {operation} declares no inputs, so nothing can invoke it; \
                     give it a request or subscription input before its program is written"
                ),
            ));
        }
    }

    diagnostics
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

        Mutation::PutTopicRuntime { topic, value } => {
            workspace.runtime.topics.insert(topic.clone(), value.clone());
        }

        Mutation::PutSubscriptionRuntime {
            operation,
            input,
            value,
        } => {
            workspace
                .runtime
                .subscriptions
                .entry(operation.clone())
                .or_default()
                .insert(input.clone(), value.clone());
        }

        Mutation::PutOutboxRuntime {
            operation,
            input,
            value,
        } => {
            workspace
                .runtime
                .outboxes
                .entry(operation.clone())
                .or_default()
                .insert(input.clone(), value.clone());
        }

        Mutation::PutExecutionPool { id, value } => {
            workspace
                .runtime
                .execution_pools
                .insert(id.clone(), value.clone());
        }

        Mutation::PutRouter { id, value } => {
            workspace.runtime.routers.insert(id.clone(), value.clone());
        }

        Mutation::PutStorageLayout { id, value } => {
            workspace
                .runtime
                .storage_layouts
                .insert(id.clone(), value.clone());
        }

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

                SymbolKey::TopicRuntime(id) => workspace.runtime.topics.remove(id).is_some(),

                SymbolKey::SubscriptionRuntime { operation, input } => {
                    let removed = workspace
                        .runtime
                        .subscriptions
                        .get_mut(operation)
                        .and_then(|inputs| inputs.remove(input))
                        .is_some();

                    // An empty per-operation map would keep
                    // `RuntimeModel::is_empty` false, so a workspace
                    // that now declares no runtime facts would still
                    // assemble and export a `runtime:` block.
                    workspace
                        .runtime
                        .subscriptions
                        .retain(|_, inputs| !inputs.is_empty());

                    removed
                }

                SymbolKey::OutboxRuntime { operation, input } => {
                    let removed = workspace
                        .runtime
                        .outboxes
                        .get_mut(operation)
                        .and_then(|inputs| inputs.remove(input))
                        .is_some();

                    // Same emptiness rule as subscription runtimes.
                    workspace
                        .runtime
                        .outboxes
                        .retain(|_, inputs| !inputs.is_empty());

                    removed
                }

                SymbolKey::ExecutionPool(id) => {
                    workspace.runtime.execution_pools.remove(id).is_some()
                }

                SymbolKey::Router(id) => workspace.runtime.routers.remove(id).is_some(),

                SymbolKey::StorageLayout(id) => {
                    workspace.runtime.storage_layouts.remove(id).is_some()
                }

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

            Mutation::PutRouter { id, value } => {
                check_router(candidate, id, value, &mut diagnostics);
            }

            Mutation::PutSubscriptionRuntime {
                operation,
                input,
                value,
            } => {
                check_subscription_runtime(candidate, operation, input, value, &mut diagnostics);
            }

            Mutation::PutOutboxRuntime {
                operation,
                input,
                value,
            } => {
                check_outbox_runtime(candidate, operation, input, value, &mut diagnostics);
            }

            Mutation::PutTopicRuntime { topic, value } => {
                check_topic_runtime(candidate, topic, value, &mut diagnostics);
            }

            Mutation::PutStorageLayout { id, value } => {
                check_storage_layout(candidate, id, value, &mut diagnostics);
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
/// sound over it. The runtime model rides along unchanged: the
/// program-local passes never read it, and carrying it costs nothing.
fn probe_model(candidate: &WorkspaceState, operation: &Id) -> Option<Model> {
    let draft = candidate.operations.get(operation)?;
    let program = draft.program.clone()?;

    let assembled = Operation {
        service: draft.service.clone(),
        description: draft.description.clone(),
        inputs: draft.inputs.clone(),
        program,
        requirements: draft.requirements.clone(),
    };

    let mut operations = std::collections::BTreeMap::new();
    operations.insert(operation.clone(), assembled);

    Some(Model {
        dsl: crate::spec::DSL_VERSION,
        revision: candidate.revision,
        services: candidate.services.clone(),
        schemas: candidate.schemas.clone(),
        data_models: candidate.data_models.clone(),
        topics: candidate.topics.clone(),
        state_machines: candidate.state_machines.clone(),
        operations,
        runtime: (!candidate.runtime.is_empty()).then(|| candidate.runtime.clone()),
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

        Input::Outbox(declared) => {
            let outbox = find_outbox(candidate, &declared.outbox);

            if outbox.is_none() {
                diagnostics.push(DraftDiagnostic::new(
                    Some(SymbolKey::DataModel(declared.outbox.clone())),
                    format!(
                        "input {input_id} of {operation} consumes outbox {}, which no data model declares",
                        declared.outbox
                    ),
                ));
            }

            if let (MessageSelector::Only(schemas), Some((_, outbox))) =
                (&declared.messages, outbox)
            {
                for schema in schemas {
                    if !outbox.messages.contains(schema) {
                        diagnostics.push(DraftDiagnostic::new(
                            Some(SymbolKey::Schema(schema.clone())),
                            format!(
                                "input {input_id} of {operation} selects {schema}, which outbox {} does not admit",
                                declared.outbox
                            ),
                        ));
                    }
                }
            }
        }
    }
}

/// The named outbox and its owning data model, resolved across the
/// candidate's data models.
fn find_outbox<'a>(
    candidate: &'a WorkspaceState,
    outbox: &Id,
) -> Option<(&'a Id, &'a crate::spec::Outbox)> {
    candidate
        .data_models
        .iter()
        .find_map(|(data_model_id, data_model)| {
            data_model
                .outboxes
                .get(outbox)
                .map(|declared| (data_model_id, declared))
        })
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
    let mut handle_ids = Vec::new();

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

            // Legal only as a transaction's `write_outbox` step — the
            // whole-model validator rejects this site — but its
            // references are still checked so both defects surface at
            // once.
            Effect::OutboxWrite(write) => {
                if find_outbox(candidate, &write.outbox).is_none() {
                    diagnostics.push(DraftDiagnostic::new(
                        Some(SymbolKey::DataModel(write.outbox.clone())),
                        format!(
                            "effect {site} of {operation} targets outbox {}, which no data model declares",
                            write.outbox
                        ),
                    ));
                }

                require_schema(candidate, &write.schema, diagnostics, || {
                    format!("effect {site} of {operation}")
                });
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

                        TransactionStep::WriteOutbox(write) => {
                            effect_ids.push(write.effect_id.clone());

                            require_schema(candidate, &write.effect.schema, diagnostics, || {
                                format!("outbox write {} of {operation}", write.effect_id)
                            });

                            match find_outbox(candidate, &write.effect.outbox) {
                                None => diagnostics.push(DraftDiagnostic::new(
                                    Some(SymbolKey::DataModel(write.effect.outbox.clone())),
                                    format!(
                                        "outbox write {} of {operation} targets outbox {}, which no data model declares",
                                        write.effect_id, write.effect.outbox
                                    ),
                                )),

                                Some((owner, outbox)) => {
                                    if transaction.data_model.as_ref() != Some(owner) {
                                        diagnostics.push(DraftDiagnostic::new(
                                            Some(SymbolKey::DataModel(owner.clone())),
                                            format!(
                                                "outbox write {} of {operation} targets outbox {} of data model {owner}, but transaction {} declares {}",
                                                write.effect_id,
                                                write.effect.outbox,
                                                transaction.id,
                                                transaction
                                                    .data_model
                                                    .as_ref()
                                                    .map(|id| id.to_string())
                                                    .unwrap_or_else(|| "no data model".to_string()),
                                            ),
                                        ));
                                    }

                                    if !outbox.messages.contains(&write.effect.schema) {
                                        diagnostics.push(DraftDiagnostic::new(
                                            Some(SymbolKey::Schema(write.effect.schema.clone())),
                                            format!(
                                                "outbox write {} of {operation} admits {}, which outbox {} does not admit",
                                                write.effect_id,
                                                write.effect.schema,
                                                write.effect.outbox
                                            ),
                                        ));
                                    }
                                }
                            }
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

            OperationStep::ExecuteEffectAsync(execute) => {
                effect_ids.push(execute.effect_id.clone());
                handle_ids.push(execute.handle.clone());

                check_effect(&execute.effect_id, &execute.effect, diagnostics);
            }

            OperationStep::ExecuteEffectIntent(execute) => {
                if let Some(bind) = &execute.bind {
                    binding_ids.push(bind.clone());
                }
            }

            OperationStep::ExecuteEffectIntentAsync(execute) => {
                handle_ids.push(execute.handle.clone());
            }

            OperationStep::JoinAll(join) => {
                for entry in &join.handles {
                    if let Some(bind) = &entry.bind {
                        binding_ids.push(bind.clone());
                    }
                }
            }

            OperationStep::Race(race) => {
                if let Some(bind) = &race.bind {
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

            OperationStep::ExecuteEffectAsync(execute) => {
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
        ("async handle", handle_ids),
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

/// A router must name a request boundary that exists and a pool the
/// runtime declares.
///
/// Whole-model validation would catch both asynchronously; catching
/// them at the gate lets the authoring agent fix them in the same
/// session, which is the point of the draft checks.
fn check_router(
    candidate: &WorkspaceState,
    router: &Id,
    value: &crate::spec::Router,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    check_boundary(
        candidate,
        SymbolKey::Router(router.clone()),
        &value.boundary.operation,
        &value.boundary.input,
        BoundaryKind::Request,
        diagnostics,
    );

    check_pool(
        candidate,
        SymbolKey::Router(router.clone()),
        &value.pool,
        diagnostics,
    );
}

fn check_subscription_runtime(
    candidate: &WorkspaceState,
    operation: &Id,
    input: &Id,
    value: &crate::spec::SubscriptionRuntime,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    let subject = SymbolKey::SubscriptionRuntime {
        operation: operation.clone(),
        input: input.clone(),
    };

    check_boundary(
        candidate,
        subject.clone(),
        operation,
        input,
        BoundaryKind::Subscription,
        diagnostics,
    );

    check_pool(candidate, subject.clone(), &value.dispatch.pool, diagnostics);

    // The scope invariant, checked here rather than left to
    // whole-model validation: a violation committed during fan-out
    // poisons the head for every task, and the agent that caused it has
    // already finished by the time analysis reports.
    if !value.declares_transport_semantics() {
        return;
    }

    let Some(Input::Subscription(subscription)) = candidate
        .operations
        .get(operation)
        .and_then(|draft| draft.inputs.get(input))
    else {
        return;
    };

    if candidate
        .runtime
        .topics
        .get(&subscription.topic)
        .is_some_and(TopicRuntime::declares_transport_semantics)
    {
        diagnostics.push(DraftDiagnostic::new(
            Some(subject),
            format!(
                "{} already declares transport semantics for every subscription of it, \
                 so this one may not declare its own",
                subscription.topic
            ),
        ));
    }
}

/// A topic runtime's grouping must be well formed, and it may not
/// claim the topic scope while subscriptions of that topic hold it.
fn check_topic_runtime(
    candidate: &WorkspaceState,
    topic: &Id,
    value: &TopicRuntime,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    let subject = SymbolKey::TopicRuntime(topic.clone());

    if !candidate.topics.contains_key(topic) {
        diagnostics.push(DraftDiagnostic::new(
            Some(subject.clone()),
            format!("topic {topic} is not declared"),
        ));

        return;
    }

    if value.ordering == Some(crate::spec::OrderingSemantics::WithinGroup)
        && value.grouping.is_none()
    {
        diagnostics.push(DraftDiagnostic::new(
            Some(subject.clone()),
            "ordering `within_group` needs a grouping at the same scope to be \
             interpreted over"
                .to_string(),
        ));
    }

    if !value.declares_transport_semantics() {
        return;
    }

    for (operation, inputs) in &candidate.runtime.subscriptions {
        for (input, runtime) in inputs {
            if !runtime.declares_transport_semantics() {
                continue;
            }

            let Some(Input::Subscription(subscription)) = candidate
                .operations
                .get(operation)
                .and_then(|draft| draft.inputs.get(input))
            else {
                continue;
            };

            if &subscription.topic == topic {
                diagnostics.push(DraftDiagnostic::new(
                    Some(subject.clone()),
                    format!(
                        "{input} of {operation} already declares its own transport \
                         semantics for {topic}, so the topic may not declare them for \
                         every subscription"
                    ),
                ));
            }
        }
    }
}

/// A storage layout must name a declared object and carry a key.
fn check_storage_layout(
    candidate: &WorkspaceState,
    layout: &Id,
    value: &crate::spec::StorageLayout,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    let subject = SymbolKey::StorageLayout(layout.clone());

    if value.partition_key.is_empty() {
        diagnostics.push(DraftDiagnostic::new(
            Some(subject.clone()),
            "a partition key must name at least one field".to_string(),
        ));
    }

    let declared = candidate
        .data_models
        .get(&value.object.data_model)
        .is_some_and(|data_model| data_model.objects.contains_key(&value.object.object));

    if !declared {
        diagnostics.push(DraftDiagnostic::new(
            Some(subject),
            format!(
                "data object {}/{} is not declared",
                value.object.data_model, value.object.object
            ),
        ));
    }
}

/// The outbox runtime's gate checks: the boundary exists and is an
/// outbox input, and the dispatch pool is declared. Partition-mapping
/// shape is left to whole-model validation — unlike the transport
/// scope invariant, a defect there poisons no sibling declaration.
fn check_outbox_runtime(
    candidate: &WorkspaceState,
    operation: &Id,
    input: &Id,
    value: &crate::spec::OutboxRuntime,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    let subject = SymbolKey::OutboxRuntime {
        operation: operation.clone(),
        input: input.clone(),
    };

    check_boundary(
        candidate,
        subject.clone(),
        operation,
        input,
        BoundaryKind::Outbox,
        diagnostics,
    );

    check_pool(candidate, subject, &value.dispatch.pool, diagnostics);
}

#[derive(Clone, Copy)]
enum BoundaryKind {
    Request,
    Subscription,
    Outbox,
}

impl BoundaryKind {
    fn label(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Subscription => "subscription",
            Self::Outbox => "outbox",
        }
    }

    fn matches(self, input: &Input) -> bool {
        matches!(
            (self, input),
            (Self::Request, Input::Request(_))
                | (Self::Subscription, Input::Subscription(_))
                | (Self::Outbox, Input::Outbox(_))
        )
    }
}

fn check_boundary(
    candidate: &WorkspaceState,
    subject: SymbolKey,
    operation: &Id,
    input: &Id,
    expected: BoundaryKind,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    let Some(draft) = candidate.operations.get(operation) else {
        diagnostics.push(DraftDiagnostic::new(
            Some(subject),
            format!("operation {operation} is not declared; plan its interface first"),
        ));

        return;
    };

    let Some(declared) = draft.inputs.get(input) else {
        diagnostics.push(DraftDiagnostic::new(
            Some(subject),
            format!("{operation} declares no input {input}"),
        ));

        return;
    };

    if !expected.matches(declared) {
        diagnostics.push(DraftDiagnostic::new(
            Some(subject),
            format!(
                "{input} of {operation} is not a {} input",
                expected.label()
            ),
        ));
    }
}

fn check_pool(
    candidate: &WorkspaceState,
    subject: SymbolKey,
    pool: &Id,
    diagnostics: &mut Vec<DraftDiagnostic>,
) {
    if !candidate.runtime.execution_pools.contains_key(pool) {
        diagnostics.push(DraftDiagnostic::new(
            Some(subject),
            format!("execution pool {pool} is not declared"),
        ));
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
