//! Agent tasks: the unit of pinned reasoning and write authority.
//!
//! A task is permanently tied to one snapshot revision, one agent
//! session, one read tracker, and one write scope (§17 of the
//! confluence spec). An invalidated task never runs again — its
//! replacement is a new task in a fresh agent session, because stale
//! semantic facts may remain latent inside the old conversation
//! (§2.6).

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::spec::{Id, Revision};

use super::patch::Mutation;
use super::symbol::SymbolKey;
use super::workspace::EvidenceRef;

/// Identity of one agent task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(pub Uuid);

impl TaskId {
    pub fn fresh() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task-{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub id: TaskId,
    pub kind: TaskKind,
    pub objective: String,

    /// The one immutable revision every shared-architecture read of
    /// this task resolves against.
    pub snapshot_revision: Revision,

    pub write_scope: WriteScope,

    pub prompt_evidence: Vec<PromptEvidence>,

    pub budget: TaskBudget,

    pub completion_gate: TaskCompletionGate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Decompose,
    OperationSynthesis,
    RequirementDiscovery,
    RequirementRepair,
    SharedDependencyRepair,
    DependencyReview,
}

impl fmt::Display for TaskKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Decompose => "decompose",
            Self::OperationSynthesis => "operation_synthesis",
            Self::RequirementDiscovery => "requirement_discovery",
            Self::RequirementRepair => "requirement_repair",
            Self::SharedDependencyRepair => "shared_dependency_repair",
            Self::DependencyReview => "dependency_review",
        })
    }
}

/// Task lifecycle. `Invalidated` is terminal for commit authority: a
/// task never transitions back to `Running`; its replacement is a new
/// task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Planned,
    Running,
    Invalidated,
    Committing,
    Committed,
    DependencyRequested,
    Unresolved,
    Failed,
    Cancelled,
    Completed,
}

impl TaskState {
    /// Whether the task may still read and submit.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Committing)
    }
}

impl fmt::Display for TaskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Planned => "planned",
            Self::Running => "running",
            Self::Invalidated => "invalidated",
            Self::Committing => "committing",
            Self::Committed => "committed",
            Self::DependencyRequested => "dependency_requested",
            Self::Unresolved => "unresolved",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Completed => "completed",
        })
    }
}

/// The semantic write capabilities of one task. A scope is a set of
/// grants because typical assignments pair them — operation synthesis
/// holds the program and the execution facts of its operation.
///
/// The scheduler creates scopes; an agent can never select its own.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WriteScope {
    pub grants: Vec<WriteGrant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum WriteGrant {
    /// Create or replace shared-skeleton symbols: services, schemas,
    /// data models, topics, state machines, operation interfaces, and
    /// prompt obligations. Deletion is not included — it is a
    /// separately granted, normally coordinator-only capability.
    SharedSkeleton,

    /// Full authority over one specific top-level symbol, deletion
    /// included.
    TopLevelSymbol(SymbolKey),

    /// Full authority over one operation's draft.
    Operation(Id),

    OperationProgram(Id),

    OperationRequirements(Id),

    OperationExecution(Id),

    OperationInterface(Id),
}

impl WriteScope {
    pub fn none() -> Self {
        Self { grants: Vec::new() }
    }

    pub fn of(grants: impl IntoIterator<Item = WriteGrant>) -> Self {
        Self {
            grants: grants.into_iter().collect(),
        }
    }

    /// The decomposer's scope: the shared skeleton, interfaces
    /// included.
    pub fn shared_skeleton() -> Self {
        Self::of([WriteGrant::SharedSkeleton])
    }

    /// An operation-synthesis task's scope: the operation's program
    /// and execution facts.
    pub fn operation_synthesis(operation: Id) -> Self {
        Self::of([
            WriteGrant::OperationProgram(operation.clone()),
            WriteGrant::OperationExecution(operation),
        ])
    }

    pub fn requirement_discovery(operation: Id) -> Self {
        Self::of([WriteGrant::OperationRequirements(operation)])
    }

    pub fn requirement_repair(operation: Id) -> Self {
        Self::of([WriteGrant::OperationProgram(operation)])
    }

    /// The symbol a mutation is not authorized to touch, if any.
    pub fn violation(&self, mutation: &Mutation) -> Option<SymbolKey> {
        let target = mutation.write_target();

        if self.authorizes(mutation) {
            None
        } else {
            Some(target)
        }
    }

    fn authorizes(&self, mutation: &Mutation) -> bool {
        self.grants.iter().any(|grant| grant_covers(grant, mutation))
    }
}

fn grant_covers(grant: &WriteGrant, mutation: &Mutation) -> bool {
    match grant {
        WriteGrant::SharedSkeleton => matches!(
            mutation,
            Mutation::PutService { .. }
                | Mutation::PutSchema { .. }
                | Mutation::PutDataModel { .. }
                | Mutation::PutTopic { .. }
                | Mutation::PutStateMachine { .. }
                | Mutation::PutOperationInterface { .. }
                | Mutation::PutPromptObligation { .. }
        ),

        WriteGrant::TopLevelSymbol(symbol) => match mutation {
            Mutation::DeleteTopLevel { symbol: target } => symbol == target,
            other => other.write_target() == *symbol,
        },

        WriteGrant::Operation(operation) => match mutation {
            Mutation::PutOperationInterface { operation: target, .. }
            | Mutation::ReplaceOperationProgram { operation: target, .. }
            | Mutation::ReplaceOperationExecution { operation: target, .. }
            | Mutation::ReplaceOperationRequirements { operation: target, .. }
            | Mutation::ProposeRequirements { operation: target, .. } => operation == target,
            _ => false,
        },

        WriteGrant::OperationProgram(operation) => matches!(
            mutation,
            Mutation::ReplaceOperationProgram { operation: target, .. } if operation == target
        ),

        WriteGrant::OperationRequirements(operation) => matches!(
            mutation,
            Mutation::ReplaceOperationRequirements { operation: target, .. }
                | Mutation::ProposeRequirements { operation: target, .. }
                if operation == target
        ),

        WriteGrant::OperationExecution(operation) => matches!(
            mutation,
            Mutation::ReplaceOperationExecution { operation: target, .. } if operation == target
        ),

        WriteGrant::OperationInterface(operation) => matches!(
            mutation,
            Mutation::PutOperationInterface { operation: target, .. } if operation == target
        ),
    }
}

/// An excerpt of the user's prompt handed to a task as evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptEvidence {
    pub source: EvidenceRef,
    pub excerpt: String,
}

/// Advisory resource bounds for one task. Enforcement belongs to the
/// harness supervisor, not the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBudget {
    pub max_wall_time_secs: Option<u64>,
    pub max_tokens: Option<u64>,
    pub max_usd_cents: Option<u64>,
}

/// What a task must do to be complete. V1 tasks end after one commit —
/// a task's snapshot is immutable, so a second patch against the same
/// snapshot would conflict with the task's own first commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskCompletionGate {
    #[default]
    SinglePatch,
}

/// Identity of one dependency request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DependencyRequestId(pub Uuid);

impl DependencyRequestId {
    pub fn fresh() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for DependencyRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dep-{}", self.0)
    }
}

/// An out-of-scope change request: how an agent asks for a mutation it
/// is not authorized to make itself (§19). The scheduler routes it to
/// the symbol's owner or the coordinator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyRequest {
    pub id: DependencyRequestId,
    pub task: TaskId,
    pub target: SymbolKey,
    pub requested_change: String,
    pub reason: String,
    pub evidence: Vec<EvidenceRef>,
}
