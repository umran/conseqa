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
    TopologySynthesis,
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
            Self::TopologySynthesis => "topology_synthesis",
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
/// holds the program of its operation. Runtime topology is not its
/// to write: where an invocation executes is an architectural
/// decision about the whole system.
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
    /// Full authoring authority over every symbol, deletion included —
    /// the coordinator grant. Assigned to an interactive human session
    /// (§6.2), which authors freely including operations it creates
    /// mid-session. It relaxes only write scope; read-before-reference,
    /// draft validation, and OCC still apply. Never assigned to a
    /// concurrent synthesis agent, which gets a narrow scope so the
    /// scheduler keeps ownership boundaries.
    All,

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

    OperationInterface(Id),

    /// Authority over the L1 runtime topology: topic runtimes,
    /// subscription runtimes, execution pools, routers, and storage
    /// layouts. Held separately from the skeleton so a run may hand
    /// topology to a dedicated authority, though the coordinator holds
    /// both by default.
    RuntimeTopology,
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

    /// The decomposer's scope: the L0 shared skeleton.
    ///
    /// Deliberately not the runtime topology. L1 exists to discharge
    /// serialization and ordering requirements, and at decomposition
    /// no requirement has been discovered yet — authoring topology
    /// there is guessing at facts the run has not established.
    /// [`Self::runtime_topology`] holds it instead, once the unproven
    /// set says what the runtime has to achieve.
    pub fn shared_skeleton() -> Self {
        Self::of([WriteGrant::SharedSkeleton])
    }

    /// The topology author's scope: the whole L1 runtime model, and
    /// nothing else.
    ///
    /// Held by one task at a time. Where invocations execute, what
    /// groups them, and how many run at once are facts about the
    /// system as a whole; splitting them per operation would let two
    /// workers declare contradictory halves of one pool.
    pub fn runtime_topology() -> Self {
        Self::of([WriteGrant::RuntimeTopology])
    }

    /// An operation-synthesis task's scope: the operation's program.
    ///
    /// Deliberately not the runtime topology. Where an invocation
    /// executes is an architectural decision about the whole system,
    /// and one operation's synthesis is the wrong place to make it.
    pub fn operation_synthesis(operation: Id) -> Self {
        Self::of([WriteGrant::OperationProgram(operation)])
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
        WriteGrant::All => true,

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

        WriteGrant::RuntimeTopology => matches!(
            mutation,
            Mutation::PutTopicRuntime { .. }
                | Mutation::PutSubscriptionRuntime { .. }
                | Mutation::PutOutboxRuntime { .. }
                | Mutation::PutExecutionPool { .. }
                | Mutation::PutRouter { .. }
                | Mutation::PutStorageLayout { .. }
        ),

        WriteGrant::TopLevelSymbol(symbol) => match mutation {
            Mutation::DeleteTopLevel { symbol: target } => symbol == target,
            other => other.write_target() == *symbol,
        },

        WriteGrant::Operation(operation) => match mutation {
            Mutation::PutOperationInterface { operation: target, .. }
            | Mutation::ReplaceOperationProgram { operation: target, .. }
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

/// What a task must do to be complete. Normal tasks end after one
/// commit — a task's snapshot is immutable, so a second patch against
/// the same snapshot would conflict with the task's own first commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskCompletionGate {
    #[default]
    SinglePatch,

    /// An interactive authoring session (§6.2): one human driving one
    /// session with no concurrent agents. On each successful commit the
    /// engine rolls the session's capability token to a fresh successor
    /// task pinned to the new head, so the session can commit repeatedly
    /// while every individual task keeps its frozen-snapshot invariant.
    Interactive,
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
///
/// Every write scope in the workflow is narrow, so this is the only way
/// work crosses one. A program worker needing a schema field files one;
/// so does the L1 topology author when no grouping key can carry a
/// serialization key because the message schema does not carry the
/// field at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyRequest {
    pub id: DependencyRequestId,
    pub task: TaskId,
    pub target: SymbolKey,
    pub requested_change: String,
    pub reason: String,
    pub evidence: Vec<EvidenceRef>,

    /// How the request was settled, or `None` while it is still open.
    /// An open request blocks the run's success condition (§75).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<DependencyResolution>,
}

/// How a dependency request was settled.
///
/// Determined by observing the target symbol, not by asking the
/// repairing agent to self-report: a symbol whose version advanced was
/// changed, and one whose version did not was not. That keeps the
/// outcome a fact about the workspace rather than a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyResolution {
    /// The target symbol changed. Whether the change is the one asked
    /// for is the requester's judgment on its next attempt.
    Applied,

    /// The repair ran and the target symbol did not change: the owner
    /// judged no change was needed, or could not make one.
    Declined,
}

impl fmt::Display for DependencyResolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Applied => "applied",
            Self::Declined => "declined",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{WriteGrant, WriteScope};
    use crate::confluence::patch::Mutation;
    use crate::spec::{Id, OutboxDispatch, OutboxOrdering, OutboxPartitioning, OutboxRuntime};

    // The topology author's grant must cover every L1 mutation. The
    // outbox runtime is the one added after the grant table was first
    // written, so it is the one a regression would drop: without it the
    // fanout's topology worker is told it owns "outbox partitioning,
    // ordering, and dispatch" and then has its `put_outbox_runtime`
    // rejected as out of scope.
    #[test]
    fn the_topology_grant_covers_outbox_runtimes() {
        let mutation = Mutation::PutOutboxRuntime {
            operation: Id("operation.dispatch".to_string()),
            input: Id("input.dispatch.outbox".to_string()),
            value: OutboxRuntime {
                partitioning: OutboxPartitioning::None,
                ordering: OutboxOrdering::None,
                dispatch: OutboxDispatch {
                    pool: Id("pool.dispatchers".to_string()),
                    routing: None,
                    batching: None,
                },
            },
        };

        assert_eq!(
            WriteScope::runtime_topology().violation(&mutation),
            None,
            "the runtime-topology grant must authorize put_outbox_runtime"
        );

        assert!(
            WriteScope::of([WriteGrant::SharedSkeleton])
                .violation(&mutation)
                .is_some(),
            "an outbox runtime is L1: the skeleton grant must not cover it"
        );
    }
}
