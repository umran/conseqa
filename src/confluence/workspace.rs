//! The confluence authoring workspace: the shared architecture state
//! agents synthesize concurrently.
//!
//! This is deliberately not `spec::Model`. During synthesis the model
//! may be incomplete — operations planned but not yet programmed,
//! requirements not yet discovered, references not yet resolvable —
//! and that drafting state is represented here as authoring metadata
//! rather than by weakening the normative DSL. A real `Model` is
//! assembled from a workspace only once every required draft is
//! complete.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::spec::{
    DataModel, ExecutionSemantics, Id, IdempotencyRequirement, Input, Model, Operation,
    OperationBlock, OperationRequirements, OrderingRequirement, RecoverabilityRequirement,
    Revision, Schema, SerializationRequirement, Service, StateMachine, Topic,
};

use super::symbol::RequirementFamily;

/// Why a workspace cannot yet become a `Model`, operation by
/// operation and field by field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("the workspace is not assemblable: {}", self.describe())]
pub struct AssemblyError {
    pub gaps: Vec<AssemblyGap>,
}

impl AssemblyError {
    fn describe(&self) -> String {
        self.gaps
            .iter()
            .map(AssemblyGap::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssemblyGap {
    #[error("operation {operation} has no program")]
    MissingProgram { operation: Id },

    #[error("operation {operation} has no execution facts")]
    MissingExecution { operation: Id },
}

/// The authoritative shared architecture state at one revision.
///
/// Owned by the confluence engine; agents interact with it only
/// through tracked reads and the serializable commit gate. The
/// canonical `conseqa.yaml` is materialized from it only at
/// finalization or explicit export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceState {
    pub revision: Revision,

    pub services: BTreeMap<Id, Service>,
    pub schemas: BTreeMap<Id, Schema>,
    pub data_models: BTreeMap<Id, DataModel>,
    pub topics: BTreeMap<Id, Topic>,
    pub state_machines: BTreeMap<Id, StateMachine>,

    pub operations: BTreeMap<Id, DraftOperation>,

    pub prompt_obligations: BTreeMap<PromptObligationId, PromptObligation>,
    pub requirement_proposals: Vec<RequirementProposal>,

    pub run_meta: RunMetadata,
}

impl WorkspaceState {
    /// An empty workspace at revision 0, before any commit.
    pub fn empty(run_meta: RunMetadata) -> Self {
        Self {
            revision: Revision(0),
            services: BTreeMap::new(),
            schemas: BTreeMap::new(),
            data_models: BTreeMap::new(),
            topics: BTreeMap::new(),
            state_machines: BTreeMap::new(),
            operations: BTreeMap::new(),
            prompt_obligations: BTreeMap::new(),
            requirement_proposals: Vec::new(),
            run_meta,
        }
    }

    /// Assembles a real `Model` from the workspace (§8). Succeeds only
    /// when every draft carries a program and execution facts; the
    /// error names every gap precisely. Structural validation is the
    /// analyzer's judgment over the assembled model, never implied
    /// here.
    pub fn assemble_model(&self) -> Result<Model, AssemblyError> {
        let mut gaps = Vec::new();
        let mut operations = BTreeMap::new();

        for (id, draft) in &self.operations {
            match (&draft.program, &draft.execution) {
                (Some(program), Some(execution)) => {
                    operations.insert(
                        id.clone(),
                        Operation {
                            service: draft.service.clone(),
                            description: draft.description.clone(),
                            inputs: draft.inputs.clone(),
                            program: program.clone(),
                            requirements: draft.requirements.clone(),
                            execution: execution.clone(),
                        },
                    );
                }

                (program, execution) => {
                    if program.is_none() {
                        gaps.push(AssemblyGap::MissingProgram {
                            operation: id.clone(),
                        });
                    }

                    if execution.is_none() {
                        gaps.push(AssemblyGap::MissingExecution {
                            operation: id.clone(),
                        });
                    }
                }
            }
        }

        if !gaps.is_empty() {
            return Err(AssemblyError { gaps });
        }

        Ok(Model {
            revision: self.revision,
            services: self.services.clone(),
            schemas: self.schemas.clone(),
            data_models: self.data_models.clone(),
            topics: self.topics.clone(),
            state_machines: self.state_machines.clone(),
            operations,
        })
    }

    /// A workspace holding an existing complete model: every operation
    /// becomes a fully populated draft, ready for assembly. This is how
    /// a confluence run adopts an existing `conseqa.yaml`.
    pub fn from_model(model: &Model, run_meta: RunMetadata) -> Self {
        Self {
            revision: model.revision,
            services: model.services.clone(),
            schemas: model.schemas.clone(),
            data_models: model.data_models.clone(),
            topics: model.topics.clone(),
            state_machines: model.state_machines.clone(),
            operations: model
                .operations
                .iter()
                .map(|(id, operation)| (id.clone(), DraftOperation::from_operation(operation)))
                .collect(),
            prompt_obligations: BTreeMap::new(),
            requirement_proposals: Vec::new(),
            run_meta,
        }
    }
}

/// An operation under authorship.
///
/// The interface — service, description, inputs — is established by
/// decomposition before operation fanout, so callers can reason
/// against a stable callee contract while the callee's program is
/// still being synthesized. The program, execution facts, and
/// requirements arrive through later scoped commits.
///
/// This type is confluence-authoring metadata, not a DSL primitive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftOperation {
    pub service: Id,
    pub description: Option<String>,
    pub inputs: BTreeMap<Id, Input>,

    /// None until the operation synthesis task commits.
    pub program: Option<OperationBlock>,

    /// None until operation synthesis determines execution facts.
    pub execution: Option<ExecutionSemantics>,

    /// The adopted requirements. Proposals and their provenance live
    /// on the workspace ([`RequirementProposal`]); adoption per run
    /// policy is what lands one here.
    pub requirements: OperationRequirements,

    pub stage: OperationDraftStage,
}

impl DraftOperation {
    pub fn from_operation(operation: &Operation) -> Self {
        Self {
            service: operation.service.clone(),
            description: operation.description.clone(),
            inputs: operation.inputs.clone(),
            program: Some(operation.program.clone()),
            execution: Some(operation.execution.clone()),
            requirements: operation.requirements.clone(),
            stage: OperationDraftStage::ReadyForAssembly,
        }
    }

    /// A draft holding only an interface: how decomposition plans an
    /// operation before its body exists.
    pub fn planned(interface: OperationInterfaceDraft) -> Self {
        Self {
            service: interface.service,
            description: interface.description,
            inputs: interface.inputs,
            program: None,
            execution: None,
            requirements: OperationRequirements::default(),
            stage: OperationDraftStage::Planned,
        }
    }

    /// The caller-facing contract slice of this draft.
    pub fn interface(&self) -> OperationInterfaceDraft {
        OperationInterfaceDraft {
            service: self.service.clone(),
            description: self.description.clone(),
            inputs: self.inputs.clone(),
        }
    }

    /// Recomputes the mechanical part of the stage from what the draft
    /// holds. `ReadyForAssembly` is a workflow sign-off, not a derived
    /// fact, so it is preserved once reached.
    pub fn recompute_stage(&mut self) {
        if self.stage == OperationDraftStage::ReadyForAssembly {
            return;
        }

        self.stage = if self.program.is_none() || self.execution.is_none() {
            OperationDraftStage::Planned
        } else if self.has_requirements() {
            OperationDraftStage::RequirementsProposed
        } else {
            OperationDraftStage::ProgramProposed
        };
    }

    fn has_requirements(&self) -> bool {
        !(self.requirements.serialization.is_empty()
            && self.requirements.ordering.is_empty()
            && self.requirements.idempotency.is_empty()
            && self.requirements.recoverability.is_empty())
    }

    /// Whether the draft carries everything assembly needs.
    pub fn assemblable(&self) -> bool {
        self.program.is_some() && self.execution.is_some()
    }
}

/// How far along an operation draft is. Authoring metadata for
/// scheduling and diagnostics; assembly itself gates only on the
/// presence of a program and execution facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationDraftStage {
    Planned,
    ProgramProposed,
    RequirementsProposed,
    ReadyForAssembly,
}

/// The caller-facing contract of an operation: what decomposition
/// establishes before fanout, and what callers may rely on while the
/// body is synthesized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationInterfaceDraft {
    pub service: Id,
    pub description: Option<String>,
    pub inputs: BTreeMap<Id, Input>,
}

/// Identity of one explicit correctness statement extracted from the
/// user's prompt.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PromptObligationId(pub String);

impl fmt::Display for PromptObligationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An explicit correctness statement from the prompt, tracked
/// separately from DSL requirements so multi-agent synthesis cannot
/// silently drop it. Finalization fails while any obligation remains
/// `Unmapped`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptObligation {
    /// The prompt text the obligation was extracted from, when
    /// available.
    pub source_span: Option<String>,

    /// The obligation restated as one normalized correctness intent.
    pub normalized_intent: String,

    /// Operations the obligation concerns.
    pub targets: Vec<Id>,

    pub status: PromptObligationStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptObligationStatus {
    Unmapped,

    /// Discharged into declared requirements.
    Mapped { requirements: Vec<RequirementRef> },

    /// The current DSL cannot express the obligation.
    UnsupportedByCurrentDsl { reason: String },

    ExplicitlyWaivedByUser,
}

/// Names one declared requirement: the operation, the family, and the
/// position in that family's list at the time of reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementRef {
    pub operation: Id,
    pub family: RequirementFamily,
    pub index: usize,
}

/// A requirement an agent proposed, with its provenance. Proposals are
/// recorded outside the normative DSL; adoption per run policy is what
/// moves a requirement into `DraftOperation::requirements`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementProposal {
    pub operation: Id,
    pub requirement: ProposedRequirement,
    pub origin: RequirementOrigin,
    pub status: ProposalStatus,
}

/// One proposed requirement, in the family it belongs to. Idempotency
/// carries its result-replay setting exactly as the DSL declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "family", content = "requirement", rename_all = "snake_case")]
pub enum ProposedRequirement {
    Serialization(SerializationRequirement),
    Ordering(OrderingRequirement),
    Idempotency(IdempotencyRequirement),
    Recoverability(RecoverabilityRequirement),
}

impl ProposedRequirement {
    pub fn family(&self) -> RequirementFamily {
        match self {
            Self::Serialization(_) => RequirementFamily::Serialization,
            Self::Ordering(_) => RequirementFamily::Ordering,
            Self::Idempotency(_) => RequirementFamily::Idempotency,
            Self::Recoverability(_) => RequirementFamily::Recoverability,
        }
    }
}

/// Why a requirement was proposed. Adoption policy keys off this:
/// explicit prompt obligations are always adopted; strongly implied
/// requirements under strict policy; recommendations only when the run
/// opts in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequirementOrigin {
    ExplicitPrompt {
        obligation: PromptObligationId,
    },

    StronglyImplied {
        rationale: String,
        evidence: Vec<EvidenceRef>,
    },

    Recommended {
        rationale: String,
        evidence: Vec<EvidenceRef>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposalStatus {
    /// Recorded; adoption not yet decided.
    Proposed,

    /// Adopted into the operation's declared requirements.
    Adopted { reference: RequirementRef },

    /// Recorded as advisory under the run policy; not adopted.
    Advisory,

    /// An equivalent requirement was already declared or adopted.
    Duplicate { reference: RequirementRef },
}

/// A pointer to evidence backing a proposal or request: a prompt span,
/// a report obligation id, a source location. Deliberately loose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EvidenceRef(pub String);

/// Identity of one confluence run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub String);

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Facts about the run the workspace belongs to: the prompt being
/// designed against and the policies commits are judged under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunMetadata {
    pub run: RunId,

    /// The exact natural-language application prompt, persisted
    /// verbatim.
    pub prompt: Option<String>,

    pub policy: RunPolicy,
}

impl RunMetadata {
    pub fn new(run: RunId) -> Self {
        Self {
            run,
            prompt: None,
            policy: RunPolicy::default(),
        }
    }
}

/// Run-level policy for requirement adoption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPolicy {
    /// Under strict architecture policy, strongly implied requirement
    /// proposals are auto-adopted.
    pub strict_requirements: bool,

    /// Whether `Recommended` proposals are adopted rather than
    /// recorded as advisory.
    pub adopt_recommended: bool,
}

impl Default for RunPolicy {
    fn default() -> Self {
        Self {
            strict_requirements: true,
            adopt_recommended: false,
        }
    }
}
