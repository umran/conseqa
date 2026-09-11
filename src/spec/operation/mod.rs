pub mod effect;
pub mod idempotency;
pub mod input;
pub mod program;
pub mod result;
pub mod state_machine;
pub mod transaction;
pub mod value;

pub use effect::*;
pub use idempotency::*;
pub use input::*;
pub use program::*;
pub use result::*;
use serde::{Deserialize, Serialize};
pub use state_machine::*;
pub use transaction::*;
pub use value::*;

use std::collections::BTreeMap;

use super::Id;

/// An operation: its invocation sources, one explicit causal program,
/// and requirements.
///
/// An operation declares no execution-concurrency fact. Runtime
/// concurrency is a property of the execution resource an invocation
/// is assigned to, and is declared exclusively by
/// [`ExecutionPool::member_concurrency`](crate::spec::ExecutionPool).
///
/// Execution-local transactions, direct effects, transaction outputs,
/// and effect intents are declared at the program or transaction site
/// that executes or establishes them. They are not predeclared as
/// operation-level capabilities or handles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub service: Id,
    pub description: Option<String>,

    pub inputs: BTreeMap<Id, Input>,

    /// The operation's declared entry synchronization, if any. Not a
    /// program step: the lock brackets the whole program, and its
    /// absence is epistemic — no synchronization fact, not evidence
    /// that invocations overlap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_lock: Option<InvocationLock>,

    /// The operation's one explicit control structure — the source of
    /// truth for every operation-owned execution occurrence.
    pub program: OperationBlock,

    pub requirements: OperationRequirements,
}

/// An exclusive lock acquired at operation entry and held to the
/// invocation's terminal.
///
/// Semantics: the key is evaluated from the invocation context before
/// any program step executes, the exclusive lock on that evaluated key
/// is acquired before the first step, and it is released when the
/// invocation reaches `return` or `complete`. Two invocations whose
/// evaluated keys are equal therefore never execute their operation
/// programs concurrently.
///
/// The declaration asserts the abstract exclusion guarantee, not a
/// mechanism: advisory database locks, distributed mutexes, and fenced
/// lock services are all conforming realizations. It is L0 — no
/// routing, pool, or handoff fact participates — which is what makes
/// it the one serialization proof route that survives any change of
/// runtime topology.
///
/// Three non-implications are load-bearing. The lock establishes no
/// ordering: acquisition makes no FIFO guarantee, so same-key
/// invocations exclude one another in no particular order. It is not a
/// transaction [`Lock`](super::Lock) step, which protects selected
/// object instances for a transaction's span; this one guards the
/// whole invocation under a semantic key. And async effects permitted
/// to outlive the operation terminal (§16) are not implicitly kept
/// under it after terminal — the lock spans the program, not the
/// effect lifetimes that escape it.
///
/// The key must be evaluable at entry, before any step: only an input
/// payload exists there, so the key's source must be an input of the
/// operation — and its only input, because every invocation acquires
/// the lock and an invocation triggered by another input carries no
/// value for the key. Validation enforces both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationLock {
    pub key: ValueRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRequirements {
    pub serialization: Vec<SerializationRequirement>,
    pub ordering: Vec<OrderingRequirement>,
    pub idempotency: Vec<IdempotencyRequirement>,
    pub recoverability: Vec<RecoverabilityRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerializationRequirement {
    pub key: ValueRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderingRequirement {
    pub key: ValueRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdempotencyRequirement {
    pub key: IdempotencyKey,
    pub result: ResultReplayRequirement,
}

/// Whether repeated attempts that return a request result must return
/// the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultReplayRequirement {
    /// No replay-stability requirement is declared for the result. This
    /// does not waive the requirement's side-effect obligation.
    Unspecified,

    /// Repeated admitted attempts in the same logical idempotency class
    /// that return a request result must return the same result
    /// variant and a replay-equivalent payload.
    ReplayConsistent,
}

/// An obligation that a logical invocation reaches a valid terminal of
/// the operation program.
///
/// This is a progress obligation and is deliberately separate from
/// `IdempotencyRequirement`, which is a safety obligation. Idempotency
/// constrains what repeated attempts may do; it is satisfied vacuously
/// by never retrying, and therefore says nothing about whether the
/// remaining steps of an interrupted program ever execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverabilityRequirement {
    /// Identity of the logical invocation that must reach a terminal.
    ///
    /// Attempts sharing this key are attempts at the same logical
    /// invocation, so re-driving one of them continues that invocation
    /// rather than starting a new one.
    pub key: IdempotencyKey,

    /// How strongly completion must be established.
    pub completion: CompletionRequirement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionRequirement {
    /// An interrupted attempt must be able to resume and drive the
    /// program to a `Return` or `Complete` terminal.
    ///
    /// The solver must establish that every prefix at which the
    /// invocation may fail admits a continuation: already-committed
    /// transactions resolve on re-encounter, and every artifact a
    /// later step consumes is replay-available.
    ///
    /// This does not oblige the architecture to actually re-drive the
    /// invocation.
    Resumable,

    /// In addition to resumability, the architecture must guarantee
    /// that the logical invocation is re-driven until a terminal is
    /// reached.
    ///
    /// This is a liveness obligation and additionally requires a
    /// modeled retry driver, such as at-least-once delivery on the
    /// triggering subscription or an inbound request effect that may
    /// repeat.
    Guaranteed,
}
